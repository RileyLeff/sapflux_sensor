//! Completed binned windows persisted to flash slots (upper 8 MiB).
//!
//! Each window gets one 32 KiB slot, used in ring order:
//!
//! | offset | bytes | contents |
//! |---|---|---|
//! | 0 | 4 | magic `"SFWN"` |
//! | 4 | 1 | version 1 (then 3 zero bytes) |
//! | 8 | 4 | `seq` (board-wide slot counter) |
//! | 16 | 102 | `WindowInfo` as a protocol frame (header, payload, CRC) |
//! | 124 | 4 | CRC-32 over bytes 0..124 |
//! | 255 | 1 | delivered marker: `0xFF` pending, `0x00` delivered |
//! | 256 | `n_bins × 26` | window data, exactly as `WindowChunk`s carry it |
//!
//! A finished window stays in the RAM [`WindowStore`] while it is written:
//! erase the slot's 8 sectors, program the data pages, program the header
//! page last (the commit), then read everything back against the RAM copy.
//! Only a verified window is released from RAM, so a reset at any point
//! loses nothing that was not yet in flash, and a window is never in both
//! places as "pending". `AckWindow` programs the marker byte 0xFF → 0x00
//! (deferred to [`WindowLog::service`], never during a pulse). At boot,
//! [`WindowLog::mount`] reads the 256 header pages and serves undelivered
//! windows straight from flash.
//!
//! A new window takes the slot after the newest one; if that slot still
//! holds an undelivered window, that window is dropped (counted). A window
//! that fails read-back has its header magic zeroed and is retried in the
//! next slot; three failures in a row declare the chip dead (windows then
//! stay in RAM as before flash support).

use crate::flash::{Flash, Nor, Ready, PAGE_BYTES, SECTOR_BYTES, WIN_SLOT_BYTES};
use crate::window::WindowStore;
use sap_flux_protocol::crc::crc32;
use sap_flux_protocol::field::chunk_range;
use sap_flux_protocol::{
    decode_frame, encode_frame, Message, WindowChunk, WindowInfo, WINDOW_INFO_FRAME_LEN,
    WINDOW_OLDEST,
};

const MAGIC: u32 = 0x4E57_4653; // "SFWN"
const VERSION: u8 = 1;
const INFO_OFFSET: usize = 16;
const HEADER_CRC_OFFSET: usize = 124;
/// Offset of the delivered marker in the header page.
pub const MARKER_OFFSET: u32 = 255;
/// Offset of the window data in a slot.
pub const DATA_OFFSET: u32 = PAGE_BYTES;
const SLOT_SECTORS: u32 = WIN_SLOT_BYTES / SECTOR_BYTES;
const MAX_VERIFY_FAILURES: u8 = 3;

const _: () = assert!(INFO_OFFSET + WINDOW_INFO_FRAME_LEN <= HEADER_CRC_OFFSET);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum SlotState {
    Empty = 0,
    Pending = 1,
    Delivered = 2,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    state: SlotState,
    /// Marker program still owed (acknowledged since the last service).
    marker_owed: bool,
    run_id: u32,
    seq: u32,
}

