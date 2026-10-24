//! W25Q256JV SPI NOR flash: layout, address math and operation tracking.
//!
//! The chip is 32 MiB: 256 B program pages, 4 KiB erase sectors. Every
//! command uses the dedicated 4-byte-address opcodes (`0x13` read, `0x12`
//! page program, `0x21` sector erase), so the chip's 3/4-byte address mode
//! (which survives an MCU reset) never matters.
//!
//! | region | bytes | contents |
//! |---|---|---|
//! | `0x0000_0000..0x0180_0000` | 24 MiB | raw 25 Hz frame log ([`crate::rawlog`]), 6144 sectors |
//! | `0x0180_0000..0x0200_0000` | 8 MiB | binned window slots ([`crate::winlog`]), 256 × 32 KiB |
//!
//! [`Nor`] is the byte-level chip interface (the firmware implements it over
//! SPI2; tests use [`mem::MemNor`]). [`Flash`] wraps it and tracks the one
//! program or erase that may be in flight: [`Flash::poll`] reads the BUSY bit
//! only while an operation is outstanding, and declares the chip failed when
//! one outlasts its datasheet maximum, so a dead chip can never hang a caller.

/// JEDEC ID (manufacturer, memory type, capacity) of the W25Q256JV.
pub const JEDEC_W25Q256JV: [u8; 3] = [0xEF, 0x40, 0x19];

pub const FLASH_BYTES: u32 = 32 * 1024 * 1024;
pub const PAGE_BYTES: u32 = 256;
pub const SECTOR_BYTES: u32 = 4096;
pub const PAGES_PER_SECTOR: u32 = SECTOR_BYTES / PAGE_BYTES;

/// Raw frame ring: the lower 24 MiB.
pub const RAW_BASE: u32 = 0;
pub const RAW_BYTES: u32 = 24 * 1024 * 1024;
pub const RAW_SECTORS: u32 = RAW_BYTES / SECTOR_BYTES;

/// Window slots: the upper 8 MiB.
pub const WIN_BASE: u32 = RAW_BASE + RAW_BYTES;
pub const WIN_SLOT_BYTES: u32 = 32 * 1024;
pub const WIN_SLOTS: usize = ((FLASH_BYTES - WIN_BASE) / WIN_SLOT_BYTES) as usize;

/// W25Q256JV maxima (datasheet tPP 3 ms, tSE 400 ms) with margin. An
/// operation still busy after this is treated as a dead chip.
pub const PROGRAM_TIMEOUT_MS: u64 = 10;
pub const ERASE_TIMEOUT_MS: u64 = 600;

const _: () = assert!(RAW_BASE % SECTOR_BYTES == 0 && RAW_BYTES % SECTOR_BYTES == 0);
const _: () = assert!(WIN_BASE % WIN_SLOT_BYTES == 0);
const _: () = assert!(WIN_BASE + WIN_SLOTS as u32 * WIN_SLOT_BYTES == FLASH_BYTES);

/// Byte-level chip access. Every method returns `false` on a bus error.
/// `program` and `erase_sector` only start the operation (write enable +
/// opcode); completion is seen through `busy`.
pub trait Nor {
    /// Read `buf.len()` bytes at `addr`. Only valid while not busy.
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> bool;
    /// Start a page program. `data` must not cross a 256 B page boundary.
    fn program(&mut self, addr: u32, data: &[u8]) -> bool;
    /// Start a 4 KiB sector erase at a sector-aligned `addr`.
    fn erase_sector(&mut self, addr: u32) -> bool;
    /// Status register 1 BUSY bit; `None` on a bus error.
    fn busy(&mut self) -> Option<bool>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ready {
    /// No operation in flight: reads, programs and erases may start.
    Ready,
    /// A program or erase is still running.
    Busy,
    /// The chip is dead (bus error, timeout or verify failures).
    Failed,
}

/// A [`Nor`] plus the one outstanding operation and its deadline.
pub struct Flash<N: Nor> {
    pub nor: N,
    deadline_ms: Option<u64>,
    failed: bool,
}

impl<N: Nor> Flash<N> {
    /// Wrap a chip that has already passed its JEDEC check (`ok`), or a
    /// failed one (`!ok`), on which every call reports `Failed`.
    pub fn new(nor: N, ok: bool) -> Self {
        Self {
            nor,
            deadline_ms: None,
            failed: !ok,
        }
    }

    pub fn is_ok(&self) -> bool {
        !self.failed
    }

