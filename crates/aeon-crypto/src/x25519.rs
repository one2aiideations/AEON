//! X25519 Diffie-Hellman (RFC 7748) over GF(2^255 - 19), five 51-bit limbs.
//!
//! The ladder swaps with masks and has no secret-dependent branches or indexing.

const MASK: u64 = (1 << 51) - 1;

#[derive(Clone, Copy)]
struct Fe([u64; 5]);

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn from_bytes(b: &[u8; 32]) -> Fe {
        let w = |i: usize| {
            u64::from_le_bytes([
                b[i],
                b[i + 1],
                b[i + 2],
                b[i + 3],
                b[i + 4],
                b[i + 5],
                b[i + 6],
                b[i + 7],
            ])
        };
        // 255 bits (the top bit is ignored) cut into 51-bit limbs.
        Fe([
            w(0) & MASK,
            (w(6) >> 3) & MASK,
            (w(12) >> 6) & MASK,
            (w(19) >> 1) & MASK,
            (w(24) >> 12) & MASK,
        ])
    }

    fn carry(mut self) -> Fe {
        let t = &mut self.0;
        let mut c = t[0] >> 51;
        t[0] &= MASK;
        for limb in &mut t[1..] {
            *limb += c;
            c = *limb >> 51;
            *limb &= MASK;
        }
        t[0] += c * 19;
        let c = t[0] >> 51;
        t[0] &= MASK;
        t[1] += c;
        self
    }

    fn add(self, o: Fe) -> Fe {
        let mut r = [0u64; 5];
        for i in 0..5 {
            r[i] = self.0[i] + o.0[i];
        }
        Fe(r).carry()
    }

    fn sub(self, o: Fe) -> Fe {
        // add 2p first so no limb underflows
        const TWO_P: [u64; 5] = [
            0xf_ffff_ffff_ffda,
            0xf_ffff_ffff_fffe,
            0xf_ffff_ffff_fffe,
            0xf_ffff_ffff_fffe,
            0xf_ffff_ffff_fffe,
        ];
        let mut r = [0u64; 5];
        for i in 0..5 {
            r[i] = self.0[i] + TWO_P[i] - o.0[i];
        }
        Fe(r).carry()
    }

    fn mul(self, o: Fe) -> Fe {
        let a = self.0.map(u128::from);
        let b = o.0.map(u128::from);
        let (b1, b2, b3, b4) = (b[1] * 19, b[2] * 19, b[3] * 19, b[4] * 19);
        let mut r0 = a[0] * b[0] + a[1] * b4 + a[2] * b3 + a[3] * b2 + a[4] * b1;
        let mut r1 = a[0] * b[1] + a[1] * b[0] + a[2] * b4 + a[3] * b3 + a[4] * b2;
        let mut r2 = a[0] * b[2] + a[1] * b[1] + a[2] * b[0] + a[3] * b4 + a[4] * b3;
        let mut r3 = a[0] * b[3] + a[1] * b[2] + a[2] * b[1] + a[3] * b[0] + a[4] * b4;
        let mut r4 = a[0] * b[4] + a[1] * b[3] + a[2] * b[2] + a[3] * b[1] + a[4] * b[0];
        let m = u128::from(MASK);
        r1 += r0 >> 51;
        r0 &= m;
        r2 += r1 >> 51;
        r1 &= m;
        r3 += r2 >> 51;
        r2 &= m;
        r4 += r3 >> 51;
        r3 &= m;
        r0 += (r4 >> 51) * 19;
        r4 &= m;
        r1 += r0 >> 51;
        r0 &= m;
        let lim = |x: u128| u64::try_from(x).expect("limb fits after carry");
        Fe([lim(r0), lim(r1), lim(r2), lim(r3), lim(r4)])
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    /// `self^(p-2)`: the inverse, by square-and-multiply on the public exponent
    /// `2^255 - 21` (bits 254..5 set, low five bits `01011`).
    fn invert(self) -> Fe {
        let mut r = Fe::ONE;
        for bit in (0..255).rev() {
            r = r.square();
            let set = if bit >= 5 {
                true
            } else {
                (0b01011 >> bit) & 1 == 1
            };
            if set {
                r = r.mul(self);
            }
        }
        r
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut t = self.carry().carry().0;
        // Reduce fully: subtract p when t >= p.
        let mut q = (t[0] + 19) >> 51;
        for limb in &t[1..] {
            q = (limb + q) >> 51;
        }
        t[0] += 19 * q;
        let mut c = t[0] >> 51;
        t[0] &= MASK;
        for limb in &mut t[1..] {
            *limb += c;
            c = *limb >> 51;
            *limb &= MASK;
        }
        let words = [
            t[0] | (t[1] << 51),
            (t[1] >> 13) | (t[2] << 38),
            (t[2] >> 26) | (t[3] << 25),
            (t[3] >> 39) | (t[4] << 12),
        ];
        let mut out = [0u8; 32];
        for (i, w) in words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// Swap `a` and `b` when `swap` is 1, without branching.
fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let t = mask & (a.0[i] ^ b.0[i]);
        a.0[i] ^= t;
        b.0[i] ^= t;
    }
}

