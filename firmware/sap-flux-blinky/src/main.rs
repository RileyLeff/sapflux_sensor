#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let mut blue_cathode = Output::new(p.PB3, Level::High, Speed::Low);
    let mut red_cathode = Output::new(p.PB4, Level::High, Speed::Low);
    let mut accidental_anode = Output::new(p.PB5, Level::High, Speed::Low);

    loop {
        accidental_anode.set_high();
        blue_cathode.set_high();
        red_cathode.set_high();
        Timer::after_millis(1_000).await;

        red_cathode.set_low();
        Timer::after_millis(1_000).await;
        red_cathode.set_high();
        Timer::after_millis(700).await;

        blue_cathode.set_low();
        Timer::after_millis(1_000).await;
        blue_cathode.set_high();
    }
}
