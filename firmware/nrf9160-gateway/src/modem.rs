//! nRF9160 modem: init, LTE-M/PSM configuration, attach, and AT parsing.
//!
//! Uses `nrf_modem::init_with_custom_layout_core` (the path the diagnostic
//! proved on this board under TF-M) and configures the modem with plain AT
//! commands. AT notifications are not initialized, so attach is detected by
//! polling `AT+CEREG?`.
//!
//! Power: LTE-M only, PSM requested (`AT+CPSMS`, timers below), eDRX off.
//! The modem stays attached between cycles and sleeps in PSM; a persistent
//! `LteLink` is held because dropping the last one sends `AT+CFUN=0`.

use arrayvec::ArrayString;
use cortex_m::peripheral::NVIC;
use embassy_nrf::pac;
use embassy_time::{Duration, Instant, Timer};
use nrf_modem::{Error, LteLink, MemoryLayout};

use crate::config::APN;
use crate::info;

/// Requested periodic TAU (T3412 extended): `001` = 1 h units, `00010` =
/// 2 → 2 h. Longer than the hourly check-in so the modem is never woken
/// just for a TAU between check-ins (the network may grant something else).
pub const PSM_T3412: &str = "00100010";
/// Requested active time (T3324): `000` = 2 s units, `01010` = 10 → 20 s.
/// Long enough to receive the server's reply to the last datagram.
pub const PSM_T3324: &str = "00001010";

/// Attach attempts that time out in a row before the radio is cycled
/// (CFUN=4 → CFUN=21).
const ATTACH_FAILS_BEFORE_RADIO_CYCLE: u32 = 2;

pub fn init() -> Result<(), Error> {
    // SAFETY: the IPC handler is bound in `main`; priority 0 as in the
    // diagnostic and the nrf-modem examples.
    unsafe {
        let mut cp = cortex_m::Peripherals::steal();
        NVIC::unmask(pac::Interrupt::IPC);
        cp.NVIC.set_priority(pac::Interrupt::IPC, 0);
    }
    nrf_modem::init_with_custom_layout_core(MemoryLayout {
        base_address: 0x2000_8000,
        tx_area_size: 0x2000,
        rx_area_size: 0x2000,
        trace_area_size: 0,
    })
}

/// Send an AT command; returns the response (truncated to `N`).
pub async fn at<const N: usize>(cmd: &str) -> Result<ArrayString<N>, Error> {
    let r = nrf_modem::send_at::<N>(cmd).await;
    if r.is_err() {
        info!("modem: {} -> {:?}", cmd, r.as_ref().err());
    }
    r
}

/// One-time configuration while the radio is off (CFUN=0 after init).
pub async fn configure() -> Result<(), Error> {
    at::<32>("AT+CFUN=0").await?;
    at::<32>("AT+CMEE=1").await?;
    // LTE-M only, no NB-IoT, no GNSS, no preference.
    at::<32>("AT%XSYSTEMMODE=1,0,0,0").await?;
    let mut cmd = ArrayString::<64>::new();
    let _ = core::fmt::write(
        &mut cmd,
        format_args!("AT+CPSMS=1,\"\",\"\",\"{}\",\"{}\"", PSM_T3412, PSM_T3324),
    );
    at::<32>(&cmd).await?;
    at::<32>("AT+CEDRXS=0").await?;
    let mut cmd = ArrayString::<64>::new();
    let _ = core::fmt::write(&mut cmd, format_args!("AT+CGDCONT=0,\"IP\",\"{}\"", APN));
    at::<32>(&cmd).await?;
    // Registration status with PSM timers in the reply; network time zone
    // updates for AT+CCLK.
    at::<32>("AT+CEREG=5").await?;
    at::<32>("AT+CTZU=1").await?;
    Ok(())
}

pub struct Modem {
    link: Option<LteLink>,
    attach_failures: u32,
    pub attached: bool,
    pub last_cereg: ArrayString<96>,
}

impl Modem {
    pub const fn new() -> Self {
        Self {
            link: None,
            attach_failures: 0,
            attached: false,
            last_cereg: ArrayString::new_const(),
        }
    }

    /// Registration status from `AT+CEREG?` (1 home, 5 roaming = attached).
    pub async fn registration(&mut self) -> Option<u8> {
        let r = at::<96>("AT+CEREG?").await.ok()?;
        self.last_cereg = r;
        cereg_stat(&r)
    }

