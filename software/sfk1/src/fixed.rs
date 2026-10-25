//! Integer log-time B-spline basis for the encoder (no FPU, no divide).
//!
//! Post-onset sample `n` sits at `s = log2((n + T0) / T0) * KF` knot
//! intervals (`T0` = the profile's `T0_FRAMES`, `KF` its `KF_Q28`): the
//! reference's `u = ln(t + t0)` scaled so the `K = knots + 1` intervals have
//! unit width. `s` is computed in Q24 from an integer `log2`, and the
//! four cubic pieces that are non-zero there are evaluated by Horner from
//! exact integer coefficients (`x 12`). Values are returned as
//! `B * 12 * 2^16`; the factor 12 is folded into the encoder matrix.

use crate::tables::{BASIS_C12, LOG2_POLY};

/// `log2(v)` in Q30 for `1 <= v < 2^31`; error below 6e-8.
pub(crate) const fn log2_q30(v: u32) -> i64 {
    let e = match v.checked_ilog2() {
        Some(e) => e,
        None => 0, // v = 0 never happens (v > T0_FRAMES >= 1)
    };
    // mantissa in [1, 2) as Q30; v < 2^17 here (n < 2^16, T0 <= 2^16)
    let f = ((v as i64) << (30 - e)) - (1 << 30);
    let mut i = LOG2_POLY.len() - 1;
    let mut acc = LOG2_POLY[i];
    while i > 0 {
        i -= 1;
        // |acc * f| < 2^60 over the whole domain (checked by the generator)
        acc = ((acc * f) >> 30) + LOG2_POLY[i];
    }
    ((e as i64) << 30) + acc
}

/// Index of the first non-zero basis function and the four values
/// `B_{i+m}(t_n) * 12 * 2^16` for post-onset sample `n >= 1`, on
/// `intervals >= 4` knot intervals. `log2_t0 = log2_q30(T0_FRAMES)`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub(crate) fn basis(n: u16, t0_frames: u32, log2_t0: i64, kf_q28: i64, intervals: usize) -> (usize, [u32; 4]) {
    // n + T0 < 2^17, so log2 < 2^35; the product is exact in i128 (East30:
    // < 2^62.1, so the reference's i64 arithmetic gives the same value)
    let l = i128::from(log2_q30(u32::from(n) + t0_frames) - log2_t0);
    let s = ((l * i128::from(kf_q28) + (1 << 33)) >> 34).clamp(0, i128::from(i64::MAX)) as i64;
    let mut i = (s >> 24) as usize;
    let mut x = s & 0xFF_FFFF;
    if i >= intervals {
        // the last sample sits on the end knot: x = 1 in the last interval
        // (up to log2 error; clamped so a mis-set KF cannot overflow Horner)
        i = intervals - 1;
        x = (s - ((i as i64) << 24)).min(1 << 25);
    }
    let kind = match i {
        0 => 0,
        1 => 1,
        _ if i == intervals - 1 => 4,
        _ if i == intervals - 2 => 3,
        _ => 2,
    };
    let mut out = [0u32; 4];
    for (o, c) in out.iter_mut().zip(BASIS_C12[kind].iter()) {
        // |h| < 2^32 and x <= 2^24, so h * x < 2^56
        let mut h = i64::from(c[3]) << 24;
        h = ((h * x) >> 24) + (i64::from(c[2]) << 24);
        h = ((h * x) >> 24) + (i64::from(c[1]) << 24);
        h = ((h * x) >> 24) + (i64::from(c[0]) << 24);
        *o = ((h + 128) >> 8).max(0) as u32;
    }
    (i, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log2_accuracy() {
        let mut worst = 0.0f64;
        for v in 50..=4575u32 {
            #[allow(clippy::cast_precision_loss)]
            let got = log2_q30(v) as f64 / f64::from(1u32 << 30);
            worst = worst.max((got - f64::from(v).log2()).abs());
        }
        assert!(worst < 6e-8, "{worst}");
    }

    #[test]
    fn partition_of_unity() {
        let l50 = log2_q30(50);
        for &(kf, k) in &[(crate::tables::KF_K12, 13), (crate::tables::KF_K16, 17)] {
            for n in 1..4526u16 {
                let (_, b) = basis(n, 50, l50, kf, k);
                let sum: u32 = b.iter().sum();
                // sum of B = 1, i.e. 12 * 2^16, up to rounding of each term
                assert!(sum.abs_diff(12 << 16) <= 3, "n {n}: {sum}");
            }
            // the last sample sits on the end knot (x = 1 up to log2 error)
            let (i, b) = basis(4525, 50, l50, kf, k);
            assert_eq!((i, &b[..3]), (k - 1, &[0, 0, 0][..]));
            assert!(b[3].abs_diff(12 << 16) <= 1, "{}", b[3]);
        }
    }
}
