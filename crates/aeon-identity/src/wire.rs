//! Big-endian wire encoding of a [`Frame`].
//!
//! Layout: magic u32, version u16, flags u16, digest\[32\], subject, predicate,
//! object (each u16 length + bytes), auth (key\[32\], sig\[64\], vc\[32\], did,
//! human u8), friction (nonce u64, ctx\[32\]), attestation (nonce u64, ctx\[32\],
//! key\[32\], sig\[64\]), cid. Trailing bytes are rejected.

use crate::friction::{Attestation, Challenge};
use crate::semantic::{AuthPayload, Frame, Triple};
use crate::IdError;

/// Wire magic ("AEW2").
pub const MAGIC: u32 = 0x4145_5732;
/// Wire version.
pub const VERSION: u16 = 2;

struct Writer(Vec<u8>);

impl Writer {
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn str16(&mut self, s: &str) -> Result<(), IdError> {
        let n = u16::try_from(s.len()).map_err(|_| IdError::BadField)?;
        self.bytes(&n.to_be_bytes());
        self.bytes(s.as_bytes());
        Ok(())
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], IdError> {
        if self.0.len() < n {
            return Err(IdError::Malformed);
        }
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Ok(h)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], IdError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u16(&mut self) -> Result<u16, IdError> {
        Ok(u16::from_be_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32, IdError> {
        Ok(u32::from_be_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Result<u64, IdError> {
        Ok(u64::from_be_bytes(self.arr()?))
    }
    fn str16(&mut self) -> Result<String, IdError> {
        let n = usize::from(self.u16()?);
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| IdError::Malformed)
    }
}

/// Encode a structurally valid frame.
pub fn encode(f: &Frame) -> Result<Vec<u8>, IdError> {
    f.validate()?;
    let mut w = Writer(Vec::with_capacity(512));
    w.bytes(&MAGIC.to_be_bytes());
    w.bytes(&VERSION.to_be_bytes());
    w.bytes(&f.flags.to_be_bytes());
    w.bytes(&f.digest);
    w.str16(&f.triple.subject)?;
    w.str16(&f.triple.predicate)?;
    w.str16(&f.triple.object)?;
    w.bytes(&f.auth.public_key);
    w.bytes(&f.auth.signature);
    w.bytes(&f.auth.vc_hash);
    w.str16(&f.auth.did)?;
    w.bytes(&[u8::from(f.auth.human_verified)]);
    w.bytes(&f.friction.nonce.to_be_bytes());
    w.bytes(&f.friction.context_hash);
    w.bytes(&f.attestation.nonce.to_be_bytes());
    w.bytes(&f.attestation.context_hash);
    w.bytes(&f.attestation.verifier_key);
    w.bytes(&f.attestation.signature);
    w.str16(&f.cid)?;
    Ok(w.0)
}

/// Decode and structurally validate a frame.
pub fn decode(b: &[u8]) -> Result<Frame, IdError> {
    let mut r = Reader(b);
    if r.u32()? != MAGIC || r.u16()? != VERSION {
        return Err(IdError::Malformed);
    }
    let flags = r.u16()?;
    let digest = r.arr()?;
    let triple = Triple {
        subject: r.str16()?,
        predicate: r.str16()?,
        object: r.str16()?,
    };
    let public_key = r.arr()?;
    let signature = r.arr()?;
    let vc_hash = r.arr()?;
    let did = r.str16()?;
    let human_verified = match r.take(1)?[0] {
        0 => false,
        1 => true,
        _ => return Err(IdError::Malformed),
    };
    let friction = Challenge {
        nonce: r.u64()?,
        context_hash: r.arr()?,
    };
    let attestation = Attestation {
        nonce: r.u64()?,
        context_hash: r.arr()?,
        verifier_key: r.arr()?,
        signature: r.arr()?,
    };
    let cid = r.str16()?;
    if !r.0.is_empty() {
        return Err(IdError::Malformed);
    }
    let f = Frame {
        flags,
        digest,
        triple,
        auth: AuthPayload {
            did,
            public_key,
            signature,
            vc_hash,
            human_verified,
        },
        friction,
        attestation,
        cid,
    };
    f.validate().map_err(|_| IdError::Malformed)?;
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{tests::signed_frame, Verdict};

    #[test]
    fn round_trip_keeps_verdict() {
        let (f, reg) = signed_frame();
        let bytes = encode(&f).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.check(&reg), Verdict::Allow);
    }

    #[test]
    fn rejects_truncation_and_trailing() {
        let (f, _) = signed_frame();
        let bytes = encode(&f).unwrap();
        for cut in [0, 4, 10, bytes.len() - 1] {
            assert_eq!(decode(&bytes[..cut]), Err(IdError::Malformed));
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode(&extra), Err(IdError::Malformed));
        let mut magic = bytes;
        magic[0] ^= 1;
        assert_eq!(decode(&magic), Err(IdError::Malformed));
    }
}
