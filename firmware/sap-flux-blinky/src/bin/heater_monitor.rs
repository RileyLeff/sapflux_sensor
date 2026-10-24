#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::bind_interrupts;
use embassy_stm32::dma;
use embassy_stm32::gpio::{Input, Level, Output, OutputType, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c, Master as I2cMaster};
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::peripherals::{self, ADC1, PB1};
use embassy_stm32::rtc::{DateTime, DayOfWeek, Rtc, RtcConfig, RtcTimeProvider};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{
    CountingMode, OutputCompareMode, OutputPolarity, Timer as LowLevelTimer,
};
use embassy_stm32::timer::simple_pwm::PwmPin;
use embassy_stm32::timer::{Ch1, Channel};
use embassy_stm32::usart::{self, Config as UartConfig, RingBufferedUartRx, Uart, UartTx};
use embassy_stm32::Peri;
use embassy_time::{Duration, Instant, Timer};
use panic_halt as _;
use sap_flux_core::civil_time::{datetime_to_unix_ms, unix_ms_to_datetime, UtcDateTime};
use sap_flux_core::pulse::{
    BaselineRequest, Effect, Health, Machine, PulseDuration, Rejection, State,
};
use sap_flux_protocol::{
    encode, Ack, BoardState, Decoder, Hello, Message, Sample, Status, CAP_ADC, CAP_HEATER_CONTROL,
    CAP_RTC, CAP_V24_MONITOR, FLAG_ADC_ERROR, FLAG_COMMAND_WINDOW, FLAG_EXCITATION_ACTIVE,
    FLAG_HEATER_ACTIVE, FLAG_PGOOD, FLAG_RTC_VALID, MAX_ENCODED_LEN,
};
use static_cell::StaticCell;
use stm32_metapac as pac;

const INVALID_PARAMETER_RESULT: u8 = 0x02;
const BACKUP_WRITE_FAILED_RESULT: u8 = 0x03;
const PERSISTENCE_FAILED_RESULT: u8 = 0x20;
const OK_RESULT: u8 = 0;
const ADS1115_ADDR: u8 = 0x48;
const EEPROM_ADDR: u8 = 0x50;
const LOCKOUT_ADDR: u16 = 0x0fc0;
const LOCKOUT_MAGIC: u32 = 0x5346_4c4b;
const ADS_MUX_AIN0_GND: u16 = 0x4000;
const ADS_MUX_AIN1_GND: u16 = 0x5000;
const RTC_MAGIC: u32 = 0x5346_5031;
const RTC_MAGIC_INVERSE: u32 = !RTC_MAGIC;
#[cfg(not(feature = "dev-short-cooldown"))]
const FIRMWARE_VERSION: u32 = 0x00_01_02;
// Patch 32 is intentionally conspicuous in the monitor: this is the
// supervised bench build with a 30-second persistent cooldown.
#[cfg(feature = "dev-short-cooldown")]
const FIRMWARE_VERSION: u32 = 0x00_01_20;
const SAMPLE_PERIOD: Duration = Duration::from_millis(20);
const COMMAND_FRAME_PERIOD: Duration = Duration::from_millis(60);
const COMMAND_WINDOW_EVERY_SAMPLES: u32 = 50;
// 32.768 kHz ticks: a wait of n ticks lasts at least n-1 ticks (30.5 us each).
// 200 us -> 7 ticks -> >= 183 us; 1350 us -> 45 ticks -> >= 1343 us, above the
// ADS1115 860 SPS worst case of 1279 us (1/860 s + 10 % oscillator).
const MUX_SETTLE_US: u64 = 200;
const ADS_CONVERSION_US: u64 = 1_350;
const MIN_EXCITATION_RAW: i16 = 100;
const INVALID_ADC: i16 = i16::MIN;
const STATUS_PERIOD: Duration = Duration::from_secs(1);
const UART_HELLO_PERIOD: Duration = Duration::from_secs(30);
const FAULT_PERSISTENCE: u8 = 1;
const FAULT_PULSE_OVERRUN: u8 = 2;

static RS485_RX_DMA_BUFFER: StaticCell<[u8; 256]> = StaticCell::new();
static TTL_UART_RX_DMA_BUFFER: StaticCell<[u8; 256]> = StaticCell::new();

