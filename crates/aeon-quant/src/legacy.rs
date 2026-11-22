//! 32-element block formats: Q4_0, Q4_1, Q5_0, Q5_1, Q8_0.
//!
//! Layouts (little-endian):
//!
//! * `Q4_0` (18 bytes): `d: f16`, `qs: [u8; 16]`. Element `j` is the low nibble of `qs[j]`,
//!   element `j + 16` the high nibble; value = `(nibble - 8) * d`.
//! * `Q4_1` (20 bytes): `d: f16`, `m: f16`, `qs: [u8; 16]`; value = `nibble * d + m`.
//! * `Q5_0` (22 bytes): `d: f16`, `qh: u32`, `qs: [u8; 16]`; the fifth bit of element `j` is bit
//!   `j` of `qh`, of element `j + 16` bit `j + 16`; value = `(q5 - 16) * d`.
//! * `Q5_1` (24 bytes): `d: f16`, `m: f16`, `qh: u32`, `qs: [u8; 16]`; value = `q5 * d + m`.
//! * `Q8_0` (34 bytes): `d: f16`, `qs: [i8; 32]`; value = `q * d`.

use crate::float::f16_to_f32;
use crate::sz;

fn h(bytes: &[u8], at: usize) -> f32 {
    f16_to_f32(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

pub(crate) fn q4_0(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes.chunks_exact(sz(18)).zip(out.chunks_exact_mut(sz(32))) {
        let d = h(b, 0);
        for j in 0..16 {
            let q = b[2 + j];
            y[j] = f32::from(i32::from(q & 0xf) as i16 - 8) * d;
            y[j + 16] = f32::from(i32::from(q >> 4) as i16 - 8) * d;
        }
    }
}

pub(crate) fn q4_1(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes.chunks_exact(sz(20)).zip(out.chunks_exact_mut(sz(32))) {
        let (d, m) = (h(b, 0), h(b, 2));
        for j in 0..16 {
            let q = b[4 + j];
            y[j] = f32::from(q & 0xf) * d + m;
            y[j + 16] = f32::from(q >> 4) * d + m;
        }
    }
}

pub(crate) fn q5_0(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes.chunks_exact(sz(22)).zip(out.chunks_exact_mut(sz(32))) {
        let d = h(b, 0);
        let qh = u32::from_le_bytes([b[2], b[3], b[4], b[5]]);
        for j in 0..16 {
            let q = b[6 + j];
            let lo = (q & 0xf) | ((((qh >> j) & 1) as u8) << 4);
            let hi = (q >> 4) | ((((qh >> (j + 16)) & 1) as u8) << 4);
            y[j] = (f32::from(lo) - 16.0) * d;
            y[j + 16] = (f32::from(hi) - 16.0) * d;
        }
    }
}

pub(crate) fn q5_1(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes.chunks_exact(sz(24)).zip(out.chunks_exact_mut(sz(32))) {
        let (d, m) = (h(b, 0), h(b, 2));
        let qh = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        for j in 0..16 {
            let q = b[8 + j];
            let lo = (q & 0xf) | ((((qh >> j) & 1) as u8) << 4);
            let hi = (q >> 4) | ((((qh >> (j + 16)) & 1) as u8) << 4);
            y[j] = f32::from(lo) * d + m;
            y[j + 16] = f32::from(hi) * d + m;
        }
    }
}

pub(crate) fn q8_0(bytes: &[u8], out: &mut [f32]) {
    for (b, y) in bytes.chunks_exact(sz(34)).zip(out.chunks_exact_mut(sz(32))) {
        let d = h(b, 0);
        for (o, q) in y.iter_mut().zip(&b[2..34]) {
            *o = f32::from(*q as i8) * d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: u16 = 0x3c00; // 1.0
    const HALF: u16 = 0x3800; // 0.5
    const TWO: u16 = 0x4000; // 2.0

    fn le(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }

    #[test]
    fn q8_0_scales_signed_bytes() {
        let mut b = le(HALF).to_vec();
        b.extend((0..32).map(|i| (i as i8 - 16) as u8));
        let mut y = [0.0f32; 32];
        q8_0(&b, &mut y);
        for (i, v) in y.iter().enumerate() {
            assert_eq!(*v, (i as f32 - 16.0) * 0.5);
        }
    }

    #[test]
    fn q4_0_low_nibbles_first_then_high() {
        let mut b = le(TWO).to_vec();
        // qs[j]: low nibble j, high nibble 15 - j
        b.extend((0..16u8).map(|j| j | ((15 - j) << 4)));
        let mut y = [0.0f32; 32];
        q4_0(&b, &mut y);
        for j in 0..16 {
            assert_eq!(y[j], (j as f32 - 8.0) * 2.0);
            assert_eq!(y[j + 16], ((15 - j) as f32 - 8.0) * 2.0);
        }
    }

    #[test]
    fn q4_1_adds_minimum() {
        let mut b = le(HALF).to_vec();
        b.extend_from_slice(&le(ONE)); // m = 1.0
        b.extend((0..16u8).map(|j| j | (j << 4)));
        let mut y = [0.0f32; 32];
        q4_1(&b, &mut y);
        for j in 0..16 {
            assert_eq!(y[j], j as f32 * 0.5 + 1.0);
            assert_eq!(y[j + 16], j as f32 * 0.5 + 1.0);
        }
    }

    #[test]
    fn q5_0_uses_the_high_bit_from_qh() {
        let mut b = le(ONE).to_vec();
        // fifth bit set for element 3 (bit 3) and element 20 (bit 20)
        let qh: u32 = (1 << 3) | (1 << 20);
        b.extend_from_slice(&qh.to_le_bytes());
        b.extend(std::iter::repeat_n(0x00u8, 16)); // all nibbles zero
        let mut y = [0.0f32; 32];
        q5_0(&b, &mut y);
        for (i, v) in y.iter().enumerate() {
            let q5 = if i == 3 || i == 20 { 16.0 } else { 0.0 };
            assert_eq!(*v, q5 - 16.0, "element {i}");
        }
    }

    #[test]
    fn q5_1_combines_nibbles_bits_scale_and_min() {
        let mut b = le(HALF).to_vec();
        b.extend_from_slice(&le(TWO)); // m = 2.0
        let qh: u32 = 0xffff_0000; // fifth bit set for the upper 16 elements
        b.extend_from_slice(&qh.to_le_bytes());
        b.extend((0..16u8).map(|j| j | (0xf << 4)));
        let mut y = [0.0f32; 32];
        q5_1(&b, &mut y);
        for j in 0..16 {
            assert_eq!(y[j], j as f32 * 0.5 + 2.0);
            assert_eq!(y[j + 16], (15.0 + 16.0) * 0.5 + 2.0);
        }
    }

    #[test]
    fn multiple_blocks_are_laid_out_consecutively() {
        let mut b = Vec::new();
        for scale in [ONE, TWO] {
            b.extend_from_slice(&le(scale));
            b.extend(std::iter::repeat_n(1u8, 32));
        }
        let mut y = [0.0f32; 64];
        q8_0(&b, &mut y);
        assert!(y[..32].iter().all(|v| *v == 1.0));
        assert!(y[32..].iter().all(|v| *v == 2.0));
    }

    #[test]
    fn extreme_nibbles() {
        let mut b = le(ONE).to_vec();
        b.extend(std::iter::repeat_n(0xf0u8, 16)); // low 0, high 15
        let mut y = [0.0f32; 32];
        q4_0(&b, &mut y);
        assert!(y[..16].iter().all(|v| *v == -8.0));
        assert!(y[16..].iter().all(|v| *v == 7.0));
    }
}