    /// Make sure the radio is on and registered, waiting up to `timeout`.
    pub async fn ensure_attached(&mut self, timeout: Duration) -> Result<(), Error> {
        if self.link.is_none() {
            // AT%XDATAPRFL=0, AT+CEPPI=1, AT+CFUN=21 inside nrf-modem.
            self.link = Some(LteLink::new().await?);
            info!("modem: radio on (CFUN=21)");
        } else if self.attach_failures >= ATTACH_FAILS_BEFORE_RADIO_CYCLE {
            info!("modem: {} attach failures, cycling radio", self.attach_failures);
            // Flight mode keeps the configuration; nrf-modem's link count
            // is unaffected because the LteLink is kept.
            let _ = at::<32>("AT+CFUN=4").await;
            Timer::after_secs(2).await;
            at::<32>("AT+CFUN=21").await?;
            self.attach_failures = 0;
        }
        let start = Instant::now();
        loop {
            match self.registration().await {
                Some(1) | Some(5) => {
                    if !self.attached {
                        info!(
                            "modem: attached after {} s: {}",
                            start.elapsed().as_secs(),
                            self.last_cereg.trim()
                        );
                    }
                    self.attached = true;
                    self.attach_failures = 0;
                    return Ok(());
                }
                Some(3) => {
                    self.attached = false;
                    self.attach_failures += 1;
                    info!("modem: registration denied: {}", self.last_cereg.trim());
                    return Err(Error::LteRegistrationDenied);
                }
                _ => self.attached = false,
            }
            if start.elapsed() >= timeout {
                self.attach_failures += 1;
                info!("modem: attach timeout: {}", self.last_cereg.trim());
                return Err(Error::NrfError(-116)); // ETIMEDOUT
            }
            Timer::after_secs(2).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Parsers.

/// The integers in `s` after `prefix`, separated by commas (quoted fields
/// count as non-integers and are returned as `None`).
fn fields<'a>(s: &'a str, prefix: &str) -> Option<impl Iterator<Item = Option<i64>> + 'a> {
    let start = s.find(prefix)? + prefix.len();
    let line = s[start..].split(['\r', '\n']).next()?;
    Some(line.split(',').map(|f| f.trim().parse::<i64>().ok()))
}

/// `+CEREG: <n>,<stat>[,...]` → stat.
pub fn cereg_stat(r: &str) -> Option<u8> {
    fields(r, "+CEREG:")?.nth(1).flatten().map(|v| v as u8)
}

/// IMEI digits from `AT+CGSN` → u64.
pub fn parse_imei(r: &str) -> Option<u64> {
    let digits = r.split(['\r', '\n']).map(str::trim).find(|l| {
        l.len() >= 14 && l.bytes().all(|b| b.is_ascii_digit())
    })?;
    digits.parse().ok()
}

/// RSRP in dBm from `AT+CESQ` (`+CESQ: rxlev,ber,rscp,ecno,rsrq,rsrp`);
/// index 0..=97 maps to roughly −141..−44 dBm, 255 = unknown → 0.
pub fn parse_rsrp(r: &str) -> i16 {
    match fields(r, "+CESQ:").and_then(|mut f| f.nth(5).flatten()) {
        Some(v) if (0..=97).contains(&v) => (v - 141) as i16,
        _ => 0,
    }
}

/// Unix ms from `+CCLK: "yy/MM/dd,hh:mm:ss±zz"` (local time with the zone
/// in quarter hours; UTC = local − zone). `None` before network time.
pub fn parse_cclk(r: &str) -> Option<i64> {
    let start = r.find("+CCLK:")?;
    let q1 = r[start..].find('"')? + start + 1;
    let q2 = r[q1..].find('"')? + q1;
    let s = r[q1..q2].as_bytes();
    // yy/MM/dd,hh:mm:ss±zz
    if s.len() < 20 {
        return None;
    }
    let n = |i: usize| -> Option<i64> {
        let a = s.get(i)?.checked_sub(b'0')?;
        let b = s.get(i + 1)?.checked_sub(b'0')?;
        if a > 9 || b > 9 {
            return None;
        }
        Some((a * 10 + b) as i64)
    };
    let (yy, mo, dd, hh, mi, ss) = (n(0)?, n(3)?, n(6)?, n(9)?, n(12)?, n(15)?);
    let sign = match s[17] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let zone_q = n(18)?;
    // The modem reports a 1980s/2000 default before NITZ; reject anything
    // earlier than 2024.
    if yy < 24 || !(1..=12).contains(&mo) || !(1..=31).contains(&dd) || hh > 23 || mi > 59 || ss > 60
    {
        return None;
    }
    let days = days_from_civil(2000 + yy, mo, dd);
    let local_s = days * 86_400 + hh * 3600 + mi * 60 + ss;
    let utc_s = local_s - sign * zone_q * 15 * 60;
    Some(utc_s * 1000)
}

/// Days since 1970-01-01 (proleptic Gregorian; Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
