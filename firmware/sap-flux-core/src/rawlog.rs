//! Append-only ring of raw 25 Hz frame blocks in the lower flash region.
//!
//! Frames are packed into 256 B blocks ([`sap_flux_protocol::raw`]), one per
//! flash page, written in ring order with a board-wide `seq`. Every run
//! starts on a fresh 4 KiB sector, so a boot scan that reads only the first
//! 16 bytes of each sector finds:
//!
//! * the newest sector (highest `seq`): writing resumes at the next sector;
//! * the oldest one (lowest `seq`) and how many hold data;
//! * every run start (`block_in_run == 0` on page 0), which rebuilds the
//!   run index used by `GetRaw`.
//!
//! Nothing here blocks. [`RawLog::push`] only touches RAM; completed blocks
//! wait in a two-deep queue (2 × 12 frames ≈ 1 s at 25 Hz, more than the
//! 400 ms worst-case sector erase). [`RawLog::service`] issues at most one
//! page program or one sector erase per call and only when the chip is idle.
//! Erases run ahead of the write head (`ahead_sectors`, erasing the oldest
//! data) and only when the caller allows it, so none happens while the
//! heater is on. A block that finds the queue full is dropped and counted.

use crate::flash::{Flash, Nor, Ready, PAGES_PER_SECTOR, PAGE_BYTES, SECTOR_BYTES};
use sap_flux_protocol::raw::{block_run_id, parse_prefix, seal_block, BlockBuilder, RawFrame, BLOCK_LEN, PREFIX_LEN};

/// Bytes per `RawChunk`.
pub const CHUNK_LEN: usize = 100;
/// Blocks waiting for flash.
const QUEUE: usize = 2;
/// Sectors scanned between `tick` calls during [`RawLog::mount`].
const TICK_SECTORS: u32 = 64;

/// Where the ring lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Sector-aligned start address.
    pub base: u32,
    /// Ring length in sectors (≥ `ahead_sectors + 2`).
    pub sectors: u32,
    /// Erased sectors kept ready ahead of the write head.
    pub ahead_sectors: u32,
}

#[derive(Clone, Copy)]
struct Pending {
    block: [u8; BLOCK_LEN],
    starts_run: bool,
}

#[derive(Clone, Copy, Default)]
struct RunEntry {
    run_id: u32,
    start_seq: u32,
    /// Ring page of block 0 (always a sector's page 0).
    start_page: u32,
}

/// The ring state. All-zero until [`RawLog::mount`], so a `static` of it
/// lives in `.bss`.
pub struct RawLog<const RUNS: usize> {
    geo: Geometry,
    mounted: bool,
    /// Ring page of the next write.
    head: u32,
    /// Erased pages from `head` on (`head + ready` is sector-aligned).
    ready: u32,
    next_seq: u32,
    run_block: u32,
    used_sectors: u32,
    oldest_sector: u32,
    builder: BlockBuilder,
    queue: [Pending; QUEUE],
    queued: usize,
    /// The next block continues the last enqueued run (cleared by
    /// [`RawLog::end_run`]).
    continuing: bool,
    last_run: u32,
    /// A dropped block started a run: the next one of that run does.
    carry_start: bool,
    runs: [RunEntry; RUNS],
    run_count: usize,
    dropped_frames: u32,
}

impl<const RUNS: usize> Default for RawLog<RUNS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const RUNS: usize> RawLog<RUNS> {
    pub const fn new() -> Self {
        Self {
            geo: Geometry {
                base: 0,
                sectors: 0,
                ahead_sectors: 0,
            },
            mounted: false,
            head: 0,
            ready: 0,
            next_seq: 0,
            run_block: 0,
            used_sectors: 0,
            oldest_sector: 0,
            builder: BlockBuilder::new(),
            queue: [Pending {
                block: [0; BLOCK_LEN],
                starts_run: false,
            }; QUEUE],
            queued: 0,
            continuing: false,
            last_run: 0,
            carry_start: false,
            runs: [RunEntry {
                run_id: 0,
                start_seq: 0,
                start_page: 0,
            }; RUNS],
            run_count: 0,
            dropped_frames: 0,
        }
    }

