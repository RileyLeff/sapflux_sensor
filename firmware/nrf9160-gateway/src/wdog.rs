//! nRF WDT supervision and panic-to-reset.
//!
//! The hardware watchdog (40 s, keeps counting while the CPU sleeps) is
//! petted by a small task, but only while the main loop has recently
//! declared itself alive with [`alive_for`]. A hung main loop, a hung
//! executor, or a panic therefore all end in a watchdog reset.
//!
//! Under TF-M the non-secure side cannot use SYSRESETREQ (TF-M sets
//! AIRCR.SYSRESETREQS), so the panic handler stops everything and lets the
//! watchdog reset the chip.

use core::fmt::Write;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_nrf::wdt::{self, HaltConfig, SleepConfig, Watchdog, WatchdogHandle};
use embassy_nrf::{Peri, peripherals};
use embassy_time::{Instant, Timer};

/// Hardware timeout, seconds.
pub const HW_TIMEOUT_S: u32 = 40;
/// Pet period, seconds.
const PET_PERIOD_S: u64 = 10;
/// Default grace the main loop gets per [`alive`] call, seconds.
pub const DEFAULT_GRACE_S: u32 = 180;

static DEADLINE_S: AtomicU32 = AtomicU32::new(0);
static STARVE: AtomicBool = AtomicBool::new(false);

fn now_s() -> u32 {
    Instant::now().as_secs() as u32
}

/// Allow the main loop to go `secs` seconds without checking in.
pub fn alive_for(secs: u32) {
    DEADLINE_S.store(now_s().saturating_add(secs), Ordering::Relaxed);
}

/// [`alive_for`] with the default grace.
pub fn alive() {
    alive_for(DEFAULT_GRACE_S);
}

/// Stop petting; the chip resets within [`HW_TIMEOUT_S`].
pub fn request_reset() {
    STARVE.store(true, Ordering::Relaxed);
}

/// Start (or adopt) the WDT. Returns the handle for [`pet_task`].
pub fn start(wdt: Peri<'static, peripherals::WDT>) -> (WatchdogHandle, bool) {
    let mut config = wdt::Config::default();
    config.timeout_ticks = HW_TIMEOUT_S * 32_768;
    config.action_during_sleep = SleepConfig::Run;
    config.action_during_debug_halt = HaltConfig::Pause;
    alive();
    match Watchdog::try_new::<_, 1>(wdt, config) {
        Ok((_wdt, [handle])) => (handle, true),
        // Already running with another configuration (e.g. a previous
        // image): it cannot be reconfigured until reset, so keep petting
        // reload register 0, which every configuration enables.
        Err(_) => (unsafe { WatchdogHandle::steal::<peripherals::WDT>(0) }, false),
    }
}

#[embassy_executor::task]
pub async fn pet_task(mut handle: WatchdogHandle) {
    loop {
        if !STARVE.load(Ordering::Relaxed) && now_s() <= DEADLINE_S.load(Ordering::Relaxed) {
            handle.pet();
        }
        Timer::after_secs(PET_PERIOD_S).await;
    }
}

// ---------------------------------------------------------------------------
// Panic record that survives the watchdog reset (RAM is retained across a
// WDT reset on the nRF9160, not across power loss).

const PANIC_MAGIC: u32 = 0x5041_4E43; // "PANC"
const PANIC_TEXT: usize = 120;

#[repr(C)]
struct PanicRecord {
    magic: u32,
    len: u32,
    text: [u8; PANIC_TEXT],
}

#[unsafe(link_section = ".uninit.PANIC")]
static mut PANIC_RECORD: MaybeUninit<PanicRecord> = MaybeUninit::uninit();

/// Take the panic message left by the previous boot, if any.
pub fn take_panic_record(out: &mut crate::log::Line) -> bool {
    // SAFETY: single-threaded access at boot, before any panic can occur
    // concurrently; the record is plain bytes validated by magic and len.
    unsafe {
        let rec = &raw mut PANIC_RECORD;
        let rec = (*rec).as_mut_ptr();
        let magic = core::ptr::read_volatile(&raw const (*rec).magic);
        let len = core::ptr::read_volatile(&raw const (*rec).len) as usize;
        core::ptr::write_volatile(&raw mut (*rec).magic, 0);
        if magic != PANIC_MAGIC || len > PANIC_TEXT {
            return false;
        }
        let text: [u8; PANIC_TEXT] = core::ptr::read_volatile(&raw const (*rec).text);
        let _ = out.write_str(core::str::from_utf8(&text[..len]).unwrap_or("?"));
        true
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    cortex_m::interrupt::disable();
    let mut line = crate::log::Line::new();
    let _ = write!(line, "{}", info);
    // SAFETY: interrupts are off and nothing else runs after a panic.
    unsafe {
        let rec = (*(&raw mut PANIC_RECORD)).as_mut_ptr();
        let bytes = line.as_bytes();
        let n = bytes.len().min(PANIC_TEXT);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), (*rec).text.as_mut_ptr(), n);
        core::ptr::write_volatile(&raw mut (*rec).len, n as u32);
        core::ptr::write_volatile(&raw mut (*rec).magic, PANIC_MAGIC);
    }
    crate::log::log_fmt(format_args!("PANIC: {} (watchdog reset follows)", line.as_str()));
    // Executor is stopped (interrupts off), so the pet task never runs again.
    loop {
        cortex_m::asm::wfe();
    }
}
