//! Per-boot random `boot_id`.
//!
//! The nRF9160's CryptoCell RNG is secure-only and this image runs
//! non-secure under TF-M (the RNG is not reachable without TF-M PSA veneers,
//! which this firmware does not link). Instead the boot id is SHA-256 over:
//!
//! - jitter between the 16 MHz TIMER (HFCLK) and the 32 kHz RTC (LFCLK)
//!   sampled around every modem AT exchange (modem response latency varies
//!   by many microseconds),
//! - the AT responses themselves (IMEI, signal, modem temperature),
//! - 256 bytes of RAM left uninitialized since power-on (random after a
//!   cold start), and
//! - a boot counter kept in that same retained RAM, which makes ids differ
//!   across watchdog/soft resets even if everything else repeated.
//!
//! This is not a cryptographic RNG. `boot_id` only needs to be unique per
//! boot so the server's replay window resets; collisions are improbable.

use core::mem::MaybeUninit;

use embassy_nrf::timer::Timer;
use embassy_time::Instant;
use sha2::{Digest, Sha256};

const COUNTER_MAGIC: u32 = 0xB007_C0DE;

#[repr(C)]
struct Retained {
    noise: [u8; 256],
    magic: u32,
    boot_counter: u32,
}

#[unsafe(link_section = ".uninit.ENTROPY")]
static mut RETAINED: MaybeUninit<Retained> = MaybeUninit::uninit();

pub struct Pool<'d> {
    hash: Sha256,
    timer: Timer<'d>,
}

impl<'d> Pool<'d> {
    pub fn new(timer: Timer<'d>) -> Self {
        timer.start();
        let mut pool = Self {
            hash: Sha256::new(),
            timer,
        };
        // SAFETY: read once at boot, single-threaded; contents are
        // arbitrary bytes and only hashed, then the counter is updated.
        unsafe {
            let r = (*(&raw mut RETAINED)).as_mut_ptr();
            let noise = core::ptr::read_volatile(&raw const (*r).noise);
            pool.hash.update(noise);
            let magic = core::ptr::read_volatile(&raw const (*r).magic);
            let mut counter = core::ptr::read_volatile(&raw const (*r).boot_counter);
            if magic != COUNTER_MAGIC {
                counter = 0;
            }
            counter = counter.wrapping_add(1);
            pool.hash.update(counter.to_le_bytes());
            core::ptr::write_volatile(&raw mut (*r).magic, COUNTER_MAGIC);
            core::ptr::write_volatile(&raw mut (*r).boot_counter, counter);
        }
        pool.mix_timing();
        pool
    }

    /// Mix the current HFCLK timer and LFCLK RTC readings.
    pub fn mix_timing(&mut self) {
        let hf = self.timer.cc(0).capture();
        let lf = Instant::now().as_ticks();
        self.hash.update(hf.to_le_bytes());
        self.hash.update(lf.to_le_bytes());
    }

    pub fn mix(&mut self, bytes: &[u8]) {
        self.mix_timing();
        self.hash.update(bytes);
        self.mix_timing();
    }

    /// Finish; stops the timer. Never returns 0.
    pub fn finish(mut self) -> u32 {
        self.mix_timing();
        self.timer.stop();
        let digest = self.hash.finalize();
        let id = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
        if id == 0 { 1 } else { id }
    }
}

/// Number of boots since the retained RAM was last valid (power loss).
pub fn boot_counter() -> u32 {
    // SAFETY: plain read of retained RAM written in Pool::new.
    unsafe {
        let r = (*(&raw const RETAINED)).as_ptr();
        core::ptr::read_volatile(&raw const (*r).boot_counter)
    }
}