    fn ring_pages(&self) -> u32 {
        self.geo.sectors * PAGES_PER_SECTOR
    }

    /// Scan the sector headers and resume after the newest sector. Calls
    /// `tick` every 64 sectors (watchdog). Returns `false` (log stays off)
    /// if the chip fails.
    pub fn mount<N: Nor>(&mut self, flash: &mut Flash<N>, geo: Geometry, mut tick: impl FnMut()) -> bool {
        *self = Self::new();
        assert!(geo.base % SECTOR_BYTES == 0 && geo.sectors >= geo.ahead_sectors + 2);
        self.geo = geo;
        if flash.poll(0) != Ready::Ready {
            return false;
        }
        let mut newest: Option<(u32, u32)> = None; // (seq, sector)
        let mut oldest: Option<(u32, u32)> = None;
        let mut used = 0;
        let mut prefix = [0u8; PREFIX_LEN];
        for sector in 0..geo.sectors {
            if sector % TICK_SECTORS == 0 {
                tick();
            }
            if !flash.read(geo.base + sector * SECTOR_BYTES, &mut prefix) {
                return false;
            }
            let Some(p) = parse_prefix(&prefix) else {
                continue;
            };
            used += 1;
            if newest.is_none_or(|(s, _)| p.seq > s) {
                newest = Some((p.seq, sector));
            }
            if oldest.is_none_or(|(s, _)| p.seq < s) {
                oldest = Some((p.seq, sector));
            }
            if p.block_in_run == 0 {
                self.insert_run(RunEntry {
                    run_id: p.run_id,
                    start_seq: p.seq,
                    start_page: sector * PAGES_PER_SECTOR,
                });
            }
        }
        if let (Some((seq, sector)), Some((_, old))) = (newest, oldest) {
            // Resume on the next sector; `seq` advances one per page so a
            // run's block k always sits at start_seq + k.
            self.head = (sector + 1) % geo.sectors * PAGES_PER_SECTOR;
            self.next_seq = seq.wrapping_add(PAGES_PER_SECTOR);
            self.used_sectors = used;
            self.oldest_sector = old;
        }
        self.mounted = true;
        true
    }

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    /// Flash holding logged frames, KiB.
    pub fn used_kib(&self) -> u32 {
        self.used_sectors * (SECTOR_BYTES / 1024)
    }

    /// Frames lost because the queue was full.
    pub fn dropped_frames(&self) -> u32 {
        self.dropped_frames
    }

    /// Log one frame of `run_id` (RAM only).
    pub fn push(&mut self, run_id: u32, frame: &RawFrame) {
        if !self.mounted {
            return;
        }
        if let Some(block) = self.builder.push(run_id, frame) {
            self.enqueue(block);
        }
    }

    /// Close the current run: its last partial block is queued and the next
    /// frame starts a new run on a fresh sector, even with the same id.
    pub fn end_run(&mut self) {
        if let Some(block) = self.builder.finish() {
            self.enqueue(block);
        }
        self.continuing = false;
    }

    fn enqueue(&mut self, block: [u8; BLOCK_LEN]) {
        let run_id = block_run_id(&block);
        let starts_run = self.carry_start || !self.continuing || run_id != self.last_run;
        self.continuing = true;
        self.last_run = run_id;
        if self.queued == QUEUE {
            self.dropped_frames = self.dropped_frames.saturating_add(block[3] as u32);
            self.carry_start = starts_run;
            return;
        }
        self.queue[self.queued] = Pending { block, starts_run };
        self.queued += 1;
        self.carry_start = false;
    }

    /// Blocks waiting for flash.
    pub fn queued(&self) -> usize {
        self.queued
    }

