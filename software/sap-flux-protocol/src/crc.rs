//! Standard IEEE CRC-32 (zlib/Ethernet variant).
//!
//! Reflected, polynomial `0xEDB88320`, initial value `0xFFFFFFFF`, final XOR
//! `0xFFFFFFFF`. `crc32(b"123456789") == 0xCBF43926`.

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static TABLE: [u32; 256] = build_table();

/// Incremental CRC-32 state.
#[derive(Clone, Copy, Debug)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    /// Start a new CRC computation.
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Feed bytes into the CRC.
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.state;
        for &b in data {
            c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        self.state = c;
    }

    /// Finish and return the CRC value.
    pub const fn finalize(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        // The canonical CRC-32/ISO-HDLC check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn incremental_matches_oneshot() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut c = Crc32::new();
        c.update(&data[..10]);
        c.update(&data[10..]);
        assert_eq!(c.finalize(), crc32(data));
    }
}
