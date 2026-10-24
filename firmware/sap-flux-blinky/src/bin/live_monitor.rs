#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::bind_interrupts;
use embassy_stm32::dma;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c, Master as I2cMaster};
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::peripherals::{self, ADC1, PB1};
use embassy_stm32::rtc::{DateTime, DayOfWeek, Rtc, RtcConfig, RtcTimeProvider};
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{self, Config as UartConfig, RingBufferedUartRx, Uart, UartTx};
use embassy_stm32::Peri;
use embassy_time::{Duration, Instant, Timer};
use panic_halt as _;
use sap_flux_core::civil_time::{datetime_to_unix_ms, unix_ms_to_datetime, UtcDateTime};
use sap_flux_protocol::{
    encode, Ack, BoardState, Decoder, Hello, Message, Sample, Status, CAP_ADC, CAP_RTC,
    CAP_V24_MONITOR, FLAG_ADC_ERROR, FLAG_COMMAND_WINDOW, FLAG_EXCITATION_ACTIVE, FLAG_PGOOD,
    FLAG_RTC_VALID, MAX_ENCODED_LEN,
};
use static_cell::StaticCell;
use stm32_metapac as pac;

const HEATER_DISABLED_RESULT: u8 = 0x40;
const INVALID_PARAMETER_RESULT: u8 = 0x02;
const BACKUP_WRITE_FAILED_RESULT: u8 = 0x03;
const OK_RESULT: u8 = 0;
const ADS1115_ADDR: u8 = 0x48;
const ADS_MUX_AIN0_GND: u16 = 0x4000;
const ADS_MUX_AIN1_GND: u16 = 0x5000;
const RTC_MAGIC: u32 = 0x5346_5031;
const FIRMWARE_VERSION: u32 = 0x00_00_01;
const SAMPLE_PERIOD: Duration = Duration::from_millis(20);
const COMMAND_FRAME_PERIOD: Duration = Duration::from_millis(60);
const COMMAND_WINDOW_EVERY_SAMPLES: u32 = 50;
const MUX_SETTLE_US: u64 = 150;
const ADS_CONVERSION_US: u64 = 1_300;
const INVALID_ADC: i16 = i16::MIN;

static RS485_RX_DMA_BUFFER: StaticCell<[u8; 256]> = StaticCell::new();

bind_interrupts!(struct Irqs {
    USART2_LPUART2 => usart::InterruptHandler<peripherals::USART2>;
    DMA1_CHANNEL1 => dma::InterruptHandler<peripherals::DMA1_CH1>;
    DMA1_CHANNEL2_3 => dma::InterruptHandler<peripherals::DMA1_CH2>;
});

type BoardI2c<'d> = I2c<'d, Blocking, I2cMaster>;
type BoardAdc<'d> = Adc<'d, ADC1>;
type V24MonitorPin<'d> = Peri<'d, PB1>;

struct Rs485<'d> {
    tx: UartTx<'d, Async>,
    rx: RingBufferedUartRx<'d>,
    de: Output<'d>,
    n_re: Output<'d>,
    tx_sequence: u32,
    decoder: Decoder,
}

impl<'d> Rs485<'d> {
    async fn send_message(&mut self, message: Message) {
        let mut frame = [0_u8; MAX_ENCODED_LEN];
        let Ok(length) = encode(&message, self.tx_sequence, &mut frame) else {
            return;
        };
        self.tx_sequence = self.tx_sequence.wrapping_add(1);

        self.n_re.set_high();
        self.de.set_high();
        Timer::after_micros(20).await;
        let _ = self.tx.write(&frame[..length]).await;
        let _ = self.tx.flush().await;
        self.de.set_low();
        self.n_re.set_low();
    }

    fn poll_message(&mut self) -> Option<Message> {
        for _ in 0..MAX_ENCODED_LEN {
            match embedded_hal_nb::serial::Read::read(&mut self.rx) {
                Ok(byte) => {
                    if let Some(result) = self.decoder.push(byte) {
                        if let Ok(frame) = result {
                            return Some(frame.message);
                        }
                    }
                }
                Err(nb::Error::WouldBlock) => return None,
                Err(nb::Error::Other(_)) => return None,
            }
        }
        None
    }
}

