#![no_std]
#![no_main]

use arrayvec::ArrayString;
use cortex_m::peripheral::NVIC;
use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::interrupt::typelevel;
use embassy_nrf::pac;
use embassy_nrf::uarte::{self, Baudrate, Uarte};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_time::Timer;
use nrf_modem::MemoryLayout;
use panic_halt as _;

extern crate tinyrlibc;

struct ModemInterruptHandler;

impl typelevel::Handler<typelevel::IPC> for ModemInterruptHandler {
    unsafe fn on_interrupt() {
        nrf_modem::ipc_irq_handler();
    }
}

bind_interrupts!(struct Irqs {
    SERIAL0 => uarte::InterruptHandler<peripherals::SERIAL0>;
    IPC => ModemInterruptHandler;
});

fn write(uart: &mut Uarte<'_>, bytes: &[u8]) {
    let _ = uart.blocking_write(bytes);
}

fn line(uart: &mut Uarte<'_>, bytes: &[u8]) {
    write(uart, bytes);
    write(uart, b"\r\n");
}

fn report_reset_reason(uart: &mut Uarte<'_>) {
    let reason = pac::POWER.resetreas().read();
    write(uart, b"reset reason:");
    if reason.resetpin() {
        write(uart, b" RESET_PIN");
    }
    if reason.dog() {
        write(uart, b" WATCHDOG");
    }
    if reason.off() {
        write(uart, b" SYSTEM_OFF_WAKE");
    }
    if reason.dif() {
        write(uart, b" DEBUG_WAKE");
    }
    if reason.sreq() {
        write(uart, b" SOFTWARE_REQUEST");
    }
    if reason.lockup() {
        write(uart, b" CPU_LOCKUP");
    }
    if reason.ctrlap() {
        write(uart, b" CTRL_AP");
    }
    if reason.0 == 0 {
        write(uart, b" POWER_ON_OR_BROWNOUT");
    }
    write(uart, b"\r\n");
    // RESETREAS is write-one-to-clear.
    pac::POWER.resetreas().write_value(reason);
}

async fn report_at<const N: usize>(
    uart: &mut Uarte<'_>,
    label: &[u8],
    command: &str,
) -> Option<ArrayString<N>> {
    write(uart, label);
    write(uart, b": ");
    match nrf_modem::send_at::<N>(command).await {
        Ok(response) => {
            write(uart, response.as_bytes());
            if !response.ends_with("\r\n") {
                write(uart, b"\r\n");
            }
            Some(response)
        }
        Err(_) => {
            line(uart, b"ERROR");
            None
        }
    }
}

