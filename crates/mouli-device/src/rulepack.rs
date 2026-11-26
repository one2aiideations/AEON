//! Security findings written as data: rule trees over audit records, evaluated by the shared
//! `mouli-systems` rule engine, and scored by its scoring module.
//!
//! A rule matches individual audit records. A finding appears when at least one record matches;
//! its title carries the number of matches and its detail can quote the latest matching record.
//! Findings that depend on state other than the log (a person locked out, requests waiting for
//! approval, a broken chain) stay in code in `security.rs`.
//!
//! What this does not show: rules look at what the log records. They cannot see anything that was
//! never written to it, and they do not decide who may act.

use mouli_interface::audit::AuditRecord;
use mouli_screen::screens::Severity;
use mouli_systems::security::rules::{evaluate, RuleError};
use mouli_systems::security::scoring::{inherent, Factors, Weights};
use serde_json::{json, Value};

/// One finding defined as a rule over audit records.
pub(crate) struct PackRule {
    /// The finding id (stable across restarts; acknowledgements refer to it).
    pub id: &'static str,
    /// Title; `{n}` becomes the number of matching records.
    pub title: &'static str,
    /// Detail; `{subject}` becomes the subject of the latest matching record.
    pub detail: &'static str,
    /// Severity with a single match.
    pub severity: Severity,
    /// A higher severity once this many records match.
    pub escalate: Option<(usize, Severity)>,
    /// The rule tree (`mouli-systems` rule syntax) applied to each audit record.
    pub rule: Value,
}

/// A rule and how many records matched it.
pub(crate) struct Hit<'a> {
    pub rule: &'a PackRule,
    pub count: usize,
    pub last: Option<&'a AuditRecord>,
}

impl Hit<'_> {
    /// The severity after any escalation.
    pub(crate) fn severity(&self) -> Severity {
        match self.rule.escalate {
            Some((at, higher)) if self.count >= at => higher,
            _ => self.rule.severity,
        }
    }

    /// The finding title with the count filled in.
    pub(crate) fn title(&self) -> String {
        self.rule.title.replace("{n}", &self.count.to_string())
    }

    /// The finding detail with the latest matching subject filled in.
    pub(crate) fn detail(&self) -> String {
        self.rule
            .detail
            .replace("{subject}", self.last.map_or("", |r| r.subject.as_str()))
    }
}

fn leaf(field: &str, operator: &str, value: &str) -> Value {
    json!({"field": field, "operator": operator, "value": value})
}

/// The built-in findings.
pub(crate) fn default_pack() -> Vec<PackRule> {
    vec![
        PackRule {
            id: "checkpoint",
            title: "The audit log does not match its saved checkpoint",
            detail: "At start-up, {subject}. Someone may have edited or cut the log file. Treat the history before this record as unproven.",
            severity: Severity::Critical,
            escalate: None,
            rule: leaf("action", "eq", "checkpoint.failed"),
        },
        PackRule {
            id: "denied",
            title: "{n} request(s) were refused by policy",
            detail: "Look at the Audit screen for who asked and what was refused. A burst of refusals can mean someone is probing what they may do.",
            severity: Severity::Medium,
            escalate: Some((5, Severity::High)),
            rule: json!({"operator": "or", "conditions": [
                leaf("outcome", "contains", "denied"),
                leaf("outcome", "contains", "rejected"),
            ]}),
        },
        PackRule {
            id: "failures",
            title: "{n} action(s) failed",
            detail: "These ended in an error rather than a refusal; see the Audit screen.",
            severity: Severity::Low,
            escalate: None,
            rule: leaf("outcome", "contains", "failed"),
        },
    ]
}

/// A record as the rule engine sees it.
fn event(r: &AuditRecord) -> Value {
    json!({
        "seq": r.seq,
        "tick": r.tick,
        "actor": r.actor,
        "action": r.action,
        "subject": r.subject,
        "outcome": r.outcome,
    })
}

/// Runs every rule over every record. Rules with no match are left out.
///
/// # Errors
/// A [`RuleError`] when a rule is malformed.
pub(crate) fn evaluate_pack<'a>(
    pack: &'a [PackRule],
    records: &'a [AuditRecord],
) -> Result<Vec<Hit<'a>>, RuleError> {
    let events: Vec<Value> = records.iter().map(event).collect();
    let mut hits = Vec::new();
    for rule in pack {
        let mut count = 0;
        let mut last = None;
        for (record, ev) in records.iter().zip(&events) {
            if evaluate(&rule.rule, ev)?.matched {
                count += 1;
                last = Some(record);
            }
        }
        if count > 0 {
            hits.push(Hit { rule, count, last });
        }
    }
    Ok(hits)
}

