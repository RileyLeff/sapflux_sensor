#![no_std]
#![no_main]

use core::fmt::Write;

use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c, Master as I2cMaster};
use embassy_stm32::mode::Blocking;
use embassy_stm32::peripherals::{ADC1, PB1};
use embassy_stm32::spi::mode::Master as SpiMaster;
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{Config as UartConfig, Uart};
use embassy_stm32::Peri;
use embassy_time::Timer;
use heapless::String;
use panic_halt as _;
use stm32_metapac as pac;

const EEPROM_ADDR: u8 = 0x50;
const ADS1115_ADDR: u8 = 0x48;
const ADS_MUX_AIN0_GND: u16 = 0x4000;
const ADS_MUX_AIN1_GND: u16 = 0x5000;
const ADS_MUX_AIN2_GND: u16 = 0x6000;
const ADS_MUX_AIN3_GND: u16 = 0x7000;
const EEPROM_SCRATCH_ADDR: u16 = 0x0ff0;
const FLASH_TEST_ADDR: u32 = 0x000000;
const FLASH_SECTOR_SIZE: usize = 4096;
const FLASH_PAGE_SIZE: usize = 256;

type BoardI2c<'d> = I2c<'d, Blocking, I2cMaster>;
type BoardSpi<'d> = Spi<'d, Blocking, SpiMaster>;
type BoardAdc<'d> = Adc<'d, ADC1>;
type V24MonitorPin<'d> = Peri<'d, PB1>;

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

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();
    config.rcc.ls = embassy_stm32::rcc::LsConfig::default_lse();
    let p = sap_flux_blinky::init(config);

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
    let mut eeprom_wp = Output::new(p.PA12, Level::High, Speed::Low);
    let mut flash_cs = Output::new(p.PB12, Level::High, Speed::VeryHigh);
    let mut led_red = Output::new(p.PB3, Level::High, Speed::Low);
    let mut led_green = Output::new(p.PB4, Level::High, Speed::Low);
    let mut led_blue = Output::new(p.PB5, Level::High, Speed::Low);
    let pgood = Input::new(p.PC7, Pull::None);
    let mut v24_monitor = p.PB1;
    let mut adc1 = Adc::new(p.ADC1);

    heater_gate.set_low();
    excitation.set_low();
    mux_en_a.set_high();
    mux_en_b.set_high();
    mux_en_c.set_high();
    mux_s0.set_low();
    mux_s1.set_low();
    mux_s2.set_low();
    mux_s3.set_low();
    eeprom_wp.set_high();
    flash_cs.set_high();
    led_red.set_high();
    led_green.set_high();
    led_blue.set_high();

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

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = Hertz::mhz(1);
    let mut spi = Spi::new_blocking(p.SPI2, p.PB13, p.PB15, p.PB14, spi_config);

    rs485
        .line("sap-flux board-test ready; send any byte to run")
        .await;

    loop {
        let command = rs485.read_byte();
        let mut line: String<96> = String::new();
        let _ = write!(&mut line, "running board-test command 0x{command:02x}");
        rs485.line(&line).await;

        run_i2c_scan(&mut rs485, &mut i2c).await;
        run_pgood_test(&mut rs485, &pgood).await;
        run_v24_monitor_test(&mut rs485, &mut adc1, &mut v24_monitor).await;
        run_ads1115_test(
            &mut rs485,
            &mut i2c,
            &mut excitation,
            &mut mux_en_a,
            &mut mux_en_b,
            &mut mux_en_c,
        )
        .await;
        run_mux_smoke_test(
            &mut rs485,
            &mut i2c,
            &mut excitation,
            &mut mux_en_a,
            &mut mux_en_b,
            &mut mux_en_c,
            &mut mux_s0,
            &mut mux_s1,
            &mut mux_s2,
            &mut mux_s3,
        )
        .await;
        run_therm1_known_resistor_probe(
            &mut rs485,
            &mut i2c,
            &mut excitation,
            &mut mux_en_a,
            &mut mux_en_b,
            &mut mux_en_c,
            &mut mux_s0,
            &mut mux_s1,
            &mut mux_s2,
            &mut mux_s3,
        )
        .await;
        run_lse_rtc_test(&mut rs485).await;
        run_eeprom_test(&mut rs485, &mut i2c, &mut eeprom_wp).await;
        run_flash_test(&mut rs485, &mut spi, &mut flash_cs).await;

        heater_gate.set_low();
        excitation.set_low();
        mux_en_a.set_high();
        mux_en_b.set_high();
        mux_en_c.set_high();
        mux_s0.set_low();
        mux_s1.set_low();
        mux_s2.set_low();
        mux_s3.set_low();
        eeprom_wp.set_high();
        flash_cs.set_high();
        led_red.set_high();
        led_green.set_high();
        led_blue.set_high();

        rs485
            .line("board-test complete; send any byte to run again")
            .await;
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

async fn run_i2c_scan(rs485: &mut Rs485<'_>, i2c: &mut BoardI2c<'_>) {
    rs485.line("I2C scan:").await;

    let mut found_eeprom = false;
    let mut found_ads = false;
    for addr in 0x08..=0x77 {
        if i2c.blocking_write(addr, &[]).is_ok() {
            let mut line: String<64> = String::new();
            let _ = write!(&mut line, "  found 0x{addr:02x}");
            rs485.line(&line).await;
            found_eeprom |= addr == EEPROM_ADDR;
            found_ads |= addr == 0x48;
        }
    }

    report_bool(rs485, "  EEPROM addr 0x50", found_eeprom).await;
    report_bool(rs485, "  ADS1115 addr 0x48", found_ads).await;
}

async fn run_pgood_test(rs485: &mut Rs485<'_>, pgood: &Input<'_>) {
    rs485.line("PGOOD test:").await;

    let high = pgood.is_high();
    let mut line: String<96> = String::new();
    let _ = write!(
        &mut line,
        "  PC7 reads {}",
        if high { "HIGH" } else { "LOW" }
    );
    rs485.line(&line).await;
    report_bool(rs485, "  buck PGOOD", high).await;
}

async fn run_v24_monitor_test(
    rs485: &mut Rs485<'_>,
    adc: &mut BoardAdc<'_>,
    v24_monitor: &mut V24MonitorPin<'_>,
) {
    rs485.line("MCU ADC V24 monitor:").await;

    let mut raw_total = 0_u32;
    for _ in 0..8 {
        raw_total += adc.blocking_read(v24_monitor, SampleTime::CYCLES160_5) as u32;
        Timer::after_millis(2).await;
    }

    let raw = raw_total / 8;
    let divider_mv = (raw * 3300 + 2047) / 4095;
    let v24_mv = divider_mv * 11;

    let mut line: String<128> = String::new();
    let _ = write!(
        &mut line,
        "  PB1 raw {raw}; divider {divider_mv} mV; V24 approx {}.{:03} V",
        v24_mv / 1000,
        v24_mv % 1000
    );
    rs485.line(&line).await;

    report_bool(
        rs485,
        "  V24 monitor plausible",
        (5_000..=35_000).contains(&v24_mv),
    )
    .await;
}

async fn run_ads1115_test(
    rs485: &mut Rs485<'_>,
    i2c: &mut BoardI2c<'_>,
    excitation: &mut Output<'_>,
    mux_en_a: &mut Output<'_>,
    mux_en_b: &mut Output<'_>,
    mux_en_c: &mut Output<'_>,
) {
    rs485.line("ADS1115 analog test:").await;

    excitation.set_low();
    mux_en_a.set_high();
    mux_en_b.set_high();
    mux_en_c.set_high();
    Timer::after_millis(10).await;

    let ain1_off_mv = match ads1115_read_mv(i2c, ADS_MUX_AIN1_GND).await {
        Ok(mv) => mv,
        Err(_) => {
            rs485.line("  FAIL AIN1 read with excitation off").await;
            return;
        }
    };
    let ain2_mv = match ads1115_read_mv(i2c, ADS_MUX_AIN2_GND).await {
        Ok(mv) => mv,
        Err(_) => {
            rs485.line("  FAIL AIN2 read").await;
            return;
        }
    };
    let ain3_mv = match ads1115_read_mv(i2c, ADS_MUX_AIN3_GND).await {
        Ok(mv) => mv,
        Err(_) => {
            rs485.line("  FAIL AIN3 read").await;
            return;
        }
    };

    excitation.set_high();
    Timer::after_millis(20).await;
    let ain1_on_mv = match ads1115_read_mv(i2c, ADS_MUX_AIN1_GND).await {
        Ok(mv) => mv,
        Err(_) => {
            excitation.set_low();
            rs485.line("  FAIL AIN1 read with excitation on").await;
            return;
        }
    };
    excitation.set_low();

    report_ads_mv(rs485, "  AIN1 excitation off", ain1_off_mv).await;
    report_ads_mv(rs485, "  AIN1 excitation on", ain1_on_mv).await;
    report_ads_mv(rs485, "  AIN2 tied GND", ain2_mv).await;
    report_ads_mv(rs485, "  AIN3 tied GND", ain3_mv).await;

    let pass = mv_abs(ain1_off_mv) <= 100
        && ain1_on_mv >= 2_500
        && mv_abs(ain2_mv) <= 100
        && mv_abs(ain3_mv) <= 100;
    report_bool(rs485, "  ADS1115 excitation/GND channels", pass).await;
}

async fn run_mux_smoke_test(
    rs485: &mut Rs485<'_>,
    i2c: &mut BoardI2c<'_>,
    excitation: &mut Output<'_>,
    mux_en_a: &mut Output<'_>,
    mux_en_b: &mut Output<'_>,
    mux_en_c: &mut Output<'_>,
    mux_s0: &mut Output<'_>,
    mux_s1: &mut Output<'_>,
    mux_s2: &mut Output<'_>,
    mux_s3: &mut Output<'_>,
) {
    rs485
        .line("Analog mux smoke test: each mux at Y0 and Y15")
        .await;

    excitation.set_low();
    mux_en_a.set_high();
    mux_en_b.set_high();
    mux_en_c.set_high();

    mux_s0.set_low();
    mux_s1.set_low();
    mux_s2.set_low();
    mux_s3.set_low();
    Timer::after_millis(2).await;

    mux_en_a.set_low();
    Timer::after_millis(2).await;
    let a_y0 = ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await.ok();
    mux_en_a.set_high();

    mux_en_b.set_low();
    Timer::after_millis(2).await;
    let b_y0 = ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await.ok();
    mux_en_b.set_high();

    mux_en_c.set_low();
    Timer::after_millis(2).await;
    let c_y0 = ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await.ok();
    mux_en_c.set_high();

    mux_s0.set_high();
    mux_s1.set_high();
    mux_s2.set_high();
    mux_s3.set_high();
    Timer::after_millis(2).await;

    mux_en_a.set_low();
    Timer::after_millis(2).await;
    let a_y15 = ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await.ok();
    mux_en_a.set_high();

    mux_en_b.set_low();
    Timer::after_millis(2).await;
    let b_y15 = ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await.ok();
    mux_en_b.set_high();

    mux_en_c.set_low();
    Timer::after_millis(2).await;
    let c_y15 = ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await.ok();
    mux_en_c.set_high();

    restore_analog_safe(
        excitation, mux_en_a, mux_en_b, mux_en_c, mux_s0, mux_s1, mux_s2, mux_s3,
    );

    report_mux_mv(rs485, "  mux A Y0", a_y0).await;
    report_mux_mv(rs485, "  mux B Y0", b_y0).await;
    report_mux_mv(rs485, "  mux C Y0", c_y0).await;
    report_mux_mv(rs485, "  mux A Y15", a_y15).await;
    report_mux_mv(rs485, "  mux B Y15", b_y15).await;
    report_mux_mv(rs485, "  mux C Y15", c_y15).await;

    let readable = [a_y0, b_y0, c_y0, a_y15, b_y15, c_y15]
        .iter()
        .all(Option::is_some);
    report_bool(rs485, "  all mux selections readable", readable).await;
}

async fn run_therm1_known_resistor_probe(
    rs485: &mut Rs485<'_>,
    i2c: &mut BoardI2c<'_>,
    excitation: &mut Output<'_>,
    mux_en_a: &mut Output<'_>,
    mux_en_b: &mut Output<'_>,
    mux_en_c: &mut Output<'_>,
    mux_s0: &mut Output<'_>,
    mux_s1: &mut Output<'_>,
    mux_s2: &mut Output<'_>,
    mux_s3: &mut Output<'_>,
) {
    rs485
        .line("Therm1 known-resistor probe: expects resistor J2 pin2(EXC) to pin3(T1)")
        .await;

    excitation.set_low();
    mux_en_a.set_high();
    mux_en_b.set_high();
    mux_en_c.set_high();
    mux_s0.set_high();
    mux_s1.set_low();
    mux_s2.set_low();
    mux_s3.set_low();
    Timer::after_millis(10).await;

    mux_en_a.set_low();
    excitation.set_high();
    Timer::after_millis(50).await;

    let ain0_mv = match ads1115_read_mv(i2c, ADS_MUX_AIN0_GND).await {
        Ok(mv) => mv,
        Err(_) => {
            restore_analog_safe(
                excitation, mux_en_a, mux_en_b, mux_en_c, mux_s0, mux_s1, mux_s2, mux_s3,
            );
            rs485.line("  FAIL AIN0 therm1 read").await;
            return;
        }
    };
    let ain1_mv = match ads1115_read_mv(i2c, ADS_MUX_AIN1_GND).await {
        Ok(mv) => mv,
        Err(_) => {
            restore_analog_safe(
                excitation, mux_en_a, mux_en_b, mux_en_c, mux_s0, mux_s1, mux_s2, mux_s3,
            );
            rs485.line("  FAIL AIN1 excitation read").await;
            return;
        }
    };

    restore_analog_safe(
        excitation, mux_en_a, mux_en_b, mux_en_c, mux_s0, mux_s1, mux_s2, mux_s3,
    );

    report_ads_mv(rs485, "  AIN0 via mux A/Y1/T1", ain0_mv).await;
    report_ads_mv(rs485, "  AIN1 excitation sense", ain1_mv).await;

    if ain0_mv >= 2_000 && ain1_mv >= 2_500 {
        rs485.line("  therm1 path pulled high OK").await;
    } else {
        rs485
            .line("  therm1 path not high; OK if resistor is not installed, otherwise investigate")
            .await;
    }
}

async fn run_lse_rtc_test(rs485: &mut Rs485<'_>) {
    rs485.line("RTC/LSE test:").await;

    pac::RCC.apbenr1().modify(|w| {
        w.set_pwren(true);
        w.set_rtcapben(true);
    });
    pac::PWR.cr1().modify(|w| w.set_dbp(true));

    for _ in 0..20 {
        if pac::PWR.cr1().read().dbp() {
            break;
        }
        Timer::after_millis(1).await;
    }
    if !pac::PWR.cr1().read().dbp() {
        rs485.line("  FAIL backup-domain write enable").await;
        return;
    }

    if !pac::RCC.bdcr().read().lseon() {
        pac::RCC.bdcr().modify(|w| {
            w.set_lsebyp(false);
            w.set_lsedrv(pac::rcc::vals::Lsedrv::MEDIUM_HIGH);
            w.set_lseon(true);
        });
    }

    let mut lse_ready_ms = 0_u32;
    while lse_ready_ms <= 5_000 && !pac::RCC.bdcr().read().lserdy() {
        Timer::after_millis(50).await;
        lse_ready_ms += 50;
    }

    let bdcr = pac::RCC.bdcr().read();
    let mut line: String<128> = String::new();
    let _ = write!(
        &mut line,
        "  LSE on {}; ready {}; waited {lse_ready_ms} ms",
        yes_no(bdcr.lseon()),
        yes_no(bdcr.lserdy())
    );
    rs485.line(&line).await;

    let mut rtc_line: String<128> = String::new();
    let _ = write!(
        &mut rtc_line,
        "  RTCEN {}; RTCSEL {}",
        yes_no(bdcr.rtcen()),
        rtcsel_name(bdcr.rtcsel())
    );
    rs485.line(&rtc_line).await;

    if bdcr.rtcen() {
        let ss_before = pac::RTC.ssr().read().ss();
        Timer::after_millis(1_100).await;
        let ss_after = pac::RTC.ssr().read().ss();
        let ticking = ss_before != ss_after;
        let mut tick_line: String<96> = String::new();
        let _ = write!(
            &mut tick_line,
            "  RTC subsecond before {ss_before}; after {ss_after}"
        );
        rs485.line(&tick_line).await;
        report_bool(rs485, "  RTC counter ticking", ticking).await;
    } else {
        rs485
            .line("  RTC counter skipped; RTC clock is not enabled")
            .await;
    }

    report_bool(rs485, "  LSE oscillator", bdcr.lserdy()).await;
}

async fn run_eeprom_test(rs485: &mut Rs485<'_>, i2c: &mut BoardI2c<'_>, wp: &mut Output<'_>) {
    rs485.line("EEPROM test:").await;

    let addr = [(EEPROM_SCRATCH_ADDR >> 8) as u8, EEPROM_SCRATCH_ADDR as u8];
    let mut original = [0_u8; 16];
    if i2c
        .blocking_write_read(EEPROM_ADDR, &addr, &mut original)
        .is_err()
    {
        rs485.line("  FAIL read scratch window").await;
        wp.set_high();
        return;
    }
    rs485.line("  read scratch window OK").await;

    let mut pattern = [0_u8; 16];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = 0xa0 ^ (index as u8).wrapping_mul(17);
    }

    wp.set_low();
    Timer::after_millis(1).await;
    if eeprom_write_page(i2c, EEPROM_SCRATCH_ADDR, &pattern).is_err() {
        rs485.line("  FAIL write pattern").await;
        wp.set_high();
        return;
    }
    if !eeprom_wait_ready(i2c).await {
        rs485.line("  FAIL write-ready timeout").await;
        wp.set_high();
        return;
    }

    let mut verify = [0_u8; 16];
    if i2c
        .blocking_write_read(EEPROM_ADDR, &addr, &mut verify)
        .is_err()
        || verify != pattern
    {
        rs485.line("  FAIL verify pattern").await;
        let _ = eeprom_write_page(i2c, EEPROM_SCRATCH_ADDR, &original);
        let _ = eeprom_wait_ready(i2c).await;
        wp.set_high();
        return;
    }
    rs485.line("  write/verify pattern OK").await;

    if eeprom_write_page(i2c, EEPROM_SCRATCH_ADDR, &original).is_err() {
        rs485.line("  FAIL restore original").await;
        wp.set_high();
        return;
    }
    if !eeprom_wait_ready(i2c).await {
        rs485.line("  FAIL restore-ready timeout").await;
        wp.set_high();
        return;
    }
    wp.set_high();

    let mut restored = [0_u8; 16];
    if i2c
        .blocking_write_read(EEPROM_ADDR, &addr, &mut restored)
        .is_err()
        || restored != original
    {
        rs485.line("  FAIL verify restore").await;
        return;
    }

    rs485.line("  restore original OK").await;
    rs485.line("  EEPROM PASS").await;
}

