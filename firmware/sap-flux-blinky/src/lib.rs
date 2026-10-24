//! Shared board support for the sap-flux firmware binaries.
#![no_std]

pub mod timebase;
pub mod w25q;

/// `embassy_stm32::init` followed by starting the LSE/LPTIM1 time base.
///
/// Every binary in this package must use this instead of calling
/// `embassy_stm32::init` directly, or `embassy_time` never advances.
pub fn init(config: embassy_stm32::Config) -> embassy_stm32::Peripherals {
    let p = embassy_stm32::init(config);
    timebase::init();
    p
}
