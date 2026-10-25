//! MSB-first bit IO, zigzag mapping and Rice codes with the SFK1 escape.

/// The Rice escape: unary quotients of `q` or more are sent as `q` ones
/// followed by the zigzag value in `bits` raw bits (East30: 24 and 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Escape {
    pub(crate) q: u32,
    pub(crate) bits: u32,
}

impl Escape {
    /// The profile's escape (`q <= 31`, `bits <= 24`, checked by the profile).
    pub(crate) const fn of<P: crate::Profile>() -> Self {
        Self { q: P::ESCAPE_Q, bits: P::ESCAPE_BITS }
    }
}

#[cfg(test)]
const EAST30: Escape = Escape { q: 24, bits: 20 };

/// Zigzag map: 0, -1, 1, -2, ... to 0, 1, 2, 3, ...
#[inline]
#[allow(clippy::cast_sign_loss)]
pub(crate) const fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

/// Inverse of [`zigzag`].
#[cfg(any(test, feature = "decode"))]
#[inline]
#[allow(clippy::cast_possible_wrap)]
pub(crate) const fn unzigzag(u: u32) -> i32 {
    ((u >> 1) as i32) ^ -((u & 1) as i32)
}

/// Bits taken by `Rice_k(zigzag(v))`, escape included (`k <= 31`).
#[inline]
pub(crate) const fn rice_len(v: i32, k: u32, esc: Escape) -> u32 {
    let q = zigzag(v) >> k;
    if q >= esc.q {
        esc.q + esc.bits
    } else {
        q + 1 + k
    }
}

/// Writes MSB first into a caller buffer. Bits past the end of the buffer
/// are counted but dropped, so a too-small buffer reports the size it needed.
pub(crate) struct BitWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> BitWriter<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bits written so far (including any that did not fit).
    #[cfg(test)]
    pub(crate) fn bit_len(&self) -> usize {
        self.pos
    }

    /// The low `n` bits of `value`, most significant first (`n <= 32`).
    /// Out of line: it runs once per field, and inlining it into every call
    /// site costs kilobytes at `opt-level = 3` on Cortex-M0.
    #[inline(never)]
    pub(crate) fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            let mask = 0x80u8 >> (self.pos & 7);
            if let Some(byte) = self.buf.get_mut(self.pos >> 3) {
                if (value >> i) & 1 == 1 {
                    *byte |= mask;
                } else {
                    *byte &= !mask;
                }
            }
            self.pos += 1;
        }
    }

    /// `Rice_k(zigzag(v))`: unary quotient (ones, then a zero) and `k` low
    /// bits, or `esc.q` ones and the `esc.bits`-bit zigzag value when the
    /// quotient is `esc.q` or more (`k <= 31`).
    pub(crate) fn rice(&mut self, v: i32, k: u32, esc: Escape) {
        let u = zigzag(v);
        let q = u >> k;
        if q >= esc.q {
            self.put((1 << esc.q) - 1, esc.q);
            self.put(u, esc.bits);
        } else {
            // q < esc.q <= 31, so q ones and the terminating zero fit one put
            self.put(((1 << q) - 1) << 1, q + 1);
            self.put(u & ((1 << k) - 1), k);
        }
    }

    /// Zero bits up to the next byte boundary; returns the length in bytes.
    pub(crate) fn pad(&mut self) -> usize {
        let pad = (8 - (self.pos & 7)) & 7;
        #[allow(clippy::cast_possible_truncation)]
        self.put(0, pad as u32);
        self.pos >> 3
    }
}

/// The input ended inside a field.
#[cfg(any(test, feature = "decode"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Truncated;

/// Reads MSB first.
#[cfg(any(test, feature = "decode"))]
#[derive(Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

