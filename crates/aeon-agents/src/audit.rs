//! Append-only, hash-chained audit log.
//!
//! Each entry stores the SHA-256 of the previous entry, so editing or
//! removing any entry breaks [`AuditLog::verify`].

use sha2::{Digest, Sha256};

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Position in the log.
    pub seq: u64,
    /// Unix seconds.
    pub time: u64,
    /// Who acted (user, agent, system).
    pub actor: String,
    /// What was done.
    pub action: String,
    /// What it was done to.
    pub resource: String,
    /// Outcome text.
    pub outcome: String,
    /// Hash of the previous entry (zeros for the first).
    pub prev: [u8; 32],
    /// Hash of this entry.
    pub hash: [u8; 32],
}

fn field(h: &mut Sha256, s: &str) {
    h.update((s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

fn entry_hash(e: &Entry) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(e.seq.to_le_bytes());
    h.update(e.time.to_le_bytes());
    field(&mut h, &e.actor);
    field(&mut h, &e.action);
    field(&mut h, &e.resource);
    field(&mut h, &e.outcome);
    h.update(e.prev);
    h.finalize().into()
}

/// The log.
#[derive(Debug, Clone, Default)]
pub struct AuditLog {
    entries: Vec<Entry>,
}

impl AuditLog {
    /// Empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a record and return its hash.
    pub fn append(&mut self, time: u64, actor: &str, action: &str, resource: &str, outcome: &str) -> [u8; 32] {
        let prev = self.entries.last().map_or([0; 32], |e| e.hash);
        let mut e = Entry {
            seq: self.entries.len() as u64,
            time,
            actor: actor.to_owned(),
            action: action.to_owned(),
            resource: resource.to_owned(),
            outcome: outcome.to_owned(),
            prev,
            hash: [0; 32],
        };
        e.hash = entry_hash(&e);
        let h = e.hash;
        self.entries.push(e);
        h
    }

    /// Index of the first broken entry, or `None` if the chain is intact.
    #[must_use]
    pub fn verify(&self) -> Option<usize> {
        let mut prev = [0u8; 32];
        for (i, e) in self.entries.iter().enumerate() {
            if e.seq != i as u64 || e.prev != prev || entry_hash(e) != e.hash {
                return Some(i);
            }
            prev = e.hash;
        }
        None
    }

    /// All entries.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Mutable access (for repair tools and tamper tests).
    pub fn entries_mut(&mut self) -> &mut [Entry] {
        &mut self.entries
    }

    /// Hash of the newest entry (the value to anchor externally).
    #[must_use]
    pub fn head(&self) -> [u8; 32] {
        self.entries.last().map_or([0; 32], |e| e.hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_detects_tampering() {
        let mut log = AuditLog::new();
        log.append(1, "agent:mail", "summarise", "inbox", "ok");
        log.append(2, "user", "share", "file:report", "ok");
        log.append(3, "agent:sync", "upload", "photos", "denied");
        assert_eq!(log.verify(), None);
        log.entries_mut()[1].outcome = "denied".into();
        assert_eq!(log.verify(), Some(1));
    }

    #[test]
    fn field_boundaries_matter() {
        let mut a = AuditLog::new();
        let mut b = AuditLog::new();
        let ha = a.append(0, "ab", "c", "", "");
        let hb = b.append(0, "a", "bc", "", "");
        assert_ne!(ha, hb);
    }
}
