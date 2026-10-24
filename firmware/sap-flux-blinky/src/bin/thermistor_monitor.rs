#![no_std]
#![no_main]

use core::fmt::Write;

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c, Master as I2cMaster};
use embassy_stm32::mode::Blocking;
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{Config as UartConfig, Uart};
use embassy_time::Timer;
use heapless::String;
use panic_halt as _;

const ADS1115_ADDR: u8 = 0x48;
const ADS_MUX_AIN0_GND: u16 = 0x4000;
const ADS_MUX_AIN1_GND: u16 = 0x5000;

type BoardI2c<'d> = I2c<'d, Blocking, I2cMaster>;

struct Rs485<'d> {
    uart: Uart<'d, Blocking>,
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
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = sap_flux_blinky::init(Default::default());

    let mut heater_gate = Output::new(p.PA8, Level::Low, Speed::Low);
    let watchdog_done = Output::new(p.PA5, Level::Low, Speed::Low);
    spawner.spawn(watchdog_keepalive(watchdog_done).unwrap());

    let mut excitation = Output::new(p.PC6, Level::Low, Speed::Low);
    let mut mux_en_a = Output::new(p.PA9, Level::High, Speed::Low);
    let mut mux_en_b = Output::new(p.PA10, Level::High, Speed::Low);
    let mut mux_en_c = Output::new(p.PA11, Level::High, Speed::Low);
    let mut mux_s0 = Output::new(p.PD0, Level::Low, Speed::Low);
    let mut mux_s1 = Output::new(p.PD1, Level::Low, Speed::Low);
    let mut mux_s2 = Output::new(p.PD2, Level::Low, Speed::Low);
    let mut mux_s3 = Output::new(p.PD3, Level::Low, Speed::Low);

    let mut _eeprom_wp = Output::new(p.PA12, Level::High, Speed::Low);
    let mut _flash_cs = Output::new(p.PB12, Level::High, Speed::VeryHigh);
    let mut _led_red = Output::new(p.PB3, Level::High, Speed::Low);
    let mut _led_green = Output::new(p.PB4, Level::High, Speed::Low);
    let mut _led_blue = Output::new(p.PB5, Level::High, Speed::Low);

    heater_gate.set_low();
    excitation.set_low();
    disable_muxes(&mut mux_en_a, &mut mux_en_b, &mut mux_en_c);

    let mut uart_config = UartConfig::default();
    uart_config.baudrate = 115_200;
    let uart = Uart::new_blocking(p.USART2, p.PA3, p.PA2, uart_config).unwrap();
    let de = Output::new(p.PA1, Level::Low, Speed::Low);
    let n_re = Output::new(p.PA0, Level::Low, Speed::Low);
    let mut rs485 = Rs485 { uart, de, n_re };

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = Hertz::khz(100);
    i2c_config.sda_pullup = true;
    i2c_config.scl_pullup = true;
    let mut i2c = I2c::new_blocking(p.I2C1, p.PB6, p.PB7, i2c_config);

    rs485
        .line("thermistor-monitor ready; heater hard-off; T1-T6 on mux A")
        .await;

    // External mux channels corresponding to thermistor inputs T1 through T6.
    let mux_channels = [1_u8, 0, 3, 2, 5, 4];

    loop {
        heater_gate.set_low();
        disable_muxes(&mut mux_en_a, &mut mux_en_b, &mut mux_en_c);
        excitation.set_high();
        Timer::after_millis(5).await;

        let exc_before = read_average(&mut i2c, ADS_MUX_AIN1_GND).await;
        let mut therm = [None; 6];

        mux_en_a.set_low();
        for (index, mux_channel) in mux_channels.iter().enumerate() {
            select_mux_channel(
                *mux_channel,
                &mut mux_s0,
                &mut mux_s1,
                &mut mux_s2,
                &mut mux_s3,
            );
            Timer::after_millis(2).await;
            therm[index] = read_average(&mut i2c, ADS_MUX_AIN0_GND).await;
        }
        mux_en_a.set_high();

        let exc_after = read_average(&mut i2c, ADS_MUX_AIN1_GND).await;
        excitation.set_low();
        heater_gate.set_low();

        let mut line: String<192> = String::new();
        let _ = write!(&mut line, "sample");
        append_reading(&mut line, " exc0", exc_before);
        for (index, value) in therm.iter().enumerate() {
            let _ = write!(&mut line, " t{}=", index + 1);
            append_value(&mut line, *value);
        }
        append_reading(&mut line, " exc1", exc_after);
        rs485.line(&line).await;

        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn watchdog_keepalive(mut done: Output<'static>) {
    loop {
        done.set_high();
        Timer::after_millis(1).await;
        done.set_low();
        Timer::after_secs(1).await;
    }
}

fn disable_muxes(en_a: &mut Output<'_>, en_b: &mut Output<'_>, en_c: &mut Output<'_>) {
    en_a.set_high();
    en_b.set_high();
    en_c.set_high();
}

fn select_mux_channel(
    channel: u8,
    s0: &mut Output<'_>,
    s1: &mut Output<'_>,
    s2: &mut Output<'_>,
    s3: &mut Output<'_>,
) {
    s0.set_level(if channel & 0x01 != 0 {
        Level::High
    } else {
        Level::Low
    });
    s1.set_level(if channel & 0x02 != 0 {
        Level::High
    } else {
        Level::Low
    });
    s2.set_level(if channel & 0x04 != 0 {
        Level::High
    } else {
        Level::Low
    });
    s3.set_level(if channel & 0x08 != 0 {
        Level::High
    } else {
        Level::Low
    });
}

async fn read_average(i2c: &mut BoardI2c<'_>, mux_bits: u16) -> Option<i32> {
    let mut sum = 0_i32;
    for _ in 0..4 {
        sum += ads1115_read_single(i2c, mux_bits).await.ok()? as i32;
    }
    Some(sum / 4)
}

async fn ads1115_read_single(
    i2c: &mut BoardI2c<'_>,
    mux_bits: u16,
) -> Result<i16, embassy_stm32::i2c::Error> {
    // Single-shot, +/-4.096 V PGA, 128 SPS, comparator disabled.
    let config = 0x8000_u16 | mux_bits | 0x0200 | 0x0100 | 0x0080 | 0x0003;
    let write = [0x01, (config >> 8) as u8, config as u8];
    i2c.blocking_write(ADS1115_ADDR, &write)?;
    Timer::after_millis(10).await;

    let mut read = [0_u8; 2];
    i2c.blocking_write_read(ADS1115_ADDR, &[0x00], &mut read)?;
    Ok(i16::from_be_bytes(read))
}

fn append_reading(line: &mut String<192>, label: &str, value: Option<i32>) {
    let _ = write!(line, "{label}=");
    append_value(line, value);
}

fn append_value(line: &mut String<192>, value: Option<i32>) {
    match value {
        Some(value) => {
            let _ = write!(line, "{value}");
        }
        None => {
            let _ = write!(line, "ERR");
        }
    }
}