const EMPTY: Entry = Entry {
    state: SlotState::Empty,
    marker_owed: false,
    run_id: 0,
    seq: 0,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Stage {
    Idle = 0,
    Erase,
    Program,
    Header,
    VerifyHeader,
    VerifyData,
    Kill,
}

/// Flash window slots plus the write job. All-zero until mounted, so a
/// `static` of it lives in `.bss`.
pub struct WindowLog<const SLOTS: usize> {
    base: u32,
    mounted: bool,
    entries: [Entry; SLOTS],
    next_slot: usize,
    next_seq: u32,
    dropped: u16,
    stage: Stage,
    /// Sector (Erase) or page (Program, VerifyData) index of the stage.
    step: u32,
    job_slot: usize,
    job_run_id: u32,
    job_seq: u32,
    failures: u8,
}

impl<const SLOTS: usize> Default for WindowLog<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

/// The header page of a slot holding `info`.
fn header_page(seq: u32, info: &WindowInfo) -> [u8; PAGE_BYTES as usize] {
    let mut page = [0xFF; PAGE_BYTES as usize];
    page[..HEADER_CRC_OFFSET].fill(0);
    page[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    page[4] = VERSION;
    page[8..12].copy_from_slice(&seq.to_le_bytes());
    let _ = encode_frame(
        &Message::WindowInfo(*info),
        seq,
        &mut page[INFO_OFFSET..INFO_OFFSET + WINDOW_INFO_FRAME_LEN],
    );
    let crc = crc32(&page[..HEADER_CRC_OFFSET]);
    page[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    page
}

/// `(seq, info, delivered)` of a valid header page.
fn parse_header(page: &[u8; PAGE_BYTES as usize]) -> Option<(u32, WindowInfo, bool)> {
    let word = |i: usize| u32::from_le_bytes([page[i], page[i + 1], page[i + 2], page[i + 3]]);
    if word(0) != MAGIC
        || page[4] != VERSION
        || crc32(&page[..HEADER_CRC_OFFSET]) != word(HEADER_CRC_OFFSET)
    {
        return None;
    }
    let frame = decode_frame(&page[INFO_OFFSET..INFO_OFFSET + WINDOW_INFO_FRAME_LEN]).ok()?;
    let Message::WindowInfo(info) = frame.message else {
        return None;
    };
    // Any programmed bit in the marker means an acknowledgement started.
    Some((word(8), info, page[MARKER_OFFSET as usize] != 0xFF))
}

impl<const SLOTS: usize> WindowLog<SLOTS> {
    pub const fn new() -> Self {
        Self {
            base: 0,
            mounted: false,
            entries: [EMPTY; SLOTS],
            next_slot: 0,
            next_seq: 0,
            dropped: 0,
            stage: Stage::Idle,
            step: 0,
            job_slot: 0,
            job_run_id: 0,
            job_seq: 0,
            failures: 0,
        }
    }

    fn slot_addr(&self, slot: usize) -> u32 {
        self.base + slot as u32 * WIN_SLOT_BYTES
    }

    /// Read every slot header at `base`. Returns `false` (store stays off,
    /// windows stay in RAM) if the chip fails.
    pub fn mount<N: Nor>(
        &mut self,
        flash: &mut Flash<N>,
        base: u32,
        mut tick: impl FnMut(),
    ) -> bool {
        *self = Self::new();
        self.base = base;
        if flash.poll(0) != Ready::Ready {
            return false;
        }
        let mut newest: Option<(u32, usize)> = None;
        let mut page = [0u8; PAGE_BYTES as usize];
        for slot in 0..SLOTS {
            if slot % 32 == 0 {
                tick();
            }
            if !flash.read(self.slot_addr(slot), &mut page) {
                return false;
            }
            let Some((seq, info, delivered)) = parse_header(&page) else {
                continue;
            };
            self.entries[slot] = Entry {
                state: if delivered {
                    SlotState::Delivered
                } else {
                    SlotState::Pending
                },
                marker_owed: false,
                run_id: info.run_id,
                seq,
            };
            if newest.is_none_or(|(s, _)| seq > s) {
                newest = Some((seq, slot));
            }
        }
        if let Some((seq, slot)) = newest {
            self.next_slot = (slot + 1) % SLOTS;
            self.next_seq = seq.wrapping_add(1);
        }
        self.mounted = true;
        true
    }

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    /// The chip died: stop using it. Its undelivered windows are lost to the
    /// host, so they count as dropped; new windows stay in RAM.
    pub fn disable(&mut self) {
        let lost = self.flash_pending().min(u16::MAX as usize) as u16;
        let dropped = self.dropped.saturating_add(lost);
        *self = Self::new();
        self.dropped = dropped;
    }

    /// Flash taken by stored windows (delivered ones included), KiB.
    pub fn used_kib(&self) -> u32 {
        let used = self
            .entries
            .iter()
            .filter(|e| e.state != SlotState::Empty)
            .count() as u32;
        used * (WIN_SLOT_BYTES / 1024)
    }

    /// Whether a window is being written (its RAM copy is still pending).
    pub fn busy(&self) -> bool {
        self.stage != Stage::Idle
    }

    fn flash_pending(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.state == SlotState::Pending)
            .count()
    }

    fn oldest_flash(&self) -> Option<usize> {
        (0..SLOTS)
            .filter(|&i| self.entries[i].state == SlotState::Pending)
            .min_by_key(|&i| self.entries[i].seq)
    }

    fn find_flash(&self, run_id: u32, state: SlotState) -> Option<usize> {
        if run_id == WINDOW_OLDEST {
            return if state == SlotState::Pending {
                self.oldest_flash()
            } else {
                None
            };
        }
        // The oldest copy, as `WINDOW_OLDEST` would pick, so a repeated id
        // resolves to one window for header, chunks and ack alike.
        (0..SLOTS)
            .filter(|&i| self.entries[i].state == state && self.entries[i].run_id == run_id)
            .min_by_key(|&i| self.entries[i].seq)
    }

    /// Run ids of every stored window, delivered ones included.
    pub fn run_ids(&self) -> impl Iterator<Item = u32> + Clone + '_ {
        self.entries
            .iter()
            .filter(|e| e.state != SlotState::Empty)
            .map(|e| e.run_id)
    }

    // ----- What the board reports: RAM windows (not yet persisted) plus
    // ----- flash windows. Flash windows are always the older ones.

    pub fn windows_pending<const S: usize, const B: usize>(
        &self,
        ram: &WindowStore<S, B>,
    ) -> usize {
        self.flash_pending() + ram.windows_pending()
    }

    pub fn windows_dropped<const S: usize, const B: usize>(&self, ram: &WindowStore<S, B>) -> u16 {
        self.dropped.saturating_add(ram.windows_dropped())
    }

    pub fn oldest_pending_run_id<const S: usize, const B: usize>(
        &self,
        ram: &WindowStore<S, B>,
    ) -> u32 {
        match self.oldest_flash() {
            Some(i) => self.entries[i].run_id,
            None => ram.oldest_pending_run_id(),
        }
    }

    /// The newest completed window since boot, else the newest in flash.
    pub fn last_run_id<const S: usize, const B: usize>(&self, ram: &WindowStore<S, B>) -> u32 {
        if ram.last_run_id() != 0 {
            return ram.last_run_id();
        }
        (0..SLOTS)
            .filter(|&i| self.entries[i].state != SlotState::Empty)
            .max_by_key(|&i| self.entries[i].seq)
            .map_or(0, |i| self.entries[i].run_id)
    }

    /// Whether answering for `run_id` needs a flash read (it is not a RAM
    /// window but a stored one).
    pub fn needs_flash<const S: usize, const B: usize>(
        &self,
        ram: &WindowStore<S, B>,
        run_id: u32,
    ) -> bool {
        // Flash windows are the older ones, so a flash copy of `run_id`
        // wins over a RAM window that reuses the id.
        let in_ram =
            run_id == WINDOW_OLDEST && self.oldest_flash().is_none() && ram.windows_pending() > 0;
        !in_ram && self.find_flash(run_id, SlotState::Pending).is_some()
    }

    /// `WindowInfo` of a pending window (`run_id` 0 = oldest). Reads flash
    /// only for a stored window; call then only while the chip is idle.
    pub fn info<N: Nor, const S: usize, const B: usize>(
        &self,
        flash: &mut Flash<N>,
        ram: &WindowStore<S, B>,
        run_id: u32,
    ) -> Option<WindowInfo> {
        if !self.needs_flash(ram, run_id) {
            return ram.info(run_id);
        }
        let slot = self.find_flash(run_id, SlotState::Pending)?;
        self.read_info(flash, slot)
    }

    fn read_info<N: Nor>(&self, flash: &mut Flash<N>, slot: usize) -> Option<WindowInfo> {
        let mut page = [0u8; PAGE_BYTES as usize];
        if !flash.read(self.slot_addr(slot), &mut page) {
            return None;
        }
        parse_header(&page)
            .filter(|(seq, info, _)| {
                *seq == self.entries[slot].seq && info.run_id == self.entries[slot].run_id
            })
            .map(|(_, info, _)| info)
    }

    /// One data chunk of a pending window.
    pub fn chunk<N: Nor, const S: usize, const B: usize>(
        &self,
        flash: &mut Flash<N>,
        ram: &WindowStore<S, B>,
        run_id: u32,
        chunk: u16,
    ) -> Option<WindowChunk> {
        if !self.needs_flash(ram, run_id) {
            return ram.chunk(run_id, chunk);
        }
        let slot = self.find_flash(run_id, SlotState::Pending)?;
        let info = self.read_info(flash, slot)?;
        let range = chunk_range(chunk, info.n_bins as usize * crate::window::BIN_BYTES)?;
        let mut data = [0u8; sap_flux_protocol::WINDOW_CHUNK_DATA_LEN];
        let out = &mut data[..range.len()];
        if !flash.read(self.slot_addr(slot) + DATA_OFFSET + range.start as u32, out) {
            return None;
        }
        WindowChunk::new(info.run_id, chunk, out)
    }

    /// `AckWindow`: free a delivered window. True if it was pending (RAM or
    /// flash) or was already acknowledged. The flash marker is programmed
    /// later by [`Self::service`].
    pub fn ack<const S: usize, const B: usize>(
        &mut self,
        ram: &mut WindowStore<S, B>,
        run_id: u32,
    ) -> bool {
        if run_id == WINDOW_OLDEST {
            return false;
        }
        if let Some(slot) = self.find_flash(run_id, SlotState::Pending) {
            self.entries[slot].state = SlotState::Delivered;
            self.entries[slot].marker_owed = true;
            return true;
        }
        if ram.has_pending(run_id) {
            return ram.ack(run_id);
        }
        self.find_flash(run_id, SlotState::Delivered).is_some() || ram.ack(run_id)
    }

    /// Do at most one flash operation: an owed delivered marker, else the
    /// next step of writing the oldest RAM window. Call only while the
    /// heater is off. Returns whether an operation was started.
    pub fn service<N: Nor, const S: usize, const B: usize>(
        &mut self,
        flash: &mut Flash<N>,
        now_ms: u64,
        ram: &mut WindowStore<S, B>,
    ) -> bool {
        if !self.mounted || flash.poll(now_ms) != Ready::Ready {
            return false;
        }
        if let Some(slot) = (0..SLOTS).find(|&i| self.entries[i].marker_owed) {
            self.entries[slot].marker_owed = false;
            return flash.program(now_ms, self.slot_addr(slot) + MARKER_OFFSET, &[0x00]);
        }
        if self.stage == Stage::Idle {
            let run_id = ram.oldest_pending_run_id();
            if run_id == 0 {
                return false;
            }
            self.start_job(run_id);
        }
        // The RAM copy can vanish mid-write (acknowledged, or overwritten
        // when RAM is full). Before the commit that just abandons the slot.
        let Some((info, data)) = ram.pending_window(self.job_run_id) else {
            if self.stage == Stage::VerifyHeader || self.stage == Stage::VerifyData {
                // Committed: the host already has it.
                self.entries[self.job_slot] = Entry {
                    state: SlotState::Delivered,
                    marker_owed: true,
                    run_id: self.job_run_id,
                    seq: self.job_seq,
                };
            }
            self.stage = Stage::Idle;
            return false;
        };
        let info = *info;
        let addr = self.slot_addr(self.job_slot);
        let data_pages = (data.len() as u32).div_ceil(PAGE_BYTES);
        match self.stage {
            Stage::Idle => false,
            Stage::Erase => {
                let ok = flash.erase_sector(now_ms, addr + self.step * SECTOR_BYTES);
                self.step += 1;
                if self.step == SLOT_SECTORS {
                    self.stage = if data_pages == 0 {
                        Stage::Header
                    } else {
                        Stage::Program
                    };
                    self.step = 0;
                }
                ok
            }
            Stage::Program => {
                let start = (self.step * PAGE_BYTES) as usize;
                let end = (start + PAGE_BYTES as usize).min(data.len());
                let ok =
                    flash.program(now_ms, addr + DATA_OFFSET + start as u32, &data[start..end]);
                self.step += 1;
                if self.step == data_pages {
                    self.stage = Stage::Header;
                }
                ok
            }
            Stage::Header => {
                let ok = flash.program(now_ms, addr, &header_page(self.job_seq, &info));
                self.stage = Stage::VerifyHeader;
                ok
            }
            Stage::VerifyHeader => {
                let mut page = [0u8; PAGE_BYTES as usize];
                if flash.read(addr, &mut page) && page == header_page(self.job_seq, &info) {
                    self.stage = Stage::VerifyData;
                    self.step = 0;
                    if data_pages == 0 {
                        self.commit(ram);
                    }
                } else {
                    self.stage = Stage::Kill;
                }
                false
            }
            Stage::VerifyData => {
                let start = (self.step * PAGE_BYTES) as usize;
                let end = (start + PAGE_BYTES as usize).min(data.len());
                let mut page = [0u8; PAGE_BYTES as usize];
                let page = &mut page[..end - start];
                if !flash.read(addr + DATA_OFFSET + start as u32, page) || page != &data[start..end]
                {
                    self.stage = Stage::Kill;
                    return false;
                }
                self.step += 1;
                if self.step == data_pages {
                    self.commit(ram);
                }
                false
            }
            Stage::Kill => {
                // Zero the magic so a boot scan never trusts this slot, then
                // retry in the next one.
                let ok = flash.program(now_ms, addr, &[0; 4]);
                self.failures += 1;
                self.stage = Stage::Idle;
                if self.failures >= MAX_VERIFY_FAILURES {
                    flash.fail();
                }
                ok
            }
        }
    }

    fn start_job(&mut self, run_id: u32) {
        let slot = self.next_slot;
        if self.entries[slot].state == SlotState::Pending {
            self.dropped = self.dropped.saturating_add(1);
        }
        self.entries[slot] = EMPTY;
        self.next_slot = (slot + 1) % SLOTS;
        self.job_slot = slot;
        self.job_seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.job_run_id = run_id;
        self.stage = Stage::Erase;
        self.step = 0;
    }

    fn commit<const S: usize, const B: usize>(&mut self, ram: &mut WindowStore<S, B>) {
        self.entries[self.job_slot] = Entry {
            state: SlotState::Pending,
            marker_owed: false,
            run_id: self.job_run_id,
            seq: self.job_seq,
        };
        ram.release(self.job_run_id);
        self.stage = Stage::Idle;
        self.failures = 0;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::flash::mem::{settle, MemNor};
    use crate::window::{WindowParams, BIN_BYTES, CHANNELS};
    use std::vec::Vec;

    const SLOTS: usize = 4;
    const BASE: u32 = WIN_SLOT_BYTES; // not at 0, to catch base mistakes
    type Ram = WindowStore<3, { 40 * BIN_BYTES }>;
    type Log = WindowLog<SLOTS>;

    fn chip() -> Flash<MemNor> {
        Flash::new(
            MemNor::with_len(((SLOTS + 2) as u32 * WIN_SLOT_BYTES) as usize),
            true,
        )
    }

    /// Record and finish a 10 s window of `run_id` in RAM.
    fn record(ram: &mut Ram, run_id: u32, bins: u64) {
        let start = 1_000_000 * run_id as u64;
        ram.begin(WindowParams {
            run_id,
            start_us: start,
            start_rtc_ms: 1_793_000_000_000,
            bin_ms: 250,
            sample_hz: 25,
            pulse_duration_ms: 1000,
            time_invalid: false,
        });
        ram.note_heater_on(start + 100_000);
        for i in 0..bins * 6 {
            let mut v = [0i16; CHANNELS];
            for (c, x) in v.iter_mut().enumerate() {
                *x = (1000 + run_id * 10 + c as u32 * 7 + (i % 11) as u32) as i16;
            }
            ram.push(start + i * 40_000, &v, 2400);
        }
        ram.finish(
            start + bins * 250_000,
            1_793_000_000_000 + bins as i64 * 250,
        );
    }

    fn download(
        log: &Log,
        f: &mut Flash<MemNor>,
        ram: &Ram,
        run_id: u32,
    ) -> Option<(WindowInfo, Vec<u8>)> {
        let info = log.info(f, ram, run_id)?;
        let mut data = Vec::new();
        for c in 0..info.chunk_count {
            data.extend_from_slice(log.chunk(f, ram, info.run_id, c)?.bytes());
        }
        assert!(log.chunk(f, ram, info.run_id, info.chunk_count).is_none());
        assert_eq!(crc32(&data), info.data_crc32);
        Some((info, data))
    }

    fn snapshot(ram: &Ram, run_id: u32) -> (WindowInfo, Vec<u8>) {
        let (i, d) = ram.pending_window(run_id).unwrap();
        (*i, d.to_vec())
    }

    fn persist_all(log: &mut Log, f: &mut Flash<MemNor>, ram: &mut Ram) -> usize {
        let mut ops = 0;
        for _ in 0..10_000 {
            settle(f, 0);
            if log.service(f, 0, ram) {
                ops += 1;
            } else if !log.busy() && (ram.windows_pending() == 0 || !f.is_ok()) {
                break;
            }
        }
        settle(f, 0);
        ops
    }

    #[test]
    fn windows_move_to_flash_and_survive_a_reset() {
        let mut f = chip();
        f.nor.busy_polls = 3;
        let mut ram = Ram::new();
        let mut log = Log::new();
        assert!(log.mount(&mut f, BASE, || {}));
        record(&mut ram, 11, 40);
        record(&mut ram, 12, 7);
        let w11 = snapshot(&ram, 11);
        let w12 = snapshot(&ram, 12);
        assert_eq!(log.windows_pending(&ram), 2);
        // 8 erases + data pages + header, per window.
        let ops = persist_all(&mut log, &mut f, &mut ram);
        assert_eq!(ops, (8 + 5 + 1) + (8 + 1 + 1));
        assert_eq!(ram.windows_pending(), 0);
        assert_eq!(log.windows_pending(&ram), 2);
        assert_eq!(log.oldest_pending_run_id(&ram), 11);
        assert_eq!(log.last_run_id(&ram), 12);
        assert_eq!(download(&log, &mut f, &ram, 0).unwrap(), w11);
        assert_eq!(download(&log, &mut f, &ram, 12).unwrap(), w12);
        assert_eq!(log.used_kib(), 64);
        // Nothing outside the slots was touched.
        assert!(f.nor.bytes[..BASE as usize].iter().all(|&b| b == 0xFF));

        // Reset: RAM is gone, flash still serves both.
        let ram = Ram::new();
        let mut log = Log::new();
        assert!(log.mount(&mut f, BASE, || {}));
        assert_eq!(log.windows_pending(&ram), 2);
        assert_eq!(log.last_run_id(&ram), 12);
        assert_eq!(download(&log, &mut f, &ram, 0).unwrap(), w11);
        assert_eq!(download(&log, &mut f, &ram, 12).unwrap(), w12);
    }

    #[test]
    fn ack_programs_the_marker_and_is_idempotent() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        record(&mut ram, 21, 10);
        record(&mut ram, 22, 10);
        persist_all(&mut log, &mut f, &mut ram);
        assert!(log.ack(&mut ram, 21));
        assert_eq!(log.windows_pending(&ram), 1);
        assert_eq!(log.oldest_pending_run_id(&ram), 22);
        assert!(log.info(&mut f, &ram, 21).is_none());
        assert!(log.ack(&mut ram, 21)); // retried ack
        assert!(!log.ack(&mut ram, 99));
        // The marker is programmed on the next service.
        let marker = (BASE + MARKER_OFFSET) as usize;
        assert_eq!(f.nor.bytes[marker], 0xFF);
        assert!(log.service(&mut f, 0, &mut ram));
        assert_eq!(f.nor.bytes[marker], 0x00);
        settle(&mut f, 0);

        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        assert_eq!(log.windows_pending(&ram), 1);
        assert_eq!(log.oldest_pending_run_id(&ram), 22);
        assert!(log.ack(&mut ram, 21));
        assert_eq!(log.used_kib(), 64);
    }

    #[test]
    fn reset_mid_write_keeps_nothing_half_written() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        record(&mut ram, 31, 40);
        // Erase + every data page, but not the header.
        for _ in 0..8 + 5 {
            settle(&mut f, 0);
            assert!(log.service(&mut f, 0, &mut ram));
        }
        settle(&mut f, 0);
        // Still pending in RAM, not in flash: counted once.
        assert_eq!(log.windows_pending(&ram), 1);
        let mut log2 = Log::new();
        log2.mount(&mut f, BASE, || {});
        assert_eq!(log2.windows_pending(&Ram::new()), 0);
    }

    #[test]
    fn served_from_ram_while_writing_and_acked_mid_write() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        record(&mut ram, 41, 40);
        let w = snapshot(&ram, 41);
        for _ in 0..3 {
            settle(&mut f, 0);
            log.service(&mut f, 0, &mut ram);
        }
        settle(&mut f, 0);
        assert!(!log.needs_flash(&ram, 41));
        assert_eq!(download(&log, &mut f, &ram, 41).unwrap(), w);
        // Acked before the commit: the slot is abandoned, nothing pending.
        assert!(log.ack(&mut ram, 41));
        persist_all(&mut log, &mut f, &mut ram);
        assert_eq!(log.windows_pending(&ram), 0);
        assert!(!log.busy());
        let mut log2 = Log::new();
        log2.mount(&mut f, BASE, || {});
        assert_eq!(log2.windows_pending(&ram), 0);

        // Acked after the commit, during read-back: delivered, marker set.
        record(&mut ram, 42, 40);
        for _ in 0..8 + 5 + 1 + 1 {
            settle(&mut f, 0);
            log.service(&mut f, 0, &mut ram);
        }
        settle(&mut f, 0);
        assert!(log.busy());
        assert!(log.ack(&mut ram, 42));
        persist_all(&mut log, &mut f, &mut ram);
        settle(&mut f, 0);
        log.service(&mut f, 0, &mut ram); // owed marker
        settle(&mut f, 0);
        let mut log3 = Log::new();
        log3.mount(&mut f, BASE, || {});
        assert_eq!(log3.windows_pending(&ram), 0);
        assert!(log3.ack(&mut ram, 42));
    }

    #[test]
    fn a_reused_run_id_resolves_to_one_window_oldest_first() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        // Three windows share id 41: two in flash, the newest in RAM.
        let mut windows = Vec::new();
        for bins in [40, 30, 20] {
            record(&mut ram, 41, bins);
            windows.push(snapshot(&ram, 41));
            if bins != 20 {
                persist_all(&mut log, &mut f, &mut ram);
            }
        }
        for w in &windows {
            // The oldest header and the chunks fetched by id agree, and the
            // ack retires that same window.
            assert_eq!(log.oldest_pending_run_id(&ram), 41);
            assert_eq!(
                download(&log, &mut f, &ram, WINDOW_OLDEST).as_ref(),
                Some(w)
            );
            assert_eq!(download(&log, &mut f, &ram, 41).as_ref(), Some(w));
            assert!(log.ack(&mut ram, 41));
        }
        assert_eq!(log.windows_pending(&ram), 0);
    }

    #[test]
    fn a_full_ring_drops_the_oldest_and_counts_it() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        for r in 0..6u32 {
            record(&mut ram, 100 + r, 4);
            persist_all(&mut log, &mut f, &mut ram);
        }
        assert_eq!(log.windows_pending(&ram), SLOTS);
        assert_eq!(log.windows_dropped(&ram), 2);
        assert_eq!(log.oldest_pending_run_id(&ram), 102);
        assert!(log.info(&mut f, &ram, 101).is_none());
        let mut dead = Log::new();
        dead.mount(&mut f, BASE, || {});
        dead.disable();
        assert_eq!(dead.windows_pending(&ram), 0);
        assert_eq!(dead.windows_dropped(&ram), SLOTS as u16);
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        assert_eq!(log.oldest_pending_run_id(&ram), 102);
        assert_eq!(log.last_run_id(&ram), 105);
        // The next window goes after the newest (slot of 102 → dropped).
        record(&mut ram, 200, 4);
        persist_all(&mut log, &mut f, &mut ram);
        assert_eq!(log.oldest_pending_run_id(&ram), 103);
        assert!(download(&log, &mut f, &ram, 200).is_some());
    }

    #[test]
    fn verify_failures_retry_then_give_up_keeping_ram() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        record(&mut ram, 51, 8);
        let w = snapshot(&ram, 51);
        f.nor.corrupt_program = 0x01;
        persist_all(&mut log, &mut f, &mut ram);
        assert!(!f.is_ok());
        assert!(!log.busy());
        assert_eq!(ram.windows_pending(), 1);
        assert_eq!(log.windows_pending(&ram), 1);
        assert_eq!(download(&log, &mut f, &ram, 51).unwrap(), w);
        log.disable();
        assert_eq!(log.windows_pending(&ram), 1);
        assert_eq!(download(&log, &mut f, &ram, 51).unwrap(), w);
        // No killed slot looks valid after a reset.
        let mut nor = MemNor::with_len(0);
        nor.bytes = core::mem::take(&mut f.nor.bytes);
        let mut f3 = Flash::new(nor, true);
        let mut log2 = Log::new();
        log2.mount(&mut f3, BASE, || {});
        assert_eq!(log2.windows_pending(&Ram::new()), 0);
    }

    #[test]
    fn one_bad_write_is_retried_in_the_next_slot() {
        let mut f = chip();
        let mut ram = Ram::new();
        let mut log = Log::new();
        log.mount(&mut f, BASE, || {});
        record(&mut ram, 61, 8);
        let w = snapshot(&ram, 61);
        // Corrupt only the first data page program.
        for _ in 0..8 {
            settle(&mut f, 0);
            log.service(&mut f, 0, &mut ram);
        }
        settle(&mut f, 0);
        f.nor.corrupt_program = 0x80;
        log.service(&mut f, 0, &mut ram);
        settle(&mut f, 0);
        f.nor.corrupt_program = 0;
        persist_all(&mut log, &mut f, &mut ram);
        assert!(f.is_ok());
        assert_eq!(log.windows_pending(&ram), 1);
        assert_eq!(ram.windows_pending(), 0);
        assert_eq!(download(&log, &mut f, &ram, 61).unwrap(), w);
        let mut log2 = Log::new();
        log2.mount(&mut f, BASE, || {});
        assert_eq!(log2.windows_pending(&ram), 1);
        assert_eq!(download(&log2, &mut f, &ram, 0).unwrap(), w);
    }
}
