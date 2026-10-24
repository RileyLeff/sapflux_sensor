//! `latch-sleep` (off by default, NOT bench-verified): sleep by powering
//! the whole Feather off.
//!
//! The PCF85063A (I2C 0x51 on TWIM1, P0.26/P0.27) is set to a relative
//! time of day 1 00:00:00 and an alarm `sleep_s` later (seconds, minutes,
//! hours and day matched; weekday ignored). Then NRF_PS_EN (P0.31) goes low.
//! On battery the TPS63031 turns off; the alarm's !INT is expected to
//! re-enable it through the enable diode-OR (docs/field-gateway-design.md),
//! and every wake is a cold boot (new boot_id, clock re-synced by CheckIn).
//!
//! Evidence gap: the I2C pins and address come from the NCS v2.1.2 board
//! files; that !INT reaches the regulator enable on the v1–v3 boards is
//! from the design doc only (the schematic on hand is a later USB-C rev
//! where !INT goes to a jumper). Verify on the bench before enabling.
//!
//! If power stays on (USB/VBUS present, or the latch does not drop) this
//! returns after 3 s and the caller sleeps on the timer instead. The alarm
//! is then cleared so !INT is not left asserted.

use embassy_nrf::gpio::Output;
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{Peri, peripherals};
use embassy_time::Timer;

use crate::info;

const ADDR: u8 = 0x51;
const REG_CONTROL_2: u8 = 0x01;
const REG_SECONDS: u8 = 0x04;
const REG_SECOND_ALARM: u8 = 0x0B;
/// Control_2: AIE (alarm interrupt enable).
const AIE: u8 = 1 << 7;
/// Alarm registers: AEN_x = 1 disables matching on that field.
const AEN_OFF: u8 = 1 << 7;

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Alarm fields (second, minute, hour, day) for a wake `sleep_s` after
/// day 1 00:00:00. `sleep_s` is clamped to 1 s .. 28 days.
pub fn alarm_fields(sleep_s: u32) -> [u8; 4] {
    let s = sleep_s.clamp(1, 28 * 86_400);
    let day = 1 + s / 86_400;
    let rem = s % 86_400;
    [bcd(rem % 60), bcd(rem / 60 % 60), bcd(rem / 3600), bcd(day)]
}

pub async fn power_off_for(
    twim: Peri<'_, peripherals::SERIAL1>,
    sda: Peri<'_, peripherals::P0_26>,
    scl: Peri<'_, peripherals::P0_27>,
    ps_en: &mut Output<'_>,
    sleep_s: u32,
) {
    let mut ram = [0u8; 16];
    let mut i2c = Twim::new(twim, crate::Irqs, sda, scl, twim::Config::default(), &mut ram);
    // Time: 00:00:00, day 1, weekday 0, month 1, year 00 (12/24h: 24 h is
    // the Control_1 reset default).
    let set_time = [REG_SECONDS, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00];
    let [s, m, h, d] = alarm_fields(sleep_s);
    let set_alarm = [REG_SECOND_ALARM, s, m, h, d, AEN_OFF];
    let ok = i2c.write(ADDR, &set_time).await.is_ok()
        && i2c.write(ADDR, &set_alarm).await.is_ok()
        // AIE set, AF cleared.
        && i2c.write(ADDR, &[REG_CONTROL_2, AIE]).await.is_ok();
    if !ok {
        info!("latch-sleep: PCF85063A not answering; timer sleep");
        return;
    }
    info!("latch-sleep: alarm in {} s, dropping NRF_PS_EN", sleep_s);
    Timer::after_millis(20).await; // let the log line drain
    ps_en.set_low();
    Timer::after_secs(3).await;
    // Still running: restore the latch and disarm the alarm.
    ps_en.set_high();
    let _ = i2c.write(ADDR, &[REG_CONTROL_2, 0x00]).await;
}