    /// Do at most one flash operation: program the oldest queued block, or
    /// (if `allow_erase`) erase the next sector ahead of the head. Returns
    /// whether an operation was started.
    pub fn service<N: Nor>(&mut self, flash: &mut Flash<N>, now_ms: u64, allow_erase: bool) -> bool {
        if !self.mounted || flash.poll(now_ms) != Ready::Ready {
            return false;
        }
        let ring = self.ring_pages();
        if self.queued > 0 {
            let starts_run = self.queue[0].starts_run;
            let skip = if starts_run {
                (PAGES_PER_SECTOR - self.head % PAGES_PER_SECTOR) % PAGES_PER_SECTOR
            } else {
                0
            };
            if self.ready > skip {
                self.head = (self.head + skip) % ring;
                self.ready -= skip;
                self.next_seq = self.next_seq.wrapping_add(skip);
                if starts_run {
                    self.run_block = 0;
                    let run_id = block_run_id(&self.queue[0].block);
                    self.insert_run(RunEntry {
                        run_id,
                        start_seq: self.next_seq,
                        start_page: self.head,
                    });
                }
                let mut block = self.queue[0].block;
                seal_block(&mut block, self.next_seq, self.run_block);
                if !flash.program(now_ms, self.geo.base + self.head * PAGE_BYTES, &block) {
                    return false;
                }
                if self.head % PAGES_PER_SECTOR == 0 {
                    if self.used_sectors == 0 {
                        self.oldest_sector = self.head / PAGES_PER_SECTOR;
                    }
                    self.used_sectors = (self.used_sectors + 1).min(self.geo.sectors);
                }
                self.head = (self.head + 1) % ring;
                self.ready -= 1;
                self.next_seq = self.next_seq.wrapping_add(1);
                self.run_block += 1;
                self.queue[0] = self.queue[1];
                self.queued -= 1;
                return true;
            }
        }
        let ahead = self.geo.ahead_sectors * PAGES_PER_SECTOR;
        if allow_erase && self.ready < ahead && self.ready + PAGES_PER_SECTOR <= ring - PAGES_PER_SECTOR {
            let sector = (self.head + self.ready) % ring / PAGES_PER_SECTOR;
            if !flash.erase_sector(now_ms, self.geo.base + sector * SECTOR_BYTES) {
                return false;
            }
            self.forget_sector(sector);
            self.ready += PAGES_PER_SECTOR;
            return true;
        }
        false
    }

    fn forget_sector(&mut self, sector: u32) {
        if self.used_sectors > 0 && sector == self.oldest_sector {
            self.used_sectors -= 1;
            self.oldest_sector = (sector + 1) % self.geo.sectors;
        }
        let mut i = 0;
        while i < self.run_count {
            if self.runs[i].start_page / PAGES_PER_SECTOR == sector {
                self.run_count -= 1;
                self.runs[i] = self.runs[self.run_count];
            } else {
                i += 1;
            }
        }
    }

    /// Remember a run start, evicting the oldest when full.
    fn insert_run(&mut self, entry: RunEntry) {
        if self.run_count < RUNS {
            self.runs[self.run_count] = entry;
            self.run_count += 1;
            return;
        }
        let Some((i, oldest)) = self.runs.iter().enumerate().min_by_key(|(_, e)| e.start_seq) else {
            return;
        };
        if entry.start_seq > oldest.start_seq {
            self.runs[i] = entry;
        }
    }

    /// Newest logged run with this id.
    fn find_run(&self, run_id: u32) -> Option<RunEntry> {
        self.runs[..self.run_count]
            .iter()
            .filter(|e| e.run_id == run_id)
            .max_by_key(|e| e.start_seq)
            .copied()
    }

    /// Whether `run_id` has blocks in flash.
    pub fn has_run(&self, run_id: u32) -> bool {
        self.find_run(run_id).is_some()
    }

