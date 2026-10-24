#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::usart::{Config, Uart};
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = sap_flux_blinky::init(Default::default());

    let mut de = Output::new(p.PA1, Level::Low, Speed::Low);
    let mut n_re = Output::new(p.PA0, Level::Low, Speed::Low);

    let mut blue_cathode = Output::new(p.PB3, Level::High, Speed::Low);
    let mut red_cathode = Output::new(p.PB4, Level::High, Speed::Low);
    let mut accidental_anode = Output::new(p.PB5, Level::High, Speed::Low);

    let mut config = Config::default();
    config.baudrate = 115_200;
    let mut uart = Uart::new_blocking(p.USART2, p.PA3, p.PA2, config).unwrap();

    accidental_anode.set_high();
    red_cathode.set_low();
    Timer::after_millis(80).await;
    red_cathode.set_high();

    loop {
        de.set_low();
        n_re.set_low();

        let mut byte = [0_u8; 1];
        if uart.blocking_read(&mut byte).is_err() {
            continue;
        }

        blue_cathode.set_low();
        Timer::after_millis(20).await;
        blue_cathode.set_high();

        n_re.set_high();
        de.set_high();
        Timer::after_millis(2).await;

        let _ = uart.blocking_write(b"echo: ");
        let _ = uart.blocking_write(&byte);
        let _ = uart.blocking_write(b"\r\n");
        let _ = uart.blocking_flush();

        Timer::after_millis(2).await;
        de.set_low();
        n_re.set_low();
    }
}