async fn run_flash_test(rs485: &mut Rs485<'_>, spi: &mut BoardSpi<'_>, cs: &mut Output<'_>) {
    rs485.line("SPI flash test:").await;

    let id = match flash_read_jedec(spi, cs) {
        Ok(id) => id,
        Err(_) => {
            rs485.line("  FAIL read JEDEC ID").await;
            return;
        }
    };
    let mut line: String<96> = String::new();
    let _ = write!(
        &mut line,
        "  JEDEC ID {:02x} {:02x} {:02x}",
        id[0], id[1], id[2]
    );
    rs485.line(&line).await;

    if id != [0xef, 0x40, 0x19] {
        rs485
            .line("  WARN unexpected ID; continuing cautiously")
            .await;
    }

    let mut original_sector = [0_u8; FLASH_SECTOR_SIZE];
    if flash_read(spi, cs, FLASH_TEST_ADDR, &mut original_sector).is_err() {
        rs485.line("  FAIL back up test sector").await;
        return;
    }
    rs485.line("  backed up sector 0 OK").await;

    if flash_write_enable(spi, cs).is_err() {
        rs485.line("  FAIL write enable before erase").await;
        return;
    }
    if flash_sector_erase(spi, cs, FLASH_TEST_ADDR).is_err() {
        rs485.line("  FAIL sector erase command").await;
        return;
    }
    if !flash_wait_ready(spi, cs, 400).await {
        rs485.line("  FAIL erase timeout").await;
        return;
    }

    let mut erased_page = [0_u8; FLASH_PAGE_SIZE];
    if flash_read(spi, cs, FLASH_TEST_ADDR, &mut erased_page).is_err()
        || erased_page.iter().any(|byte| *byte != 0xff)
    {
        rs485.line("  FAIL erase verify").await;
        let _ = flash_restore_sector(spi, cs, FLASH_TEST_ADDR, &original_sector).await;
        return;
    }
    rs485.line("  erase verify OK").await;

    let mut pattern = [0_u8; FLASH_PAGE_SIZE];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = 0x5a ^ (index as u8).wrapping_mul(37);
    }

    if flash_write_enable(spi, cs).is_err() {
        rs485.line("  FAIL write enable before program").await;
        let _ = flash_restore_sector(spi, cs, FLASH_TEST_ADDR, &original_sector).await;
        return;
    }
    if flash_page_program(spi, cs, FLASH_TEST_ADDR, &pattern).is_err() {
        rs485.line("  FAIL page program command").await;
        let _ = flash_restore_sector(spi, cs, FLASH_TEST_ADDR, &original_sector).await;
        return;
    }
    if !flash_wait_ready(spi, cs, 100).await {
        rs485.line("  FAIL program timeout").await;
        let _ = flash_restore_sector(spi, cs, FLASH_TEST_ADDR, &original_sector).await;
        return;
    }

    let mut verify = [0_u8; FLASH_PAGE_SIZE];
    if flash_read(spi, cs, FLASH_TEST_ADDR, &mut verify).is_err() || verify != pattern {
        rs485.line("  FAIL program verify").await;
        let _ = flash_restore_sector(spi, cs, FLASH_TEST_ADDR, &original_sector).await;
        return;
    }
    rs485.line("  program verify OK").await;

    if !flash_restore_sector(spi, cs, FLASH_TEST_ADDR, &original_sector).await {
        rs485.line("  FAIL restore sector").await;
        return;
    }

    let mut restored = [0_u8; FLASH_SECTOR_SIZE];
    if flash_read(spi, cs, FLASH_TEST_ADDR, &mut restored).is_err() || restored != original_sector {
        rs485.line("  FAIL restore verify").await;
        return;
    }

    rs485.line("  restore sector OK").await;
    rs485.line("  SPI flash PASS").await;
}

