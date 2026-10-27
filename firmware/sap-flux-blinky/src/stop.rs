//! Stop 1 between measurements, woken by the time base or the field link.
//!
//! The system clock is HSI16 with no PLL, which is also the clock Stop exits
//! on, so nothing needs restoring afterwards. LPTIM1 (the embassy time base,
//! on the LSE) keeps counting in Stop and its CMP/ARRM interrupts end it; the
//! IWDG keeps counting too, so callers sleep well under its timeout.
//!
//! The LPUART2 RX pin (PA13) also wakes the core: EXTI line 13 on its falling
//! edge. The UART clock is stopped while the line starts toggling, so the
//! byte that woke the core (the host's `0x00` wake byte,
//! `docs/field-protocol.md`) is lost or garbled; the host waits ≥ 10 ms before
//! its first command. The interrupt is enabled only while stopped, so UART
//! traffic never interrupts the awake firmware.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_time::Instant;
use stm32_metapac as pac;
use stm32_metapac::interrupt;

const RX_LINE: usize = 13;
/// PWR_CR1.LPMS: Stop 1 (low-power regulator). With FPD_STOP the internal
/// flash is off too: 3.7 uA instead of 7.5 uA at 3 V, 9 us wakeup instead of
/// 5 us (STM32G0B1 datasheet, Stop 1 tables).
const LPMS_STOP1: u8 = 0b001;

static RX_EDGE: AtomicBool = AtomicBool::new(false);

/// Route EXTI line 13 to PA13 on falling edges. Call once at boot.
pub fn init_rx_wake() {
    let exti = pac::EXTI;
    exti.exticr(RX_LINE / 4).modify(|w| w.set_exti(RX_LINE % 4, 0)); // port A
    exti.ftsr(0).modify(|w| w.set_line(RX_LINE, true));
    exti.imr(0).modify(|w| w.set_line(RX_LINE, false));
    pac::PWR.cr1().modify(|w| {
        w.set_lpms(LPMS_STOP1);
        w.set_fpd_stop(true);
    });
    unsafe { cortex_m::peripheral::NVIC::unmask(pac::Interrupt::EXTI4_15) };
}

/// Stop until `deadline` or a falling edge on PA13, whichever comes first.
/// Returns whether the RX edge woke it. The caller must have an embassy
/// timer due at (or before) `deadline`, or only the 2 s LPTIM overflow ends
/// the sleep.
pub fn stop_until(deadline: Instant) -> bool {
    let exti = pac::EXTI;
    RX_EDGE.store(false, Ordering::Relaxed);
    exti.fpr(0).write(|w| w.set_line(RX_LINE, true));
    exti.imr(0).modify(|w| w.set_line(RX_LINE, true));
    // SAFETY: single core; only this function touches SCR.SLEEPDEEP.
    let mut scb = unsafe { cortex_m::Peripherals::steal() }.SCB;
    scb.set_sleepdeep();
    // WFE, not WFI: an interrupt taken between the check and the sleep sets
    // the event flag, so the WFE returns at once instead of missing it.
    while !RX_EDGE.load(Ordering::Relaxed) && Instant::now() < deadline {
        cortex_m::asm::wfe();
    }
    scb.clear_sleepdeep();
    exti.imr(0).modify(|w| w.set_line(RX_LINE, false));
    RX_EDGE.load(Ordering::Relaxed)
}

#[interrupt]
fn EXTI4_15() {
    let exti = pac::EXTI;
    if exti.fpr(0).read().line(RX_LINE) {
        exti.fpr(0).write(|w| w.set_line(RX_LINE, true));
        RX_EDGE.store(true, Ordering::Relaxed);
    }
}
