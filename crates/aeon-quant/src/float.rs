//! Float element types.

use crate::sz;

/// IEEE 754 binary16 to `f32` (exact for every input, including subnormals, infinities, NaN).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    let bits = match (exp, frac) {
        (0, 0) => sign,
        (0, f) => {
            // subnormal: normalise
            let shift = f.leading_zeros() - 21; // bring the top set bit to bit 10
            let frac = (f << shift) & 0x3ff;
            let exp = 127 - 15 + 1 - shift;
            sign | (exp << 23) | (frac << 13)
        }
        (0x1f, 0) => sign | 0x7f80_0000,
        (0x1f, f) => sign | 0x7f80_0000 | (f << 13),
        (e, f) => sign | ((e + 127 - 15) << 23) | (f << 13),
    };
    f32::from_bits(bits)
}

/// bfloat16 to `f32` (the top 16 bits of an `f32`).
pub fn bf16_to_f32(b: u16) -> f32 {
    f32::from_bits(u32::from(b) << 16)
}

pub(crate) fn f32s(bytes: &[u8], out: &mut [f32]) {
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(sz(4))) {
        *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
}

pub(crate) fn f16s(bytes: &[u8], out: &mut [f32]) {
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(sz(2))) {
        *o = f16_to_f32(u16::from_le_bytes([c[0], c[1]]));
    }
}

pub(crate) fn bf16s(bytes: &[u8], out: &mut [f32]) {
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(sz(2))) {
        *o = bf16_to_f32(u16::from_le_bytes([c[0], c[1]]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_known_values() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x8000).to_bits(), (-0.0f32).to_bits());
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xbc00), -1.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x4000), 2.0);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0400), 2.0f32.powi(-14)); // smallest normal
        assert_eq!(f16_to_f32(0x0001), 2.0f32.powi(-24)); // smallest subnormal
        assert_eq!(f16_to_f32(0x03ff), 1023.0 * 2.0f32.powi(-24));
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert_eq!(f16_to_f32(0xfc00), f32::NEG_INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95); // nearest to 1/3
    }

    #[test]
    fn f16_every_subnormal_is_exact() {
        for f in 1u16..0x400 {
            assert_eq!(f16_to_f32(f), f32::from(f) * 2.0f32.powi(-24), "frac {f}");
        }
    }

    #[test]
    fn f16_is_monotonic_over_positive_finite_values() {
        let mut last = -1.0f32;
        for h in 0u16..0x7c00 {
            let v = f16_to_f32(h);
            assert!(v > last || (h == 0 && v == 0.0), "{h:#x}");
            last = v;
        }
    }

    #[test]
    fn bf16_known_values() {
        assert_eq!(bf16_to_f32(0x3f80), 1.0);
        assert_eq!(bf16_to_f32(0xc000), -2.0);
        assert_eq!(bf16_to_f32(0x0000), 0.0);
        assert_eq!(bf16_to_f32(0x7f80), f32::INFINITY);
        assert!(bf16_to_f32(0x7fc0).is_nan());
    }

    #[test]
    fn slices_decode_in_order() {
        let mut out = [0.0f32; 2];
        f32s(&[0, 0, 0x80, 0x3f, 0, 0, 0, 0x40], &mut out);
        assert_eq!(out, [1.0, 2.0]);
        f16s(&[0x00, 0x3c, 0x00, 0x40], &mut out);
        assert_eq!(out, [1.0, 2.0]);
        bf16s(&[0x80, 0x3f, 0x00, 0x40], &mut out);
        assert_eq!(out, [1.0, 2.0]);
    }
}
