//! ECDSA signature verification on NIST P-256 and P-384.

use crate::bignum::Uint;
use crate::der::{self, tag};
use crate::Hash;
use std::cmp::Ordering;

/// A supported curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// secp256r1 / prime256v1.
    P256,
    /// secp384r1.
    P384,
}

struct Params {
    p: Uint,
    b: Uint,
    n: Uint,
    gx: Uint,
    gy: Uint,
    size: usize,
}

impl Curve {
    fn params(self) -> Params {
        match self {
            Self::P256 => Params {
                p: Uint::from_hex("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff"),
                b: Uint::from_hex("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b"),
                n: Uint::from_hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"),
                gx: Uint::from_hex("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"),
                gy: Uint::from_hex("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"),
                size: 32,
            },
            Self::P384 => Params {
                p: Uint::from_hex("fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff"),
                b: Uint::from_hex("b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef"),
                n: Uint::from_hex("ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973"),
                gx: Uint::from_hex("aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7"),
                gy: Uint::from_hex("3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f"),
                size: 48,
            },
        }
    }

    /// Bytes per coordinate.
    #[must_use]
    pub fn size(self) -> usize {
        self.params().size
    }

    /// The curve for a named-curve OID (dotted form).
    #[must_use]
    pub fn from_oid(oid: &str) -> Option<Self> {
        match oid {
            "1.2.840.10045.3.1.7" => Some(Self::P256),
            "1.3.132.0.34" => Some(Self::P384),
            _ => None,
        }
    }
}

/// A point in Jacobian coordinates; `z == 0` is the point at infinity.
#[derive(Clone)]
struct Point {
    x: Uint,
    y: Uint,
    z: Uint,
}

struct Field<'a> {
    p: &'a Uint,
}

impl Field<'_> {
    fn mul(&self, a: &Uint, b: &Uint) -> Uint {
        a.mul_mod(b, self.p)
    }
    fn add(&self, a: &Uint, b: &Uint) -> Uint {
        a.add_mod(b, self.p)
    }
    fn sub(&self, a: &Uint, b: &Uint) -> Uint {
        a.sub_mod(b, self.p)
    }
    fn dbl(&self, a: &Uint) -> Uint {
        self.add(a, a)
    }
}

impl Point {
    fn infinity() -> Self {
        Self {
            x: Uint::from_u64(1),
            y: Uint::from_u64(1),
            z: Uint::zero(),
        }
    }

    fn affine(x: &Uint, y: &Uint) -> Self {
        Self {
            x: x.clone(),
            y: y.clone(),
            z: Uint::from_u64(1),
        }
    }

    fn is_infinity(&self) -> bool {
        self.z.is_zero()
    }

    /// Doubling for a = -3 (dbl-2001-b).
    fn double(&self, f: &Field) -> Self {
        if self.is_infinity() || self.y.is_zero() {
            return Self::infinity();
        }
        let delta = f.mul(&self.z, &self.z);
        let gamma = f.mul(&self.y, &self.y);
        let beta = f.mul(&self.x, &gamma);
        let alpha = {
            let t = f.mul(&f.sub(&self.x, &delta), &f.add(&self.x, &delta));
            f.add(&f.dbl(&t), &t)
        };
        let beta4 = f.dbl(&f.dbl(&beta));
        let beta8 = f.dbl(&beta4);
        let x3 = f.sub(&f.mul(&alpha, &alpha), &beta8);
        let yz = f.add(&self.y, &self.z);
        let z3 = f.sub(&f.sub(&f.mul(&yz, &yz), &gamma), &delta);
        let g2 = f.mul(&gamma, &gamma);
        let g8 = f.dbl(&f.dbl(&f.dbl(&g2)));
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &g8);
        Self {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// Addition (add-2007-bl) with the equal and opposite cases handled.
    fn add(&self, o: &Self, f: &Field) -> Self {
        if self.is_infinity() {
            return o.clone();
        }
        if o.is_infinity() {
            return self.clone();
        }
        let z1z1 = f.mul(&self.z, &self.z);
        let z2z2 = f.mul(&o.z, &o.z);
        let u1 = f.mul(&self.x, &z2z2);
        let u2 = f.mul(&o.x, &z1z1);
        let s1 = f.mul(&f.mul(&self.y, &o.z), &z2z2);
        let s2 = f.mul(&f.mul(&o.y, &self.z), &z1z1);
        if u1 == u2 {
            return if s1 == s2 {
                self.double(f)
            } else {
                Self::infinity()
            };
        }
        let h = f.sub(&u2, &u1);
        let i = {
            let t = f.dbl(&h);
            f.mul(&t, &t)
        };
        let j = f.mul(&h, &i);
        let r = f.dbl(&f.sub(&s2, &s1));
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.mul(&r, &r), &j), &f.dbl(&v));
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.dbl(&f.mul(&s1, &j)));
        let zs = f.add(&self.z, &o.z);
        let z3 = f.mul(&f.sub(&f.sub(&f.mul(&zs, &zs), &z1z1), &z2z2), &h);
        Self {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    fn mul_scalar(&self, k: &Uint, f: &Field) -> Self {
        let mut acc = Self::infinity();
        for i in (0..k.bits()).rev() {
            acc = acc.double(f);
            if k.bit(i) {
                acc = acc.add(self, f);
            }
        }
        acc
    }

    /// The affine x coordinate, `None` at infinity.
    fn affine_x(&self, f: &Field) -> Option<Uint> {
        let zi = self.z.inv_mod_prime(f.p)?;
        Some(f.mul(&self.x, &f.mul(&zi, &zi)))
    }
}

/// A public key on a supported curve.
#[derive(Clone, Debug)]
pub struct PublicKey {
    curve: Curve,
    x: Uint,
    y: Uint,
}

