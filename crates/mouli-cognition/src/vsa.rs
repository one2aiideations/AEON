//! Vector-symbolic architecture on 2048-bit binary hypervectors.

use crate::CogError;

/// Bits per hypervector.
pub const BITS: usize = 2048;
const WORDS: usize = BITS / 64;

/// A binary hypervector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hv([u64; WORDS]);

impl Default for Hv {
    fn default() -> Self {
        Self([0; WORDS])
    }
}

impl Hv {
    /// All-zero vector.
    #[must_use]
    pub fn zero() -> Self {
        Self::default()
    }

    /// Deterministic pseudo-random vector from a seed (splitmix64).
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        let mut s = seed;
        let mut w = [0u64; WORDS];
        for x in &mut w {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *x = z ^ (z >> 31);
        }
        Self(w)
    }

    /// Bit `i`.
    #[must_use]
    pub fn bit(&self, i: usize) -> bool {
        (self.0[i / 64] >> (i % 64)) & 1 == 1
    }

    fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1u64 << (i % 64);
    }

    /// Binding (XOR). Self-inverse: `a.bind(&a.bind(&b)) == b`.
    #[must_use]
    pub fn bind(&self, other: &Self) -> Self {
        let mut out = *self;
        for (o, b) in out.0.iter_mut().zip(other.0) {
            *o ^= b;
        }
        out
    }

    /// Majority bundle; ties resolve to 1.
    #[must_use]
    pub fn bundle(items: &[Self]) -> Self {
        let mut out = Self::zero();
        if items.is_empty() {
            return out;
        }
        for i in 0..BITS {
            let ones = items.iter().filter(|h| h.bit(i)).count();
            if ones * 2 >= items.len() {
                out.set(i);
            }
        }
        out
    }

    /// Cyclic rotation by `shift` bits (encodes sequence position).
    #[must_use]
    pub fn permute(&self, shift: usize) -> Self {
        let shift = shift % BITS;
        let mut out = Self::zero();
        for i in (0..BITS).filter(|&i| self.bit(i)) {
            out.set((i + shift) % BITS);
        }
        out
    }

    /// Hamming distance.
    #[must_use]
    pub fn hamming(&self, other: &Self) -> u32 {
        self.0
            .iter()
            .zip(other.0)
            .map(|(a, b)| (a ^ b).count_ones())
            .sum()
    }
}

/// Nearest bipolar (`-1/+1` as i8) dictionary entry by normalised dot product.
///
/// Returns `(index, score)`.
pub fn cleanup(query: &[i8], dictionary: &[&[i8]]) -> Result<(usize, f32), CogError> {
    if query.is_empty() || dictionary.is_empty() {
        return Err(CogError::Empty);
    }
    let mut best = (0usize, f32::NEG_INFINITY);
    for (i, d) in dictionary.iter().enumerate() {
        if d.len() != query.len() {
            return Err(CogError::Shape);
        }
        let dot: i64 = query
            .iter()
            .zip(*d)
            .map(|(&a, &b)| i64::from(a) * i64::from(b))
            .sum();
        let score = dot as f32 / query.len() as f32;
        if score > best.1 {
            best = (i, score);
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_is_self_inverse() {
        let a = Hv::from_seed(1);
        let b = Hv::from_seed(2);
        assert_eq!(a.bind(&a.bind(&b)), b);
        let d = a.hamming(&b);
        assert!(d > 900 && d < 1150, "random vectors are ~half apart: {d}");
    }

    #[test]
    fn bundle_is_similar_to_members() {
        let xs = [Hv::from_seed(3), Hv::from_seed(4), Hv::from_seed(5)];
        let s = Hv::bundle(&xs);
        let other = Hv::from_seed(99);
        for x in &xs {
            assert!(s.hamming(x) < s.hamming(&other));
        }
        assert_eq!(Hv::bundle(&[]), Hv::zero());
    }

    #[test]
    fn permute_round_trip() {
        let a = Hv::from_seed(7);
        assert_eq!(a.permute(5).permute(BITS - 5), a);
        assert_eq!(a.permute(BITS), a);
        assert_ne!(a.permute(1), a);
    }

    #[test]
    fn cleanup_finds_nearest() {
        let q = [1i8, 1, -1, -1];
        let d0 = [-1i8, -1, 1, 1];
        let d1 = [1i8, 1, -1, 1];
        let (i, s) = cleanup(&q, &[&d0, &d1]).unwrap();
        assert_eq!(i, 1);
        assert!((s - 0.5).abs() < 1e-6);
        assert_eq!(cleanup(&q, &[&[1i8][..]]), Err(CogError::Shape));
    }
}
