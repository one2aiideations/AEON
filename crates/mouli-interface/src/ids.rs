//! Deterministic identifier generator (no clock or randomness, so runs are reproducible).

/// Hands out `prefix-000001` style identifiers. Every call returns a fresh value, unlike the
/// Python original whose dataclass defaults were evaluated once and shared by all instances.
#[derive(Debug, Default, Clone)]
pub struct IdGen {
    next: u64,
}

impl IdGen {
    /// Start counting at one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes sure identifiers handed out from now on are newer than `id` (a saved identifier such
    /// as `apr-000007`), so a restored item can never collide with a new one.
    pub fn observe(&mut self, id: &str) {
        if let Some(n) = id.rsplit('-').next().and_then(|d| d.parse::<u64>().ok()) {
            self.next = self.next.max(n);
        }
    }

    /// Next identifier with the given prefix.
    pub fn next(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{:06}", self.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_ids_are_never_reused() {
        let mut g = IdGen::new();
        g.observe("apr-000007");
        g.observe("ses-000003");
        g.observe("junk");
        assert_eq!(g.next("apr"), "apr-000008");
    }

    #[test]
    fn ids_are_unique_and_prefixed() {
        let mut g = IdGen::new();
        let (a, b) = (g.next("msg"), g.next("msg"));
        assert_ne!(a, b);
        assert_eq!(a, "msg-000001");
        assert!(g.next("art").starts_with("art-"));
    }
}