/// Size of keys and shared secrets.
pub const KEY_LEN: usize = 32;

/// `X25519(scalar, u)` as in RFC 7748 section 5.
#[must_use]
pub fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let x1 = Fe::from_bytes(u);
    let (mut x2, mut z2) = (Fe::ONE, Fe::ZERO);
    let (mut x3, mut z3) = (x1, Fe::ONE);
    let mut swap = 0u64;
    let a24 = Fe([121_665, 0, 0, 0, 0]);
    for t in (0..255).rev() {
        let bit = u64::from((k[t / 8] >> (t % 8)) & 1);
        swap ^= bit;
        cswap(swap, &mut x2, &mut x3);
        cswap(swap, &mut z2, &mut z3);
        swap = bit;
        let a = x2.add(z2);
        let aa = a.square();
        let b = x2.sub(z2);
        let bb = b.square();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).square();
        z3 = x1.mul(da.sub(cb).square());
        x2 = aa.mul(bb);
        z2 = e.mul(aa.add(a24.mul(e)));
    }
    cswap(swap, &mut x2, &mut x3);
    cswap(swap, &mut z2, &mut z3);
    x2.mul(z2.invert()).to_bytes()
}

/// The public key for `private`: `X25519(private, 9)`.
#[must_use]
pub fn public_key(private: &[u8; 32]) -> [u8; 32] {
    let mut nine = [0u8; 32];
    nine[0] = 9;
    x25519(private, &nine)
}

/// The shared secret with a peer's public key, or `None` when it is all zeros (a low-order
/// point supplied by an attacker; TLS requires aborting).
#[must_use]
pub fn shared_secret(private: &[u8; 32], peer_public: &[u8; 32]) -> Option<[u8; 32]> {
    let s = x25519(private, peer_public);
    let zero = s.iter().fold(0u8, |a, b| a | b) == 0;
    (!zero).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::from_hex;

    fn a32(s: &str) -> [u8; 32] {
        from_hex(s).unwrap().try_into().unwrap()
    }

    #[test]
    fn rfc7748_function_vectors() {
        assert_eq!(
            x25519(
                &a32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
                &a32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c")
            ),
            a32("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")
        );
        assert_eq!(
            x25519(
                &a32("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
                &a32("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493")
            ),
            a32("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957")
        );
    }

    #[test]
    fn rfc7748_diffie_hellman() {
        let alice = a32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let bob = a32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let (ap, bp) = (public_key(&alice), public_key(&bob));
        assert_eq!(
            ap,
            a32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        assert_eq!(
            bp,
            a32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        );
        let shared = a32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(shared_secret(&alice, &bp), Some(shared));
        assert_eq!(shared_secret(&bob, &ap), Some(shared));
    }

    #[test]
    fn rfc7748_iterated_once_and_1000() {
        let mut k = [0u8; 32];
        k[0] = 9;
        let mut u = k;
        for i in 1..=1000 {
            let out = x25519(&k, &u);
            u = k;
            k = out;
            if i == 1 {
                assert_eq!(
                    k,
                    a32("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")
                );
            }
        }
        assert_eq!(
            k,
            a32("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51")
        );
    }

    #[test]
    fn low_order_points_are_rejected() {
        let k = a32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        assert_eq!(shared_secret(&k, &[0u8; 32]), None);
        let mut one = [0u8; 32];
        one[0] = 1;
        assert_eq!(shared_secret(&k, &one), None);
    }
}
