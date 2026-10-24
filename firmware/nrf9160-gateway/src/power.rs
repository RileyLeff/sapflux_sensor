//! Board peripherals that cost sleep current, and the battery reading.
//!
//! Pin map from the Zephyr board files for `circuitdojo_feather_nrf9160`
//! (NCS v2.1.2, the micro-USB generation). Bench-verify on this board:
//!
//! - LIS2DH12 at 0x18 on I2C (TWIM1, SDA P0.26, SCL P0.27): it powers up in
//!   power-down mode, but its SDO/SA0 pull-up to VDD conducts into the SA0
//!   strap; CTRL_REG0 = 0x90 disconnects that pull-up.
//! - W25Q32 SPI flash (CS P0.07, SCK P0.11, MOSI P0.09, WP P0.08, HOLD
//!   P0.10): standby is ~10–25 µA, deep power-down (0xB9) ~1 µA. The command
//!   is bit-banged; CS, WP and HOLD are then held high for the whole run.
//! - Battery: AIN7 (P0.20) through a 100 k/100 k divider enabled by P0.25.

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::saadc::{self, ChannelConfig, Saadc, Time};
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{Peri, peripherals};
use embassy_time::Timer;

use crate::info;

const LIS2DH12_ADDR: u8 = 0x18;
const LIS2DH12_WHO_AM_I: u8 = 0x0F;
const LIS2DH12_CTRL_REG0: u8 = 0x1E;
const LIS2DH12_CTRL_REG1: u8 = 0x20;

/// Put the accelerometer in its lowest-current state. Returns true if it
/// answered.
pub async fn accel_off(
    twim: Peri<'_, peripherals::SERIAL1>,
    sda: Peri<'_, peripherals::P0_26>,
    scl: Peri<'_, peripherals::P0_27>,
) -> bool {
    let mut buf = [0u8; 4];
    let mut i2c = Twim::new(twim, crate::Irqs, sda, scl, twim::Config::default(), &mut buf);
    let mut who = [0u8; 1];
    let ok = i2c
        .write_read(LIS2DH12_ADDR, &[LIS2DH12_WHO_AM_I], &mut who)
        .await
        .is_ok();
    if !ok || who[0] != 0x33 {
        info!("power: LIS2DH12 not found (ok={}, who=0x{:02x})", ok, who[0]);
        return false;
    }
    // ODR = 0 (power-down), axes enabled (reset value 0x07).
    let r1 = i2c.write(LIS2DH12_ADDR, &[LIS2DH12_CTRL_REG1, 0x07]).await;
    // SDO_PU_DISC = 1 plus the mandatory 0b0010000 bits.
    let r0 = i2c.write(LIS2DH12_ADDR, &[LIS2DH12_CTRL_REG0, 0x90]).await;
    info!("power: LIS2DH12 power-down {:?}/{:?}", r1.is_ok(), r0.is_ok());
    r0.is_ok() && r1.is_ok()
}

/// Flash control pins, held high after deep power-down.
pub struct FlashPins {
    _cs: Output<'static>,
    _wp: Output<'static>,
    _hold: Output<'static>,
}

pub fn flash_deep_power_down(
    cs: Peri<'static, peripherals::P0_07>,
    sck: Peri<'static, peripherals::P0_11>,
    mosi: Peri<'static, peripherals::P0_09>,
    wp: Peri<'static, peripherals::P0_08>,
    hold: Peri<'static, peripherals::P0_10>,
) -> FlashPins {
    let mut cs = Output::new(cs, Level::High, OutputDrive::Standard);
    let wp = Output::new(wp, Level::High, OutputDrive::Standard);
    let hold = Output::new(hold, Level::High, OutputDrive::Standard);
    {
        let mut sck = Output::new(sck, Level::Low, OutputDrive::Standard);
        let mut mosi = Output::new(mosi, Level::Low, OutputDrive::Standard);
        cortex_m::asm::delay(64);
        cs.set_low();
        cortex_m::asm::delay(64);
        // SPI mode 0, MSB first: data valid on the rising edge.
        let cmd: u8 = 0xB9;
        for bit in (0..8).rev() {
            if cmd & (1 << bit) != 0 {
                mosi.set_high();
            } else {
                mosi.set_low();
            }
            cortex_m::asm::delay(32);
            sck.set_high();
            cortex_m::asm::delay(32);
            sck.set_low();
        }
        cortex_m::asm::delay(32);
        cs.set_high();
        // sck/mosi drop here → disconnected inputs.
    }
    FlashPins {
        _cs: cs,
        _wp: wp,
        _hold: hold,
    }
}

/// Battery voltage in mV, or 0 if implausible.
pub async fn battery_mv(
    adc: Peri<'_, peripherals::SAADC>,
    ain: Peri<'_, peripherals::P0_20>,
    enable: Peri<'_, peripherals::P0_25>,
) -> u16 {
    let _en = Output::new(enable, Level::High, OutputDrive::Standard);
    Timer::after_millis(2).await;
    let mut ch = ChannelConfig::single_ended(ain);
    // 100 k/100 k source: 50 kΩ → 40 µs acquisition.
    ch.time = Time::_40US;
    let mut saadc = Saadc::new(adc, crate::Irqs, saadc::Config::default(), [ch]);
    saadc.calibrate().await;
    let mut buf = [0i16; 1];
    saadc.sample(&mut buf).await;
    drop(saadc);
    let raw = buf[0].max(0) as u32;
    // Gain 1/6, 0.6 V reference → 3.6 V full scale at 12 bits; ×2 divider.
    let mv = raw * 7200 / 4096;
    if (1000..=5000).contains(&mv) { mv as u16 } else { 0 }
}
