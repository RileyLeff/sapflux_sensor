#![no_std]
#![no_main]

use core::fmt::Write;

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::usart::{Config as UartConfig, Uart};
use embassy_time::Timer;
use heapless::String;
use panic_halt as _;

struct Rs485<'d> {
    uart: Uart<'d, embassy_stm32::mode::Blocking>,
    de: Output<'d>,
    n_re: Output<'d>,
}

impl<'d> Rs485<'d> {
    async fn send(&mut self, bytes: &[u8]) {
        self.n_re.set_high();
        self.de.set_high();
        Timer::after_millis(2).await;

        let _ = self.uart.blocking_write(bytes);
        let _ = self.uart.blocking_flush();

        Timer::after_millis(2).await;
        self.de.set_low();
        self.n_re.set_low();
    }

    async fn line(&mut self, line: &str) {
        self.send(line.as_bytes()).await;
        self.send(b"\r\n").await;
    }

    fn read_byte(&mut self) -> u8 {
        self.de.set_low();
        self.n_re.set_low();

        let mut byte = [0_u8; 1];
        loop {
            if self.uart.blocking_read(&mut byte).is_ok() {
                return byte[0];
            }
        }
    }
}

#[derive(Clone, Copy)]
enum MuxMode {
    Disabled,
    Therm1,
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = sap_flux_blinky::init(Default::default());

    let mut heater_gate = Output::new(p.PA8, Level::Low, Speed::Low);
    let mut excitation = Output::new(p.PC6, Level::High, Speed::Low);
    let mut mux_en_a = Output::new(p.PA9, Level::High, Speed::Low);
    let mut mux_en_b = Output::new(p.PA10, Level::High, Speed::Low);
    let mut mux_en_c = Output::new(p.PA11, Level::High, Speed::Low);
    let mut mux_s0 = Output::new(p.PD0, Level::High, Speed::Low);
    let mut mux_s1 = Output::new(p.PD1, Level::Low, Speed::Low);
    let mut mux_s2 = Output::new(p.PD2, Level::Low, Speed::Low);
    let mut mux_s3 = Output::new(p.PD3, Level::Low, Speed::Low);
    let mut _eeprom_wp = Output::new(p.PA12, Level::High, Speed::Low);
    let mut _flash_cs = Output::new(p.PB12, Level::High, Speed::VeryHigh);

    heater_gate.set_low();
    excitation.set_high();
    disable_muxes(&mut mux_en_a, &mut mux_en_b, &mut mux_en_c);
    select_y1(&mut mux_s0, &mut mux_s1, &mut mux_s2, &mut mux_s3);
    _eeprom_wp.set_high();
    _flash_cs.set_high();

    let mut uart_config = UartConfig::default();
    uart_config.baudrate = 115_200;
    let uart = Uart::new_blocking(p.USART2, p.PA3, p.PA2, uart_config).unwrap();
    let de = Output::new(p.PA1, Level::Low, Speed::Low);
    let n_re = Output::new(p.PA0, Level::Low, Speed::Low);
    let mut rs485 = Rs485 { uart, de, n_re };

    let mut excitation_on = true;
    let mut mux_mode = MuxMode::Disabled;
    report_status(&mut rs485, excitation_on, mux_mode).await;

    loop {
        match rs485.read_byte() {
            b'd' | b'D' => {
                mux_mode = MuxMode::Disabled;
                disable_muxes(&mut mux_en_a, &mut mux_en_b, &mut mux_en_c);
                select_y1(&mut mux_s0, &mut mux_s1, &mut mux_s2, &mut mux_s3);
            }
            b'1' => {
                mux_mode = MuxMode::Therm1;
                select_y1(&mut mux_s0, &mut mux_s1, &mut mux_s2, &mut mux_s3);
                mux_en_a.set_low();
                mux_en_b.set_high();
                mux_en_c.set_high();
            }
            b'h' | b'H' => {
                excitation_on = true;
                excitation.set_high();
            }
            b'x' | b'X' => {
                excitation_on = false;
                excitation.set_low();
            }
            _ => {}
        }

        heater_gate.set_low();
        report_status(&mut rs485, excitation_on, mux_mode).await;
    }
}

fn disable_muxes(en_a: &mut Output<'_>, en_b: &mut Output<'_>, en_c: &mut Output<'_>) {
    en_a.set_high();
    en_b.set_high();
    en_c.set_high();
}

fn select_y1(s0: &mut Output<'_>, s1: &mut Output<'_>, s2: &mut Output<'_>, s3: &mut Output<'_>) {
    s0.set_high();
    s1.set_low();
    s2.set_low();
    s3.set_low();
}

async fn report_status(rs485: &mut Rs485<'_>, excitation_on: bool, mux_mode: MuxMode) {
    let mut line: String<128> = String::new();
    let mux = match mux_mode {
        MuxMode::Disabled => "disabled",
        MuxMode::Therm1 => "mux A/Y1 therm1 selected",
    };
    let _ = write!(
        &mut line,
        "excitation-hold: EXC {}, {}; heater off; commands d,1,h,x,?",
        if excitation_on { "HIGH" } else { "LOW" },
        mux
    );
    rs485.line(&line).await;
}