struct MuxPins<'d> {
    en_a: Output<'d>,
    en_b: Output<'d>,
    en_c: Output<'d>,
    s0: Output<'d>,
    s1: Output<'d>,
    s2: Output<'d>,
    s3: Output<'d>,
}

impl MuxPins<'_> {
    fn enable_bank_a(&mut self) {
        self.en_a.set_low();
        self.en_b.set_high();
        self.en_c.set_high();
    }

    fn select(&mut self, channel: u8) {
        self.s0.set_level(if channel & 0x01 != 0 {
            Level::High
        } else {
            Level::Low
        });
        self.s1.set_level(if channel & 0x02 != 0 {
            Level::High
        } else {
            Level::Low
        });
        self.s2.set_level(if channel & 0x04 != 0 {
            Level::High
        } else {
            Level::Low
        });
        self.s3.set_level(if channel & 0x08 != 0 {
            Level::High
        } else {
            Level::Low
        });
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();
    config.rcc.ls = embassy_stm32::rcc::LsConfig::default_lse();
    let p = embassy_stm32::init(config);

    // This image is intentionally incapable of enabling the heater.
    let mut heater_gate = Output::new(p.PA8, Level::Low, Speed::Low);
    let mut excitation = Output::new(p.PC6, Level::Low, Speed::Low);
    let mut mux = MuxPins {
        en_a: Output::new(p.PA9, Level::High, Speed::Low),
        en_b: Output::new(p.PA10, Level::High, Speed::Low),
        en_c: Output::new(p.PA11, Level::High, Speed::Low),
        s0: Output::new(p.PD0, Level::Low, Speed::Low),
        s1: Output::new(p.PD1, Level::Low, Speed::Low),
        s2: Output::new(p.PD2, Level::Low, Speed::Low),
        s3: Output::new(p.PD3, Level::Low, Speed::Low),
    };
    let mut watchdog_done = Output::new(p.PA5, Level::Low, Speed::Low);
    let watchdog_wake = Input::new(p.PA4, Pull::None);
    let pgood = Input::new(p.PC7, Pull::None);
    let _eeprom_wp = Output::new(p.PA12, Level::High, Speed::Low);
    let _flash_cs = Output::new(p.PB12, Level::High, Speed::VeryHigh);
    let _led_red = Output::new(p.PB3, Level::High, Speed::Low);
    let _led_green = Output::new(p.PB4, Level::High, Speed::Low);
    let _led_blue = Output::new(p.PB5, Level::High, Speed::Low);

    heater_gate.set_low();
    mux.enable_bank_a();

    let mut uart_config = UartConfig::default();
    uart_config.baudrate = 115_200;
    let uart = Uart::new(
        p.USART2,
        p.PA3,
        p.PA2,
        p.DMA1_CH1,
        p.DMA1_CH2,
        Irqs,
        uart_config,
    )
    .unwrap();
    let (tx, rx) = uart.split();
    let rx_buffer = RS485_RX_DMA_BUFFER.init([0; 256]);
    let mut rx = rx.into_ring_buffered(rx_buffer);
    rx.start_uart();
    let mut rs485 = Rs485 {
        tx,
        rx,
        de: Output::new(p.PA1, Level::Low, Speed::Low),
        n_re: Output::new(p.PA0, Level::Low, Speed::Low),
        tx_sequence: 0,
        decoder: Decoder::new(),
    };

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = Hertz::khz(400);
    i2c_config.sda_pullup = true;
    i2c_config.scl_pullup = true;
    let mut i2c = I2c::new_blocking(p.I2C1, p.PB6, p.PB7, i2c_config);

    let mut v24_monitor = p.PB1;
    let mut internal_adc = Adc::new(p.ADC1);
    let (mut rtc, rtc_provider) = Rtc::new(p.RTC, RtcConfig::default());
    enable_backup_domain_access().await;
    let mut rtc_valid = read_rtc_magic() || rtc_calendar_plausible(&rtc_provider);
    if rtc_valid && !read_rtc_magic() {
        write_rtc_magic();
        rtc_valid = read_rtc_magic();
    }
    let board_id = board_id();

    excitation.set_high();
    Timer::after_millis(5).await;
    rs485.send_message(hello_message(board_id)).await;
    rs485
        .send_message(status_message(&rtc_provider, rtc_valid))
        .await;

    let mut watchdog_bootstrapped = false;
    let mut watchdog_serviced = false;
    let mut sample_counter = 0_u32;

    loop {
        let frame_started = Instant::now();
        let command_window =
            sample_counter % COMMAND_WINDOW_EVERY_SAMPLES == COMMAND_WINDOW_EVERY_SAMPLES - 1;
        sample_counter = sample_counter.wrapping_add(1);
        heater_gate.set_low();
        mux.enable_bank_a();
        excitation.set_high();

        let (sample, sample_healthy) = acquire_sample(
            &mut i2c,
            &mut internal_adc,
            &mut v24_monitor,
            &mut mux,
            &rtc_provider,
            rtc_valid,
            pgood.is_high(),
            command_window,
        )
        .await;

        // TPL5010 does not emit WAKE at the beginning of its first cycle.
        // Prove the board is healthy during that initial interval so the
        // first WAKE edge does not arrive as an already-missed deadline.
        if !watchdog_bootstrapped && sample_healthy {
            pulse_watchdog_done(&mut watchdog_done).await;
            watchdog_bootstrapped = true;
            watchdog_serviced = watchdog_wake.is_high();
        }
        rs485.send_message(Message::Sample(sample)).await;

        let frame_deadline = frame_started
            + if command_window {
                COMMAND_FRAME_PERIOD
            } else {
                SAMPLE_PERIOD
            };
        loop {
            while let Some(message) = rs485.poll_message() {
                handle_command(
                    message,
                    &mut rs485,
                    &mut rtc,
                    &rtc_provider,
                    &mut rtc_valid,
                    board_id,
                )
                .await;
                heater_gate.set_low();
            }

            if watchdog_wake.is_high() {
                if !watchdog_serviced && sample_healthy {
                    pulse_watchdog_done(&mut watchdog_done).await;
                    watchdog_serviced = true;
                }
            } else {
                watchdog_serviced = false;
            }

            heater_gate.set_low();
            if Instant::now() >= frame_deadline {
                break;
            }
            Timer::after_micros(100).await;
        }
    }
}

