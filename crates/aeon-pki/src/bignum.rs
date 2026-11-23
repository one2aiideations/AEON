//! Arbitrary-size unsigned integers for signature verification.
//!
//! Verification only touches public values (certificates, signatures, public keys), so these
//! routines are written for clarity and correctness, not constant time. Never use them with
//! secret data.

use std::cmp::Ordering;

/// An unsigned integer, little-endian 32-bit limbs, no trailing zero limbs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Uint {
    limbs: Vec<u32>,
}

impl Uint {
    /// Zero.
    #[must_use]
    pub fn zero() -> Self {
        Self::default()
    }

    /// A small value.
    #[must_use]
    pub fn from_u64(v: u64) -> Self {
        let mut u = Self {
            limbs: vec![v as u32, (v >> 32) as u32],
        };
        u.trim();
        u
    }

    /// Parse big-endian bytes (leading zeros allowed).
    #[must_use]
    pub fn from_be(bytes: &[u8]) -> Self {
        let mut limbs = Vec::with_capacity(bytes.len() / 4 + 1);
        for chunk in bytes.rchunks(4) {
            let mut w = 0u32;
            for &b in chunk {
                w = (w << 8) | u32::from(b);
            }
            limbs.push(w);
        }
        let mut u = Self { limbs };
        u.trim();
        u
    }

