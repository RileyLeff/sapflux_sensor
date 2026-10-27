//! Stop 1 between measurements, woken by the time base or the host link.
//!
//! The system clock is HSI16 with no PLL, which is also the clock Stop exits
//! on, so nothing needs restoring afterwards. LPTIM1 (the embassy time base,
//! on the LSE) keeps counting in Stop and its CMP/ARRM interrupts end it; the
//! IWDG keeps counting too, so callers sleep well under its timeout.
//!
//! The link's RX pin also wakes the core: an EXTI line on its falling edge
//! (PA13 for LPUART2 on J6, PA3 for USART2 behind the SP3485). The UART clock
//! is stopped while the line starts toggling, so the byte that woke the core
//! (the host's `0x00` wake byte, `docs/field-protocol.md`) is lost or garbled;
//! the host waits ≥ 10 ms before its first command. The interrupt is enabled
//! only while stopped, so UART traffic never interrupts the awake firmware.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use embassy_time::Instant;
use stm32_metapac as pac;
use stm32_metapac::interrupt;

/// PWR_CR1.LPMS: Stop 1 (low-power regulator). With FPD_STOP the internal
/// flash is off too: 3.7 uA instead of 7.5 uA at 3 V, 9 us wakeup instead of
/// 5 us (STM32G0B1 datasheet, Stop 1 tables).
const LPMS_STOP1: u8 = 0b001;

static RX_LINE: AtomicUsize = AtomicUsize::new(0);
static RX_EDGE: AtomicBool = AtomicBool::new(false);

/// The UART RX pin whose falling edge ends a Stop.
#[derive(Clone, Copy)]
pub enum RxPin {
    /// USART2 RX from the SP3485 receiver.
    Pa3,
    /// LPUART2 RX on J6 p2.
    Pa13,
}

/// Select Stop 1 and route the EXTI line of `pin` to its falling edge. Call
/// once at boot.
pub fn init(pin: RxPin) {
    let (interrupt, line) = match pin {
        RxPin::Pa3 => (pac::Interrupt::EXTI2_3, 3),
        RxPin::Pa13 => (pac::Interrupt::EXTI4_15, 13),
    };
    RX_LINE.store(line, Ordering::Relaxed);
    let exti = pac::EXTI;
    exti.exticr(line / 4).modify(|w| w.set_exti(line % 4, 0)); // port A
    exti.ftsr(0).modify(|w| w.set_line(line, true));
    exti.imr(0).modify(|w| w.set_line(line, false));
    pac::PWR.cr1().modify(|w| {
        w.set_lpms(LPMS_STOP1);
        w.set_fpd_stop(true);
    });
    unsafe { cortex_m::peripheral::NVIC::unmask(interrupt) };
}

/// Keep the debug port alive in Stop (DBGMCU_CR.DBG_STOP), so SWD can
/// attach to a sleeping board. The clocks then run in Stop: bench use only.
pub fn keep_debug_in_stop() {
    pac::RCC.apbenr1().modify(|w| w.set_dbgen(true));
    pac::DBGMCU.cr().modify(|w| w.set_dbg_stop(true));
}

/// Stop until `deadline` or a falling edge on the RX pin, whichever comes
/// first. Returns whether the RX edge woke it. The caller must have an
/// embassy timer due at (or before) `deadline`, or only the 2 s LPTIM
/// overflow ends the sleep.
pub fn stop_until(deadline: Instant) -> bool {
    let exti = pac::EXTI;
    let line = RX_LINE.load(Ordering::Relaxed);
    RX_EDGE.store(false, Ordering::Relaxed);
    exti.fpr(0).write(|w| w.set_line(line, true));
    exti.imr(0).modify(|w| w.set_line(line, true));
    // SAFETY: single core; only this function touches SCR.SLEEPDEEP.
    let mut scb = unsafe { cortex_m::Peripherals::steal() }.SCB;
    scb.set_sleepdeep();
    // WFE, not WFI: an interrupt taken between the check and the sleep sets
    // the event flag, so the WFE returns at once instead of missing it.
    while !RX_EDGE.load(Ordering::Relaxed) && Instant::now() < deadline {
        cortex_m::asm::wfe();
    }
    scb.clear_sleepdeep();
    exti.imr(0).modify(|w| w.set_line(line, false));
    RX_EDGE.load(Ordering::Relaxed)
}

fn on_rx_edge() {
    let exti = pac::EXTI;
    let line = RX_LINE.load(Ordering::Relaxed);
    if exti.fpr(0).read().line(line) {
        exti.fpr(0).write(|w| w.set_line(line, true));
        RX_EDGE.store(true, Ordering::Relaxed);
    }
}

#[interrupt]
fn EXTI2_3() {
    on_rx_edge();
}

#[interrupt]
fn EXTI4_15() {
    on_rx_edge();
}