bind_interrupts!(struct Irqs {
    USART2_LPUART2 =>
        usart::InterruptHandler<peripherals::USART2>,
        usart::InterruptHandler<peripherals::LPUART2>;
    DMA1_CHANNEL1 => dma::InterruptHandler<peripherals::DMA1_CH1>;
    DMA1_CHANNEL2_3 =>
        dma::InterruptHandler<peripherals::DMA1_CH2>,
        dma::InterruptHandler<peripherals::DMA1_CH3>;
    DMA1_CH4_7_DMA2_CH1_5_DMAMUX1_OVR => dma::InterruptHandler<peripherals::DMA1_CH4>;
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

struct TtlUart<'d> {
    tx: UartTx<'d, Async>,
    rx: RingBufferedUartRx<'d>,
    tx_sequence: u32,
    decoder: Decoder,
}

#[derive(Clone, Copy)]
enum Ingress {
    Rs485,
    TtlUart,
}

struct CommandResponses {
    messages: [Option<Message>; 3],
}

impl CommandResponses {
    fn none() -> Self {
        Self {
            messages: [None, None, None],
        }
    }

    fn ack_and_status(ack: Message, status: Message) -> Self {
        Self {
            messages: [Some(ack), Some(status), None],
        }
    }

    fn query(ack: Message, hello: Message, status: Message) -> Self {
        Self {
            messages: [Some(ack), Some(hello), Some(status)],
        }
    }
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
        // DE setup before the first start bit. A sub-tick Timer would round to
        // one 30.5 us tick of random phase, so busy-wait ~20 us at HSI16.
        cortex_m::asm::delay(320);
        let _ = self.tx.write(&frame[..length]).await;
        let _ = self.tx.flush().await;
        self.de.set_low();
        self.n_re.set_low();
    }

    fn poll_message(&mut self) -> Option<Message> {
        for _ in 0..MAX_ENCODED_LEN {
            match embedded_hal_nb::serial::Read::read(&mut self.rx) {
                Ok(byte) => {
                    if let Some(Ok(frame)) = self.decoder.push(byte) {
                        return Some(frame.message);
                    }
                }
                Err(nb::Error::WouldBlock) => return None,
                Err(nb::Error::Other(_)) => return None,
            }
        }
        None
    }
}

impl<'d> TtlUart<'d> {
    async fn send_message(&mut self, message: Message) {
        let mut frame = [0_u8; MAX_ENCODED_LEN];
        let Ok(length) = encode(&message, self.tx_sequence, &mut frame) else {
            return;
        };
        self.tx_sequence = self.tx_sequence.wrapping_add(1);
        let _ = self.tx.write(&frame[..length]).await;
        let _ = self.tx.flush().await;
    }