    /// Give up on the chip (e.g. after repeated verify failures).
    pub fn fail(&mut self) {
        self.failed = true;
    }

    /// Whether the chip can take a new command. Reads the status register
    /// only while an operation is outstanding.
    pub fn poll(&mut self, now_ms: u64) -> Ready {
        if self.failed {
            return Ready::Failed;
        }
        let Some(deadline) = self.deadline_ms else {
            return Ready::Ready;
        };
        match self.nor.busy() {
            Some(false) => {
                self.deadline_ms = None;
                Ready::Ready
            }
            Some(true) if now_ms <= deadline => Ready::Busy,
            _ => {
                self.failed = true;
                Ready::Failed
            }
        }
    }

    /// Read now (caller saw [`Ready::Ready`]).
    pub fn read(&mut self, addr: u32, buf: &mut [u8]) -> bool {
        if self.failed || self.deadline_ms.is_some() {
            return false;
        }
        if !self.nor.read(addr, buf) {
            self.failed = true;
            return false;
        }
        true
    }

    /// Start a page program (caller saw [`Ready::Ready`]).
    pub fn program(&mut self, now_ms: u64, addr: u32, data: &[u8]) -> bool {
        if self.failed
            || self.deadline_ms.is_some()
            || data.is_empty()
            || addr % PAGE_BYTES + data.len() as u32 > PAGE_BYTES
        {
            return false;
        }
        self.start(now_ms + PROGRAM_TIMEOUT_MS, |nor| nor.program(addr, data))
    }

    /// Start a sector erase (caller saw [`Ready::Ready`]).
    pub fn erase_sector(&mut self, now_ms: u64, addr: u32) -> bool {
        if self.failed || self.deadline_ms.is_some() || addr % SECTOR_BYTES != 0 {
            return false;
        }
        self.start(now_ms + ERASE_TIMEOUT_MS, |nor| nor.erase_sector(addr))
    }

    fn start(&mut self, deadline_ms: u64, op: impl FnOnce(&mut N) -> bool) -> bool {
        if !op(&mut self.nor) {
            self.failed = true;
            return false;
        }
        self.deadline_ms = Some(deadline_ms);
        true
    }
}

/// Address of `page` within a ring of `sectors` sectors starting at `base`,
/// taken modulo the ring.
pub const fn ring_page_addr(base: u32, sectors: u32, page: u32) -> u32 {
    base + (page % (sectors * PAGES_PER_SECTOR)) * PAGE_BYTES
}

/// Read `buf.len()` bytes starting at byte `offset` of a ring of `sectors`
/// sectors at `base`, wrapping at the ring end.
pub fn ring_read<N: Nor>(flash: &mut Flash<N>, base: u32, sectors: u32, offset: u32, buf: &mut [u8]) -> bool {
    let ring = sectors * SECTOR_BYTES;
    let offset = offset % ring;
    let first = ((ring - offset) as usize).min(buf.len());
    let (a, b) = buf.split_at_mut(first);
    flash.read(base + offset, a) && (b.is_empty() || flash.read(base, b))
}

/// RAM-backed chip for host tests: erased to 0xFF, programs AND bits in
/// (like NOR), and can be told to stay busy or to corrupt programs.
#[cfg(test)]
pub mod mem {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    pub struct MemNor {
        pub bytes: Vec<u8>,
        /// Polls each program/erase stays busy for.
        pub busy_polls: u32,
        pending: u32,
        /// Stay busy forever (dead chip).
        pub stuck: bool,
        /// XOR this into the first byte of every programmed page.
        pub corrupt_program: u8,
        pub programs: u32,
        pub erases: u32,
        pub reads: u32,
    }

    impl MemNor {
        pub fn new() -> Self {
            Self::with_len(FLASH_BYTES as usize)
        }

        pub fn with_len(len: usize) -> Self {
            Self {
                bytes: vec![0xFF; len],
                busy_polls: 0,
                pending: 0,
                stuck: false,
                corrupt_program: 0,
                programs: 0,
                erases: 0,
                reads: 0,
            }
        }
    }

    impl Default for MemNor {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Nor for MemNor {
        fn read(&mut self, addr: u32, buf: &mut [u8]) -> bool {
            assert_eq!(self.pending, 0, "read while busy");
            self.reads += 1;
            let a = addr as usize;
            buf.copy_from_slice(&self.bytes[a..a + buf.len()]);
            true
        }