async fn pulse_watchdog_done(done: &mut Output<'_>) {
    done.set_high();
    Timer::after_millis(1).await;
    done.set_low();
}

async fn acquire_sample(
    i2c: &mut BoardI2c<'_>,
    internal_adc: &mut BoardAdc<'_>,
    v24_monitor: &mut V24MonitorPin<'_>,
    mux: &mut MuxPins<'_>,
    rtc_provider: &RtcTimeProvider,
    rtc_valid: bool,
    pgood: bool,
    command_window: bool,
) -> (Sample, bool) {
    let mut thermistor_raw = [INVALID_ADC; 6];
    let mut adc_error = false;

    // Interleave matched-depth channels to minimize pair timing skew.
    for (index, channel) in [(0, 1), (3, 2), (1, 0)] {
        thermistor_raw[index] = read_mux_channel(i2c, mux, channel)
            .await
            .unwrap_or_else(|| {
                adc_error = true;
                INVALID_ADC
            });
    }

    let monotonic_us = Instant::now().as_micros();
    let excitation_raw = read_ads_single(i2c, ADS_MUX_AIN1_GND)
        .await
        .unwrap_or_else(|| {
            adc_error = true;
            INVALID_ADC
        });

    for (index, channel) in [(4, 5), (2, 3), (5, 4)] {
        thermistor_raw[index] = read_mux_channel(i2c, mux, channel)
            .await
            .unwrap_or_else(|| {
                adc_error = true;
                INVALID_ADC
            });
    }

    let v24_raw = internal_adc
        .blocking_read(v24_monitor, SampleTime::CYCLES160_5)
        .min(i16::MAX as u16) as i16;
    let rtc_unix_ms = rtc_now_ms(rtc_provider, rtc_valid).unwrap_or(-1);

    let mut flags = FLAG_EXCITATION_ACTIVE;
    if rtc_unix_ms >= 0 {
        flags |= FLAG_RTC_VALID;
    }
    if pgood {
        flags |= FLAG_PGOOD;
    }
    if adc_error {
        flags |= FLAG_ADC_ERROR;
    }
    if command_window {
        flags |= FLAG_COMMAND_WINDOW;
    }

    (
        Sample {
            monotonic_us,
            rtc_unix_ms,
            run_id: 0,
            flags,
            excitation_raw,
            thermistor_raw,
            v24_raw,
            state: BoardState::SafeIdle,
        },
        !adc_error && pgood,
    )
}