async fn ads1115_read_mv(
    i2c: &mut BoardI2c<'_>,
    mux_bits: u16,
) -> Result<i32, embassy_stm32::i2c::Error> {
    let raw = ads1115_read_single(i2c, mux_bits).await?;
    Ok((raw as i32 * 125) / 1000)
}

async fn ads1115_read_single(
    i2c: &mut BoardI2c<'_>,
    mux_bits: u16,
) -> Result<i16, embassy_stm32::i2c::Error> {
    let config = 0x8000_u16 | mux_bits | 0x0200 | 0x0100 | 0x0080 | 0x0003;
    let write = [0x01, (config >> 8) as u8, config as u8];
    i2c.blocking_write(ADS1115_ADDR, &write)?;

    Timer::after_millis(10).await;

    let mut read = [0_u8; 2];
    i2c.blocking_write_read(ADS1115_ADDR, &[0x00], &mut read)?;
    Ok(i16::from_be_bytes(read))
}

async fn report_ads_mv(rs485: &mut Rs485<'_>, label: &str, mv: i32) {
    let mut line: String<96> = String::new();
    let sign = if mv < 0 { "-" } else { "" };
    let abs = mv_abs(mv);
    let _ = write!(
        &mut line,
        "{label}: {sign}{}.{:03} V",
        abs / 1000,
        abs % 1000
    );
    rs485.line(&line).await;
}

