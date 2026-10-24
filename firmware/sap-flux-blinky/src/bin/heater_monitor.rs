#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::bind_interrupts;
use embassy_stm32::dma;
use embassy_stm32::gpio::{Input, Level, Output, OutputType, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c, Master as I2cMaster};
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::peripherals::{self, ADC1, PB1};
use embassy_stm32::rtc::{DateTime, DayOfWeek, Rtc, RtcConfig, RtcTimeProvider};
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{
    CountingMode, OutputCompareMode, OutputPolarity, Timer as LowLevelTimer,
};
use embassy_stm32::timer::simple_pwm::PwmPin;
use embassy_stm32::timer::{Ch1, Channel};
use embassy_stm32::wdg::IndependentWatchdog;
use embassy_stm32::usart::{self, Config as UartConfig, RingBufferedUartRx, Uart, UartTx};
use embassy_stm32::Peri;
use embassy_time::{Duration, Instant, Timer};
use panic_halt as _;
use sap_flux_blinky::w25q::W25q;
use sap_flux_core::civil_time::{datetime_to_unix_ms, unix_ms_to_datetime, UtcDateTime};
use sap_flux_core::flash::{
    Flash, Nor, Ready, ERASE_TIMEOUT_MS, JEDEC_W25Q256JV, RAW_BASE, RAW_SECTORS,
};
use sap_flux_core::pulse::{
    BaselineRequest, Effect, Health, Machine, PulseDuration, Rejection, State, COOLDOWN_MS,
};
use sap_flux_core::rawlog::{Geometry, RawLog, CHUNK_LEN};
use sap_flux_core::records;
use sap_flux_core::schedule::{Action, Inputs, Planner};
use sap_flux_core::trim::{Calr, TrimLearner, TRIM_LIMIT_PPB};
use sap_flux_core::window::{WindowParams, WindowStore, BIN_BYTES};
use sap_flux_protocol::raw::RawFrame;
use sap_flux_protocol::{
    encode, Ack, BoardState, Decoder, ErrorReport, FieldStatus, Hello, Message, Sample12,
    Schedule, Status, CAP_ADC, CAP_HEATER_CONTROL, CAP_RTC, CAP_V24_MONITOR, FIELD_STATUS_ENABLED,
    FIELD_STATUS_FLASH_OK,
    FIELD_STATUS_HEATER_LOCKOUT, FIELD_STATUS_LEASE_VALID, FIELD_STATUS_REQUIRE_LEASE,
    FIELD_STATUS_RTC_VALID, FLAG_ADC_ERROR, FLAG_COMMAND_WINDOW, FLAG_EXCITATION_ACTIVE,
    FLAG_HEATER_ACTIVE, FLAG_PGOOD, FLAG_RTC_VALID, MAX_ENCODED_LEN, RawChunk,
    WINDOW_FLAG_HEATER_FAULT,
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
// TAMP backup registers (5 on STM32G0B1). 0/1 hold the RTC-valid magic and
// share the RTC's backup domain, which also validates the others.
const BKP_LEASE_UNTIL: usize = 2;
// RTC trim learning state (TAMP, same backup domain as the RTC): anchor
// correction time (unix s, 0 = none) and the sum of steps since then (ms).
const BKP_TRIM_ANCHOR_UNIX_S: usize = 3;
const BKP_TRIM_STEPS_MS: usize = 4;
const EEPROM_PAGE_LEN: usize = records::PAGE_LEN as usize;
#[cfg(not(feature = "dev-short-cooldown"))]
const FIRMWARE_VERSION: u32 = 0x00_02_00;
// Patch 32 is intentionally conspicuous in the monitor: this is the
// supervised bench build with a 30-second persistent cooldown.
#[cfg(feature = "dev-short-cooldown")]
const FIRMWARE_VERSION: u32 = 0x00_02_20;
// One frame = 12 thermistors + excitation at 25 Hz. Budget (Python, 400 kHz
// I2C, 32.768 kHz ticks): 12 x 1.88 ms + 1.67 ms = 24.3 ms acquisition, plus
// a 5.7 ms Sample12 and, once per second, a 3.6 ms Status on RS-485.
const FRAME_PERIOD: Duration = Duration::from_millis(40);
// RS-485 bench streaming cycle (25 frames = 1 s): the last frame of the cycle
// carries FLAG_COMMAND_WINDOW and nothing follows it; the first frame of the
// next cycle sends nothing, so the bus stays quiet ~45 ms for one host
// command. Status rides on a mid-cycle frame so it never closes the window.
const FRAMES_PER_CYCLE: u32 = 25;
const STATUS_FRAME_IN_CYCLE: u32 = 12;

/// The 12 window/Sample12 thermistor channels (probe 1 T1..T6, then probe 2
/// T1..T6) as (mux bank, 74HC4067 channel).
///
/// Probe 1 is the bench-verified J2 block A wiring (J2 p3..p8 = therm_1..6 →
/// mux A Y1, Y0, Y3, Y2, Y5, Y4). Probe 2 assumes an identical cable on J2
/// block B (p21 GND, p22 EXC, p23..p28 = therm_17..22), which elec/adc.ato
/// routes to mux B (PA10) Y1, Y0, Y3, Y2, Y5, Y4.
/// NEEDS BENCH CONFIRMATION: probe 2 has never been read on hardware. Touch
/// each probe-2 thermistor and check that P2T1..P2T6 respond in order.
const THERMISTOR_MUX_MAP: [(MuxBank, u8); 12] = [
    (MuxBank::A, 1),
    (MuxBank::A, 0),
    (MuxBank::A, 3),
    (MuxBank::A, 2),
    (MuxBank::A, 5),
    (MuxBank::A, 4),
    (MuxBank::B, 1),
    (MuxBank::B, 0),
    (MuxBank::B, 3),
    (MuxBank::B, 2),
    (MuxBank::B, 5),
    (MuxBank::B, 4),
];
// Read order (indices into THERMISTOR_MUX_MAP): matched-depth pairs (T1/T4,
// T2/T5, T3/T6) are read back to back to minimize pair skew, as before; the
// excitation read sits after the first three probe-1 channels, near the
// frame's midpoint, and stamps the frame time.
const READ_ORDER_BEFORE_EXCITATION: [usize; 3] = [0, 3, 1];
const READ_ORDER_AFTER_EXCITATION: [usize; 9] = [4, 2, 5, 6, 9, 7, 10, 8, 11];
// 32.768 kHz ticks: a wait of n ticks lasts at least n-1 ticks (30.5 us each).
// 200 us -> 7 ticks -> >= 183 us; 1350 us -> 45 ticks -> >= 1343 us, above the
// ADS1115 860 SPS worst case of 1279 us (1/860 s + 10 % oscillator).
const MUX_SETTLE_US: u64 = 200;
const ADS_CONVERSION_US: u64 = 1_350;
const MIN_EXCITATION_RAW: i16 = 100;
const INVALID_ADC: i16 = i16::MIN;
// Field link: TX (PA14) is re-enabled this long before a reply so the
// Feather's receiver sees idle-high before the leading delimiter.
const TTL_TX_ENABLE_US: u64 = 200;
const PA14: usize = 14;
const FAULT_PERSISTENCE: u8 = 1;
const FAULT_PULSE_OVERRUN: u8 = 2;
// Internal watchdog (LSI, about +/-7 %). The longest deliberate stall is a
// SetRtc wait for the next whole second (<= 1 s) plus EEPROM writes; a
// reset forces the heater gate low through the pin reset state.
const IWDG_TIMEOUT_US: u32 = 4_000_000;
/// TPL5010 DONE cadence, independent of WAKE (period ~9.6 s).
const WATCHDOG_DONE_PERIOD: Duration = Duration::from_secs(1);
// W25Q256JV on SPI2. 4 MHz (PCLK 16 MHz / 4) is 4x board_test's proven
// 1 MHz and well under the chip's limit: a 256 B page moves in ~0.6 ms.
const FLASH_SPI_HZ: u32 = 4_000_000;
/// ErrorReport fault code sent at boot on the bench link when the flash
/// JEDEC ID is wrong or the chip stays busy; detail = ID bytes 2..3.
const ERROR_FLASH: u16 = 0x0100;

// Window store: 4 windows (the spec's "newest 4 undelivered") of up to 1000
// bins each = 104 000 B of the 144 KiB RAM (Python budget). The default
// schedule needs 482 bins; longer windows are cut and flagged TRUNCATED.
// A new window overwrites the oldest undelivered one when all are taken.
const WINDOW_SLOTS: usize = 4;
const WINDOW_MAX_BINS: usize = 1000;
type Windows = WindowStore<WINDOW_SLOTS, { WINDOW_MAX_BINS * BIN_BYTES }>;
// All-zero when empty, so this lands in .bss rather than flash.
static mut WINDOWS: Windows = Windows::new();

// Raw 25 Hz frame log: the lower 24 MiB of the W25Q256 (sap_flux_core::flash
// layout), two sectors kept erased ahead of the head (a 1.5 s pulse needs at
// most 7 pages), and an index of the newest 256 run starts for GetRaw.
const RAW_RUNS: usize = 256;
const RAW_GEOMETRY: Geometry = Geometry {
    base: RAW_BASE,
    sectors: RAW_SECTORS,
    ahead_sectors: 2,
};
// GetRaw waits this long for a program/erase in flight to finish: up to a
// worst-case erase between windows, but only a page program's worth while a
// window is recording, so a raw fetch never stalls the frame grid for long.
const RAW_READ_WAIT_IDLE_MS: u64 = 450;
const RAW_READ_WAIT_RUN_MS: u64 = 5;
type RawLogStore = RawLog<RAW_RUNS>;
// All-zero until mounted: .bss.
static mut RAW_LOG: RawLogStore = RawLogStore::new();

static RS485_RX_DMA_BUFFER: StaticCell<[u8; 256]> = StaticCell::new();
// Holds the commands the Feather sends during a heater pulse (not polled).
static TTL_UART_RX_DMA_BUFFER: StaticCell<[u8; 512]> = StaticCell::new();

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

/// The one host link chosen by the PA6 strap at boot.
enum Link<'d> {
    /// Bench (strap open): v1 RS-485 streaming of Sample12/Status.
    Rs485(Rs485<'d>),
    /// Field (strap low): LPUART2 on J6, replies only, SP3485 shut down.
    Ttl(TtlUart<'d>),
}

impl Link<'_> {
    fn streams(&self) -> bool {
        matches!(self, Link::Rs485(_))
    }

    fn poll_message(&mut self) -> Option<Message> {
        match self {
            Link::Rs485(link) => link.poll_message(),
            Link::Ttl(link) => link.poll_message(),
        }
    }

    async fn send(&mut self, messages: &[Option<Message>]) {
        match self {
            Link::Rs485(link) => {
                for message in messages.iter().flatten() {
                    link.send_message(*message).await;
                }
            }
            Link::Ttl(link) => link.send_burst(messages).await,
        }
    }
}

struct CommandResponses {
    messages: [Option<Message>; 4],
}

impl CommandResponses {
    fn none() -> Self {
        Self {
            messages: [None, None, None, None],
        }
    }

    fn one(message: Message) -> Self {
        Self {
            messages: [Some(message), None, None, None],
        }
    }

    fn ack_and_status(ack: Message, status: Message) -> Self {
        Self {
            messages: [Some(ack), Some(status), None, None],
        }
    }

    fn query(ack: Message, hello: Message, status: Message, field: Option<Message>) -> Self {
        Self {
            messages: [Some(ack), Some(hello), Some(status), field],
        }
    }
}

/// Field-mode state: autonomous schedule and bookkeeping for FieldStatus.
struct FieldState {
    /// PA6 strapped low at boot (LPUART field link).
    field_mode: bool,
    planner: Planner,
    /// What the EEPROM schedule record holds (None if unknown/unreadable).
    schedule_in_eeprom: Option<Schedule>,
    boot_count: u32,
    v24_raw: i16,
    windows: &'static mut Windows,
    /// RTC_CALR trim in effect, ppb (positive = slowed).
    trim_ppb: i32,
    /// Last applied SetRtc correction, new - old (0 if the RTC was invalid).
    last_rtc_step_ms: i32,
    /// Nonce of the last applied SetRtc: a retry is answered, not re-applied.
    last_set_rtc_nonce: Option<u64>,
}

/// Everything command handling and the scheduler act on.
struct Ctx<'d> {
    rtc: Rtc,
    rtc_provider: RtcTimeProvider,
    rtc_valid: bool,
    board_id: u64,
    machine: Machine,
    hardware_pulse: HardwarePulse<'d>,
    i2c: BoardI2c<'d>,
    eeprom_wp: Output<'d>,
    fault_code: u8,
    field: FieldState,
    flash: Flash<W25q<'d>>,
    raw: &'static mut RawLogStore,
}

