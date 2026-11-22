//! Per-host token-bucket rate limiter.

use std::collections::HashMap;

/// Token buckets keyed by host. Time is in milliseconds.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    capacity: f64,
    per_ms: f64,
    buckets: HashMap<String, (f64, u64)>,
}

impl RateLimiter {
    /// Allow `per_minute` requests per host, bursting up to `burst`.
    #[must_use]
    pub fn new(per_minute: u32, burst: u32) -> Self {
        Self {
            capacity: f64::from(burst.max(1)),
            per_ms: f64::from(per_minute) / 60_000.0,
            buckets: HashMap::new(),
        }
    }

    /// Take one token for `host` at `now_ms`. Returns false when limited.
    pub fn allow(&mut self, host: &str, now_ms: u64) -> bool {
        let cap = self.capacity;
        let (tokens, last) = self
            .buckets
            .entry(host.to_ascii_lowercase())
            .or_insert((cap, now_ms));
        let elapsed = now_ms.saturating_sub(*last) as f64;
        *tokens = (*tokens + elapsed * self.per_ms).min(cap);
        *last = (*last).max(now_ms);
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursts_then_refills() {
        let mut r = RateLimiter::new(60, 2);
        assert!(r.allow("A.example", 0));
        assert!(r.allow("a.example", 0));
        assert!(!r.allow("a.example", 0));
        assert!(r.allow("b.example", 0));
        assert!(!r.allow("a.example", 500));
        assert!(r.allow("a.example", 1000));
    }
}
