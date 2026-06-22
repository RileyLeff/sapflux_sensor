#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::usart::{Config, Uart};
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let mut de = Output::new(p.PA1, Level::Low, Speed::Low);
    let mut n_re = Output::new(p.PA0, Level::Low, Speed::Low);

    let mut blue_cathode = Output::new(p.PB3, Level::High, Speed::Low);
    let mut red_cathode = Output::new(p.PB4, Level::High, Speed::Low);
    let mut accidental_anode = Output::new(p.PB5, Level::High, Speed::Low);

    let mut config = Config::default();
    config.baudrate = 115_200;
    let mut uart = Uart::new_blocking(p.USART2, p.PA3, p.PA2, config).unwrap();

    loop {
        accidental_anode.set_high();
        blue_cathode.set_high();
        red_cathode.set_low();
        Timer::after_millis(40).await;
        red_cathode.set_high();

        n_re.set_high();
        de.set_high();
        Timer::after_millis(2).await;

        let _ = uart.blocking_write(b"sap-flux rs485 alive\r\n");
        let _ = uart.blocking_flush();

        Timer::after_millis(2).await;
        de.set_low();
        n_re.set_low();

        blue_cathode.set_low();
        Timer::after_millis(40).await;
        blue_cathode.set_high();

        Timer::after_millis(920).await;
    }
}