impl Ctx<'_> {
    fn unix_ms(&self) -> Option<i64> {
        rtc_now_ms(&self.rtc_provider, self.rtc_valid)
    }

    fn status(&self) -> Message {
        status_message(&self.machine, &self.rtc_provider, self.rtc_valid, self.fault_code)
    }

    fn ack(&self, command_type: u8, result: u8, command_nonce: u64) -> Message {
        ack_message(command_type, result, command_nonce, self.machine.run_id())
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
    /// One reply burst: PA14 to AF, idle-high settle, a leading delimiter
    /// (resyncs the Feather's decoder after the line sat low), the frames,
    /// then PA14 back to analog so the BOOT0 pull-down draws nothing.
    async fn send_burst(&mut self, messages: &[Option<Message>]) {
        if messages.iter().all(Option::is_none) {
            return;
        }
        set_pa14_mode(pac::gpio::vals::Moder::ALTERNATE);
        Timer::after_micros(TTL_TX_ENABLE_US).await;
        let _ = self.tx.write(&[0x00]).await;
        for message in messages.iter().flatten() {
            let mut frame = [0_u8; MAX_ENCODED_LEN];
            let Ok(length) = encode(message, self.tx_sequence, &mut frame) else {
                continue;
            };
            self.tx_sequence = self.tx_sequence.wrapping_add(1);
            let _ = self.tx.write(&frame[..length]).await;
        }
        let _ = self.tx.flush().await;
        set_pa14_mode(pac::gpio::vals::Moder::ANALOG);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MuxBank {
    A,
    B,
}

/// One acquired frame.
#[derive(Clone, Copy)]
struct Frame {
    monotonic_us: u64,
    rtc_unix_ms: i64,
    thermistor_raw: [i16; 12],
    excitation_raw: i16,
    v24_raw: i16,
    flags: u16,
    heater_health_valid: bool,
}

impl MuxPins<'_> {
    /// Enable exactly one mux (active-low ~E); the others float off the
    /// shared Z → AIN0 node.
    fn enable_bank(&mut self, bank: MuxBank) {
        self.en_a.set_high();
        self.en_b.set_high();
        self.en_c.set_high();
        match bank {
            MuxBank::A => self.en_a.set_low(),
            MuxBank::B => self.en_b.set_low(),
        }
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
    let mut iwdg = IndependentWatchdog::new(p.IWDG, IWDG_TIMEOUT_US);
    iwdg.unleash();
    let mut watchdog_done = Output::new(p.PA5, Level::Low, Speed::Low);
    // WAKE (PA4) is not polled; see the DONE cadence in the main loop.
    let _watchdog_wake = Input::new(p.PA4, Pull::None);
    let pgood = Input::new(p.PC7, Pull::None);
    let mut eeprom_wp = Output::new(p.PA12, Level::High, Speed::Low);
    let mut spi_config = SpiConfig::default();
    spi_config.frequency = Hertz(FLASH_SPI_HZ);
    let mut w25q = W25q::new(
        Spi::new_blocking(p.SPI2, p.PB13, p.PB15, p.PB14, spi_config),
        Output::new(p.PB12, Level::High, Speed::VeryHigh),
    );
    let (flash_id, flash_idle) = flash_boot(&mut w25q, &mut || iwdg.pet()).await;
    let flash_ok = flash_idle && flash_id == Some(JEDEC_W25Q256JV);
    let mut flash = Flash::new(w25q, flash_ok);
    // SAFETY: the only reference to RAW_LOG, taken once in main.
    let raw = unsafe { &mut *core::ptr::addr_of_mut!(RAW_LOG) };
    // Reads 16 B of each of the 6144 sector headers (~0.4 s at 4 MHz).
    raw.mount(&mut flash, RAW_GEOMETRY, || iwdg.pet());
    let _led_red = Output::new(p.PB3, Level::High, Speed::Low);
    let _led_green = Output::new(p.PB4, Level::High, Speed::Low);
    let _led_blue = Output::new(p.PB5, Level::High, Speed::Low);

    hardware_pulse.force_off();
    mux.enable_bank(MuxBank::A);

    // J4 pin 5 / PA6 is a boot-only mode strap. Open selects normal SWD on
    // J6; grounding it before reset repurposes J6 PA13/PA14 as LPUART2.
    // Deliberately do not support hot switching: changing the strap requires
    // a board reset so the SWD pins cannot become UART pins unexpectedly.
    let ttl_uart_enabled = Input::new(p.PA6, Pull::Up).is_low();

    // Field mode holds the SP3485 in shutdown (~RE high, DE low) and leaves
    // USART2 unconfigured (PA2/PA3 stay analog).
    let mut _rs485_shutdown = None;
    let mut link = if ttl_uart_enabled {
        _rs485_shutdown = Some((
            Output::new(p.PA1, Level::Low, Speed::Low),
            Output::new(p.PA0, Level::High, Speed::Low),
        ));
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
        // TX idles high into PA14's 10 kOhm BOOT0 pull-down (330 uA):
        // keep the pin analog except while replying.
        set_pa14_mode(pac::gpio::vals::Moder::ANALOG);
        let (tx, rx) = uart.split();
        let rx_buffer = TTL_UART_RX_DMA_BUFFER.init([0; 512]);
        let mut rx = rx.into_ring_buffered(rx_buffer);
        rx.start_uart();
        Link::Ttl(TtlUart {
            tx,
            rx,
            tx_sequence: 0,
            decoder: Decoder::new(),
        })
    } else {
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
        Link::Rs485(Rs485 {
            tx,
            rx,
            de: Output::new(p.PA1, Level::Low, Speed::Low),
            n_re: Output::new(p.PA0, Level::Low, Speed::Low),
            tx_sequence: 0,
            decoder: Decoder::new(),
        })
    };

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = Hertz::khz(400);
    i2c_config.sda_pullup = true;
    i2c_config.scl_pullup = true;
    let mut i2c = I2c::new_blocking(p.I2C1, p.PB6, p.PB7, i2c_config);

    let mut v24_monitor = p.PB1;
    let mut internal_adc = Adc::new(p.ADC1);
    let (rtc, rtc_provider) = Rtc::new(p.RTC, RtcConfig::default());
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
    let fault_code = if lockout_read.is_err() {
        FAULT_PERSISTENCE
    } else {
        0
    };
    let machine = Machine::boot(Instant::now().as_millis(), lockout_read.unwrap_or(true));

    // Stored schedule: a blank, corrupt, unreadable or out-of-limits record
    // falls back to the factory default, which is disabled.
    let schedule_in_eeprom = eeprom_read::<{ records::SCHEDULE_RECORD_LEN }>(
        &mut i2c,
        records::SCHEDULE_ADDR,
    )
    .and_then(|r| records::decode_schedule(&r));
    let schedule = schedule_in_eeprom
        .filter(|s| s.validate(COOLDOWN_MS as u32).is_ok())
        .unwrap_or(Schedule::FACTORY_DEFAULT);
    let boot_count = eeprom_read::<{ records::WORD_RECORD_LEN }>(&mut i2c, records::BOOT_COUNT_ADDR)
        .and_then(|r| records::decode_boot_count(&r))
        .unwrap_or(0)
        .wrapping_add(1);
    let _ = eeprom_write_verified(
        &mut i2c,
        &mut eeprom_wp,
        records::BOOT_COUNT_ADDR,
        &records::encode_boot_count(boot_count),
    )
    .await;
    let lease_until = if rtc_valid { read_bkp(BKP_LEASE_UNTIL) } else { 0 };
    // Restore the learned RTC trim (CALR also survives in the backup domain
    // on VBAT, but a domain reset clears it).
    let trim_ppb = eeprom_read::<{ records::WORD_RECORD_LEN }>(&mut i2c, records::TRIM_ADDR)
        .and_then(|r| records::decode_trim(&r))
        .filter(|ppb| ppb.abs() <= TRIM_LIMIT_PPB)
        .map(Calr::from_ppb)
        .filter(|calr| write_calr(*calr))
        .map(Calr::ppb)
        .unwrap_or_else(|| read_calr().ppb());

    let mut ctx = Ctx {
        rtc,
        rtc_provider,
        rtc_valid,
        board_id,
        machine,
        hardware_pulse,
        i2c,
        eeprom_wp,
        fault_code,
        field: FieldState {
            field_mode: ttl_uart_enabled,
            planner: Planner::new(schedule, lease_until, boot_count),
            schedule_in_eeprom,
            boot_count,
            v24_raw: 0,
            // SAFETY: the only reference to WINDOWS, taken once in main.
            windows: unsafe { &mut *core::ptr::addr_of_mut!(WINDOWS) },
            trim_ppb,
            last_rtc_step_ms: 0,
            last_set_rtc_nonce: None,
        },
        flash,
        raw,
    };

    excitation.set_high();
    Timer::after_millis(5).await;
    // The field link sends nothing unsolicited.
    if link.streams() {
        // A wrong JEDEC ID (or a chip stuck busy) is reported once here;
        // the field link shows it as FieldStatus flash_ok = 0.
        let flash_error = (!flash_ok).then(|| {
            let id = flash_id.unwrap_or([0; 3]);
            Message::Error(ErrorReport {
                fault_code: ERROR_FLASH,
                detail: u16::from_be_bytes([id[1], id[2]]),
            })
        });
        link.send(&[Some(hello_message(board_id)), Some(ctx.status()), flash_error])
            .await;
    }

    let mut watchdog_bootstrapped = false;
    let mut last_watchdog_done = Instant::now();
    let mut frame_counter = 0_u32;
    let mut next_frame_at = Instant::now();

    loop {
        iwdg.pet();
        service_pulse_state(&mut ctx).await;
        let cycle_index = frame_counter % FRAMES_PER_CYCLE;
        frame_counter = frame_counter.wrapping_add(1);
        excitation.set_high();

        let frame = acquire_frame(
            &mut ctx.i2c,
            &mut internal_adc,
            &mut v24_monitor,
            &mut mux,
            &ctx.rtc_provider,
            ctx.rtc_valid,
            pgood.is_high(),
            ctx.hardware_pulse.is_running(),
        )
        .await;
        ctx.machine.update_health(Health {
            rtc_valid: ctx.rtc_valid,
            pgood: frame.flags & FLAG_PGOOD != 0,
            adc_valid: frame.heater_health_valid,
            measured_at_ms: Instant::now().as_millis(),
        });
        ctx.field.v24_raw = frame.v24_raw;

        // TPL5010 does not emit WAKE at the beginning of its first cycle.
        // Reaching this point proves the control loop completed an acquisition
        // cycle. Sensor/power health separately gates heater commands and must
        // never deliberately expire the external firmware-hang watchdog.
        if !watchdog_bootstrapped {
            pulse_watchdog_done(&mut watchdog_done).await;
            watchdog_bootstrapped = true;
            last_watchdog_done = Instant::now();
        }

        if ctx.field.windows.recording().is_some() {
            let mut values = [0_i16; 13];
            values[..12].copy_from_slice(&frame.thermistor_raw);
            values[12] = frame.excitation_raw;
            ctx.field
                .windows
                .push(frame.monotonic_us, &values, frame.v24_raw);
        }

        // Raw log: every frame of every run (scheduled or host-commanded),
        // RAM only here; flash writes happen in the wait loop below.
        if run_active(ctx.machine.state()) {
            let raw_frame = raw_frame(&frame, &ctx.machine);
            ctx.raw.push(ctx.machine.run_id(), &raw_frame);
        } else {
            ctx.raw.end_run();
        }

        run_scheduler(&mut ctx).await;
        close_window_if_done(&mut ctx);

        if link.streams() && cycle_index != 0 {
            let command_window = cycle_index == FRAMES_PER_CYCLE - 1;
            let extra = if command_window { FLAG_COMMAND_WINDOW } else { 0 };
            let status = (cycle_index == STATUS_FRAME_IN_CYCLE).then(|| ctx.status());
            link.send(&[Some(sample12_message(&frame, &ctx.machine, extra)), status])
                .await;
        }

        // Fixed-rate frames; after an overrun, restart the grid from now
        // (missed frames are simply absent; every frame carries its own time).
        next_frame_at += frame_period(&ctx);
        if next_frame_at < Instant::now() {
            next_frame_at = Instant::now();
        }
        let frame_deadline = next_frame_at;
        loop {
            // Field link: commands that arrive while the heater is on wait
            // in the RX ring buffer and are answered after the pulse.
            if link.streams() || !heater_on(&ctx) {
                while let Some(message) = link.poll_message() {
                    let responses = handle_command(message, &mut ctx).await;
                    link.send(&responses.messages).await;
                }
            }

            service_pulse_state(&mut ctx).await;
            service_storage(&mut ctx);

            // WAKE is only a 20 ms pulse, shorter than one acquisition, so
            // polling it can miss it and let the TPL5010 reset the MCU (which
            // fires the SCR heater latch). Instead pulse DONE from this loop on
            // a fixed cadence well inside the ~9.6 s period: any DONE at least
            // 20 ms before the next WAKE rising edge satisfies the TPL5010, and
            // a hung control loop still stops the pulses.
            if watchdog_bootstrapped
                && Instant::now() - last_watchdog_done >= WATCHDOG_DONE_PERIOD
            {
                pulse_watchdog_done(&mut watchdog_done).await;
                last_watchdog_done = Instant::now();
            }

            if Instant::now() >= frame_deadline {
                break;
            }
            iwdg.pet();
            Timer::after_micros(250).await;
        }
    }
}

/// Wake the flash and read its ID, then wait (bounded) for any program or
/// erase that a reset interrupted on the MCU side to finish. Returns the
/// JEDEC ID and whether the chip went idle.
async fn flash_boot(w25q: &mut W25q<'_>, pet: &mut impl FnMut()) -> (Option<[u8; 3]>, bool) {
    w25q.release_power_down();
    Timer::after_millis(1).await;
    let id = w25q.jedec_id();
    let start = Instant::now();
    loop {
        match w25q.busy() {
            Some(false) => return (id, true),
            Some(true) if Instant::now() - start < Duration::from_millis(ERASE_TIMEOUT_MS) => {}
            _ => return (id, false),
        }
        pet();
        Timer::after_millis(1).await;
    }
}

async fn pulse_watchdog_done(done: &mut Output<'_>) {
    done.set_high();
    Timer::after_millis(1).await;
    done.set_low();
}

fn set_pa14_mode(mode: pac::gpio::vals::Moder) {
    pac::GPIOA.moder().modify(|w| w.set_moder(PA14, mode));
}

async fn acquire_frame(
    i2c: &mut BoardI2c<'_>,
    internal_adc: &mut BoardAdc<'_>,
    v24_monitor: &mut V24MonitorPin<'_>,
    mux: &mut MuxPins<'_>,
    rtc_provider: &RtcTimeProvider,
    rtc_valid: bool,
    pgood: bool,
    heater_active: bool,
) -> Frame {
    let mut thermistor_raw = [INVALID_ADC; 12];
    let mut adc_error = false;

    for index in READ_ORDER_BEFORE_EXCITATION {
        thermistor_raw[index] = read_thermistor(i2c, mux, index).await.unwrap_or_else(|| {
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

    for index in READ_ORDER_AFTER_EXCITATION {
        thermistor_raw[index] = read_thermistor(i2c, mux, index).await.unwrap_or_else(|| {
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

    Frame {
        monotonic_us,
        rtc_unix_ms,
        thermistor_raw,
        excitation_raw,
        v24_raw,
        flags,
        heater_health_valid,
    }
}

fn run_active(state: State) -> bool {
    matches!(
        state,
        State::Baseline | State::Armed | State::Pulsing | State::Recovery
    )
}

fn heater_on(ctx: &Ctx<'_>) -> bool {
    ctx.machine.state() == State::Pulsing || ctx.hardware_pulse.is_running()
}

/// The Sample12 content of a frame, as the raw log stores it.
fn raw_frame(frame: &Frame, machine: &Machine) -> RawFrame {
    let mut values = [0_i16; 14];
    values[..12].copy_from_slice(&frame.thermistor_raw);
    values[12] = frame.excitation_raw;
    values[13] = frame.v24_raw;
    RawFrame {
        monotonic_us: frame.monotonic_us,
        rtc_unix_ms: frame.rtc_unix_ms,
        flags: frame.flags as u8,
        state: board_state(machine.state()) as u8,
        values,
    }
}

/// At most one flash operation per call (a page program is ~0.6 ms of SPI;
/// erases only start while the heater is off, and never block: the chip is
/// polled until done). Called every ~250 us from the frame wait loop.
fn service_storage(ctx: &mut Ctx<'_>) {
    let now_ms = Instant::now().as_millis();
    let allow_erase = !heater_on(ctx);
    ctx.raw.service(&mut ctx.flash, now_ms, allow_erase);
}

/// Wait (bounded) for the flash to finish a program or erase in flight.
async fn flash_idle(ctx: &mut Ctx<'_>, max_ms: u64) -> bool {
    let start = Instant::now().as_millis();
    loop {
        match ctx.flash.poll(Instant::now().as_millis()) {
            Ready::Ready => return true,
            Ready::Failed => return false,
            Ready::Busy if Instant::now().as_millis() - start >= max_ms => return false,
            Ready::Busy => Timer::after_micros(250).await,
        }
    }
}

/// GetRaw: one 100-byte chunk of a run's raw block stream from flash.
async fn get_raw(ctx: &mut Ctx<'_>, run_id: u32, chunk: u32, nonce: u64) -> Message {
    let not_found = || {
        ack_message(
            sap_flux_protocol::MSG_GET_RAW,
            sap_flux_protocol::ACK_NOT_FOUND,
            nonce,
            run_id,
        )
    };
    // Bench link: commands are served during a pulse, but flash reads are
    // not (no flash I/O but sample appends while the heater is on).
    if heater_on(ctx) {
        return ack_message(
            sap_flux_protocol::MSG_GET_RAW,
            rejection_result(Rejection::WrongState),
            nonce,
            run_id,
        );
    }
    if !ctx.raw.has_run(run_id) {
        return not_found();
    }
    let wait_ms = if run_active(ctx.machine.state()) {
        RAW_READ_WAIT_RUN_MS
    } else {
        RAW_READ_WAIT_IDLE_MS
    };
    if !flash_idle(ctx, wait_ms).await {
        return not_found();
    }
    let mut data = [0_u8; CHUNK_LEN];
    match ctx.raw.read_chunk(&mut ctx.flash, run_id, chunk, &mut data) {
        Some(len) if len > 0 => Message::RawChunk(RawChunk {
            run_id,
            chunk,
            len: len as u16,
            data,
        }),
        _ => not_found(),
    }
}

fn sample12_message(frame: &Frame, machine: &Machine, extra_flags: u16) -> Message {
    Message::Sample12(Sample12 {
        monotonic_us: frame.monotonic_us,
        rtc_unix_ms: frame.rtc_unix_ms,
        run_id: machine.run_id(),
        flags: frame.flags | extra_flags,
        state: board_state(machine.state()),
        excitation_raw: frame.excitation_raw,
        v24_raw: frame.v24_raw,
        thermistor_raw: frame.thermistor_raw,
    })
}

async fn read_thermistor(i2c: &mut BoardI2c<'_>, mux: &mut MuxPins<'_>, index: usize) -> Option<i16> {
    let (bank, channel) = THERMISTOR_MUX_MAP[index];
    mux.enable_bank(bank);
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

async fn handle_command(message: Message, ctx: &mut Ctx<'_>) -> CommandResponses {
    let now_ms = Instant::now().as_millis();
    match message {
        Message::Query(command) => CommandResponses::query(
            ctx.ack(sap_flux_protocol::MSG_QUERY, OK_RESULT, command.command_nonce),
            hello_message(ctx.board_id),
            ctx.status(),
            ctx.field.field_mode.then(|| field_status_message(ctx)),
        ),
        Message::SetRtc(command) => {
            let result = if ctx.field.last_set_rtc_nonce == Some(command.command_nonce) {
                // Retry of a SetRtc already applied (its reply was lost):
                // stepping again would apply a stale timestamp.
                OK_RESULT
            } else if matches!(ctx.machine.state(), State::SafeIdle | State::Cooldown)
                && ctx.field.windows.recording().is_none()
            {
                // A sync waits up to one second for a whole-second boundary,
                // so it is never applied during a window or a pulse.
                apply_set_rtc(ctx, command.unix_ms).await
            } else {
                rejection_result(Rejection::WrongState)
            };
            if result == OK_RESULT {
                ctx.field.last_set_rtc_nonce = Some(command.command_nonce);
            }
            CommandResponses::ack_and_status(
                ctx.ack(sap_flux_protocol::MSG_SET_RTC, result, command.command_nonce),
                ctx.status(),
            )
        }
        Message::StartBaseline(command) => {
            let result = if ctx.fault_code != 0 {
                PERSISTENCE_FAILED_RESULT
            } else {
                ctx.machine
                    .start_baseline(
                        BaselineRequest {
                            run_id: command.run_id,
                            baseline_duration_ms: command.baseline_duration_ms,
                            recovery_duration_ms: command.recovery_duration_ms,
                            sample_hz: command.requested_sample_hz,
                            require_rtc: true,
                        },
                        now_ms,
                    )
                    .map(|_| OK_RESULT)
                    .unwrap_or_else(rejection_result)
            };
            CommandResponses::ack_and_status(
                ctx.ack(sap_flux_protocol::MSG_START_BASELINE, result, command.command_nonce),
                ctx.status(),
            )
        }
        Message::ArmPulse(command) => {
            let result = PulseDuration::from_ms(command.pulse_duration_ms)
                .ok_or(Rejection::InvalidParameter)
                .and_then(|duration| ctx.machine.arm(command.run_id, duration, now_ms))
                .map(|_| OK_RESULT)
                .unwrap_or_else(rejection_result);
            CommandResponses::ack_and_status(
                ctx.ack(sap_flux_protocol::MSG_ARM_PULSE, result, command.command_nonce),
                ctx.status(),
            )
        }
        Message::FirePulse(command) => {
            let result = fire_sequence(ctx, command.run_id, command.command_nonce).await;
            CommandResponses::ack_and_status(
                ctx.ack(sap_flux_protocol::MSG_FIRE_PULSE, result, command.command_nonce),
                ctx.status(),
            )
        }
        Message::Abort(command) => {
            let result = if ctx.machine.run_id() != command.run_id {
                rejection_result(Rejection::WrongRun)
            } else {
                ctx.hardware_pulse.force_off();
                ctx.machine.abort(now_ms);
                OK_RESULT
            };
            CommandResponses::ack_and_status(
                ctx.ack(sap_flux_protocol::MSG_ABORT, result, command.command_nonce),
                ctx.status(),
            )
        }
        Message::SetSchedule(command) => {
            let result = set_schedule(ctx, command.schedule, command.lease_until_unix_s).await;
            CommandResponses::ack_and_status(
                ctx.ack(sap_flux_protocol::MSG_SET_SCHEDULE, result, command.command_nonce),
                field_status_message(ctx),
            )
        }
        Message::GetFieldStatus(_) => CommandResponses::one(field_status_message(ctx)),
        Message::GetWindow(command) => {
            let reply = if command.chunk == sap_flux_protocol::CHUNK_HEADER {
                ctx.field
                    .windows
                    .info(command.run_id)
                    .map(Message::WindowInfo)
            } else {
                ctx.field
                    .windows
                    .chunk(command.run_id, command.chunk)
                    .map(Message::WindowChunk)
            };
            CommandResponses::one(reply.unwrap_or_else(|| {
                ack_message(
                    sap_flux_protocol::MSG_GET_WINDOW,
                    sap_flux_protocol::ACK_NOT_FOUND,
                    command.command_nonce,
                    command.run_id,
                )
            }))
        }
        Message::AckWindow(command) => {
            let result = if ctx.field.windows.ack(command.run_id) {
                OK_RESULT
            } else {
                sap_flux_protocol::ACK_NOT_FOUND
            };
            CommandResponses::one(ack_message(
                sap_flux_protocol::MSG_ACK_WINDOW,
                result,
                command.command_nonce,
                command.run_id,
            ))
        }
        Message::GetRaw(command) => CommandResponses::one(
            get_raw(ctx, command.run_id, command.chunk, command.command_nonce).await,
        ),
        // Board-to-host messages and anything this firmware does not
        // implement are ignored, exactly like unknown bytes.
        _ => CommandResponses::none(),
    }
}

/// Validate, persist (only when it changed) and apply a schedule.
///
/// A schedule that cannot be persisted is not applied, except that a
/// disable always takes effect in RAM: the heater must be stoppable even
/// with a broken EEPROM. Both cases still report PERSISTENCE_FAILED.
async fn set_schedule(ctx: &mut Ctx<'_>, schedule: Schedule, lease_until_unix_s: u32) -> u8 {
    if schedule.validate(COOLDOWN_MS as u32).is_err() {
        return INVALID_PARAMETER_RESULT;
    }
    let persisted = ctx.field.schedule_in_eeprom == Some(schedule)
        || eeprom_write_verified(
            &mut ctx.i2c,
            &mut ctx.eeprom_wp,
            records::SCHEDULE_ADDR,
            &records::encode_schedule(&schedule),
        )
        .await;
    if persisted {
        ctx.field.schedule_in_eeprom = Some(schedule);
    } else {
        ctx.field.schedule_in_eeprom = None;
    }
    if persisted || !schedule.is_enabled() {
        ctx.field
            .planner
            .set_schedule(schedule, Instant::now().as_millis());
        ctx.field.planner.set_lease(lease_until_unix_s);
        write_bkp(BKP_LEASE_UNTIL, lease_until_unix_s);
    }
    if persisted {
        OK_RESULT
    } else {
        PERSISTENCE_FAILED_RESULT
    }
}

/// Persist the lockout, then start the hardware one-pulse: the only path to
/// the heater, shared by host FirePulse and the scheduler.
async fn fire_sequence(ctx: &mut Ctx<'_>, run_id: u32, nonce: u64) -> u8 {
    match ctx
        .machine
        .request_fire(run_id, nonce, Instant::now().as_millis())
    {
        Ok(Effect::PersistLockout) => {
            if persist_lockout(&mut ctx.i2c, &mut ctx.eeprom_wp, run_id, nonce).await {
                match ctx
                    .machine
                    .confirm_lockout_persisted(Instant::now().as_millis())
                {
                    Ok(Effect::StartHardwarePulse(duration)) => {
                        ctx.hardware_pulse.start(duration);
                        OK_RESULT
                    }
                    Ok(_) => INVALID_PARAMETER_RESULT,
                    Err(rejection) => rejection_result(rejection),
                }
            } else {
                ctx.hardware_pulse.force_off();
                ctx.machine.persistence_failed();
                ctx.fault_code = FAULT_PERSISTENCE;
                PERSISTENCE_FAILED_RESULT
            }
        }
        Ok(_) => INVALID_PARAMETER_RESULT,
        Err(rejection) => rejection_result(rejection),
    }
}

/// Execute the planner's actions for this frame through the same machine
/// calls the host commands use. Up to three actions per frame so arm and
/// fire land in the same frame.
async fn run_scheduler(ctx: &mut Ctx<'_>) {
    for _ in 0..3 {
        let now_ms = Instant::now().as_millis();
        let action = ctx.field.planner.poll(Inputs {
            now_ms,
            unix_ms: ctx.unix_ms(),
            state: ctx.machine.state(),
            machine_run_id: ctx.machine.run_id(),
            hold: ctx.fault_code != 0,
        });
        match action {
            Action::None => return,
            Action::StartBaseline(request) => {
                if ctx.machine.start_baseline(request, now_ms).is_err() {
                    ctx.field.planner.start_rejected();
                    return;
                }
                if let Some(run) = ctx.field.planner.active_run() {
                    let start_rtc_ms = ctx.unix_ms().unwrap_or(-1);
                    ctx.field.windows.begin(WindowParams {
                        run_id: run.run_id,
                        start_us: Instant::now().as_micros(),
                        start_rtc_ms,
                        bin_ms: run.schedule.bin_ms,
                        sample_hz: run.schedule.sample_hz,
                        pulse_duration_ms: run.schedule.pulse_duration_ms,
                        time_invalid: run.time_invalid,
                    });
                }
            }
            Action::Arm { run_id, duration } => {
                if ctx.machine.arm(run_id, duration, now_ms).is_err() {
                    return;
                }
            }
            Action::Fire { run_id, nonce } => {
                if fire_sequence(ctx, run_id, nonce).await != OK_RESULT {
                    return;
                }
                ctx.field.windows.note_heater_on(Instant::now().as_micros());
            }
            Action::Abort { .. } => {
                ctx.hardware_pulse.force_off();
                ctx.machine.abort(now_ms);
            }
        }
    }
}

/// A window ends when its run leaves Baseline/Armed/Pulsing/Recovery:
/// recovery done (normal), aborted before the pulse, or a fault.
fn close_window_if_done(ctx: &mut Ctx<'_>) {
    let Some(run_id) = ctx.field.windows.recording() else {
        return;
    };
    if ctx.fault_code != 0 || ctx.machine.state() == State::Fault {
        ctx.field.windows.add_flags(WINDOW_FLAG_HEATER_FAULT);
    }
    let running = ctx.machine.run_id() == run_id
        && matches!(
            ctx.machine.state(),
            State::Baseline | State::Armed | State::Pulsing | State::Recovery
        );
    if !running {
        let end_rtc_ms = ctx.unix_ms().unwrap_or(-1);
        ctx.field
            .windows
            .finish(Instant::now().as_micros(), end_rtc_ms);
    }
}

/// 25 Hz normally; a recording window uses its schedule's sample rate.
fn frame_period(ctx: &Ctx<'_>) -> Duration {
    match ctx.field.planner.active_run() {
        Some(run) if ctx.field.windows.recording() == Some(run.run_id) => {
            Duration::from_millis(1000 / run.schedule.sample_hz.max(1) as u64)
        }
        _ => FRAME_PERIOD,
    }
}

async fn service_pulse_state(ctx: &mut Ctx<'_>) {
    let now_ms = Instant::now().as_millis();
    if ctx.machine.state() == State::Pulsing && !ctx.hardware_pulse.is_running() {
        ctx.hardware_pulse.force_off();
        if ctx.machine.pulse_finished(now_ms).is_err() {
            ctx.fault_code = FAULT_PULSE_OVERRUN;
        }
    }

    match ctx.machine.tick(now_ms) {
        Effect::ForceHeaterOff => {
            ctx.hardware_pulse.force_off();
            if ctx.machine.state() == State::Fault {
                ctx.fault_code = FAULT_PULSE_OVERRUN;
            }
        }
        Effect::ClearLockout => {
            ctx.hardware_pulse.force_off();
            if !clear_lockout(&mut ctx.i2c, &mut ctx.eeprom_wp).await {
                ctx.fault_code = FAULT_PERSISTENCE;
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

fn eeprom_read<const N: usize>(i2c: &mut BoardI2c<'_>, addr: u16) -> Option<[u8; N]> {
    let mut out = [0_u8; N];
    i2c.blocking_write_read(EEPROM_ADDR, &addr.to_be_bytes(), &mut out)
        .ok()?;
    Some(out)
}

/// Write `data` within one EEPROM page and read it back. Never used for the
/// lockout record, which keeps its own dedicated path.
async fn eeprom_write_verified(
    i2c: &mut BoardI2c<'_>,
    wp: &mut Output<'_>,
    addr: u16,
    data: &[u8],
) -> bool {
    let page = EEPROM_PAGE_LEN as u16;
    if data.is_empty()
        || data.len() > EEPROM_PAGE_LEN
        || addr / page != (addr + data.len() as u16 - 1) / page
        || addr + data.len() as u16 > LOCKOUT_ADDR
    {
        return false;
    }
    let mut write = [0_u8; 2 + EEPROM_PAGE_LEN];
    write[..2].copy_from_slice(&addr.to_be_bytes());
    write[2..2 + data.len()].copy_from_slice(data);

    wp.set_low();
    Timer::after_millis(1).await;
    let wrote = i2c.blocking_write(EEPROM_ADDR, &write[..2 + data.len()]).is_ok()
        && eeprom_wait_ready(i2c).await;
    wp.set_high();
    if !wrote {
        return false;
    }
    let mut verify = [0_u8; EEPROM_PAGE_LEN];
    i2c.blocking_write_read(EEPROM_ADDR, &addr.to_be_bytes(), &mut verify[..data.len()])
        .is_ok()
        && verify[..data.len()] == *data
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

/// Step the RTC, then learn RTC_CALR from the correction.
async fn apply_set_rtc(ctx: &mut Ctx<'_>, unix_ms: i64) -> u8 {
    let (target_ms, old_ms) =
        match set_rtc(&mut ctx.rtc, &ctx.rtc_provider, ctx.rtc_valid, unix_ms).await {
            Ok(set) => set,
            Err(result) => return result,
        };
    write_rtc_magic();
    ctx.rtc_valid = read_rtc_magic();
    if !ctx.rtc_valid {
        return BACKUP_WRITE_FAILED_RESULT;
    }

    let step_ms = old_ms.map(|old| target_ms - old);
    ctx.field.last_rtc_step_ms = step_ms
        .unwrap_or(0)
        .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    let mut learner = TrimLearner {
        anchor_unix_s: read_bkp(BKP_TRIM_ANCHOR_UNIX_S),
        steps_ms: read_bkp(BKP_TRIM_STEPS_MS) as i32,
    };
    let new_unix_s = (target_ms / 1_000).clamp(0, u32::MAX as i64) as u32;
    let learned = learner.on_correction(
        step_ms.is_some(),
        new_unix_s,
        step_ms.unwrap_or(0),
        ctx.field.trim_ppb,
    );
    write_bkp(BKP_TRIM_ANCHOR_UNIX_S, learner.anchor_unix_s);
    write_bkp(BKP_TRIM_STEPS_MS, learner.steps_ms as u32);
    if let Some(ppb) = learned {
        let calr = Calr::from_ppb(ppb);
        if write_calr(calr) {
            ctx.field.trim_ppb = calr.ppb();
            // Best effort: the clock is set either way; a failed write only
            // means the trim is re-learned after the next backup-domain loss.
            let _ = eeprom_write_verified(
                &mut ctx.i2c,
                &mut ctx.eeprom_wp,
                records::TRIM_ADDR,
                &records::encode_trim(ctx.field.trim_ppb),
            )
            .await;
        }
    }
    OK_RESULT
}

/// Set the RTC at the next whole second of `unix_ms`. Returns the time set
/// and the RTC's own reading just before the step (None if it was invalid).
async fn set_rtc(
    rtc: &mut Rtc,
    provider: &RtcTimeProvider,
    rtc_valid: bool,
    unix_ms: i64,
) -> Result<(i64, Option<i64>), u8> {
    let fraction_ms = unix_ms.rem_euclid(1_000) as u64;
    let wait_ms = 1_000 - fraction_ms;
    let Some(target_ms) = unix_ms.checked_add(wait_ms as i64) else {
        return Err(INVALID_PARAMETER_RESULT);
    };
    let Some(value) = unix_ms_to_datetime(target_ms) else {
        return Err(INVALID_PARAMETER_RESULT);
    };
    let Some(day_of_week) = day_of_week(value.weekday) else {
        return Err(INVALID_PARAMETER_RESULT);
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
        return Err(INVALID_PARAMETER_RESULT);
    };
    Timer::after_millis(wait_ms).await;
    let old_ms = rtc_now_ms(provider, rtc_valid);
    if rtc.set_datetime(value).is_err() {
        return Err(INVALID_PARAMETER_RESULT);
    }
    Ok((target_ms, old_ms))
}

/// Program RTC_CALR (32 s cycle). False if a previous recalibration is
/// still pending, in which case the register is left unchanged.
fn write_calr(calr: Calr) -> bool {
    use pac::rtc::vals::{Calp, Key, Recalpf};
    let rtc = pac::RTC;
    rtc.wpr().write(|w| w.set_key(Key::DEACTIVATE1));
    rtc.wpr().write(|w| w.set_key(Key::DEACTIVATE2));
    // RECALPF clears within a few RTCCLK cycles after the previous write.
    let mut ready = false;
    for _ in 0..200_000 {
        if rtc.icsr().read().recalpf() != Recalpf::PENDING {
            ready = true;
            break;
        }
    }
    if ready {
        rtc.calr().write(|w| {
            w.set_calp(if calr.calp { Calp::INCREASE_FREQ } else { Calp::NO_CHANGE });
            w.set_calm(calr.calm);
        });
    }
    rtc.wpr().write(|w| w.set_key(Key::ACTIVATE));
    ready
}

fn read_calr() -> Calr {
    let calr = pac::RTC.calr().read();
    Calr {
        calp: calr.calp() == pac::rtc::vals::Calp::INCREASE_FREQ,
        calm: calr.calm(),
    }
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

fn read_bkp(index: usize) -> u32 {
    pac::TAMP.bkpr(index).read().bkp()
}

fn write_bkp(index: usize, value: u32) {
    pac::TAMP.bkpr(index).write(|w| w.set_bkp(value));
}

fn field_status_message(ctx: &Ctx<'_>) -> Message {
    let now_ms = Instant::now().as_millis();
    let unix_ms = ctx.unix_ms();
    let planner = &ctx.field.planner;
    let schedule = planner.schedule();
    let mut flags = 0;
    if schedule.is_enabled() {
        flags |= FIELD_STATUS_ENABLED;
    }
    if schedule.requires_lease() {
        flags |= FIELD_STATUS_REQUIRE_LEASE;
    }
    if planner.lease_valid(unix_ms) {
        flags |= FIELD_STATUS_LEASE_VALID;
    }
    if unix_ms.is_some() {
        flags |= FIELD_STATUS_RTC_VALID;
    }
    if ctx.flash.is_ok() {
        flags |= FIELD_STATUS_FLASH_OK;
    }
    // Heater inhibited: persisted lockout/cooldown, a pulse in progress, or
    // a latched fault.
    if matches!(
        ctx.machine.state(),
        State::Pulsing | State::Recovery | State::Cooldown | State::Fault
    ) || ctx.fault_code != 0
    {
        flags |= FIELD_STATUS_HEATER_LOCKOUT;
    }
    Message::FieldStatus(FieldStatus {
        monotonic_us: Instant::now().as_micros(),
        rtc_unix_ms: unix_ms.unwrap_or(-1),
        next_pulse_unix_s: planner.next_pulse_unix_s(now_ms, unix_ms),
        schedule_flags: flags,
        windows_pending: ctx.field.windows.windows_pending() as u8,
        windows_dropped: ctx.field.windows.windows_dropped(),
        oldest_pending_run_id: ctx.field.windows.oldest_pending_run_id(),
        last_run_id: ctx.field.windows.last_run_id(),
        calr_ppb: ctx.field.trim_ppb,
        last_rtc_step_ms: ctx.field.last_rtc_step_ms,
        boot_count: ctx.field.boot_count as u16,
        v24_raw: ctx.field.v24_raw,
        flash_used_kib: ctx.raw.used_kib(),
        schedule_crc: schedule.crc16(),
    })
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