async fn read_mux_channel(
    i2c: &mut BoardI2c<'_>,
    mux: &mut MuxPins<'_>,
    channel: u8,
) -> Option<i16> {
    mux.select(channel);
    Timer::after_micros(MUX_SETTLE_US).await;
    read_ads_single(i2c, ADS_MUX_AIN0_GND).await
}

async fn read_ads_single(i2c: &mut BoardI2c<'_>, mux_bits: u16) -> Option<i16> {
    // Single-shot, +/-4.096 V PGA, 860 SPS, comparator disabled.
    let config = 0x8000_u16 | mux_bits | 0x0200 | 0x0100 | 0x00e0 | 0x0003;
    let write = [0x01, (config >> 8) as u8, config as u8];
    i2c.blocking_write(ADS1115_ADDR, &write).ok()?;
    Timer::after_micros(ADS_CONVERSION_US).await;

    let mut read = [0_u8; 2];
    i2c.blocking_write_read(ADS1115_ADDR, &[0x00], &mut read)
        .ok()?;
    Some(i16::from_be_bytes(read))
}

async fn handle_command(
    message: Message,
    rs485: &mut Rs485<'_>,
    rtc: &mut Rtc,
    rtc_provider: &RtcTimeProvider,
    rtc_valid: &mut bool,
    board_id: u64,
) {
    match message {
        Message::Query(command) => {
            rs485
                .send_message(ack_message(
                    sap_flux_protocol::MSG_QUERY,
                    OK_RESULT,
                    command.command_nonce,
                ))
                .await;
            rs485.send_message(hello_message(board_id)).await;
            rs485
                .send_message(status_message(rtc_provider, *rtc_valid))
                .await;
        }
        Message::SetRtc(command) => {
            let mut result = set_rtc(rtc, command.unix_ms).await;
            if result == OK_RESULT {
                write_rtc_magic();
                *rtc_valid = read_rtc_magic();
                if !*rtc_valid {
                    result = BACKUP_WRITE_FAILED_RESULT;
                }
            }
            rs485
                .send_message(ack_message(
                    sap_flux_protocol::MSG_SET_RTC,
                    result,
                    command.command_nonce,
                ))
                .await;
            rs485
                .send_message(status_message(rtc_provider, *rtc_valid))
                .await;
        }
        Message::StartBaseline(command) => {
            rs485
                .send_message(ack_message(
                    sap_flux_protocol::MSG_START_BASELINE,
                    HEATER_DISABLED_RESULT,
                    command.command_nonce,
                ))
                .await;
        }
        Message::ArmPulse(command) => {
            rs485
                .send_message(ack_message(
                    sap_flux_protocol::MSG_ARM_PULSE,
                    HEATER_DISABLED_RESULT,
                    command.command_nonce,
                ))
                .await;
        }
        Message::FirePulse(command) => {
            rs485
                .send_message(ack_message(
                    sap_flux_protocol::MSG_FIRE_PULSE,
                    HEATER_DISABLED_RESULT,
                    command.command_nonce,
                ))
                .await;
        }
        Message::Abort(command) => {
            rs485
                .send_message(ack_message(
                    sap_flux_protocol::MSG_ABORT,
                    HEATER_DISABLED_RESULT,
                    command.command_nonce,
                ))
                .await;
        }
        Message::Hello(_)
        | Message::Sample(_)
        | Message::Status(_)
        | Message::Ack(_)
        | Message::Error(_) => {}
    }
}