fn report_redacted_digits(uart: &mut Uarte<'_>, digits: &[u8]) {
    write(uart, b"SIM ICCID: ");
    if digits.len() >= 10 {
        write(uart, &digits[..6]);
        write(uart, b"...");
        write(uart, &digits[digits.len() - 4..]);
        write(uart, b" (");
        let tens = digits.len() / 10;
        if tens != 0 {
            write(uart, &[b'0' + tens as u8]);
        }
        write(uart, &[b'0' + (digits.len() % 10) as u8]);
        line(uart, b" digits)");
    } else {
        line(uart, b"unreadable");
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn report_crsm_iccid(uart: &mut Uarte<'_>, response: &str) {
    let mut digits = [0_u8; 22];
    let mut count = 0_usize;

    if let Some(quoted) = response.split('"').nth(1) {
        for pair in quoted.as_bytes().chunks_exact(2) {
            let Some(high) = hex_nibble(pair[0]) else {
                break;
            };
            let Some(low) = hex_nibble(pair[1]) else {
                break;
            };
            for nibble in [low, high] {
                if nibble <= 9 && count < digits.len() {
                    digits[count] = b'0' + nibble;
                    count += 1;
                }
            }
        }
    }
    report_redacted_digits(uart, &digits[..count]);
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());

    // D7 is active-low on this early nRF9160 Feather revision.
    let mut blue = Output::new(p.P0_03, Level::High, OutputDrive::Standard);

    // The onboard CP2102N is wired to UARTE0 RX=P0.05 and TX=P0.06.
    let mut uart_config = uarte::Config::default();
    uart_config.baudrate = Baudrate::Baud1m;
    let mut uart = Uarte::new(p.SERIAL0, p.P0_05, p.P0_06, Irqs, uart_config);

    blue.set_low();
    line(
        &mut uart,
        b"sapflux nRF9160 Hologram long-run diagnostic v0.3.3",
    );
    report_reset_reason(&mut uart);
    line(&mut uart, b"stage: TF-M non-secure application active");
    let mut cp = cortex_m::Peripherals::take().expect("Cortex-M peripherals unavailable");
    unsafe {
        NVIC::unmask(pac::Interrupt::IPC);
        cp.NVIC.set_priority(pac::Interrupt::IPC, 0);
    }

    line(&mut uart, b"stage: initialize Nordic modem library core");
    if nrf_modem::init_with_custom_layout_core(MemoryLayout {
        base_address: 0x2000_8000,
        tx_area_size: 0x2000,
        rx_area_size: 0x2000,
        trace_area_size: 0,
    })
    .is_err()
    {
        line(
            &mut uart,
            b"FATAL: modem library core initialization failed",
        );
        blue.set_high();
        loop {
            Timer::after_secs(10).await;
        }
    }
    line(&mut uart, b"stage: Nordic modem library core ready");

    let _ = report_at::<128>(&mut uart, b"early modem firmware", "AT+CGMR").await;
    let _ = report_at::<64>(&mut uart, b"initial radio state", "AT+CFUN?").await;
    let _ = report_at::<64>(&mut uart, b"LTE-M system mode", "AT%XSYSTEMMODE=1,0,0,1").await;

    line(&mut uart, b"stage: modem identity and radio configuration");
    let _ = report_at::<128>(&mut uart, b"modem firmware", "AT+CGMR").await;
    let _ = report_at::<32>(&mut uart, b"verbose errors", "AT+CMEE=2").await;
    let _ = report_at::<64>(&mut uart, b"radio offline", "AT+CFUN=0").await;
    let _ = report_at::<64>(
        &mut uart,
        b"APN configuration",
        "AT+CGDCONT=0,\"IP\",\"hologram\"",
    )
    .await;
    let _ = report_at::<64>(&mut uart, b"registration notifications", "AT+CEREG=5").await;
    let _ = report_at::<64>(&mut uart, b"network error notifications", "AT+CNEC=24").await;
    let _ = report_at::<64>(&mut uart, b"radio online", "AT+CFUN=1").await;

    line(
        &mut uart,
        b"stage: waiting indefinitely for UICC and LTE-M registration",
    );
    line(
        &mut uart,
        b"data policy: no DNS, sockets, HTTP, or application payloads",
    );

    let mut uicc_ready = false;
    let mut sim_identity_reported = false;
    let mut operator_selection_started = false;
    let mut registration_reported = false;
    let mut denial_reported = false;

    loop {
        if let Some(xsim) = report_at::<96>(&mut uart, b"UICC state", "AT%XSIM?").await {
            uicc_ready = xsim.contains("%XSIM: 1");
        }

        if uicc_ready && !sim_identity_reported {
            line(&mut uart, b"stage: UICC initialization complete");
            let _ = report_at::<96>(&mut uart, b"SIM PIN state", "AT+CPIN?").await;
            if let Ok(iccid) = nrf_modem::send_at::<128>("AT+CRSM=176,12258,0,0,10").await {
                report_crsm_iccid(&mut uart, iccid.as_str());
            } else {
                line(&mut uart, b"SIM ICCID: ERROR");
            }
            sim_identity_reported = true;
        }

        if uicc_ready && !operator_selection_started {
            let _ = report_at::<64>(&mut uart, b"automatic operator", "AT+COPS=0").await;
            operator_selection_started = true;
        }

        let mut registered = false;
        let mut denied = false;
        if let Some(cereg) = report_at::<192>(&mut uart, b"CEREG", "AT+CEREG?").await {
            registered = cereg.contains("+CEREG: 5,1") || cereg.contains("+CEREG: 5,5");
            denied = cereg.contains("+CEREG: 5,3");
        }
        let _ = report_at::<96>(&mut uart, b"CESQ", "AT+CESQ").await;

        if registered && !registration_reported {
            line(
                &mut uart,
                b"RESULT: LTE-M REGISTERED; ZERO APPLICATION DATA SENT",
            );
            let _ = report_at::<128>(&mut uart, b"operator", "AT+COPS?").await;
            let _ = report_at::<64>(&mut uart, b"packet attach", "AT+CGATT?").await;
            let _ = report_at::<256>(&mut uart, b"PDP context", "AT+CGDCONT?").await;
            let _ = report_at::<256>(&mut uart, b"modem monitor", "AT%XMONITOR").await;
            registration_reported = true;
            denial_reported = false;
            blue.set_high();
        } else if denied && !denial_reported {
            line(&mut uart, b"stage: network explicitly denied registration");
            let _ = report_at::<192>(&mut uart, b"extended error", "AT+CEER").await;
            denial_reported = true;
        } else if !denied {
            denial_reported = false;
        }

        if registration_reported {
            Timer::after_secs(60).await;
        } else if uicc_ready {
            Timer::after_secs(30).await;
        } else {
            Timer::after_secs(5).await;
        }
    }
}
