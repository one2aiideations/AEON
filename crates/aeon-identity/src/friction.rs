//! Human-presence friction: a challenge bound to a context, answered by a
//! verifier-signed attestation.
//!
//! The attestation signs `challenge (u64, little-endian) || context_hash`.

use crate::semantic::verify_ed25519;
use crate::IdError;
use ed25519_dalek::{Signer, SigningKey};

/// An issued challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Challenge {
    /// Non-zero nonce.
    pub nonce: u64,
    /// Hash of what the human is approving.
    pub context_hash: [u8; 32],
}

/// A verifier's signed confirmation that a human answered `nonce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attestation {
    /// Nonce answered.
    pub nonce: u64,
    /// Context the human approved.
    pub context_hash: [u8; 32],
    /// Verifier Ed25519 public key.
    pub verifier_key: [u8; 32],
    /// Signature over `message(nonce, context_hash)`.
    pub signature: [u8; 64],
}

/// Bytes signed by the verifier.
#[must_use]
pub fn message(nonce: u64, context_hash: &[u8; 32]) -> [u8; 40] {
    let mut m = [0u8; 40];
    m[..8].copy_from_slice(&nonce.to_le_bytes());
    m[8..].copy_from_slice(context_hash);
    m
}

impl Challenge {
    /// Issue a challenge; `nonce` must be non-zero.
    pub fn issue(nonce: u64, context_hash: [u8; 32]) -> Result<Self, IdError> {
        if nonce == 0 {
            return Err(IdError::BadArg);
        }
        Ok(Self {
            nonce,
            context_hash,
        })
    }

    /// Check an attestation answers this challenge and is validly signed.
    pub fn verify(&self, a: &Attestation) -> Result<(), IdError> {
        if self.nonce == 0 || a.nonce != self.nonce || a.context_hash != self.context_hash {
            return Err(IdError::Friction);
        }
        verify_ed25519(
            &a.verifier_key,
            &a.signature,
            &message(a.nonce, &a.context_hash),
        )
        .map_err(|_| IdError::Friction)
    }
}

impl Attestation {
    /// Sign an answer to `c` with the verifier key (used by the verifier service and tests).
    #[must_use]
    pub fn sign(c: &Challenge, verifier: &SigningKey) -> Self {
        Self {
            nonce: c.nonce,
            context_hash: c.context_hash,
            verifier_key: verifier.verifying_key().to_bytes(),
            signature: verifier
                .sign(&message(c.nonce, &c.context_hash))
                .to_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_round_trip() {
        let v = SigningKey::from_bytes(&[3; 32]);
        let c = Challenge::issue(7, [1; 32]).unwrap();
        let a = Attestation::sign(&c, &v);
        c.verify(&a).unwrap();
        let other = Challenge::issue(8, [1; 32]).unwrap();
        assert_eq!(other.verify(&a), Err(IdError::Friction));
        let mut bad = a;
        bad.signature[10] ^= 0xff;
        assert_eq!(c.verify(&bad), Err(IdError::Friction));
        assert_eq!(Challenge::issue(0, [0; 32]), Err(IdError::BadArg));
    }
}