async fn report_mux_mv(rs485: &mut Rs485<'_>, label: &str, mv: Option<i32>) {
    match mv {
        Some(mv) => report_ads_mv(rs485, label, mv).await,
        None => {
            let mut line: String<96> = String::new();
            let _ = write!(&mut line, "{label}: read failed");
            rs485.line(&line).await;
        }
    }
}

fn restore_analog_safe(
    excitation: &mut Output<'_>,
    mux_en_a: &mut Output<'_>,
    mux_en_b: &mut Output<'_>,
    mux_en_c: &mut Output<'_>,
    mux_s0: &mut Output<'_>,
    mux_s1: &mut Output<'_>,
    mux_s2: &mut Output<'_>,
    mux_s3: &mut Output<'_>,
) {
    excitation.set_low();
    mux_en_a.set_high();
    mux_en_b.set_high();
    mux_en_c.set_high();
    mux_s0.set_low();
    mux_s1.set_low();
    mux_s2.set_low();
    mux_s3.set_low();
}

fn mv_abs(mv: i32) -> i32 {
    if mv < 0 {
        -mv
    } else {
        mv
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn rtcsel_name(rtcsel: pac::rcc::vals::Rtcsel) -> &'static str {
    match rtcsel {
        pac::rcc::vals::Rtcsel::DISABLE => "disabled",
        pac::rcc::vals::Rtcsel::LSE => "LSE",
        pac::rcc::vals::Rtcsel::LSI => "LSI",
        pac::rcc::vals::Rtcsel::HSE_DIV_32 => "HSE/32",
    }
}

async fn report_bool(rs485: &mut Rs485<'_>, label: &str, ok: bool) {
    let mut line: String<96> = String::new();
    let _ = write!(&mut line, "{label}: {}", if ok { "OK" } else { "FAIL" });
    rs485.line(&line).await;
}

fn eeprom_write_page(
    i2c: &mut BoardI2c<'_>,
    address: u16,
    data: &[u8],
) -> Result<(), embassy_stm32::i2c::Error> {
    let mut write = [0_u8; 18];
    write[0] = (address >> 8) as u8;
    write[1] = address as u8;
    write[2..2 + data.len()].copy_from_slice(data);
    i2c.blocking_write(EEPROM_ADDR, &write[..2 + data.len()])
}

async fn eeprom_wait_ready(i2c: &mut BoardI2c<'_>) -> bool {
    for _ in 0..40 {
        if i2c.blocking_write(EEPROM_ADDR, &[]).is_ok() {
            return true;
        }
        Timer::after_millis(5).await;
    }
    false
}

fn flash_select(cs: &mut Output<'_>) {
    cs.set_low();
}

fn flash_deselect(cs: &mut Output<'_>) {
    cs.set_high();
}

fn flash_read_jedec(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
) -> Result<[u8; 3], embassy_stm32::spi::Error> {
    let mut buffer = [0x9f, 0, 0, 0];
    flash_select(cs);
    let result = spi.blocking_transfer_in_place(&mut buffer);
    flash_deselect(cs);
    result.map(|_| [buffer[1], buffer[2], buffer[3]])
}

fn flash_read_status1(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
) -> Result<u8, embassy_stm32::spi::Error> {
    let mut buffer = [0x05, 0];
    flash_select(cs);
    let result = spi.blocking_transfer_in_place(&mut buffer);
    flash_deselect(cs);
    result.map(|_| buffer[1])
}

fn flash_write_enable(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
) -> Result<(), embassy_stm32::spi::Error> {
    flash_select(cs);
    let result = spi.blocking_write(&[0x06_u8]);
    flash_deselect(cs);
    result
}

fn flash_read(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
    address: u32,
    data: &mut [u8],
) -> Result<(), embassy_stm32::spi::Error> {
    let command = [
        0x03,
        (address >> 16) as u8,
        (address >> 8) as u8,
        address as u8,
    ];
    data.fill(0);
    flash_select(cs);
    let command_result = spi.blocking_write(&command);
    let read_result = command_result.and_then(|_| spi.blocking_transfer_in_place(data));
    flash_deselect(cs);
    read_result
}

fn flash_sector_erase(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
    address: u32,
) -> Result<(), embassy_stm32::spi::Error> {
    let command = [
        0x20,
        (address >> 16) as u8,
        (address >> 8) as u8,
        address as u8,
    ];
    flash_select(cs);
    let result = spi.blocking_write(&command);
    flash_deselect(cs);
    result
}

fn flash_page_program(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
    address: u32,
    data: &[u8],
) -> Result<(), embassy_stm32::spi::Error> {
    let command = [
        0x02,
        (address >> 16) as u8,
        (address >> 8) as u8,
        address as u8,
    ];
    flash_select(cs);
    let command_result = spi.blocking_write(&command);
    let data_result = command_result.and_then(|_| spi.blocking_write(data));
    flash_deselect(cs);
    data_result
}

async fn flash_wait_ready(spi: &mut BoardSpi<'_>, cs: &mut Output<'_>, attempts: usize) -> bool {
    for _ in 0..attempts {
        if let Ok(status) = flash_read_status1(spi, cs) {
            if status & 0x01 == 0 {
                return true;
            }
        }
        Timer::after_millis(5).await;
    }
    false
}

async fn flash_restore_sector(
    spi: &mut BoardSpi<'_>,
    cs: &mut Output<'_>,
    address: u32,
    original: &[u8; FLASH_SECTOR_SIZE],
) -> bool {
    if flash_write_enable(spi, cs).is_err() {
        return false;
    }
    if flash_sector_erase(spi, cs, address).is_err() {
        return false;
    }
    if !flash_wait_ready(spi, cs, 400).await {
        return false;
    }

    for (index, chunk) in original.chunks(FLASH_PAGE_SIZE).enumerate() {
        let page_address = address + (index * FLASH_PAGE_SIZE) as u32;
        if flash_write_enable(spi, cs).is_err() {
            return false;
        }
        if flash_page_program(spi, cs, page_address, chunk).is_err() {
            return false;
        }
        if !flash_wait_ready(spi, cs, 100).await {
            return false;
        }
    }

    true
}
