//! Semantic triples, their canonical digest, and signed transmissions.

use crate::friction::{Attestation, Challenge};
use crate::registry::DidRegistry;
use crate::{check_field, IdError};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Subject / predicate / object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triple {
    /// Subject.
    pub subject: String,
    /// Predicate.
    pub predicate: String,
    /// Object.
    pub object: String,
}

impl Triple {
    /// Validate fields (non-empty, bounded, no NUL or newline).
    pub fn validate(&self) -> Result<(), IdError> {
        for f in [&self.subject, &self.predicate, &self.object] {
            check_field(f)?;
            if f.contains('\n') {
                return Err(IdError::BadField);
            }
        }
        Ok(())
    }

    /// Canonical bytes: `S=<s>\nP=<p>\nO=<o>\n`.
    pub fn canonical(&self) -> Result<Vec<u8>, IdError> {
        self.validate()?;
        Ok(format!(
            "S={}\nP={}\nO={}\n",
            self.subject, self.predicate, self.object
        )
        .into_bytes())
    }

    /// SHA-256 of the canonical bytes.
    pub fn digest(&self) -> Result<[u8; 32], IdError> {
        Ok(Sha256::digest(self.canonical()?).into())
    }
}

/// Sender identity attached to a transmission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPayload {
    /// Sender DID.
    pub did: String,
    /// Sender Ed25519 public key.
    pub public_key: [u8; 32],
    /// Ed25519 signature over the triple digest.
    pub signature: [u8; 64],
    /// Hash of the verifiable credential presented.
    pub vc_hash: [u8; 32],
    /// Sender claims human verification.
    pub human_verified: bool,
}

/// A complete semantic message as carried on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Reserved flag bits.
    pub flags: u16,
    /// Claimed digest of `triple`.
    pub digest: [u8; 32],
    /// Payload.
    pub triple: Triple,
    /// Sender.
    pub auth: AuthPayload,
    /// Human-presence challenge.
    pub friction: Challenge,
    /// Signed answer to the challenge.
    pub attestation: Attestation,
    /// Content address of the full payload.
    pub cid: String,
}

/// Why a frame was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Frame accepted.
    Allow,
    /// Structure invalid.
    Malformed,
    /// Digest mismatch.
    Digest,
    /// Identity, signature, friction, credential or CID check failed.
    Identity(IdError),
}

/// Ed25519 verification of `sig` over `msg`.
pub fn verify_ed25519(key: &[u8; 32], sig: &[u8; 64], msg: &[u8]) -> Result<(), IdError> {
    let vk = VerifyingKey::from_bytes(key).map_err(|_| IdError::Signature)?;
    vk.verify_strict(msg, &Signature::from_bytes(sig))
        .map_err(|_| IdError::Signature)
}

impl Frame {
    /// Structural checks only.
    pub fn validate(&self) -> Result<(), IdError> {
        self.triple.validate()?;
        check_field(&self.auth.did)?;
        check_field(&self.cid)
    }

    /// Full identity check (steps 2-5 of the crate docs), given the digest is right.
    pub fn verify_identity(&self, registry: &DidRegistry) -> Result<(), IdError> {
        self.friction.verify(&self.attestation)?;
        if !self.auth.human_verified {
            return Err(IdError::NotEligible);
        }
        if !registry.matches(&self.auth.did, &self.auth.public_key) {
            return Err(IdError::UnknownIdentity);
        }
        if self.auth.vc_hash == [0; 32] {
            return Err(IdError::NoCredential);
        }
        verify_ed25519(&self.auth.public_key, &self.auth.signature, &self.digest)?;
        if self.cid.is_empty() {
            return Err(IdError::NoCid);
        }
        Ok(())
    }

    /// Gate decision for this frame.
    #[must_use]
    pub fn check(&self, registry: &DidRegistry) -> Verdict {
        if self.validate().is_err() {
            return Verdict::Malformed;
        }
        match self.triple.digest() {
            Ok(d) if d == self.digest => {}
            _ => return Verdict::Digest,
        }
        match self.verify_identity(registry) {
            Ok(()) => Verdict::Allow,
            Err(e) => Verdict::Identity(e),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::registry::DidRecord;
    use ed25519_dalek::{Signer, SigningKey};

    pub(crate) fn signed_frame() -> (Frame, DidRegistry) {
        let sender = SigningKey::from_bytes(&[1; 32]);
        let verifier = SigningKey::from_bytes(&[2; 32]);
        let triple = Triple {
            subject: "file:report".into(),
            predicate: "shareWith".into(),
            object: "did:aeon:bob".into(),
        };
        let digest = triple.digest().unwrap();
        let friction = Challenge::issue(42, [9; 32]).unwrap();
        let attestation = Attestation::sign(&friction, &verifier);
        let frame = Frame {
            flags: 0,
            digest,
            triple,
            auth: AuthPayload {
                did: "did:aeon:alice".into(),
                public_key: sender.verifying_key().to_bytes(),
                signature: sender.sign(&digest).to_bytes(),
                vc_hash: [5; 32],
                human_verified: true,
            },
            friction,
            attestation,
            cid: "bafy-demo".into(),
        };
        let mut reg = DidRegistry::new(4).unwrap();
        reg.add(DidRecord {
            did: "did:aeon:alice".into(),
            public_key: sender.verifying_key().to_bytes(),
            human_verified: true,
            active: true,
        })
        .unwrap();
        (frame, reg)
    }

    #[test]
    fn canonical_form() {
        let t = Triple {
            subject: "a".into(),
            predicate: "b".into(),
            object: "c".into(),
        };
        assert_eq!(t.canonical().unwrap(), b"S=a\nP=b\nO=c\n");
        let bad = Triple {
            subject: "a\nP=x".into(),
            ..t
        };
        assert_eq!(bad.digest(), Err(IdError::BadField));
    }

    #[test]
    fn good_frame_allowed() {
        let (f, reg) = signed_frame();
        assert_eq!(f.check(&reg), Verdict::Allow);
    }

    #[test]
    fn tampering_is_caught() {
        let (f, reg) = signed_frame();
        let mut t = f.clone();
        t.triple.object = "did:aeon:mallory".into();
        assert_eq!(t.check(&reg), Verdict::Digest);

        let mut s = f.clone();
        s.auth.signature[0] ^= 1;
        assert_eq!(s.check(&reg), Verdict::Identity(IdError::Signature));

        let mut v = f.clone();
        v.auth.vc_hash = [0; 32];
        assert_eq!(v.check(&reg), Verdict::Identity(IdError::NoCredential));

        let mut a = f.clone();
        a.attestation.context_hash[0] ^= 1;
        assert_eq!(a.check(&reg), Verdict::Identity(IdError::Friction));

        let empty = DidRegistry::new(1).unwrap();
        assert_eq!(f.check(&empty), Verdict::Identity(IdError::UnknownIdentity));
    }
}