impl PublicKey {
    /// Parse an uncompressed SEC1 point (`04 || X || Y`) and check it lies on the curve.
    #[must_use]
    pub fn from_sec1(curve: Curve, bytes: &[u8]) -> Option<Self> {
        let size = curve.size();
        if bytes.len() != 1 + 2 * size || bytes[0] != 4 {
            return None;
        }
        let x = Uint::from_be(&bytes[1..=size]);
        let y = Uint::from_be(&bytes[1 + size..]);
        let pr = curve.params();
        if x.cmp_to(&pr.p) != Ordering::Less || y.cmp_to(&pr.p) != Ordering::Less {
            return None;
        }
        // y^2 = x^3 - 3x + b (mod p)
        let f = Field { p: &pr.p };
        let y2 = f.mul(&y, &y);
        let x3 = f.mul(&f.mul(&x, &x), &x);
        let three_x = f.add(&f.dbl(&x), &x);
        let rhs = f.add(&f.sub(&x3, &three_x), &pr.b);
        (y2 == rhs).then_some(Self { curve, x, y })
    }

    /// The curve.
    #[must_use]
    pub fn curve(&self) -> Curve {
        self.curve
    }

    /// Verify a DER-encoded ECDSA signature (`SEQUENCE { r, s }`) over `message`.
    #[must_use]
    pub fn verify(&self, hash: Hash, message: &[u8], signature: &[u8]) -> bool {
        self.verify_digest(&hash.digest(message), signature)
    }

    /// Verify a DER-encoded signature over an already computed digest.
    #[must_use]
    pub fn verify_digest(&self, digest: &[u8], signature: &[u8]) -> bool {
        let Some((r, s)) = parse_signature(signature) else {
            return false;
        };
        let pr = self.curve.params();
        for v in [&r, &s] {
            if v.is_zero() || v.cmp_to(&pr.n) != Ordering::Less {
                return false;
            }
        }
        // Leftmost n.bits() bits of the digest.
        let mut e = Uint::from_be(digest);
        let excess = (digest.len() * 8).saturating_sub(pr.n.bits());
        e = e.shr(excess);
        e = e.rem(&pr.n);
        let Some(w) = s.inv_mod_prime(&pr.n) else {
            return false;
        };
        let u1 = e.mul_mod(&w, &pr.n);
        let u2 = r.mul_mod(&w, &pr.n);
        let f = Field { p: &pr.p };
        let g = Point::affine(&pr.gx, &pr.gy);
        let q = Point::affine(&self.x, &self.y);
        let sum = g.mul_scalar(&u1, &f).add(&q.mul_scalar(&u2, &f), &f);
        match sum.affine_x(&f) {
            Some(x) => x.rem(&pr.n) == r,
            None => false,
        }
    }
}

fn parse_signature(sig: &[u8]) -> Option<(Uint, Uint)> {
    let mut seq = der::single(sig, tag::SEQUENCE).ok()?;
    let r = seq.expect(tag::INTEGER).ok()?;
    let s = seq.expect(tag::INTEGER).ok()?;
    seq.finish().ok()?;
    let r = der::unsigned_integer(r.content).ok()?;
    let s = der::unsigned_integer(s.content).ok()?;
    Some((Uint::from_be(r), Uint::from_be(s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_on_the_curve_and_have_the_right_order() {
        for curve in [Curve::P256, Curve::P384] {
            let pr = curve.params();
            let size = pr.size;
            let mut enc = vec![4u8];
            enc.extend(pr.gx.to_be_padded(size).unwrap());
            enc.extend(pr.gy.to_be_padded(size).unwrap());
            assert!(PublicKey::from_sec1(curve, &enc).is_some());
            // n * G is the point at infinity, (n-1) * G is -G
            let f = Field { p: &pr.p };
            let g = Point::affine(&pr.gx, &pr.gy);
            assert!(g.mul_scalar(&pr.n, &f).is_infinity());
            let m1 = g.mul_scalar(&pr.n.sub(&Uint::from_u64(1)).unwrap(), &f);
            assert_eq!(m1.affine_x(&f).unwrap(), pr.gx);
            // 2G computed by doubling equals G + G
            let two = g.mul_scalar(&Uint::from_u64(2), &f);
            assert_eq!(two.affine_x(&f), g.double(&f).affine_x(&f));
        }
    }

    #[test]
    fn rejects_points_off_the_curve_and_bad_encodings() {
        let pr = Curve::P256.params();
        let mut enc = vec![4u8];
        enc.extend(pr.gx.to_be_padded(32).unwrap());
        enc.extend(pr.gy.add(&Uint::from_u64(1)).to_be_padded(32).unwrap());
        assert!(PublicKey::from_sec1(Curve::P256, &enc).is_none());
        assert!(PublicKey::from_sec1(Curve::P256, &enc[..64]).is_none());
        enc[0] = 2;
        assert!(PublicKey::from_sec1(Curve::P256, &enc).is_none());
    }

    #[test]
    fn rejects_malformed_signatures() {
        let pr = Curve::P256.params();
        let mut enc = vec![4u8];
        enc.extend(pr.gx.to_be_padded(32).unwrap());
        enc.extend(pr.gy.to_be_padded(32).unwrap());
        let key = PublicKey::from_sec1(Curve::P256, &enc).unwrap();
        let digest = [1u8; 32];
        assert!(!key.verify_digest(&digest, &[]));
        // r = 0
        assert!(!key.verify_digest(&digest, &[0x30, 0x06, 2, 1, 0, 2, 1, 1]));
        // trailing data
        assert!(!key.verify_digest(&digest, &[0x30, 0x06, 2, 1, 1, 2, 1, 1, 0]));
    }
}
