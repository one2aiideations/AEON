//! Append-only, hash-chained audit log.

use crate::error::hex;
use sha2::{Digest, Sha256};

/// One audited action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// Position in the log, from zero.
    pub seq: u64,
    /// Logical time.
    pub tick: u64,
    /// Who acted.
    pub actor: String,
    /// What was attempted.
    pub action: String,
    /// What it acted on.
    pub subject: String,
    /// `ok`, `denied`, `pending` and so on.
    pub outcome: String,
    /// Hash of the previous record (64 zeros for the first).
    pub prev: String,
    /// Hash of this record.
    pub hash: String,
}

fn digest(r: &AuditRecord) -> String {
    let text = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        r.seq, r.tick, r.actor, r.action, r.subject, r.outcome, r.prev
    );
    hex(&Sha256::digest(text.as_bytes()))
}

/// The log. Records can be added but not changed through this API.
#[derive(Debug, Default, Clone)]
pub struct AuditLog {
    records: Vec<AuditRecord>,
}

impl AuditLog {
    /// Empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A log holding exactly these records, for loading a saved log. Nothing is checked here:
    /// call [`AuditLog::verify`] to learn whether the chain is intact, and keep a damaged log
    /// as it is so the damage stays visible.
    #[must_use]
    pub fn from_records(records: Vec<AuditRecord>) -> Self {
        Self { records }
    }

    /// Append a record and return it.
    pub fn append(
        &mut self,
        tick: u64,
        actor: &str,
        action: &str,
        subject: &str,
        outcome: &str,
    ) -> &AuditRecord {
        let prev = self
            .records
            .last()
            .map_or_else(|| "0".repeat(64), |r| r.hash.clone());
        let mut r = AuditRecord {
            seq: self.records.len() as u64,
            tick,
            actor: actor.into(),
            action: action.into(),
            subject: subject.into(),
            outcome: outcome.into(),
            prev,
            hash: String::new(),
        };
        r.hash = digest(&r);
        self.records.push(r);
        &self.records[self.records.len() - 1]
    }

    /// All records.
    #[must_use]
    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }

    /// Check the chain.
    ///
    /// # Errors
    /// Returns the index of the first record whose hash or link is wrong.
    pub fn verify(&self) -> Result<(), usize> {
        let mut prev = "0".repeat(64);
        for (i, r) in self.records.iter().enumerate() {
            if r.seq != i as u64 || r.prev != prev || r.hash != digest(r) {
                return Err(i);
            }
            prev.clone_from(&r.hash);
        }
        Ok(())
    }

    #[cfg(test)]
    fn tamper(&mut self, i: usize) {
        self.records[i].outcome = "ok".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_verifies_and_detects_tampering() {
        let mut l = AuditLog::new();
        assert!(l.verify().is_ok());
        l.append(1, "u", "mail.send", "m1", "denied");
        l.append(2, "u", "mail.send", "m2", "ok");
        l.append(3, "u", "sheet.read", "s1", "ok");
        assert!(l.verify().is_ok());
        assert_eq!(l.records()[1].prev, l.records()[0].hash);
        l.tamper(0);
        assert_eq!(l.verify(), Err(0));
    }
}