    fn poll_message(&mut self) -> Option<Message> {
        for _ in 0..MAX_ENCODED_LEN {
            match embedded_hal_nb::serial::Read::read(&mut self.rx) {
                Ok(byte) => {
                    if let Some(Ok(frame)) = self.decoder.push(byte) {
                        return Some(frame.message);
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

struct HardwarePulse<'d> {
    timer: LowLevelTimer<'d, peripherals::TIM1>,
    _pin: PwmPin<'d, peripherals::TIM1, Ch1>,
}

impl<'d> HardwarePulse<'d> {
    fn new(timer: Peri<'d, peripherals::TIM1>, pin: Peri<'d, peripherals::PA8>) -> Self {
        let pin = PwmPin::new(pin, OutputType::PushPull);
        let mut timer = LowLevelTimer::new(timer);
        timer.stop();
        timer.set_counting_mode(CountingMode::EdgeAlignedUp);
        timer.set_tick_freq(Hertz::khz(1));
        timer.set_output_polarity(Channel::Ch1, OutputPolarity::ActiveHigh);
        timer.set_output_compare_preload(Channel::Ch1, false);
        timer.set_autoreload_preload(false);
        timer.regs_gp16().cr1().modify(|w| w.set_opm(true));
        timer.enable_outputs();

        let mut pulse = Self { timer, _pin: pin };
        pulse.force_off();
        pulse
    }

    fn start(&mut self, duration: PulseDuration) {
        self.force_off();
        self.timer
            .set_output_compare_mode(Channel::Ch1, OutputCompareMode::PwmMode2);
        self.timer.set_max_compare_value(duration.as_ms());
        self.timer.set_compare_value(Channel::Ch1, 1);
        self.timer.generate_update_event();
        self.timer.reset();
        self.timer.enable_channel(Channel::Ch1, true);
        self.timer.start();
    }

    fn force_off(&mut self) {
        self.timer.stop();
        self.timer.enable_channel(Channel::Ch1, false);
        self.timer
            .set_output_compare_mode(Channel::Ch1, OutputCompareMode::ForceInactive);
        self.timer.reset();
        self.timer.enable_channel(Channel::Ch1, true);
    }

    fn is_running(&self) -> bool {
        self.timer.regs_core().cr1().read().cen()
    }
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
    let p = sap_flux_blinky::init(config);

    let mut hardware_pulse = HardwarePulse::new(p.TIM1, p.PA8);
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
    let mut eeprom_wp = Output::new(p.PA12, Level::High, Speed::Low);
    let _flash_cs = Output::new(p.PB12, Level::High, Speed::VeryHigh);
    let _led_red = Output::new(p.PB3, Level::High, Speed::Low);
    let _led_green = Output::new(p.PB4, Level::High, Speed::Low);
    let _led_blue = Output::new(p.PB5, Level::High, Speed::Low);

    hardware_pulse.force_off();
    mux.enable_bank_a();

    // J4 pin 5 / PA6 is a boot-only mode strap. Open selects normal SWD on
    // J6; grounding it before reset repurposes J6 PA13/PA14 as LPUART2.
    // Deliberately do not support hot switching: changing the strap requires
    // a board reset so the SWD pins cannot become UART pins unexpectedly.
    let ttl_uart_enabled = Input::new(p.PA6, Pull::Up).is_low();

    let mut rs485_uart_config = UartConfig::default();
    rs485_uart_config.baudrate = 115_200;
    let uart = Uart::new(
        p.USART2,
        p.PA3,
        p.PA2,
        p.DMA1_CH1,
        p.DMA1_CH2,
        Irqs,
        rs485_uart_config,
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

    let mut ttl_uart = if ttl_uart_enabled {
        let mut ttl_uart_config = UartConfig::default();
        ttl_uart_config.baudrate = 115_200;
        let uart = Uart::new(
            p.LPUART2,
            p.PA13,
            p.PA14,
            p.DMA1_CH3,
            p.DMA1_CH4,
            Irqs,
            ttl_uart_config,
        )
        .unwrap();
        let (tx, rx) = uart.split();
        let rx_buffer = TTL_UART_RX_DMA_BUFFER.init([0; 256]);
        let mut rx = rx.into_ring_buffered(rx_buffer);
        rx.start_uart();
        Some(TtlUart {
            tx,
            rx,
            tx_sequence: 0,
            decoder: Decoder::new(),
        })
    } else {
        None
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
    let mut rtc_valid = read_rtc_magic();
    // Migrate clocks set by the previous one-word marker, but do not accept a
    // merely plausible calendar after arbitrary backup-domain corruption.
    if !rtc_valid && read_legacy_rtc_magic() && rtc_calendar_plausible(&rtc_provider) {
        write_rtc_magic();
        rtc_valid = read_rtc_magic();
    }
    let board_id = board_id();
    let lockout_read = read_lockout(&mut i2c);
    let mut fault_code = if lockout_read.is_err() {
        FAULT_PERSISTENCE
    } else {
        0
    };
    let mut machine = Machine::boot(Instant::now().as_millis(), lockout_read.unwrap_or(true));

    excitation.set_high();
    Timer::after_millis(5).await;
    broadcast_message(&mut rs485, &mut ttl_uart, hello_message(board_id)).await;
    broadcast_message(
        &mut rs485,
        &mut ttl_uart,
        status_message(&machine, &rtc_provider, rtc_valid, fault_code),
    )
    .await;

    let mut watchdog_bootstrapped = false;
    let mut watchdog_serviced = false;
    let mut sample_counter = 0_u32;
    let mut next_status_at = Instant::now() + STATUS_PERIOD;
    let mut next_uart_hello_at = Instant::now() + UART_HELLO_PERIOD;

    loop {
        let frame_started = Instant::now();
        service_pulse_state(
            &mut machine,
            &mut hardware_pulse,
            &mut i2c,
            &mut eeprom_wp,
            &mut fault_code,
        )
        .await;
        let command_window =
            sample_counter % COMMAND_WINDOW_EVERY_SAMPLES == COMMAND_WINDOW_EVERY_SAMPLES - 1;
        sample_counter = sample_counter.wrapping_add(1);
        mux.enable_bank_a();
        excitation.set_high();

        let (sample, heater_health_valid) = acquire_sample(
            &mut i2c,
            &mut internal_adc,
            &mut v24_monitor,
            &mut mux,
            &rtc_provider,
            rtc_valid,
            pgood.is_high(),
            command_window,
            &machine,
            hardware_pulse.is_running(),
        )
        .await;
        machine.update_health(Health {
            rtc_valid,
            pgood: sample.flags & FLAG_PGOOD != 0,
            adc_valid: heater_health_valid,
            measured_at_ms: Instant::now().as_millis(),
        });

        // TPL5010 does not emit WAKE at the beginning of its first cycle.
        // Reaching this point proves the control loop completed an acquisition
        // cycle. Sensor/power health separately gates heater commands and must
        // never deliberately expire the external firmware-hang watchdog.
        if !watchdog_bootstrapped {
            pulse_watchdog_done(&mut watchdog_done).await;
            watchdog_bootstrapped = true;
            watchdog_serviced = watchdog_wake.is_high();
        }
        broadcast_message(&mut rs485, &mut ttl_uart, Message::Sample(sample)).await;
        if Instant::now() >= next_status_at {
            next_status_at = Instant::now() + STATUS_PERIOD;
            broadcast_message(
                &mut rs485,
                &mut ttl_uart,
                status_message(&machine, &rtc_provider, rtc_valid, fault_code),
            )
            .await;
        }
        if ttl_uart.is_some() && Instant::now() >= next_uart_hello_at {
            next_uart_hello_at = Instant::now() + UART_HELLO_PERIOD;
            broadcast_message(&mut rs485, &mut ttl_uart, hello_message(board_id)).await;
        }

        let frame_deadline = frame_started
            + if command_window {
                COMMAND_FRAME_PERIOD
            } else {
                SAMPLE_PERIOD
            };
        loop {
            while let Some(message) = rs485.poll_message() {
                let responses = handle_command(
                    message,
                    &mut rtc,
                    &rtc_provider,
                    &mut rtc_valid,
                    board_id,
                    &mut machine,
                    &mut hardware_pulse,
                    &mut i2c,
                    &mut eeprom_wp,
                    &mut fault_code,
                )
                .await;
                send_responses(Ingress::Rs485, responses, &mut rs485, &mut ttl_uart).await;
            }

            loop {
                let message = ttl_uart.as_mut().and_then(TtlUart::poll_message);
                let Some(message) = message else {
                    break;
                };
                let responses = handle_command(
                    message,
                    &mut rtc,
                    &rtc_provider,
                    &mut rtc_valid,
                    board_id,
                    &mut machine,
                    &mut hardware_pulse,
                    &mut i2c,
                    &mut eeprom_wp,
                    &mut fault_code,
                )
                .await;
                send_responses(Ingress::TtlUart, responses, &mut rs485, &mut ttl_uart).await;
            }

            service_pulse_state(
                &mut machine,
                &mut hardware_pulse,
                &mut i2c,
                &mut eeprom_wp,
                &mut fault_code,
            )
            .await;

            if watchdog_wake.is_high() {
                if !watchdog_serviced {
                    pulse_watchdog_done(&mut watchdog_done).await;
                    watchdog_serviced = true;
                }
            } else {
                watchdog_serviced = false;
            }

            if Instant::now() >= frame_deadline {
                break;
            }
            Timer::after_micros(250).await;
        }
    }
}

async fn pulse_watchdog_done(done: &mut Output<'_>) {
    done.set_high();
    Timer::after_millis(1).await;
    done.set_low();
}

async fn broadcast_message(
    rs485: &mut Rs485<'_>,
    ttl_uart: &mut Option<TtlUart<'_>>,
    message: Message,
) {
    if let Some(ttl_uart) = ttl_uart.as_mut() {
        // Both UARTs have independent DMA channels, so transmit in parallel.
        // This keeps a 50 Hz Sample inside the existing acquisition budget.
        join(rs485.send_message(message), ttl_uart.send_message(message)).await;
    } else {
        rs485.send_message(message).await;
    }
}

async fn send_responses(
    ingress: Ingress,
    responses: CommandResponses,
    rs485: &mut Rs485<'_>,
    ttl_uart: &mut Option<TtlUart<'_>>,
) {
    for message in responses.messages.into_iter().flatten() {
        match ingress {
            Ingress::Rs485 => rs485.send_message(message).await,
            Ingress::TtlUart => {
                if let Some(ttl_uart) = ttl_uart.as_mut() {
                    ttl_uart.send_message(message).await;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn acquire_sample(
    i2c: &mut BoardI2c<'_>,
    internal_adc: &mut BoardAdc<'_>,
    v24_monitor: &mut V24MonitorPin<'_>,
    mux: &mut MuxPins<'_>,
    rtc_provider: &RtcTimeProvider,
    rtc_valid: bool,
    pgood: bool,
    command_window: bool,
    machine: &Machine,
    heater_active: bool,
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
    if heater_active {
        flags |= FLAG_HEATER_ACTIVE;
    }

    // Individual thermistors are data channels, not heater interlocks. Keep
    // recording every raw value and let the host/post-processing classify
    // open, shorted, or otherwise unusable channels. Firmware admission only
    // verifies that the measurement and power infrastructure itself is alive.
    let heater_health_valid = !adc_error
        && pgood
        && excitation_raw >= MIN_EXCITATION_RAW
        && (1_000..=4_000).contains(&v24_raw);

    (
        Sample {
            monotonic_us,
            rtc_unix_ms,
            run_id: machine.run_id(),
            flags,
            excitation_raw,
            thermistor_raw,
            v24_raw,
            state: board_state(machine.state()),
        },
        heater_health_valid,
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

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    message: Message,
    rtc: &mut Rtc,
    rtc_provider: &RtcTimeProvider,
    rtc_valid: &mut bool,
    board_id: u64,
    machine: &mut Machine,
    hardware_pulse: &mut HardwarePulse<'_>,
    i2c: &mut BoardI2c<'_>,
    eeprom_wp: &mut Output<'_>,
    fault_code: &mut u8,
) -> CommandResponses {
    match message {
        Message::Query(command) => CommandResponses::query(
            ack_message(
                sap_flux_protocol::MSG_QUERY,
                OK_RESULT,
                command.command_nonce,
                machine.run_id(),
            ),
            hello_message(board_id),
            status_message(machine, rtc_provider, *rtc_valid, *fault_code),
        ),
        Message::SetRtc(command) => {
            // A sync can wait up to one second for a whole-second boundary.
            // Only SafeIdle may pause command processing for that long.
            let mut result = if machine.state() == State::SafeIdle {
                set_rtc(rtc, command.unix_ms).await
            } else {
                rejection_result(Rejection::WrongState)
            };
            if result == OK_RESULT {
                write_rtc_magic();
                *rtc_valid = read_rtc_magic();
                if !*rtc_valid {
                    result = BACKUP_WRITE_FAILED_RESULT;
                }
            }
            CommandResponses::ack_and_status(
                ack_message(
                    sap_flux_protocol::MSG_SET_RTC,
                    result,
                    command.command_nonce,
                    machine.run_id(),
                ),
                status_message(machine, rtc_provider, *rtc_valid, *fault_code),
            )
        }
        Message::StartBaseline(command) => {
            let result = if *fault_code != 0 {
                PERSISTENCE_FAILED_RESULT
            } else {
                machine
                    .start_baseline(
                        BaselineRequest {
                            run_id: command.run_id,
                            baseline_duration_ms: command.baseline_duration_ms,
                            recovery_duration_ms: command.recovery_duration_ms,
                            sample_hz: command.requested_sample_hz,
                        },
                        Instant::now().as_millis(),
                    )
                    .map(|_| OK_RESULT)
                    .unwrap_or_else(rejection_result)
            };
            CommandResponses::ack_and_status(
                ack_message(
                    sap_flux_protocol::MSG_START_BASELINE,
                    result,
                    command.command_nonce,
                    machine.run_id(),
                ),
                status_message(machine, rtc_provider, *rtc_valid, *fault_code),
            )
        }
        Message::ArmPulse(command) => {
            let result = PulseDuration::from_ms(command.pulse_duration_ms)
                .ok_or(Rejection::InvalidParameter)
                .and_then(|duration| {
                    machine.arm(command.run_id, duration, Instant::now().as_millis())
                })
                .map(|_| OK_RESULT)
                .unwrap_or_else(rejection_result);
            CommandResponses::ack_and_status(
                ack_message(
                    sap_flux_protocol::MSG_ARM_PULSE,
                    result,
                    command.command_nonce,
                    machine.run_id(),
                ),
                status_message(machine, rtc_provider, *rtc_valid, *fault_code),
            )
        }
        Message::FirePulse(command) => {
            let result = match machine.request_fire(
                command.run_id,
                command.command_nonce,
                Instant::now().as_millis(),
            ) {
                Ok(Effect::PersistLockout) => {
                    if persist_lockout(i2c, eeprom_wp, command.run_id, command.command_nonce).await
                    {
                        match machine.confirm_lockout_persisted(Instant::now().as_millis()) {
                            Ok(Effect::StartHardwarePulse(duration)) => {
                                hardware_pulse.start(duration);
                                OK_RESULT
                            }
                            Ok(_) => INVALID_PARAMETER_RESULT,
                            Err(rejection) => rejection_result(rejection),
                        }
                    } else {
                        hardware_pulse.force_off();
                        machine.persistence_failed();
                        *fault_code = FAULT_PERSISTENCE;
                        PERSISTENCE_FAILED_RESULT
                    }
                }
                Ok(_) => INVALID_PARAMETER_RESULT,
                Err(rejection) => rejection_result(rejection),
            };
            CommandResponses::ack_and_status(
                ack_message(
                    sap_flux_protocol::MSG_FIRE_PULSE,
                    result,
                    command.command_nonce,
                    machine.run_id(),
                ),
                status_message(machine, rtc_provider, *rtc_valid, *fault_code),
            )
        }
        Message::Abort(command) => {
            let result = if machine.run_id() != command.run_id {
                rejection_result(Rejection::WrongRun)
            } else {
                hardware_pulse.force_off();
                machine.abort(Instant::now().as_millis());
                OK_RESULT
            };
            CommandResponses::ack_and_status(
                ack_message(
                    sap_flux_protocol::MSG_ABORT,
                    result,
                    command.command_nonce,
                    machine.run_id(),
                ),
                status_message(machine, rtc_provider, *rtc_valid, *fault_code),
            )
        }
        Message::Hello(_)
        | Message::Sample(_)
        | Message::Status(_)
        | Message::Ack(_)
        | Message::Error(_) => CommandResponses::none(),
        _ => CommandResponses::none(),
    }
}

async fn service_pulse_state(
    machine: &mut Machine,
    hardware_pulse: &mut HardwarePulse<'_>,
    i2c: &mut BoardI2c<'_>,
    eeprom_wp: &mut Output<'_>,
    fault_code: &mut u8,
) {
    let now_ms = Instant::now().as_millis();
    if machine.state() == State::Pulsing && !hardware_pulse.is_running() {
        hardware_pulse.force_off();
        if machine.pulse_finished(now_ms).is_err() {
            *fault_code = FAULT_PULSE_OVERRUN;
        }
    }

    match machine.tick(now_ms) {
        Effect::ForceHeaterOff => {
            hardware_pulse.force_off();
            if machine.state() == State::Fault {
                *fault_code = FAULT_PULSE_OVERRUN;
            }
        }
        Effect::ClearLockout => {
            hardware_pulse.force_off();
            if !clear_lockout(i2c, eeprom_wp).await {
                *fault_code = FAULT_PERSISTENCE;
            }
        }
        Effect::None | Effect::PersistLockout | Effect::StartHardwarePulse(_) => {}
    }
}

fn read_lockout(i2c: &mut BoardI2c<'_>) -> Result<bool, embassy_stm32::i2c::Error> {
    let address = [(LOCKOUT_ADDR >> 8) as u8, LOCKOUT_ADDR as u8];
    let mut record = [0_u8; 16];
    i2c.blocking_write_read(EEPROM_ADDR, &address, &mut record)?;
    Ok(u32::from_le_bytes(record[0..4].try_into().unwrap()) == LOCKOUT_MAGIC)
}

async fn persist_lockout(
    i2c: &mut BoardI2c<'_>,
    wp: &mut Output<'_>,
    run_id: u32,
    nonce: u64,
) -> bool {
    let mut record = [0_u8; 16];
    record[0..4].copy_from_slice(&LOCKOUT_MAGIC.to_le_bytes());
    record[4..8].copy_from_slice(&run_id.to_le_bytes());
    record[8..16].copy_from_slice(&nonce.to_le_bytes());
    write_lockout_record(i2c, wp, &record).await
}

async fn clear_lockout(i2c: &mut BoardI2c<'_>, wp: &mut Output<'_>) -> bool {
    write_lockout_record(i2c, wp, &[0_u8; 16]).await
}

async fn write_lockout_record(
    i2c: &mut BoardI2c<'_>,
    wp: &mut Output<'_>,
    record: &[u8; 16],
) -> bool {
    let mut write = [0_u8; 18];
    write[0] = (LOCKOUT_ADDR >> 8) as u8;
    write[1] = LOCKOUT_ADDR as u8;
    write[2..].copy_from_slice(record);

    wp.set_low();
    Timer::after_millis(1).await;
    let wrote = i2c.blocking_write(EEPROM_ADDR, &write).is_ok() && eeprom_wait_ready(i2c).await;
    wp.set_high();
    if !wrote {
        return false;
    }

    let address = [(LOCKOUT_ADDR >> 8) as u8, LOCKOUT_ADDR as u8];
    let mut verify = [0_u8; 16];
    i2c.blocking_write_read(EEPROM_ADDR, &address, &mut verify)
        .is_ok()
        && verify == *record
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

fn rejection_result(rejection: Rejection) -> u8 {
    match rejection {
        Rejection::WrongState => 0x10,
        Rejection::WrongRun => 0x11,
        Rejection::InvalidParameter => INVALID_PARAMETER_RESULT,
        Rejection::BaselineIncomplete => 0x12,
        Rejection::RtcInvalid => 0x13,
        Rejection::PowerNotGood => 0x14,
        Rejection::AdcUnhealthy => 0x15,
        Rejection::HealthStale => 0x16,
        Rejection::LockoutPending => 0x17,
        Rejection::DuplicateFire => 0x18,
    }
}

fn board_state(state: State) -> BoardState {
    match state {
        State::SafeIdle => BoardState::SafeIdle,
        State::Baseline => BoardState::Baseline,
        State::Armed => BoardState::Armed,
        State::Pulsing => BoardState::Pulsing,
        State::Recovery => BoardState::Recovery,
        State::Cooldown => BoardState::Cooldown,
        State::Fault => BoardState::Fault,
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
        && pac::TAMP.bkpr(1).read().bkp() == RTC_MAGIC_INVERSE
}

fn read_legacy_rtc_magic() -> bool {
    pac::TAMP.bkpr(0).read().bkp() == RTC_MAGIC
}

fn write_rtc_magic() {
    pac::TAMP.bkpr(0).write(|w| w.set_bkp(RTC_MAGIC));
    pac::TAMP.bkpr(1).write(|w| w.set_bkp(RTC_MAGIC_INVERSE));
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
        capabilities: CAP_RTC | CAP_ADC | CAP_V24_MONITOR | CAP_HEATER_CONTROL,
    })
}

fn status_message(
    machine: &Machine,
    provider: &RtcTimeProvider,
    rtc_valid: bool,
    fault_code: u8,
) -> Message {
    Message::Status(Status {
        monotonic_us: Instant::now().as_micros(),
        rtc_unix_ms: rtc_now_ms(provider, rtc_valid).unwrap_or(-1),
        run_id: machine.run_id(),
        state: board_state(machine.state()),
        fault_code,
        pulse_duration_ms: machine.pulse_duration().as_ms(),
        cooldown_remaining_ms: machine.cooldown_remaining_ms(Instant::now().as_millis()),
    })
}

fn ack_message(command_type: u8, result: u8, command_nonce: u64, run_id: u32) -> Message {
    Message::Ack(Ack {
        command_type,
        result,
        command_nonce,
        run_id,
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