    /// Bytes `chunk * 100 ..` of the run's block stream (its blocks, 256 B
    /// each, in order). `None` for an unknown run or a chip error; a length
    /// under 100 marks the end. Call only while the chip is idle.
    pub fn read_chunk<N: Nor>(
        &self,
        flash: &mut Flash<N>,
        run_id: u32,
        chunk: u32,
        out: &mut [u8; CHUNK_LEN],
    ) -> Option<usize> {
        let run = self.find_run(run_id)?;
        let ring = self.ring_pages();
        let start = chunk as u64 * CHUNK_LEN as u64;
        let mut n = 0usize;
        while n < CHUNK_LEN {
            let pos = start + n as u64;
            let block = pos / BLOCK_LEN as u64;
            if block >= ring as u64 {
                break;
            }
            let block = block as u32;
            let page = (run.start_page + block) % ring;
            let addr = self.geo.base + page * PAGE_BYTES;
            let mut prefix = [0u8; PREFIX_LEN];
            if !flash.read(addr, &mut prefix) {
                return None;
            }
            let ok = parse_prefix(&prefix).is_some_and(|p| {
                p.run_id == run_id && p.block_in_run == block && p.seq == run.start_seq.wrapping_add(block)
            });
            if !ok {
                break;
            }
            let in_block = (pos % BLOCK_LEN as u64) as usize;
            let take = (BLOCK_LEN - in_block).min(CHUNK_LEN - n);
            if !flash.read(addr + in_block as u32, &mut out[n..n + take]) {
                return None;
            }
            n += take;
        }
        Some(n)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::flash::mem::{settle, MemNor};
    use sap_flux_protocol::raw::decode_block;
    use std::vec::Vec;

    const GEO: Geometry = Geometry {
        base: 2 * SECTOR_BYTES,
        sectors: 8,
        ahead_sectors: 2,
    };

    fn chip() -> Flash<MemNor> {
        Flash::new(MemNor::with_len(12 * SECTOR_BYTES as usize), true)
    }

    fn frame(i: u64) -> RawFrame {
        RawFrame {
            monotonic_us: 5_000_000 + i * 40_000,
            rtc_unix_ms: 1_793_000_000_000 + i as i64 * 40,
            flags: 0x0e,
            state: 1,
            values: core::array::from_fn(|c| 2000 + c as i16 + (i % 5) as i16),
        }
    }

    fn drain(log: &mut RawLog<16>, f: &mut Flash<MemNor>, allow_erase: bool) {
        for _ in 0..10_000 {
            settle(f, 0);
            if !log.service(f, 0, allow_erase) {
                break;
            }
        }
        settle(f, 0);
    }

    /// Log `n` frames of `run_id`, servicing the chip after every frame.
    fn log_run(log: &mut RawLog<16>, f: &mut Flash<MemNor>, run_id: u32, from: u64, n: u64) -> Vec<RawFrame> {
        let frames: Vec<RawFrame> = (from..from + n).map(frame).collect();
        for fr in &frames {
            log.push(run_id, fr);
            drain(log, f, true);
        }
        log.end_run();
        drain(log, f, true);
        frames
    }

    fn fetch(log: &RawLog<16>, f: &mut Flash<MemNor>, run_id: u32) -> Option<Vec<RawFrame>> {
        let mut bytes = Vec::new();
        for chunk in 0.. {
            let mut out = [0u8; CHUNK_LEN];
            let n = log.read_chunk(f, run_id, chunk, &mut out)?;
            bytes.extend_from_slice(&out[..n]);
            if n < CHUNK_LEN {
                break;
            }
        }
        assert_eq!(bytes.len() % BLOCK_LEN, 0);
        let mut frames = Vec::new();
        for (k, b) in bytes.chunks_exact(BLOCK_LEN).enumerate() {
            let h = decode_block(b.try_into().unwrap(), |fr| frames.push(fr)).unwrap();
            assert_eq!(h.prefix.run_id, run_id);
            assert_eq!(h.prefix.block_in_run, k as u32);
        }
        Some(frames)
    }

    #[test]
    fn runs_round_trip_and_start_on_sector_boundaries() {
        let mut f = chip();
        let mut log = RawLog::<16>::new();
        assert!(log.mount(&mut f, GEO, || {}));
        assert_eq!(log.used_kib(), 0);
        let a = log_run(&mut log, &mut f, 7, 0, 100); // 9 blocks
        let b = log_run(&mut log, &mut f, 8, 100, 30); // 3 blocks
        assert_eq!(fetch(&log, &mut f, 7).unwrap(), a);
        assert_eq!(fetch(&log, &mut f, 8).unwrap(), b);
        assert!(fetch(&log, &mut f, 9).is_none());
        // Run 8 starts on sector 1 of the ring, page 0.
        let blk: &[u8] = &f.nor.bytes[(GEO.base + SECTOR_BYTES) as usize..][..BLOCK_LEN];
        assert_eq!(parse_prefix(blk).unwrap().run_id, 8);
        assert_eq!(log.used_kib(), 8);
        assert_eq!(log.dropped_frames(), 0);
        // Nothing outside the ring was touched.
        assert!(f.nor.bytes[..GEO.base as usize].iter().all(|&x| x == 0xFF));
        assert!(f.nor.bytes[(GEO.base + 8 * SECTOR_BYTES) as usize..].iter().all(|&x| x == 0xFF));

        // A reused id is a new run; the newest one is served.
        let c = log_run(&mut log, &mut f, 7, 200, 13);
        assert_eq!(fetch(&log, &mut f, 7).unwrap(), c);
    }

    #[test]
    fn mount_rebuilds_head_index_and_usage() {
        let mut f = chip();
        let mut log = RawLog::<16>::new();
        log.mount(&mut f, GEO, || {});
        let a = log_run(&mut log, &mut f, 1, 0, 250); // 21 blocks: 2 sectors
        let b = log_run(&mut log, &mut f, 2, 250, 12);

        let mut log = RawLog::<16>::new();
        let mut ticks = 0;
        assert!(log.mount(&mut f, GEO, || ticks += 1));
        assert_eq!(ticks, 1);
        assert_eq!(log.used_kib(), 12);
        assert_eq!(fetch(&log, &mut f, 1).unwrap(), a);
        assert_eq!(fetch(&log, &mut f, 2).unwrap(), b);
        // Writing resumes on the next sector without touching old data.
        let c = log_run(&mut log, &mut f, 3, 300, 5);
        assert_eq!(fetch(&log, &mut f, 1).unwrap(), a);
        assert_eq!(fetch(&log, &mut f, 2).unwrap(), b);
        assert_eq!(fetch(&log, &mut f, 3).unwrap(), c);
        let blk: &[u8] = &f.nor.bytes[(GEO.base + 3 * SECTOR_BYTES) as usize..][..BLOCK_LEN];
        assert_eq!(parse_prefix(blk).unwrap().run_id, 3);
    }

    #[test]
    fn wrap_erases_the_oldest_runs() {
        let mut f = chip();
        let mut log = RawLog::<16>::new();
        log.mount(&mut f, GEO, || {});
        let mut runs = Vec::new();
        for r in 0..20u32 {
            runs.push(log_run(&mut log, &mut f, 100 + r, r as u64 * 1000, 30));
        }
        // 8 sectors, 2 kept erased ahead: the newest 6 runs survive.
        for (r, frames) in runs.iter().enumerate() {
            let got = fetch(&log, &mut f, 100 + r as u32);
            if r >= 14 {
                assert_eq!(got.as_ref(), Some(frames), "run {r}");
            } else {
                assert!(got.is_none(), "run {r} should be gone");
            }
        }
        assert_eq!(log.used_kib(), 6 * 4);

        let mut log2 = RawLog::<16>::new();
        log2.mount(&mut f, GEO, || {});
        assert_eq!(log2.used_kib(), 6 * 4);
        for r in 14..20 {
            assert_eq!(fetch(&log2, &mut f, 100 + r).as_ref(), Some(&runs[r as usize]));
        }
        let more = log_run(&mut log2, &mut f, 500, 0, 20);
        assert_eq!(fetch(&log2, &mut f, 500).unwrap(), more);
        assert!(fetch(&log2, &mut f, 114).is_none());
    }

    #[test]
    fn full_queue_drops_and_counts_and_the_run_still_starts() {
        let mut f = chip();
        let mut log = RawLog::<16>::new();
        log.mount(&mut f, GEO, || {});
        drain(&mut log, &mut f, true); // erase ahead
        // 60 frames without servicing: 5 blocks complete, 2 fit the queue.
        for i in 0..61 {
            log.push(9, &frame(i));
        }
        assert_eq!(log.queued(), 2);
        assert_eq!(log.dropped_frames(), 36);
        drain(&mut log, &mut f, true);
        let got = fetch(&log, &mut f, 9).unwrap();
        assert_eq!(got.len(), 24);
        assert_eq!(got[0], frame(0));

        log.end_run();
        drain(&mut log, &mut f, true);
        assert_eq!(fetch(&log, &mut f, 9).unwrap().len(), 25);

        // Run 12's first block finds the queue still full of run 11: it is
        // dropped, and its second block opens the run instead.
        for i in 0..25 {
            log.push(11, &frame(i)); // two blocks queued, frame 24 buffered
        }
        log.end_run(); // frame 24's block: dropped
        for i in 200..213 {
            log.push(12, &frame(i)); // block 0 (200..212): dropped
        }
        assert_eq!(log.dropped_frames(), 36 + 1 + 12);
        drain(&mut log, &mut f, true);
        for i in 213..230 {
            log.push(12, &frame(i));
            drain(&mut log, &mut f, true);
        }
        log.end_run();
        drain(&mut log, &mut f, true);
        assert_eq!(fetch(&log, &mut f, 11).unwrap(), (0..24).map(frame).collect::<Vec<_>>());
        assert_eq!(fetch(&log, &mut f, 12).unwrap(), (212..230).map(frame).collect::<Vec<_>>());
    }

    #[test]
    fn heater_on_never_erases_and_busy_chip_is_respected() {
        let mut f = chip();
        f.nor.busy_polls = 5;
        let mut log = RawLog::<16>::new();
        log.mount(&mut f, GEO, || {});
        // Not erased yet and erasing forbidden: blocks wait, then drop.
        for i in 0..48 {
            log.push(1, &frame(i));
            settle(&mut f, 0);
            log.service(&mut f, 0, false);
        }
        assert_eq!(f.nor.erases, 0);
        assert_eq!(f.nor.programs, 0);
        assert_eq!(log.queued(), 2);
        assert_eq!(log.dropped_frames(), 12);
        // Heater off: erases ahead, then programs. MemNor asserts that no
        // command is issued while busy.
        drain(&mut log, &mut f, true);
        assert!(f.nor.erases >= 2);
        assert_eq!(log.queued(), 0);
        // With sectors ready, logging during "heater on" programs only.
        let erases = f.nor.erases;
        for i in 48..120 {
            log.push(1, &frame(i));
            drain(&mut log, &mut f, false);
        }
        assert_eq!(f.nor.erases, erases);
        assert_eq!(log.dropped_frames(), 12);
    }

    #[test]
    fn dead_chip_disables_the_log() {
        let mut f = Flash::new(MemNor::with_len(12 * SECTOR_BYTES as usize), false);
        let mut log = RawLog::<16>::new();
        assert!(!log.mount(&mut f, GEO, || {}));
        log.push(1, &frame(0));
        assert!(!log.service(&mut f, 0, true));
        let mut out = [0u8; CHUNK_LEN];
        assert_eq!(log.read_chunk(&mut f, 1, 0, &mut out), None);
    }

    #[test]
    fn stray_garbage_in_a_sector_header_is_skipped() {
        let mut f = chip();
        let mut log = RawLog::<16>::new();
        log.mount(&mut f, GEO, || {});
        let a = log_run(&mut log, &mut f, 4, 0, 20);
        // Torn erase left junk in a later sector's first bytes.
        let junk = (GEO.base + 5 * SECTOR_BYTES) as usize;
        f.nor.bytes[junk..junk + 16].copy_from_slice(&[0x12; 16]);
        let mut log = RawLog::<16>::new();
        log.mount(&mut f, GEO, || {});
        assert_eq!(fetch(&log, &mut f, 4).unwrap(), a);
        assert_eq!(log.used_kib(), 4);
    }
}
