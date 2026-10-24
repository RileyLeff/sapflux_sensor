#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();
    config.rcc.ls = embassy_stm32::rcc::LsConfig::default_lse();
    let p = embassy_stm32::init(config);

    let mut heater_gate = Output::new(p.PA8, Level::Low, Speed::Low);
    let mut watchdog_done = Output::new(p.PA5, Level::Low, Speed::Low);
    let mut red = Output::new(p.PB3, Level::High, Speed::Low);
    let mut green = Output::new(p.PB4, Level::High, Speed::Low);
    let mut blue = Output::new(p.PB5, Level::High, Speed::Low);

    heater_gate.set_low();

    loop {
        red.set_low();
        Timer::after_millis(700).await;
        red.set_high();

        green.set_low();
        Timer::after_millis(700).await;
        green.set_high();

        blue.set_low();
        Timer::after_millis(700).await;
        blue.set_high();

        Timer::after_millis(400).await;

        watchdog_done.set_high();
        Timer::after_millis(1).await;
        watchdog_done.set_low();
        heater_gate.set_low();
    }
}
