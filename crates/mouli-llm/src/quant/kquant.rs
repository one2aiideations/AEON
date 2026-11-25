//! 256-element super-block formats: Q4_K, Q5_K, Q6_K.
//!
//! Layouts (little-endian):
//!
//! * `Q4_K` (144 bytes): `d: f16`, `dmin: f16`, `scales: [u8; 12]`, `qs: [u8; 128]`.
//!   Eight sub-blocks of 32 elements, each with a 6-bit scale `sc` and 6-bit minimum `m`
//!   (packed into `scales`, see [`scale_min`]); value = `d * sc * q - dmin * m`, `q` a 4-bit
//!   value. Sub-blocks `2c` and `2c + 1` share the 32 bytes `qs[32c..32c + 32]`: the first takes
//!   the low nibbles, the second the high nibbles.
//! * `Q5_K` (176 bytes): as Q4_K plus `qh: [u8; 32]` between `scales` and `qs`; bit `2c` of
//!   `qh[l]` is the fifth bit of the low-nibble element, bit `2c + 1` of the high-nibble one.
//! * `Q6_K` (210 bytes): `ql: [u8; 128]`, `qh: [u8; 64]`, `scales: [i8; 16]`, `d: f16`.
//!   Six-bit values with 16 sub-blocks of 16 elements; value = `d * scale * (q6 - 32)`.

use crate::quant::float::f16_to_f32;
use crate::quant::sz;

/// Unpack the 6-bit scale and minimum of sub-block `j` (0..8) from the 12 packed bytes.
///
/// Sub-blocks 0..4 keep their scale in `scales[j]` and minimum in `scales[j + 4]` (low 6 bits).
/// Sub-blocks 4..8 split each value: the low 4 bits live in `scales[j + 4]` (scale in the low
/// nibble, minimum in the high nibble) and the top 2 bits in the high bits of `scales[j - 4]`
/// (scale) and `scales[j]` (minimum).
pub(crate) fn scale_min(j: usize, s: &[u8]) -> (u8, u8) {
    if j < 4 {
        (s[j] & 63, s[j + 4] & 63)
    } else {
        (
            (s[j + 4] & 0xf) | ((s[j - 4] >> 6) << 4),
            (s[j + 4] >> 4) | ((s[j] >> 6) << 4),
        )
    }
}