#[cfg(any(test, feature = "decode"))]
impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Bits consumed so far.
    pub(crate) fn bit_pos(&self) -> usize {
        self.pos
    }

    /// `n <= 32` bits as an unsigned value.
    pub(crate) fn get(&mut self, n: u32) -> Result<u32, Truncated> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos >> 3).ok_or(Truncated)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos & 7))) & 1);
            self.pos += 1;
        }
        Ok(v)
    }

    /// Inverse of [`BitWriter::rice`] (`k <= 31`).
    pub(crate) fn rice(&mut self, k: u32, esc: Escape) -> Result<i32, Truncated> {
        let mut q = 0;
        while q < esc.q && self.get(1)? == 1 {
            q += 1;
        }
        let u = if q >= esc.q { self.get(esc.bits)? } else { (q << k) | self.get(k)? };
        Ok(unzigzag(u))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::vec::Vec;

    #[test]
    fn zigzag_small() {
        let pairs = [(0, 0), (-1, 1), (1, 2), (-2, 3), (2, 4)];
        for (v, u) in pairs {
            assert_eq!(zigzag(v), u);
            assert_eq!(unzigzag(u), v);
        }
        assert_eq!(zigzag(i32::MIN), u32::MAX);
        assert_eq!(zigzag(i32::MAX), u32::MAX - 1);
    }

    #[test]
    fn rice_escape_layout() {
        // u = 48 with k = 1 has q = 24: escape, 24 ones then 20 raw bits
        let mut buf = [0u8; 8];
        let mut w = BitWriter::new(&mut buf);
        w.rice(24, 1, EAST30);
        assert_eq!(w.bit_len(), 44);
        assert_eq!(rice_len(24, 1, EAST30), 44);
        assert_eq!(&buf[..6], &[0xff, 0xff, 0xff, 0x00, 0x03, 0x00]);
    }

    proptest! {
        #[test]
        fn zigzag_round_trip(v in any::<i32>()) {
            prop_assert_eq!(unzigzag(zigzag(v)), v);
        }

        #[test]
        fn bits_round_trip(fields in proptest::collection::vec((any::<u32>(), 0u32..=32), 0..64)) {
            let mut buf = [0u8; 300];
            let mut w = BitWriter::new(&mut buf);
            for &(v, n) in &fields {
                w.put(v, n);
            }
            let total = w.bit_len();
            let len = w.pad();
            prop_assert_eq!(len, total.div_ceil(8));
            let mut r = BitReader::new(&buf[..len]);
            for &(v, n) in &fields {
                let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
                prop_assert_eq!(r.get(n).unwrap(), v & mask);
            }
            prop_assert_eq!(r.bit_pos(), total);
        }

        #[test]
        fn rice_round_trip(vals in proptest::collection::vec((-(1i32 << 19)..(1i32 << 19), 0u32..16), 0..64)) {
            let mut buf = [0u8; 400];
            let mut w = BitWriter::new(&mut buf);
            let mut bits = 0;
            for &(v, k) in &vals {
                w.rice(v, k, EAST30);
                bits += rice_len(v, k, EAST30) as usize;
            }
            prop_assert_eq!(w.bit_len(), bits);
            let len = w.pad();
            let mut r = BitReader::new(&buf[..len]);
            let got: Vec<i32> = vals.iter().map(|&(_, k)| r.rice(k, EAST30).unwrap()).collect();
            let want: Vec<i32> = vals.iter().map(|&(v, _)| v).collect();
            prop_assert_eq!(got, want);
        }

        #[test]
        fn writer_counts_past_the_end(vals in proptest::collection::vec(any::<u32>(), 1..40), cap in 0usize..16) {
            let mut big = [0u8; 160];
            let mut small = [0u8; 16];
            let mut a = BitWriter::new(&mut big);
            let mut b = BitWriter::new(&mut small[..cap]);
            for &v in &vals {
                a.put(v, 32);
                b.put(v, 32);
            }
            prop_assert_eq!(a.bit_len(), b.bit_len());
            prop_assert_eq!(&big[..cap], &small[..cap]);
        }

        #[test]
        fn reader_never_panics(data in proptest::collection::vec(any::<u8>(), 0..32), ks in proptest::collection::vec(0u32..16, 0..64)) {
            let mut r = BitReader::new(&data);
            for k in ks {
                if r.rice(k, EAST30).is_err() {
                    break;
                }
            }
        }
    }
}
