//! Registry of decentralised identifiers (DIDs) and their Ed25519 keys.

use crate::{check_field, IdError};

/// One registered identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DidRecord {
    /// The DID string.
    pub did: String,
    /// Ed25519 public key.
    pub public_key: [u8; 32],
    /// A human was verified behind this identity.
    pub human_verified: bool,
    /// Record is in use.
    pub active: bool,
}

/// Bounded DID registry.
#[derive(Debug, Clone)]
pub struct DidRegistry {
    records: Vec<DidRecord>,
    capacity: usize,
}

impl DidRegistry {
    /// Empty registry with room for `capacity` records.
    pub fn new(capacity: usize) -> Result<Self, IdError> {
        if capacity == 0 {
            return Err(IdError::BadArg);
        }
        Ok(Self {
            records: Vec::new(),
            capacity,
        })
    }

    /// Add an active, human-verified record.
    pub fn add(&mut self, rec: DidRecord) -> Result<(), IdError> {
        check_field(&rec.did)?;
        if !rec.active || !rec.human_verified {
            return Err(IdError::NotEligible);
        }
        if self.records.len() >= self.capacity {
            return Err(IdError::Full);
        }
        if self.find(&rec.did).is_some() {
            return Err(IdError::Duplicate);
        }
        self.records.push(rec);
        Ok(())
    }

    /// Look up a DID.
    #[must_use]
    pub fn find(&self, did: &str) -> Option<&DidRecord> {
        self.records.iter().find(|r| r.did == did)
    }

    /// Mark a DID inactive (revocation). Returns whether it existed.
    pub fn revoke(&mut self, did: &str) -> bool {
        match self.records.iter_mut().find(|r| r.did == did) {
            Some(r) => {
                r.active = false;
                true
            }
            None => false,
        }
    }

    /// True when `did` is registered, active, human-verified and uses `key`.
    #[must_use]
    pub fn matches(&self, did: &str, key: &[u8; 32]) -> bool {
        self.find(did)
            .is_some_and(|r| r.active && r.human_verified && &r.public_key == key)
    }

    /// Number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(did: &str) -> DidRecord {
        DidRecord {
            did: did.into(),
            public_key: [7; 32],
            human_verified: true,
            active: true,
        }
    }

    #[test]
    fn add_find_revoke() {
        let mut r = DidRegistry::new(2).unwrap();
        r.add(rec("did:aeon:a")).unwrap();
        assert_eq!(r.add(rec("did:aeon:a")), Err(IdError::Duplicate));
        r.add(rec("did:aeon:b")).unwrap();
        assert_eq!(r.add(rec("did:aeon:c")), Err(IdError::Full));
        assert!(r.matches("did:aeon:a", &[7; 32]));
        assert!(!r.matches("did:aeon:a", &[8; 32]));
        assert!(r.revoke("did:aeon:a"));
        assert!(!r.matches("did:aeon:a", &[7; 32]));
    }

    #[test]
    fn rejects_unverified() {
        let mut r = DidRegistry::new(1).unwrap();
        let mut x = rec("did:aeon:x");
        x.human_verified = false;
        assert_eq!(r.add(x), Err(IdError::NotEligible));
        assert_eq!(r.add(rec("")), Err(IdError::BadField));
    }
}
