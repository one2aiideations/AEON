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
    fn ids_are_unique_and_prefixed() {
        let mut g = IdGen::new();
        let (a, b) = (g.next("msg"), g.next("msg"));
        assert_ne!(a, b);
        assert_eq!(a, "msg-000001");
        assert!(g.next("art").starts_with("art-"));
    }
}
