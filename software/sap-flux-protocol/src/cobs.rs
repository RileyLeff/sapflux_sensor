//! Consistent Overhead Byte Stuffing (COBS).
//!
//! Encodes arbitrary bytes into a form that contains no `0x00`, so a single
//! `0x00` can be used as an unambiguous frame delimiter on the wire. The
//! delimiter itself is *not* produced by [`encode`] or consumed by
//! [`decode`]; framing is handled by the caller (see
//! [`Decoder`](crate::Decoder)).

/// Errors from [`decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CobsError {
    /// Input violated COBS structure (embedded zero byte or a group code
    /// pointing past the end of the input).
    Malformed,
    /// Decoded output would not fit in the destination buffer.
    OutputOverflow,
}

/// COBS-encode `src` into `dst`, returning the encoded length.
///
/// Returns `None` if `dst` is too small. Worst-case output size is
/// `src.len() + src.len().div_ceil(254) + 1` bytes (`+1` covers the empty
/// input case).
pub fn encode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut code_idx = 0usize;
    let mut out = 1usize;
    let mut code = 1u8;
    if dst.is_empty() {
        return None;
    }
    for &b in src {
        if b == 0 {
            dst[code_idx] = code;
            code_idx = out;
            out += 1;
            if out > dst.len() {
                return None;
            }
            code = 1;
        } else {
            if out >= dst.len() {
                return None;
            }
            dst[out] = b;
            out += 1;
            code += 1;
            if code == 0xFF {
                dst[code_idx] = code;
                code_idx = out;
                out += 1;
                if out > dst.len() {
                    return None;
                }
                code = 1;
            }
        }
    }
    dst[code_idx] = code;
    Some(out)
}

/// COBS-decode `src` (one frame, delimiter already stripped) into `dst`,
/// returning the decoded length.
pub fn decode(src: &[u8], dst: &mut [u8]) -> Result<usize, CobsError> {
    let mut i = 0usize;
    let mut out = 0usize;
    while i < src.len() {
        let code = src[i];
        if code == 0 {
            return Err(CobsError::Malformed);
        }
        i += 1;
        let run = (code - 1) as usize;
        if i + run > src.len() {
            return Err(CobsError::Malformed);
        }
        let mut k = 0;
        while k < run {
            let b = src[i];
            if b == 0 {
                return Err(CobsError::Malformed);
            }
            if out >= dst.len() {
                return Err(CobsError::OutputOverflow);
            }
            dst[out] = b;
            out += 1;
            i += 1;
            k += 1;
        }
        if code != 0xFF && i < src.len() {
            if out >= dst.len() {
                return Err(CobsError::OutputOverflow);
            }
            dst[out] = 0;
            out += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) {
        let mut enc = vec![0u8; data.len() + data.len() / 254 + 2];
        let n = encode(data, &mut enc).expect("encode fits");
        assert!(!enc[..n].contains(&0), "encoded output must be zero-free");
        let mut dec = vec![0u8; data.len() + 1];
        let m = decode(&enc[..n], &mut dec).expect("decode ok");
        assert_eq!(&dec[..m], data);
    }

    #[test]
    fn known_vectors() {
        // Vectors from the COBS Wikipedia article.
        let cases: &[(&[u8], &[u8])] = &[
            (&[], &[0x01]),
            (&[0x00], &[0x01, 0x01]),
            (&[0x00, 0x00], &[0x01, 0x01, 0x01]),
            (&[0x00, 0x11, 0x00], &[0x01, 0x02, 0x11, 0x01]),
            (&[0x11, 0x22, 0x00, 0x33], &[0x03, 0x11, 0x22, 0x02, 0x33]),
            (&[0x11, 0x22, 0x33, 0x44], &[0x05, 0x11, 0x22, 0x33, 0x44]),
            (&[0x11, 0x00, 0x00, 0x00], &[0x02, 0x11, 0x01, 0x01, 0x01]),
        ];
        for (raw, wire) in cases {
            let mut enc = [0u8; 16];
            let n = encode(raw, &mut enc).unwrap();
            assert_eq!(&enc[..n], *wire, "encode {raw:02x?}");
            let mut dec = [0u8; 16];
            let m = decode(wire, &mut dec).unwrap();
            assert_eq!(&dec[..m], *raw, "decode {wire:02x?}");
        }
    }

    #[test]
    fn long_runs_cross_254_boundary() {
        for len in [253usize, 254, 255, 300] {
            let data: Vec<u8> = (0..len).map(|i| (i % 255) as u8 + 1).collect();
            roundtrip(&data);
        }
        // All zeros.
        roundtrip(&[0u8; 64]);
        // Mixed.
        let data: Vec<u8> = (0..300).map(|i| (i % 7) as u8).collect();
        roundtrip(&data);
    }

    #[test]
    fn decode_rejects_embedded_zero() {
        let mut dst = [0u8; 16];
        assert_eq!(
            decode(&[0x03, 0x11, 0x00], &mut dst),
            Err(CobsError::Malformed)
        );
        assert_eq!(decode(&[0x00], &mut dst), Err(CobsError::Malformed));
    }

    #[test]
    fn decode_rejects_short_group() {
        let mut dst = [0u8; 16];
        assert_eq!(decode(&[0x05, 0x11], &mut dst), Err(CobsError::Malformed));
    }

    #[test]
    fn decode_reports_overflow() {
        let mut dst = [0u8; 2];
        assert_eq!(
            decode(&[0x05, 0x11, 0x22, 0x33, 0x44], &mut dst),
            Err(CobsError::OutputOverflow)
        );
    }

    #[test]
    fn encode_reports_too_small() {
        let mut dst = [0u8; 3];
        assert_eq!(encode(&[0x11, 0x22, 0x33, 0x44], &mut dst), None);
    }
}
