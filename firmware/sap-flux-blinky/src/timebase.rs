//! embassy-time driver on LPTIM1 clocked from the 32.768 kHz LSE crystal.
//!
//! Why not embassy-stm32's own `time-driver-lptim1`? In embassy-stm32 0.6 it
//! does not compile for STM32G0 (`rcc::Lptimsel` does not exist in the G0
//! PAC), the G0 RCC code reports `lse: None` to the clock tree (so the
//! LPTIM1 kernel-clock lookup would panic even if it compiled), and the
//! driver reads `LPTIM_CNT` once although RM0444 requires two matching
//! consecutive reads when LPTIM runs from an asynchronous clock.
//!
//! Tick rate is 32 768 Hz (`embassy-time/tick-hz-32_768`), one tick per LSE
//! cycle (PRESC = /1). `Instant::as_micros()` is still microseconds; it is
//! just quantized to 30.52 µs.
//!
//! Design:
//! - ARR = 0xFFFF, continuous mode. The 64-bit tick count is extended in
//!   software from 16-bit counter deltas inside a critical section, so it
//!   can never step backwards as long as `now()` runs at least once per
//!   65 536 ticks (2 s). The ARRM interrupt guarantees that.
//! - RM0444 forbids writing LPTIM_IER while LPTIM is enabled, so both ARRM
//!   and CMPM interrupts are enabled once at init and never touched again.
//!   A CMPM match for a stale compare value just re-evaluates the queue.
//! - Alarms closer than [`MIN_LEAD_TICKS`] are not programmed into CMP (the
//!   write takes a few LSE cycles to land); `set_alarm` returns `false` and
//!   the queue loop spins until they expire (at most ~150 µs).
//! - If the LSE is not running when `init()` is called, it is started; if
//!   it still does not start, LPTIM1 falls back to the LSI (32 kHz, so time
//!   then runs about 2.3 % slow) and [`lse_fallback`] reports it. The heater
//!   pulse itself is timed by TIM1 on HSI16 and does not depend on this.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::{CriticalSection, Mutex};
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;
use stm32_metapac as pac;
use stm32_metapac::interrupt;

/// Alarms nearer than this are handled by spinning instead of CMP.
const MIN_LEAD_TICKS: u64 = 5;
/// Alarms farther than this are left to the next ARRM interrupt (they can
/// then fire up to 16 ticks late; no code here waits longer than 2 s).
const MAX_CMP_AHEAD_TICKS: u64 = 0xFFF0;

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static LSE_FALLBACK: AtomicBool = AtomicBool::new(false);

struct State {
    /// 64-bit tick count at the last counter observation.
    ticks: u64,
    /// Raw counter at the last observation.
    last_cnt: u16,
    /// Raw counter value that corresponds to tick 0.
    cnt_base: u16,
    /// Next alarm (u64::MAX = none).
    alarm_at: u64,
    /// A CMP write is in flight (CMPOK not yet observed).
    cmp_write_pending: bool,
}

struct LptimDriver {
    state: Mutex<RefCell<State>>,
    queue: Mutex<RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: LptimDriver = LptimDriver {
    state: Mutex::new(RefCell::new(State {
        ticks: 0,
        last_cnt: 0,
        cnt_base: 0,
        alarm_at: u64::MAX,
        cmp_write_pending: false,
    })),
    queue: Mutex::new(RefCell::new(Queue::new())),
});

fn read_cnt() -> u16 {
    // RM0444 §22.4.14: with an asynchronous counter clock, read until two
    // consecutive reads agree.
    loop {
        let a = pac::LPTIM1.cnt().read().cnt();
        let b = pac::LPTIM1.cnt().read().cnt();
        if a == b {
            return a;
        }
    }
}

impl LptimDriver {
    fn now_cs(&self, cs: CriticalSection) -> u64 {
        if !INITIALIZED.load(Ordering::Relaxed) {
            return 0;
        }
        let cnt = read_cnt();
        let mut st = self.state.borrow_ref_mut(cs);
        let delta = cnt.wrapping_sub(st.last_cnt);
        st.ticks += delta as u64;
        st.last_cnt = cnt;
        st.ticks
    }

    fn write_cmp(&self, cs: CriticalSection, value: u16) {
        let r = pac::LPTIM1;
        let mut st = self.state.borrow_ref_mut(cs);
        if st.cmp_write_pending {
            // A previous write must complete before the next one (~2 LSE
            // cycles). Bounded: the LPTIM clock is always running here.
            for _ in 0..20_000 {
                if r.isr().read().cmpok(0) {
                    break;
                }
            }
        }
        r.icr().write(|w| w.set_cmpokcf(0, true));
        r.cmp().write(|w| w.set_cmp(value));
        st.cmp_write_pending = true;
    }

    /// Program the hardware for `at`. Returns false if `at` is already due
    /// (or too close to program), in which case the caller must dequeue.
    fn set_alarm(&self, cs: CriticalSection, at: u64) -> bool {
        let now = self.now_cs(cs);
        self.state.borrow_ref_mut(cs).alarm_at = at;
        if at == u64::MAX {
            return true;
        }
        if at <= now + MIN_LEAD_TICKS {
            self.state.borrow_ref_mut(cs).alarm_at = u64::MAX;
            return false;
        }
        if at - now <= MAX_CMP_AHEAD_TICKS {
            let base = self.state.borrow_ref(cs).cnt_base;
            self.write_cmp(cs, (at as u16).wrapping_add(base));
            // Re-check after the (possibly slow) write.
            let now = self.now_cs(cs);
            if at <= now + 1 {
                self.state.borrow_ref_mut(cs).alarm_at = u64::MAX;
                return false;
            }
        }
        true
    }

