//! Raw 25 Hz frame log download (`GetRaw` / `RawChunk`), block
//! verification and CSV export.

use crate::link::{Link, LinkError, Transport};
use crate::window::CHANNEL_COLUMNS;
use sap_flux_protocol::raw::{decode_block, RawFrame, BLOCK_LEN};
use sap_flux_protocol::WINDOW_CHUNK_DATA_LEN;
use std::io::{self, Write};

/// A downloaded run: its raw blocks, CRC-checked and in order.
#[derive(Debug, Clone)]
pub struct RawRun {
    /// Run id.
    pub run_id: u32,
    /// The 256-byte blocks as stored on the board.
    pub blocks: Vec<[u8; BLOCK_LEN]>,
}

/// Why a raw download failed.
#[derive(Debug)]
pub enum RawDownloadError {
    /// The link failed (NOT_FOUND on chunk 0: the board has no such run).
    Link(LinkError),
    /// The bytes are not a valid block stream for this run.
    Verify(String),
}

impl std::fmt::Display for RawDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RawDownloadError::Link(e) => write!(f, "{e}"),
            RawDownloadError::Verify(s) => write!(f, "verification failed: {s}"),
        }
    }
}

impl std::error::Error for RawDownloadError {}

impl From<LinkError> for RawDownloadError {
    fn from(e: LinkError) -> Self {
        RawDownloadError::Link(e)
    }
}

/// Retries of one chunk the board answers BUSY (a flash erase is at most
/// 400 ms; 50 ms apart this allows 10 s).
const BUSY_RETRIES: u32 = 200;
const BUSY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Fetch every chunk of `run_id` until a short chunk or NOT_FOUND, then
/// verify the blocks. A BUSY chunk is retried, never taken as the end.
/// `progress` gets the bytes received so far.
pub fn download<T: Transport>(
    link: &mut Link<T>,
    run_id: u32,
    mut progress: impl FnMut(usize),
) -> Result<RawRun, RawDownloadError> {
    let mut bytes = Vec::new();
    let mut busy = 0;
    let mut chunk = 0u32;
    loop {
        match link.raw_chunk(run_id, chunk) {
            Ok(c) => {
                bytes.extend_from_slice(c.bytes());
                progress(bytes.len());
                if c.bytes().len() < WINDOW_CHUNK_DATA_LEN {
                    break;
                }
                chunk += 1;
                busy = 0;
            }
            Err(LinkError::Busy) if busy < BUSY_RETRIES => {
                busy += 1;
                std::thread::sleep(BUSY_DELAY);
            }
            Err(LinkError::NotFound) if chunk > 0 => break,
            Err(e) => return Err(e.into()),
        }
    }
    let run = assemble(run_id, &bytes)?;
    Ok(run)
}

/// Split a block stream into blocks and check each one's CRC, run id and
/// index.
pub fn assemble(run_id: u32, bytes: &[u8]) -> Result<RawRun, RawDownloadError> {
    if bytes.is_empty() || bytes.len() % BLOCK_LEN != 0 {
        return Err(RawDownloadError::Verify(format!(
            "{} bytes is not a whole number of {BLOCK_LEN}-byte blocks",
            bytes.len()
        )));
    }
    let mut blocks = Vec::with_capacity(bytes.len() / BLOCK_LEN);
    for (k, b) in bytes.chunks_exact(BLOCK_LEN).enumerate() {
        let block: [u8; BLOCK_LEN] = b.try_into().expect("exact chunk");
        let header = decode_block(&block, |_| {})
            .map_err(|e| RawDownloadError::Verify(format!("block {k}: {e:?}")))?;
        if header.prefix.run_id != run_id || header.prefix.block_in_run != k as u32 {
            return Err(RawDownloadError::Verify(format!(
                "block {k}: run {} block_in_run {}",
                header.prefix.run_id, header.prefix.block_in_run
            )));
        }
        blocks.push(block);
    }
    Ok(RawRun { run_id, blocks })
}

impl RawRun {
    /// Every frame, in order (blocks already verified).
    pub fn frames(&self) -> Vec<RawFrame> {
        let mut out = Vec::new();
        for b in &self.blocks {
            let _ = decode_block(b, |f| out.push(f));
        }
        out
    }

    /// The block stream exactly as stored on the board.
    pub fn bytes(&self) -> Vec<u8> {
        self.blocks.concat()
    }

    /// One row per frame: monotonic and RTC time, flags, state and the 14
    /// raw values.
    pub fn write_csv(&self, mut w: impl Write) -> io::Result<()> {
        write!(w, "run_id,monotonic_us,rtc_unix_ms,flags,state")?;
        for c in CHANNEL_COLUMNS {
            write!(w, ",{c}")?;
        }
        writeln!(w, ",v24_raw")?;
        for f in self.frames() {
            write!(
                w,
                "{},{},{},{},{}",
                self.run_id, f.monotonic_us, f.rtc_unix_ms, f.flags, f.state
            )?;
            for v in f.values {
                if v == sap_flux_protocol::raw::INVALID {
                    write!(w, ",")?;
                } else {
                    write!(w, ",{v}")?;
                }
            }
            writeln!(w)?;
        }
        Ok(())
    }
}
