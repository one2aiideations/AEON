//! Governance: constitution rules, budgets and quotas.
//!
//! The constitution is an ordered list of rules over action names
//! (`"files.delete"`, `"net.post"`, `"payments.send"`). The first matching
//! rule decides; no match means the default verdict. Deny always beats an
//! approval: an approval only satisfies `RequireApproval`.

use std::collections::{BTreeMap, VecDeque};

/// What a rule says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// May run.
    Allow,
    /// Must ask the user first.
    RequireApproval,
    /// Never.
    Deny,
}

/// One rule. `pattern` matches an exact action, or a prefix ending in `*`
/// (`"files.*"`), or everything (`"*"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Pattern.
    pub pattern: String,
    /// Verdict.
    pub verdict: Verdict,
    /// Why (shown to the user).
    pub reason: String,
}

fn matches(pattern: &str, action: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => action.starts_with(prefix),
        None => pattern == action,
    }
}

/// Ordered rule set.
#[derive(Debug, Clone)]
pub struct Constitution {
    rules: Vec<Rule>,
    /// Verdict when no rule matches.
    pub default: Verdict,
}

impl Default for Constitution {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            default: Verdict::RequireApproval,
        }
    }
}

impl Constitution {
    /// Empty constitution; unknown actions need approval.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a rule (earlier rules take precedence).
    pub fn rule(&mut self, pattern: &str, verdict: Verdict, reason: &str) -> &mut Self {
        self.rules.push(Rule {
            pattern: pattern.to_owned(),
            verdict,
            reason: reason.to_owned(),
        });
        self
    }

    /// Verdict and reason for `action`.
    #[must_use]
    pub fn judge(&self, action: &str) -> (Verdict, &str) {
        self.rules
            .iter()
            .find(|r| matches(&r.pattern, action))
            .map_or((self.default, "no rule matched"), |r| {
                (r.verdict, r.reason.as_str())
            })
    }

    /// Final permission given whether the user approved this action.
    #[must_use]
    pub fn permits(&self, action: &str, approved: bool) -> bool {
        match self.judge(action).0 {
            Verdict::Allow => true,
            Verdict::RequireApproval => approved,
            Verdict::Deny => false,
        }
    }
}

/// Spending result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spend {
    /// Charged.
    Ok,
    /// Charged, but past the warning threshold.
    Warn,
    /// Refused (hard stop).
    Refused,
}

/// Per-period spending budget (e.g. API credits, energy units).
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    /// Limit per period.
    pub limit: f64,
    /// Refuse spending past the limit (otherwise only warn).
    pub hard_stop: bool,
    /// Warn at this fraction of the limit.
    pub warn_at: f64,
    spent: f64,
    period: u64,
}

impl Budget {
    /// Budget with `limit` per period.
    #[must_use]
    pub fn new(limit: f64, hard_stop: bool) -> Self {
        Self {
            limit,
            hard_stop,
            warn_at: 0.8,
            spent: 0.0,
            period: 0,
        }
    }

    /// Try to spend `amount` in `period` (a new period resets the total).
    pub fn spend(&mut self, period: u64, amount: f64) -> Spend {
        if period != self.period {
            self.period = period;
            self.spent = 0.0;
        }
        let amount = amount.max(0.0);
        if self.hard_stop && self.spent + amount > self.limit {
            return Spend::Refused;
        }
        self.spent += amount;
        if self.spent >= self.limit * self.warn_at {
            Spend::Warn
        } else {
            Spend::Ok
        }
    }

    /// Spent this period.
    #[must_use]
    pub fn spent(&self) -> f64 {
        self.spent
    }
}

/// Sliding-window call quota per key (user, agent or capability).
#[derive(Debug, Clone)]
pub struct Quota {
    window: u64,
    limit: usize,
    calls: BTreeMap<String, VecDeque<u64>>,
}

impl Quota {
    /// At most `limit` calls per `window` time units per key.
    #[must_use]
    pub fn new(limit: usize, window: u64) -> Self {
        Self {
            window,
            limit,
            calls: BTreeMap::new(),
        }
    }

    /// Record a call at `now` if allowed.
    pub fn allow(&mut self, key: &str, now: u64) -> bool {
        let q = self.calls.entry(key.to_owned()).or_default();
        while q
            .front()
            .is_some_and(|&t| now.saturating_sub(t) >= self.window)
        {
            q.pop_front();
        }
        if q.len() >= self.limit {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_rule_wins() {
        let mut c = Constitution::new();
        c.rule("payments.*", Verdict::Deny, "agents never move money")
            .rule("files.read", Verdict::Allow, "reading is safe")
            .rule("files.*", Verdict::RequireApproval, "changes need the user");
        assert_eq!(c.judge("payments.send").0, Verdict::Deny);
        assert!(
            !c.permits("payments.send", true),
            "approval cannot override deny"
        );
        assert!(c.permits("files.read", false));
        assert!(!c.permits("files.delete", false));
        assert!(c.permits("files.delete", true));
        assert_eq!(
            c.judge("net.post"),
            (Verdict::RequireApproval, "no rule matched")
        );
    }

    #[test]
    fn budget_and_quota() {
        let mut b = Budget::new(10.0, true);
        assert_eq!(b.spend(1, 5.0), Spend::Ok);
        assert_eq!(b.spend(1, 3.5), Spend::Warn);
        assert_eq!(b.spend(1, 2.0), Spend::Refused);
        assert_eq!(b.spend(2, 2.0), Spend::Ok, "new period resets");
        let mut soft = Budget::new(1.0, false);
        assert_eq!(soft.spend(0, 5.0), Spend::Warn);

        let mut q = Quota::new(2, 60);
        assert!(q.allow("agent:1", 0));
        assert!(q.allow("agent:1", 10));
        assert!(!q.allow("agent:1", 20));
        assert!(q.allow("agent:2", 20));
        assert!(q.allow("agent:1", 60));
    }
}
