//! CAT24C32 EEPROM record layout (4 KiB, 32-byte pages).
//!
//! | address | record | length |
//! |---|---|---|
//! | `0x0000` | schedule | 28 B |
//! | `0x0020` | RTC trim | 12 B |
//! | `0x0040` | boot count | 12 B |
//! | `0x0FC0` | heater lockout (firmware, unchanged) | 16 B |
//! | `0x0FF0` | `board_test` scratch | 16 B |
//!
//! Each record is `magic: u32`, payload, `crc32: u32` (protocol CRC over
//! magic and payload), all little-endian, inside one page so a single page
//! write stores it. A blank (0xFF) or corrupt record decodes to `None`.

use sap_flux_protocol::crc::crc32;
use sap_flux_protocol::{Schedule, SCHEDULE_LEN};

pub const PAGE_LEN: u16 = 32;
pub const SCHEDULE_ADDR: u16 = 0x0000;
pub const TRIM_ADDR: u16 = 0x0020;
pub const BOOT_COUNT_ADDR: u16 = 0x0040;
/// Owned by the heater lockout; records here must never reach it.
pub const LOCKOUT_ADDR: u16 = 0x0fc0;

pub const SCHEDULE_MAGIC: u32 = 0x4353_4653; // "SFSC"
pub const TRIM_MAGIC: u32 = 0x5254_4653; // "SFTR"
pub const BOOT_COUNT_MAGIC: u32 = 0x4342_4653; // "SFBC"

pub const SCHEDULE_RECORD_LEN: usize = 4 + SCHEDULE_LEN + 4;
pub const WORD_RECORD_LEN: usize = 12;

fn seal<const N: usize>(magic: u32, payload: &[u8]) -> [u8; N] {
    let mut r = [0u8; N];
    r[..4].copy_from_slice(&magic.to_le_bytes());
    r[4..4 + payload.len()].copy_from_slice(payload);
    let crc = crc32(&r[..N - 4]);
    r[N - 4..].copy_from_slice(&crc.to_le_bytes());
    r
}

fn open<const N: usize>(magic: u32, record: &[u8; N]) -> Option<&[u8]> {
    let stored = u32::from_le_bytes(record[N - 4..].try_into().ok()?);
    if u32::from_le_bytes(record[..4].try_into().ok()?) != magic || crc32(&record[..N - 4]) != stored
    {
        return None;
    }
    Some(&record[4..N - 4])
}

pub fn encode_schedule(schedule: &Schedule) -> [u8; SCHEDULE_RECORD_LEN] {
    seal(SCHEDULE_MAGIC, &schedule.to_bytes())
}

/// The stored schedule, if intact. Callers still validate it.
pub fn decode_schedule(record: &[u8; SCHEDULE_RECORD_LEN]) -> Option<Schedule> {
    let p = open(SCHEDULE_MAGIC, record)?;
    Some(Schedule::from_bytes(p.try_into().ok()?))
}

fn encode_word(magic: u32, value: u32) -> [u8; WORD_RECORD_LEN] {
    let mut p = [0u8; 4];
    p.copy_from_slice(&value.to_le_bytes());
    seal(magic, &p)
}

fn decode_word(magic: u32, record: &[u8; WORD_RECORD_LEN]) -> Option<u32> {
    let p = open(magic, record)?;
    Some(u32::from_le_bytes(p[..4].try_into().ok()?))
}

pub fn encode_trim(ppb: i32) -> [u8; WORD_RECORD_LEN] {
    encode_word(TRIM_MAGIC, ppb as u32)
}

pub fn decode_trim(record: &[u8; WORD_RECORD_LEN]) -> Option<i32> {
    decode_word(TRIM_MAGIC, record).map(|v| v as i32)
}

pub fn encode_boot_count(count: u32) -> [u8; WORD_RECORD_LEN] {
    encode_word(BOOT_COUNT_MAGIC, count)
}

pub fn decode_boot_count(record: &[u8; WORD_RECORD_LEN]) -> Option<u32> {
    decode_word(BOOT_COUNT_MAGIC, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_fit_their_pages_and_avoid_the_lockout() {
        for (addr, len) in [
            (SCHEDULE_ADDR, SCHEDULE_RECORD_LEN),
            (TRIM_ADDR, WORD_RECORD_LEN),
            (BOOT_COUNT_ADDR, WORD_RECORD_LEN),
        ] {
            assert_eq!(addr % PAGE_LEN, 0);
            assert!(len <= PAGE_LEN as usize);
            assert!(addr as usize + len <= LOCKOUT_ADDR as usize);
        }
    }

    #[test]
    fn round_trip_and_reject_blank_or_corrupt() {
        let s = Schedule {
            enabled: 1,
            interval_s: 900,
            ..Schedule::FACTORY_DEFAULT
        };
        let mut r = encode_schedule(&s);
        assert_eq!(decode_schedule(&r), Some(s));
        r[6] ^= 1;
        assert_eq!(decode_schedule(&r), None);
        assert_eq!(decode_schedule(&[0xff; SCHEDULE_RECORD_LEN]), None);
        assert_eq!(decode_schedule(&[0; SCHEDULE_RECORD_LEN]), None);

        assert_eq!(decode_trim(&encode_trim(-12_345)), Some(-12_345));
        assert_eq!(decode_boot_count(&encode_boot_count(77)), Some(77));
        // Magic separates record kinds.
        assert_eq!(decode_trim(&encode_boot_count(77)), None);
        assert_eq!(decode_boot_count(&[0xff; WORD_RECORD_LEN]), None);
    }
}
