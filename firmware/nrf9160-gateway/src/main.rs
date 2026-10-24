//! sapflux field gateway for the CircuitDojo nRF9160 Feather (micro-USB
//! generation, v1–v3) under TF-M/MCUboot from NCS v2.1.2.
//!
//! Bridges the sap-flux board (J6 UART) to the server over LTE-M/UDP per
//! docs/field-protocol.md. See README.md.

#![no_std]
#![no_main]

mod board;
mod clock;
mod config;
mod cycle;
mod entropy;
mod events;
#[cfg(feature = "latch-sleep")]
mod latch;
mod log;
mod modem;
mod power;
mod uplink;
mod wdog;

use core::mem::MaybeUninit;

use embassy_executor::Spawner;
use embassy_nrf::buffered_uarte::{self, BufferedUarte};
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::interrupt::typelevel;
use embassy_nrf::timer::Timer as HwTimer;
use embassy_nrf::uarte::{self, Baudrate};
use embassy_nrf::{bind_interrupts, pac, peripherals, saadc, twim};
use embassy_time::Timer;

use crate::cycle::{Gateway, WINDOW_BUF_LEN};

extern crate tinyrlibc;

struct ModemInterruptHandler;

impl typelevel::Handler<typelevel::IPC> for ModemInterruptHandler {
    unsafe fn on_interrupt() {
        nrf_modem::ipc_irq_handler();
    }
}

bind_interrupts!(pub struct Irqs {
    SERIAL0 => uarte::InterruptHandler<peripherals::SERIAL0>;
    SERIAL1 => twim::InterruptHandler<peripherals::SERIAL1>;
    SERIAL2 => buffered_uarte::InterruptHandler<peripherals::SERIAL2>;
    SAADC => saadc::InterruptHandler;
    IPC => ModemInterruptHandler;
});

/// Window staging buffer (WindowInfo frame + data), outside the task.
#[unsafe(link_section = ".uninit.WINDOW")]
static mut WINDOW_BUF: MaybeUninit<[u8; WINDOW_BUF_LEN]> = MaybeUninit::uninit();