/// Likelihood, impact, velocity and exposure for a severity; `matches` raises the velocity.
fn factors(severity: Severity, matches: usize) -> Factors {
    let (likelihood, impact, velocity, exposure) = match severity {
        Severity::Low => (2, 1, 0, 1),
        Severity::Medium => (3, 2, 1, 2),
        Severity::High => (4, 4, 3, 3),
        Severity::Critical => (5, 5, 4, 4),
    };
    Factors {
        likelihood,
        impact,
        velocity: velocity.max(u8::try_from((matches / 2).min(5)).unwrap_or(5)),
        exposure,
    }
}

/// The inherent risk (0 to 100) of a finding, by the shared scoring module.
pub(crate) fn finding_score(severity: Severity, matches: usize) -> f64 {
    inherent(factors(severity, matches), Weights::default()).unwrap_or(100.0)
}

/// Several findings combined into one score from 0 to 100: the chance that at least one of them
/// is a real problem if each score is read as a probability. It never exceeds 100 and only rises
/// when a finding is added.
pub(crate) fn combine(scores: &[f64]) -> f64 {
    let calm: f64 = scores
        .iter()
        // A score that is not a number counts as the worst case, never as no risk.
        .map(|s| {
            if s.is_finite() {
                1.0 - (s / 100.0).clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
        .product();
    100.0 * (1.0 - calm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, actor: &str, action: &str, subject: &str, outcome: &str) -> AuditRecord {
        AuditRecord {
            seq,
            tick: seq,
            actor: actor.into(),
            action: action.into(),
            subject: subject.into(),
            outcome: outcome.into(),
            prev: String::new(),
            hash: String::new(),
        }
    }

    #[test]
    fn every_built_in_rule_evaluates_without_error_on_any_record() {
        let pack = default_pack();
        let r = [record(0, "a", "x", "y", "ok")];
        assert!(evaluate_pack(&pack, &r).unwrap().is_empty());
    }

    #[test]
    fn refusals_are_counted_and_escalate_at_five() {
        let pack = default_pack();
        let mut records = Vec::new();
        for i in 0..4 {
            records.push(record(i, "gus", "note.send", "n", "denied: policy"));
        }
        let hits = evaluate_pack(&pack, &records).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].count, 4);
        assert_eq!(hits[0].severity(), Severity::Medium);
        assert_eq!(hits[0].title(), "4 request(s) were refused by policy");
        records.push(record(4, "gus", "note.send", "n", "rejected"));
        let hits = evaluate_pack(&pack, &records).unwrap();
        assert_eq!(hits[0].count, 5);
        assert_eq!(hits[0].severity(), Severity::High);
    }

    #[test]
    fn failures_and_checkpoint_alerts_match_their_own_records_only() {
        let pack = default_pack();
        let records = [
            record(0, "a", "chat", "m", "failed: timeout"),
            record(1, "a", "checkpoint.failed", "records were removed", "alert"),
            record(2, "a", "note.read", "n", "ok"),
        ];
        let hits = evaluate_pack(&pack, &records).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.rule.id).collect();
        assert_eq!(ids, ["checkpoint", "failures"]);
        assert_eq!(hits[0].severity(), Severity::Critical);
        assert!(hits[0].detail().contains("records were removed"));
        assert_eq!(hits[1].count, 1);
    }

    #[test]
    fn a_malformed_rule_is_an_error_not_a_silent_miss() {
        let bad = [PackRule {
            id: "bad",
            title: "t",
            detail: "d",
            severity: Severity::Low,
            escalate: None,
            rule: json!({"operator": "no_such_operator", "field": "outcome", "value": 1}),
        }];
        let r = [record(0, "a", "x", "y", "ok")];
        assert!(evaluate_pack(&bad, &r).is_err());
    }

    #[test]
    fn scores_rise_with_severity_and_velocity() {
        let low = finding_score(Severity::Low, 1);
        let medium = finding_score(Severity::Medium, 1);
        let high = finding_score(Severity::High, 1);
        let critical = finding_score(Severity::Critical, 1);
        assert!(low < medium && medium < high && high < critical);
        assert!(critical >= 70.0);
        assert!(finding_score(Severity::Low, 10) > finding_score(Severity::Low, 1));
    }

    #[test]
    fn combining_scores_never_exceeds_100_and_only_rises() {
        assert_eq!(combine(&[]), 0.0);
        let one = combine(&[40.0]);
        assert!((one - 40.0).abs() < 1e-9);
        let two = combine(&[40.0, 40.0]);
        assert!(two > one && two <= 100.0);
        assert!(combine(&[100.0, 50.0]) <= 100.0);
        assert!((combine(&[100.0]) - 100.0).abs() < 1e-9);
        assert!((combine(&[f64::NAN]) - 100.0).abs() < 1e-9);
        assert!((combine(&[f64::INFINITY, 10.0]) - 100.0).abs() < 1e-9);
    }
}