    /// Parse a hex string (no prefix); used for curve constants.
    ///
    /// # Panics
    /// On a non-hex character (constants are compile-time literals).
    #[must_use]
    pub fn from_hex(s: &str) -> Self {
        let digits: Vec<u8> = s
            .bytes()
            .map(|c| match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("bad hex"),
            })
            .collect();
        let mut bytes = Vec::new();
        let mut i = digits.len() % 2;
        if i == 1 {
            bytes.push(digits[0]);
        }
        while i < digits.len() {
            bytes.push(digits[i] << 4 | digits[i + 1]);
            i += 2;
        }
        Self::from_be(&bytes)
    }

    /// Big-endian bytes, left-padded with zeros to `len`. `None` if the value does not fit.
    #[must_use]
    pub fn to_be_padded(&self, len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; len];
        for (i, limb) in self.limbs.iter().enumerate() {
            for j in 0..4 {
                let byte = (limb >> (8 * j)) as u8;
                let pos = i * 4 + j;
                if pos < len {
                    out[len - 1 - pos] = byte;
                } else if byte != 0 {
                    return None;
                }
            }
        }
        Some(out)
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    /// Whether the value is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// Whether the value is one.
    #[must_use]
    pub fn is_one(&self) -> bool {
        self.limbs == [1]
    }

    /// Whether the lowest bit is set.
    #[must_use]
    pub fn is_odd(&self) -> bool {
        self.limbs.first().is_some_and(|l| l & 1 == 1)
    }

    /// Number of significant bits (0 for zero).
    #[must_use]
    pub fn bits(&self) -> usize {
        match self.limbs.last() {
            None => 0,
            Some(top) => self.limbs.len() * 32 - top.leading_zeros() as usize,
        }
    }

    /// Bit `i` (least significant is 0).
    #[must_use]
    pub fn bit(&self, i: usize) -> bool {
        self.limbs
            .get(i / 32)
            .is_some_and(|l| (l >> (i % 32)) & 1 == 1)
    }

    /// Compare.
    #[must_use]
    pub fn cmp_to(&self, other: &Self) -> Ordering {
        self.limbs
            .len()
            .cmp(&other.limbs.len())
            .then_with(|| self.limbs.iter().rev().cmp(other.limbs.iter().rev()))
    }

    /// `self + other`.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        let n = self.limbs.len().max(other.limbs.len());
        let mut limbs = Vec::with_capacity(n + 1);
        let mut carry = 0u64;
        for i in 0..n {
            let s = u64::from(*self.limbs.get(i).unwrap_or(&0))
                + u64::from(*other.limbs.get(i).unwrap_or(&0))
                + carry;
            limbs.push(s as u32);
            carry = s >> 32;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
        Self { limbs }
    }

    /// `self - other`; `None` when `other > self`.
    #[must_use]
    pub fn sub(&self, other: &Self) -> Option<Self> {
        if self.cmp_to(other) == Ordering::Less {
            return None;
        }
        let mut limbs = Vec::with_capacity(self.limbs.len());
        let mut borrow = 0i64;
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut d = i64::from(a) - i64::from(*other.limbs.get(i).unwrap_or(&0)) - borrow;
            if d < 0 {
                d += 1 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            limbs.push(d as u32);
        }
        let mut u = Self { limbs };
        u.trim();
        Some(u)
    }

    /// `self * other`.
    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let mut limbs = vec![0u32; self.limbs.len() + other.limbs.len()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in other.limbs.iter().enumerate() {
                let t = u64::from(a) * u64::from(b) + u64::from(limbs[i + j]) + carry;
                limbs[i + j] = t as u32;
                carry = t >> 32;
            }
            limbs[i + other.limbs.len()] = carry as u32;
        }
        let mut u = Self { limbs };
        u.trim();
        u
    }

    /// `self << n` bits.
    #[must_use]
    pub fn shl(&self, n: usize) -> Self {
        if self.is_zero() {
            return Self::zero();
        }
        let (words, bits) = (n / 32, n % 32);
        let mut limbs = vec![0u32; words];
        let mut carry = 0u32;
        for &l in &self.limbs {
            if bits == 0 {
                limbs.push(l);
            } else {
                limbs.push(l << bits | carry);
                carry = l >> (32 - bits);
            }
        }
        if carry > 0 {
            limbs.push(carry);
        }
        Self { limbs }
    }

    /// `self >> n` bits.
    #[must_use]
    pub fn shr(&self, n: usize) -> Self {
        let (words, bits) = (n / 32, n % 32);
        if words >= self.limbs.len() {
            return Self::zero();
        }
        let src = &self.limbs[words..];
        let mut limbs = Vec::with_capacity(src.len());
        for i in 0..src.len() {
            let lo = src[i] >> bits;
            let hi = if bits > 0 && i + 1 < src.len() {
                src[i + 1] << (32 - bits)
            } else {
                0
            };
            limbs.push(lo | hi);
        }
        let mut u = Self { limbs };
        u.trim();
        u
    }

    /// Quotient and remainder (Knuth algorithm D). `None` on division by zero.
    #[must_use]
    pub fn divrem(&self, d: &Self) -> Option<(Self, Self)> {
        if d.is_zero() {
            return None;
        }
        if self.cmp_to(d) == Ordering::Less {
            return Some((Self::zero(), self.clone()));
        }
        if d.limbs.len() == 1 {
            let dv = u64::from(d.limbs[0]);
            let mut q = vec![0u32; self.limbs.len()];
            let mut rem = 0u64;
            for i in (0..self.limbs.len()).rev() {
                let cur = rem << 32 | u64::from(self.limbs[i]);
                q[i] = (cur / dv) as u32;
                rem = cur % dv;
            }
            let mut qu = Self { limbs: q };
            qu.trim();
            return Some((qu, Self::from_u64(rem)));
        }
        // Normalise so the divisor's top limb has its high bit set.
        let shift = d.limbs[d.limbs.len() - 1].leading_zeros() as usize;
        let v = d.shl(shift);
        let mut u = self.shl(shift).limbs;
        let n = v.limbs.len();
        if u.len() == self.limbs.len() {
            u.push(0);
        }
        while u.len() < self.limbs.len() + 1 {
            u.push(0);
        }
        let m = u.len() - n - 1;
        let mut q = vec![0u32; m + 1];
        let base = 1u64 << 32;
        let (vn1, vn2) = (u64::from(v.limbs[n - 1]), u64::from(v.limbs[n - 2]));
        for j in (0..=m).rev() {
            let top = u64::from(u[j + n]) << 32 | u64::from(u[j + n - 1]);
            let mut qhat = top / vn1;
            let mut rhat = top % vn1;
            while qhat >= base || qhat * vn2 > (rhat << 32 | u64::from(u[j + n - 2])) {
                qhat -= 1;
                rhat += vn1;
                if rhat >= base {
                    break;
                }
            }
            // Multiply and subtract qhat * v from u[j..=j+n].
            let mut borrow = 0i64;
            let mut carry = 0u64;
            for i in 0..n {
                let p = qhat * u64::from(v.limbs[i]) + carry;
                carry = p >> 32;
                let t = i64::from(u[i + j]) - borrow - i64::from(p as u32);
                u[i + j] = t as u32;
                borrow = i64::from(t < 0);
            }
            let t = i64::from(u[j + n]) - borrow - carry as i64;
            u[j + n] = t as u32;
            if t < 0 {
                // qhat was one too large: add v back.
                qhat -= 1;
                let mut c = 0u64;
                for i in 0..n {
                    let s = u64::from(u[i + j]) + u64::from(v.limbs[i]) + c;
                    u[i + j] = s as u32;
                    c = s >> 32;
                }
                u[j + n] = u[j + n].wrapping_add(c as u32);
            }
            q[j] = qhat as u32;
        }
        let mut qu = Self { limbs: q };
        qu.trim();
        let mut r = Self {
            limbs: u[..n].to_vec(),
        };
        r.trim();
        Some((qu, r.shr(shift)))
    }

    /// `self mod m`. `m` must be non-zero.
    ///
    /// # Panics
    /// If `m` is zero.
    #[must_use]
    pub fn rem(&self, m: &Self) -> Self {
        self.divrem(m).expect("modulus is zero").1
    }

    /// `(self * other) mod m`.
    #[must_use]
    pub fn mul_mod(&self, other: &Self, m: &Self) -> Self {
        self.mul(other).rem(m)
    }

    /// `(self + other) mod m` for operands already below `m`.
    #[must_use]
    pub fn add_mod(&self, other: &Self, m: &Self) -> Self {
        let s = self.add(other);
        if s.cmp_to(m) == Ordering::Less {
            s
        } else {
            s.sub(m).unwrap_or_default()
        }
    }

    /// `(self - other) mod m` for operands already below `m`.
    #[must_use]
    pub fn sub_mod(&self, other: &Self, m: &Self) -> Self {
        match self.sub(other) {
            Some(d) => d,
            None => self.add(m).sub(other).unwrap_or_default(),
        }
    }

    /// `self ^ exp mod m` (square and multiply).
    #[must_use]
    pub fn mod_pow(&self, exp: &Self, m: &Self) -> Self {
        let mut result = Self::from_u64(1).rem(m);
        let base = self.rem(m);
        for i in (0..exp.bits()).rev() {
            result = result.mul_mod(&result, m);
            if exp.bit(i) {
                result = result.mul_mod(&base, m);
            }
        }
        result
    }

    /// Inverse modulo a prime `p` (Fermat). `None` for zero.
    #[must_use]
    pub fn inv_mod_prime(&self, p: &Self) -> Option<Self> {
        if self.rem(p).is_zero() {
            return None;
        }
        let two = Self::from_u64(2);
        Some(self.mod_pow(&p.sub(&two)?, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Uint {
        Uint::from_hex(s)
    }

    #[test]
    fn bytes_roundtrip_and_trim() {
        let u = Uint::from_be(&[0, 0, 1, 2, 3, 4, 5]);
        assert_eq!(u, h("0102030405"));
        assert_eq!(u.to_be_padded(6).unwrap(), vec![0, 1, 2, 3, 4, 5]);
        assert!(u.to_be_padded(4).is_none());
        assert!(Uint::from_be(&[0, 0]).is_zero());
        assert_eq!(h("ffffffff").bits(), 32);
        assert_eq!(h("100000000").bits(), 33);
    }

    #[test]
    fn arithmetic_matches_known_values() {
        let a = h("ffffffffffffffffffffffffffffffff");
        let b = h("fffffffffffffffffffffffffffffffe");
        assert_eq!(a.add(&b), h("1fffffffffffffffffffffffffffffffd"));
        assert_eq!(a.sub(&b).unwrap(), Uint::from_u64(1));
        assert!(b.sub(&a).is_none());
        // (2^128-1)^2 = 2^256 - 2^129 + 1
        assert_eq!(
            a.mul(&a),
            h("fffffffffffffffffffffffffffffffe00000000000000000000000000000001")
        );
    }

    #[test]
    fn division_is_consistent() {
        // Cases that exercise the qhat correction and the add-back step.
        let cases = [
            ("ffffffffffffffffffffffffffffffff", "ffffffff00000001"),
            (
                "7fffffff800000010000000000000000",
                "800000008000000200000005",
            ),
            (
                "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe",
                "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
            ),
            ("0100000000000000000000000000000000", "ffffffffffffffff"),
        ];
        for (n, d) in cases {
            let (n, d) = (h(n), h(d));
            let (q, r) = n.divrem(&d).unwrap();
            assert_eq!(q.mul(&d).add(&r), n);
            assert_eq!(r.cmp_to(&d), Ordering::Less);
        }
        assert!(h("5").divrem(&Uint::zero()).is_none());
    }

    #[test]
    fn division_pseudo_random_roundtrip() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..300 {
            let na = (next() % 40 + 1) as usize;
            let nb = (next() % 24 + 1) as usize;
            let a: Vec<u8> = (0..na).map(|_| next() as u8).collect();
            let mut b: Vec<u8> = (0..nb).map(|_| next() as u8).collect();
            if b.iter().all(|&v| v == 0) {
                b[0] = 1;
            }
            let (a, b) = (Uint::from_be(&a), Uint::from_be(&b));
            let (q, r) = a.divrem(&b).unwrap();
            assert_eq!(q.mul(&b).add(&r), a);
            assert_eq!(r.cmp_to(&b), Ordering::Less);
        }
    }

    #[test]
    fn shifts() {
        let a = h("123456789abcdef0123456789abcdef");
        for n in [0, 1, 5, 31, 32, 33, 64, 100] {
            assert_eq!(a.shl(n).shr(n), a);
        }
        assert!(a.shr(200).is_zero());
    }

    #[test]
    fn modular_power_and_inverse() {
        // Fermat: a^(p-1) = 1 mod p for the P-256 prime.
        let p = h("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff");
        let a = h("deadbeefcafebabe0123456789abcdef");
        assert!(a.mod_pow(&p.sub(&Uint::from_u64(1)).unwrap(), &p).is_one());
        let inv = a.inv_mod_prime(&p).unwrap();
        assert!(a.mul_mod(&inv, &p).is_one());
        assert!(Uint::zero().inv_mod_prime(&p).is_none());
        // small known value: 3^200 mod 13 = 9
        assert_eq!(
            Uint::from_u64(3).mod_pow(&Uint::from_u64(200), &Uint::from_u64(13)),
            Uint::from_u64(9)
        );
    }

    #[test]
    fn add_sub_mod() {
        let m = Uint::from_u64(97);
        let (a, b) = (Uint::from_u64(90), Uint::from_u64(20));
        assert_eq!(a.add_mod(&b, &m), Uint::from_u64(13));
        assert_eq!(b.sub_mod(&a, &m), Uint::from_u64(27));
    }
}