fn reset_reason() -> (u32, &'static str) {
    let r = pac::POWER.resetreas().read();
    // RESETREAS is write-one-to-clear.
    pac::POWER.resetreas().write_value(r);
    let name = if r.dog() {
        "watchdog"
    } else if r.resetpin() {
        "pin"
    } else if r.sreq() {
        "soft"
    } else if r.lockup() {
        "lockup"
    } else if r.off() {
        "system-off wake"
    } else if r.0 == 0 {
        "power-on"
    } else {
        "other"
    };
    (r.0, name)
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // NRF_PS_EN: on battery the TPS63031 stays on only while P0.31 is high.
    // Latch it before anything else, straight through the PAC.
    pac::P0.outset().write(|w| w.set_pin(31, true));
    pac::P0.dirset().write(|w| w.set_pin(31, true));

    let p = embassy_nrf::init(Default::default());
    #[allow(unused_mut)]
    let mut ps_en = Output::new(p.P0_31, Level::High, OutputDrive::Standard);

    log::enable();
    let (reason_bits, reason) = reset_reason();
    info!(
        "sapflux gateway v{} (reset: {} 0x{:x})",
        env!("CARGO_PKG_VERSION"),
        reason,
        reason_bits
    );
    if config::IS_EXAMPLE {
        info!("WARNING: built with gateway.example.toml (placeholder server/key)");
    }

    let (wdt, fresh) = wdog::start(p.WDT);
    if !fresh {
        info!("watchdog was already running; adopting it");
    }
    spawner.spawn(wdog::pet_task(wdt).unwrap());

    let mut panic_text = log::Line::new();
    let had_panic = wdog::take_panic_record(&mut panic_text);
    if had_panic {
        info!("previous boot panicked: {}", panic_text.as_str());
    }

    let mut pool = entropy::Pool::new(HwTimer::new(p.TIMER2));

    // Quiet the board peripherals we don't use.
    let mut serial1 = p.SERIAL1;
    let mut p26 = p.P0_26;
    let mut p27 = p.P0_27;
    power::accel_off(serial1.reborrow(), p26.reborrow(), p27.reborrow()).await;
    let _flash = power::flash_deep_power_down(p.P0_07, p.P0_11, p.P0_09, p.P0_08, p.P0_10);

    // Modem.
    let mut modem_ok = modem::init().is_ok();
    if !modem_ok {
        info!("modem: library init failed");
    }
    let mut imei = 0u64;
    if modem_ok {
        if let Err(e) = modem::configure().await {
            info!("modem: configure failed: {:?}", e);
            modem_ok = false;
        }
        if let Ok(r) = modem::at::<64>("AT+CGSN").await {
            pool.mix(r.as_bytes());
            imei = modem::parse_imei(&r).unwrap_or(0);
        }
        for cmd in ["AT+CGMR", "AT%XTEMP?", "AT+CFUN?", "AT%XMODEMUUID"] {
            if let Ok(r) = modem::at::<128>(cmd).await {
                pool.mix(r.as_bytes());
            }
        }
    }
    let device_id = if config::DEVICE_ID_OVERRIDE != 0 {
        config::DEVICE_ID_OVERRIDE
    } else {
        imei
    };
    let boot_id = pool.finish();
    info!(
        "device_id {} (imei {}), boot_id {:08x}, boot #{}",
        device_id,
        imei,
        boot_id,
        entropy::boot_counter()
    );

    let mut events = events::Events::new();
    {
        let mut text = log::Line::new();
        let _ = core::fmt::write(
            &mut text,
            format_args!(
                "fw {} reset {} boot_id {:08x}{}{}",
                env!("CARGO_PKG_VERSION"),
                reason,
                boot_id,
                if had_panic { " panic: " } else { "" },
                panic_text.as_str()
            ),
        );
        event!(events, events::EVENT_BOOT, "{}", text.as_str());
    }
    if !modem_ok {
        event!(events, events::EVENT_MODEM_ERROR, "modem init/configure failed");
    }

    // SAFETY: the only reference to WINDOW_BUF, taken once here.
    let window_buf: &'static mut [u8; WINDOW_BUF_LEN] =
        unsafe { (*(&raw mut WINDOW_BUF)).assume_init_mut() };

    let mut gw = Gateway::new(
        modem::Modem::new(),
        uplink::Uplink::new(device_id, boot_id),
        clock::Clock::new(),
        events,
        window_buf,
    );

    // Peripherals reused every cycle.
    let mut serial2 = p.SERIAL2;
    let mut timer1 = p.TIMER1;
    let mut ppi0 = p.PPI_CH0;
    let mut ppi1 = p.PPI_CH1;
    let mut ppi_group = p.PPI_GROUP0;
    let mut rx_pin = p.P0_23;
    let mut tx_pin = p.P0_24;
    let mut saadc_p = p.SAADC;
    let mut ain7 = p.P0_20;
    let mut batt_en = p.P0_25;
    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 256];
    let mut nonce = (boot_id as u64) << 32;

    loop {
        wdog::alive();
        log::enable();
        gw.batt_mv = power::battery_mv(saadc_p.reborrow(), ain7.reborrow(), batt_en.reborrow()).await;
        info!("cycle start: battery {} mV", gw.batt_mv);

        let sleep_s = {
            let mut config = uarte::Config::default();
            config.baudrate = Baudrate::Baud115200;
            let uart = BufferedUarte::new(
                serial2.reborrow(),
                timer1.reborrow(),
                ppi0.reborrow(),
                ppi1.reborrow(),
                ppi_group.reborrow(),
                rx_pin.reborrow(),
                tx_pin.reborrow(),
                Irqs,
                config,
                &mut rx_buf,
                &mut tx_buf,
            );
            let mut link = board::BoardLink::new(uart, nonce);
            let s = gw.run(&mut link).await;
            nonce = link.next_nonce();
            info!(
                "board link: {} commands, {} retries, {} failures, {} decode errors",
                link.stats.commands,
                link.stats.retries,
                link.stats.failures,
                link.stats.decode_errors
            );
            s
            // The UARTE is disabled and its pins disconnected here.
        };

        // Between cycles hold TX at the UART idle level so the board's RX
        // never floats (and never sees a spurious wake byte).
        let _tx_idle = Output::new(tx_pin.reborrow(), Level::High, OutputDrive::Standard);

        #[cfg(feature = "latch-sleep")]
        {
            latch::power_off_for(serial1.reborrow(), p26.reborrow(), p27.reborrow(), &mut ps_en, sleep_s)
                .await;
            // Still powered (USB/VBUS or the latch did not drop): fall back
            // to a timer sleep.
            info!("latch-sleep: still powered, sleeping on the timer");
            sleep_timer(sleep_s).await;
            continue;
        }

        #[cfg(not(feature = "latch-sleep"))]
        {
            let _ = &mut ps_en;
            sleep_timer(sleep_s).await;
        }
    }
}

async fn sleep_timer(sleep_s: u32) {
    log::disable();
    wdog::alive_for(sleep_s + 120);
    Timer::after_secs(sleep_s as u64).await;
}