async fn set_rtc(rtc: &mut Rtc, unix_ms: i64) -> u8 {
    let fraction_ms = unix_ms.rem_euclid(1_000) as u64;
    let wait_ms = 1_000 - fraction_ms;
    let Some(target_ms) = unix_ms.checked_add(wait_ms as i64) else {
        return INVALID_PARAMETER_RESULT;
    };
    let Some(value) = unix_ms_to_datetime(target_ms) else {
        return INVALID_PARAMETER_RESULT;
    };
    let Some(day_of_week) = day_of_week(value.weekday) else {
        return INVALID_PARAMETER_RESULT;
    };
    let Ok(value) = DateTime::from(
        value.year,
        value.month,
        value.day,
        day_of_week,
        value.hour,
        value.minute,
        value.second,
        value.millisecond as u32 * 1_000,
    ) else {
        return INVALID_PARAMETER_RESULT;
    };
    Timer::after_millis(wait_ms).await;
    if rtc.set_datetime(value).is_err() {
        return INVALID_PARAMETER_RESULT;
    }
    OK_RESULT
}

fn rtc_now_ms(provider: &RtcTimeProvider, rtc_valid: bool) -> Option<i64> {
    if !rtc_valid {
        return None;
    }
    let value = provider.now().ok()?;
    datetime_to_unix_ms(UtcDateTime {
        year: value.year(),
        month: value.month(),
        day: value.day(),
        weekday: value.day_of_week() as u8,
        hour: value.hour(),
        minute: value.minute(),
        second: value.second(),
        millisecond: (value.microsecond() / 1_000) as u16,
    })
}

async fn enable_backup_domain_access() {
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
}

fn read_rtc_magic() -> bool {
    // embassy-stm32 0.6's STM32G0 RTC-v3 backup-register methods are stubs;
    // on this chip the registers live in TAMP, so access them through PAC.
    pac::TAMP.bkpr(0).read().bkp() == RTC_MAGIC
}

fn write_rtc_magic() {
    pac::TAMP.bkpr(0).write(|w| w.set_bkp(RTC_MAGIC));
}

fn rtc_calendar_plausible(provider: &RtcTimeProvider) -> bool {
    provider
        .now()
        .is_ok_and(|value| (2025..=2099).contains(&value.year()))
}

fn day_of_week(value: u8) -> Option<DayOfWeek> {
    Some(match value {
        1 => DayOfWeek::Monday,
        2 => DayOfWeek::Tuesday,
        3 => DayOfWeek::Wednesday,
        4 => DayOfWeek::Thursday,
        5 => DayOfWeek::Friday,
        6 => DayOfWeek::Saturday,
        7 => DayOfWeek::Sunday,
        _ => return None,
    })
}

fn hello_message(board_id: u64) -> Message {
    Message::Hello(Hello {
        board_id,
        firmware_version: FIRMWARE_VERSION,
        capabilities: CAP_RTC | CAP_ADC | CAP_V24_MONITOR,
    })
}

fn status_message(provider: &RtcTimeProvider, rtc_valid: bool) -> Message {
    Message::Status(Status {
        monotonic_us: Instant::now().as_micros(),
        rtc_unix_ms: rtc_now_ms(provider, rtc_valid).unwrap_or(-1),
        run_id: 0,
        state: BoardState::SafeIdle,
        fault_code: 0,
        pulse_duration_ms: 0,
        cooldown_remaining_ms: 0,
    })
}

fn ack_message(command_type: u8, result: u8, command_nonce: u64) -> Message {
    Message::Ack(Ack {
        command_type,
        result,
        command_nonce,
        run_id: 0,
    })
}

fn board_id() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in embassy_stm32::uid::uid() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