fn h(bytes: &[u8], at: usize) -> f32 {
    f16_to_f32(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

pub(crate) fn q4_k(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes
        .chunks_exact(sz(144))
        .zip(out.chunks_exact_mut(sz(256)))
    {
        let (d, dmin) = (h(b, 0), h(b, 2));
        let scales = &b[4..16];
        let qs = &b[16..144];
        for c in 0..4 {
            let (sc1, m1) = scale_min(2 * c, scales);
            let (sc2, m2) = scale_min(2 * c + 1, scales);
            let (d1, mm1) = (d * f32::from(sc1), dmin * f32::from(m1));
            let (d2, mm2) = (d * f32::from(sc2), dmin * f32::from(m2));
            for l in 0..32 {
                let q = qs[c * 32 + l];
                y[c * 64 + l] = d1 * f32::from(q & 0xf) - mm1;
                y[c * 64 + 32 + l] = d2 * f32::from(q >> 4) - mm2;
            }
        }
    }
}

pub(crate) fn q5_k(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes
        .chunks_exact(sz(176))
        .zip(out.chunks_exact_mut(sz(256)))
    {
        let (d, dmin) = (h(b, 0), h(b, 2));
        let scales = &b[4..16];
        let qh = &b[16..48];
        let ql = &b[48..176];
        for c in 0..4 {
            let (sc1, m1) = scale_min(2 * c, scales);
            let (sc2, m2) = scale_min(2 * c + 1, scales);
            let (d1, mm1) = (d * f32::from(sc1), dmin * f32::from(m1));
            let (d2, mm2) = (d * f32::from(sc2), dmin * f32::from(m2));
            let (u1, u2) = (1u8 << (2 * c), 2u8 << (2 * c));
            for l in 0..32 {
                let q = ql[c * 32 + l];
                let hi1 = if qh[l] & u1 != 0 { 16.0 } else { 0.0 };
                let hi2 = if qh[l] & u2 != 0 { 16.0 } else { 0.0 };
                y[c * 64 + l] = d1 * (f32::from(q & 0xf) + hi1) - mm1;
                y[c * 64 + 32 + l] = d2 * (f32::from(q >> 4) + hi2) - mm2;
            }
        }
    }
}

pub(crate) fn q6_k(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes
        .chunks_exact(sz(210))
        .zip(out.chunks_exact_mut(sz(256)))
    {
        let d = h(b, 208);
        for half in 0..2 {
            let ql = &b[half * 64..half * 64 + 64];
            let qh = &b[128 + half * 32..128 + half * 32 + 32];
            let sc = &b[192 + half * 8..192 + half * 8 + 8];
            let y = &mut y[half * 128..half * 128 + 128];
            for l in 0..32 {
                let is = l / 16;
                let q1 = i32::from((ql[l] & 0xf) | ((qh[l] & 3) << 4)) - 32;
                let q2 = i32::from((ql[l + 32] & 0xf) | (((qh[l] >> 2) & 3) << 4)) - 32;
                let q3 = i32::from((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4)) - 32;
                let q4 = i32::from((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4)) - 32;
                let s = |k: usize| f32::from(sc[is + k] as i8);
                y[l] = d * s(0) * q1 as f32;
                y[l + 32] = d * s(2) * q2 as f32;
                y[l + 64] = d * s(4) * q3 as f32;
                y[l + 96] = d * s(6) * q4 as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: u16 = 0x3c00;
    const HALF: u16 = 0x3800;

    /// Pack eight 6-bit scales and minimums into the 12-byte K-quant layout.
    fn pack_scales(sc: [u8; 8], m: [u8; 8]) -> [u8; 12] {
        let mut b = [0u8; 12];
        for j in 0..4 {
            b[j] = sc[j] | ((sc[j + 4] >> 4) << 6);
            b[j + 4] = m[j] | ((m[j + 4] >> 4) << 6);
            b[8 + j] = (sc[j + 4] & 0xf) | ((m[j + 4] & 0xf) << 4);
        }
        b
    }

    #[test]
    fn scale_min_round_trips_all_six_bit_values() {
        for seed in 0..64u8 {
            let sc: [u8; 8] =
                std::array::from_fn(|i| (seed.wrapping_mul(5).wrapping_add(i as u8 * 9)) & 63);
            let m: [u8; 8] =
                std::array::from_fn(|i| (seed.wrapping_mul(11).wrapping_add(i as u8 * 7)) & 63);
            let packed = pack_scales(sc, m);
            for j in 0..8 {
                assert_eq!(scale_min(j, &packed), (sc[j], m[j]), "seed {seed} j {j}");
            }
        }
        // extremes
        let packed = pack_scales([63; 8], [0; 8]);
        for j in 0..8 {
            assert_eq!(scale_min(j, &packed), (63, 0));
        }
        let packed = pack_scales([0; 8], [63; 8]);
        for j in 0..8 {
            assert_eq!(scale_min(j, &packed), (0, 63));
        }
    }

    /// Logical Q4_K content: per sub-block scale and min, and 256 4-bit values.
    fn build_q4_k(d: u16, dmin: u16, sc: [u8; 8], m: [u8; 8], q: &[u8; 256]) -> Vec<u8> {
        let mut b = d.to_le_bytes().to_vec();
        b.extend_from_slice(&dmin.to_le_bytes());
        b.extend_from_slice(&pack_scales(sc, m));
        for c in 0..4 {
            for l in 0..32 {
                b.push(q[c * 64 + l] | (q[c * 64 + 32 + l] << 4));
            }
        }
        b
    }

    #[test]
    fn q4_k_matches_the_formula() {
        let sc = [1, 2, 3, 4, 33, 40, 50, 63];
        let m = [0, 1, 2, 3, 34, 41, 51, 62];
        let q: [u8; 256] = std::array::from_fn(|i| ((i * 7 + i / 32) % 16) as u8);
        let b = build_q4_k(ONE, HALF, sc, m, &q);
        assert_eq!(b.len(), 144);
        let mut y = [0.0f32; 256];
        q4_k(&b, &mut y);
        for i in 0..256 {
            let sub = i / 32;
            let want = 1.0 * f32::from(sc[sub]) * f32::from(q[i]) - 0.5 * f32::from(m[sub]);
            assert_eq!(y[i], want, "element {i}");
        }
    }

    #[test]
    fn q4_k_two_blocks() {
        let q: [u8; 256] = [3; 256];
        let mut b = build_q4_k(ONE, 0, [2; 8], [0; 8], &q);
        b.extend(build_q4_k(HALF, 0, [4; 8], [0; 8], &q));
        let mut y = [0.0f32; 512];
        q4_k(&b, &mut y);
        assert!(y[..256].iter().all(|v| *v == 6.0));
        assert!(y[256..].iter().all(|v| *v == 6.0)); // 0.5 * 4 * 3
    }

    fn build_q5_k(d: u16, dmin: u16, sc: [u8; 8], m: [u8; 8], q: &[u8; 256]) -> Vec<u8> {
        // q holds 5-bit values
        let mut b = d.to_le_bytes().to_vec();
        b.extend_from_slice(&dmin.to_le_bytes());
        b.extend_from_slice(&pack_scales(sc, m));
        let mut qh = [0u8; 32];
        let mut ql = Vec::with_capacity(128);
        for c in 0..4 {
            for l in 0..32 {
                let (a, bb) = (q[c * 64 + l], q[c * 64 + 32 + l]);
                ql.push((a & 0xf) | ((bb & 0xf) << 4));
                qh[l] |= ((a >> 4) & 1) << (2 * c);
                qh[l] |= ((bb >> 4) & 1) << (2 * c + 1);
            }
        }
        b.extend_from_slice(&qh);
        b.extend_from_slice(&ql);
        b
    }

    #[test]
    fn q5_k_matches_the_formula() {
        let sc = [5, 6, 7, 8, 9, 10, 11, 12];
        let m = [1, 1, 2, 2, 3, 3, 4, 4];
        let q: [u8; 256] = std::array::from_fn(|i| ((i * 13 + i / 7) % 32) as u8);
        let b = build_q5_k(ONE, HALF, sc, m, &q);
        assert_eq!(b.len(), 176);
        let mut y = [0.0f32; 256];
        q5_k(&b, &mut y);
        for i in 0..256 {
            let sub = i / 32;
            let want = f32::from(sc[sub]) * f32::from(q[i]) - 0.5 * f32::from(m[sub]);
            assert_eq!(y[i], want, "element {i}");
        }
    }

    fn build_q6_k(d: u16, scales: [i8; 16], q: &[u8; 256]) -> Vec<u8> {
        // q holds 6-bit values (0..64); stored value is q, decoded as q - 32
        let mut ql = [0u8; 128];
        let mut qh = [0u8; 64];
        for half in 0..2 {
            for l in 0..32 {
                let base = half * 128;
                let (v1, v2, v3, v4) = (
                    q[base + l],
                    q[base + l + 32],
                    q[base + l + 64],
                    q[base + l + 96],
                );
                ql[half * 64 + l] = (v1 & 0xf) | ((v3 & 0xf) << 4);
                ql[half * 64 + l + 32] = (v2 & 0xf) | ((v4 & 0xf) << 4);
                qh[half * 32 + l] = ((v1 >> 4) & 3)
                    | (((v2 >> 4) & 3) << 2)
                    | (((v3 >> 4) & 3) << 4)
                    | (((v4 >> 4) & 3) << 6);
            }
        }
        let mut b = ql.to_vec();
        b.extend_from_slice(&qh);
        b.extend(scales.iter().map(|s| *s as u8));
        b.extend_from_slice(&d.to_le_bytes());
        b
    }

    #[test]
    fn q6_k_matches_the_formula() {
        let scales: [i8; 16] = std::array::from_fn(|i| (i as i8 - 8) * 3);
        let q: [u8; 256] = std::array::from_fn(|i| ((i * 29 + 5) % 64) as u8);
        let b = build_q6_k(HALF, scales, &q);
        assert_eq!(b.len(), 210);
        let mut y = [0.0f32; 256];
        q6_k(&b, &mut y);
        for i in 0..256 {
            // sub-blocks of 16 elements, 16 scales in element order
            let want = 0.5 * f32::from(scales[i / 16]) * (f32::from(q[i]) - 32.0);
            assert_eq!(y[i], want, "element {i}");
        }
    }

    #[test]
    fn q6_k_extreme_values() {
        let b = build_q6_k(ONE, [127; 16], &[63; 256]);
        let mut y = [0.0f32; 256];
        q6_k(&b, &mut y);
        assert!(y.iter().all(|v| *v == 127.0 * 31.0));
        let b = build_q6_k(ONE, [-128; 16], &[0; 256]);
        q6_k(&b, &mut y);
        assert!(y.iter().all(|v| *v == -128.0 * -32.0));
    }

    #[test]
    fn zero_blocks_decode_to_zero_or_the_bias() {
        let mut y = [1.0f32; 256];
        q4_k(&[0u8; 144], &mut y);
        assert!(y.iter().all(|v| *v == 0.0));
        q5_k(&[0u8; 176], &mut y);
        assert!(y.iter().all(|v| *v == 0.0));
        q6_k(&[0u8; 210], &mut y); // d = 0
        assert!(y.iter().all(|v| *v == 0.0));
    }
}