        fn program(&mut self, addr: u32, data: &[u8]) -> bool {
            assert_eq!(self.pending, 0, "program while busy");
            assert!(addr % PAGE_BYTES + data.len() as u32 <= PAGE_BYTES);
            self.programs += 1;
            let a = addr as usize;
            for (i, &d) in data.iter().enumerate() {
                let d = if i == 0 { d ^ self.corrupt_program } else { d };
                self.bytes[a + i] &= d;
            }
            self.pending = self.busy_polls;
            true
        }

        fn erase_sector(&mut self, addr: u32) -> bool {
            assert_eq!(self.pending, 0, "erase while busy");
            assert_eq!(addr % SECTOR_BYTES, 0);
            self.erases += 1;
            let a = addr as usize;
            self.bytes[a..a + SECTOR_BYTES as usize].fill(0xFF);
            self.pending = self.busy_polls;
            true
        }

        fn busy(&mut self) -> Option<bool> {
            if self.stuck {
                return Some(true);
            }
            if self.pending > 0 {
                self.pending -= 1;
            }
            Some(self.pending > 0)
        }
    }

    /// Poll until ready (tests only).
    pub fn settle(flash: &mut Flash<MemNor>, now_ms: u64) -> Ready {
        for _ in 0..10_000 {
            match flash.poll(now_ms) {
                Ready::Busy => {}
                r => return r,
            }
        }
        Ready::Busy
    }
}

#[cfg(test)]
mod tests {
    use super::mem::MemNor;
    use super::*;

    #[test]
    fn layout_is_sector_aligned_and_fills_the_chip() {
        assert_eq!(RAW_SECTORS, 6144);
        assert_eq!(WIN_SLOTS, 256);
        assert_eq!(WIN_BASE, 0x0180_0000);
        assert_eq!(PAGES_PER_SECTOR, 16);
    }

    #[test]
    fn ring_math_wraps() {
        assert_eq!(ring_page_addr(0, 2, 0), 0);
        assert_eq!(ring_page_addr(0, 2, 31), 31 * 256);
        assert_eq!(ring_page_addr(0, 2, 32), 0);
        assert_eq!(ring_page_addr(0x1000, 2, 33), 0x1000 + 256);

        let mut f = Flash::new(MemNor::with_len(3 * SECTOR_BYTES as usize), true);
        for (i, b) in f.nor.bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut buf = [0u8; 8];
        // Ring of 2 sectors at 0x1000: offset 8188 straddles the end.
        assert!(ring_read(&mut f, 0x1000, 2, 8188, &mut buf));
        let base = 0x1000usize;
        let expect: [u8; 8] = core::array::from_fn(|i| {
            let off = (8188 + i) % 8192;
            (base + off) as u8
        });
        assert_eq!(buf, expect);
    }

    #[test]
    fn ops_are_tracked_and_a_stuck_chip_fails() {
        let mut f = Flash::new(MemNor::new(), true);
        f.nor.busy_polls = 3;
        assert_eq!(f.poll(0), Ready::Ready);
        assert!(f.program(0, 0x100, &[0x12, 0x34]));
        // A second op or a read while busy is refused, not issued.
        assert!(!f.program(0, 0x200, &[0]));
        let mut b = [0u8; 2];
        assert!(!f.read(0x100, &mut b));
        assert_eq!(f.poll(1), Ready::Busy);
        assert_eq!(f.poll(1), Ready::Busy);
        assert_eq!(f.poll(1), Ready::Ready);
        assert!(f.read(0x100, &mut b));
        assert_eq!(b, [0x12, 0x34]);

        // Program across a page boundary is refused.
        assert!(!f.program(1, 0x1FF, &[0, 0]));

        f.nor.stuck = true;
        assert!(f.erase_sector(10, 0x1000));
        assert_eq!(f.poll(10 + ERASE_TIMEOUT_MS), Ready::Busy);
        assert_eq!(f.poll(11 + ERASE_TIMEOUT_MS), Ready::Failed);
        assert!(!f.is_ok());
        assert_eq!(f.poll(0), Ready::Failed);
    }

    #[test]
    fn failed_chip_refuses_everything() {
        let mut f = Flash::new(MemNor::with_len(4096), false);
        assert_eq!(f.poll(0), Ready::Failed);
        let mut b = [0u8; 1];
        assert!(!f.read(0, &mut b));
        assert!(!f.program(0, 0, &[0]));
        assert!(!f.erase_sector(0, 0));
    }
}