    fn service(&self, cs: CriticalSection) {
        let mut now = self.now_cs(cs);
        let alarm_at = self.state.borrow_ref(cs).alarm_at;
        if alarm_at == u64::MAX {
            return;
        }
        if alarm_at <= now + MIN_LEAD_TICKS {
            loop {
                let next = self.queue.borrow_ref_mut(cs).next_expiration(now);
                if self.set_alarm(cs, next) {
                    break;
                }
                now = self.now_cs(cs);
            }
        } else if alarm_at - now <= MAX_CMP_AHEAD_TICKS {
            // Far alarm that has come within range: (re)program it.
            if !self.set_alarm(cs, alarm_at) {
                loop {
                    now = self.now_cs(cs);
                    let next = self.queue.borrow_ref_mut(cs).next_expiration(now);
                    if self.set_alarm(cs, next) {
                        break;
                    }
                }
            }
        }
    }

    fn on_interrupt(&self) {
        let r = pac::LPTIM1;
        critical_section::with(|cs| {
            let isr = r.isr().read();
            r.icr().write(|w| {
                w.set_arrmcf(isr.arrm());
                w.set_cccf(0, isr.ccif(0));
            });
            if isr.cmpok(0) {
                self.state.borrow_ref_mut(cs).cmp_write_pending = false;
            }
            self.service(cs);
        });
    }
}

impl Driver for LptimDriver {
    fn now(&self) -> u64 {
        critical_section::with(|cs| self.now_cs(cs))
    }

    fn schedule_wake(&self, at: u64, waker: &core::task::Waker) {
        critical_section::with(|cs| {
            let changed = self.queue.borrow_ref_mut(cs).schedule_wake(at, waker);
            if changed {
                let mut now = self.now_cs(cs);
                loop {
                    let next = self.queue.borrow_ref_mut(cs).next_expiration(now);
                    if self.set_alarm(cs, next) {
                        break;
                    }
                    now = self.now_cs(cs);
                }
            }
        });
    }
}

#[interrupt]
fn TIM6_DAC_LPTIM1() {
    DRIVER.on_interrupt();
}

/// True if the LSE failed to start and LPTIM1 runs from the LSI instead.
pub fn lse_fallback() -> bool {
    LSE_FALLBACK.load(Ordering::Relaxed)
}

fn lse_ready() -> bool {
    pac::RCC.bdcr().read().lserdy()
}

/// Busy-wait about `ms` milliseconds at the reset HSI16 clock (no timer yet).
fn spin_ms(ms: u32) {
    cortex_m::asm::delay(16_000 * ms);
}

/// Start LPTIM1 as the embassy time base. Call once, right after
/// `embassy_stm32::init` (which may rewrite the LPTIM1 clock mux).
pub fn init() {
    let rcc = pac::RCC;

    if !lse_ready() {
        // Bins that don't configure the LSE through embassy: start it here.
        rcc.apbenr1().modify(|w| w.set_pwren(true));
        pac::PWR.cr1().modify(|w| w.set_dbp(true));
        spin_ms(1);
        rcc.bdcr().modify(|w| {
            w.set_lsedrv(pac::rcc::vals::Lsedrv::MEDIUM_HIGH);
            w.set_lseon(true);
        });
        for _ in 0..2_000 {
            if lse_ready() {
                break;
            }
            spin_ms(1);
        }
    }

    let source = if lse_ready() {
        pac::rcc::vals::Lptim1sel::LSE
    } else {
        rcc.csr().modify(|w| w.set_lsion(true));
        for _ in 0..100 {
            if rcc.csr().read().lsirdy() {
                break;
            }
            spin_ms(1);
        }
        LSE_FALLBACK.store(true, Ordering::Relaxed);
        pac::rcc::vals::Lptim1sel::LSI
    };

    rcc.ccipr().modify(|w| w.set_lptim1sel(source));
    rcc.apbenr1().modify(|w| w.set_lptim1en(true));
    rcc.apbrstr1().modify(|w| w.set_lptim1rst(true));
    rcc.apbrstr1().modify(|w| w.set_lptim1rst(false));

    let r = pac::LPTIM1;
    // CFGR and IER may only be written while LPTIM is disabled.
    r.cfgr().write(|w| w.set_presc(pac::lptim::vals::Presc::DIV1));
    r.ier().write(|w| {
        w.set_arrmie(true);
        w.set_ccie(0, true);
    });
    r.cr().write(|w| w.set_enable(true));
    // ENABLE takes effect after a couple of kernel clocks.
    spin_ms(1);
    r.icr().write(|w| w.set_arrokcf(true));
    r.arr().write(|w| w.set_arr(0xFFFF));
    for _ in 0..1_000 {
        if r.isr().read().arrok() {
            break;
        }
        spin_ms(1);
    }
    r.cmp().write(|w| w.set_cmp(0x8000));
    r.cr().modify(|w| w.set_cntstrt(true));

    critical_section::with(|cs| {
        let cnt = read_cnt();
        let mut st = DRIVER.state.borrow_ref_mut(cs);
        st.ticks = 0;
        st.last_cnt = cnt;
        st.cnt_base = cnt;
        st.alarm_at = u64::MAX;
        st.cmp_write_pending = true;
        INITIALIZED.store(true, Ordering::Relaxed);
    });
    r.icr().write(|w| {
        w.set_arrmcf(true);
        w.set_cccf(0, true);
    });

    unsafe {
        cortex_m::peripheral::NVIC::unpend(pac::Interrupt::TIM6_DAC_LPTIM1);
        cortex_m::peripheral::NVIC::unmask(pac::Interrupt::TIM6_DAC_LPTIM1);
    }
}
