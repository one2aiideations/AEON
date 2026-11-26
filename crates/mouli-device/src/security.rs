//! The security view, derived only from the audit chain and the approval queue. It reports what
//! the log shows; it does not guess about anything the log does not record.

use mouli_interface::Workspace;
use mouli_screen::screens::{Finding, Severity};

/// How many refusals by one actor within [`BURST_WINDOW`] records count as a burst.
const BURST_REFUSALS: usize = 4;
/// How many consecutive audit records make up a stretch.
const BURST_WINDOW: usize = 20;

/// The title of `id` as it was when it was last acknowledged.
fn acknowledged_as<'a>(
    records: &'a [mouli_interface::audit::AuditRecord],
    id: &str,
) -> Option<&'a str> {
    records
        .iter()
        .rev()
        .find(|r| {
            r.action == "finding.ack"
                && r.outcome == "ok"
                && r.subject.starts_with(&format!("{id}|"))
        })
        .and_then(|r| r.subject.split_once('|').map(|(_, t)| t))
}

pub(crate) fn assess(ws: &Workspace) -> (u8, Vec<Finding>) {
    let mut findings = Vec::new();
    let audit = ws.audit();
    if let Err(i) = audit.verify() {
        findings.push(Finding {
            id: "audit-chain".into(),
            severity: Severity::Critical,
            title: "The audit log has been altered".into(),
            detail: format!(
                "Record {} no longer matches the record before it. The history from that point cannot be trusted.",
                i + 1
            ),
            acknowledged: false,
        });
    }
    if let Some(r) = audit
        .records()
        .iter()
        .rev()
        .find(|r| r.action == "checkpoint.failed")
    {
        findings.push(Finding {
            id: "checkpoint".into(),
            severity: Severity::Critical,
            title: "The audit log does not match its saved checkpoint".into(),
            detail: format!(
                "At start-up, {}. Someone may have edited or cut the log file. Treat the history before this record as unproven.",
                r.subject
            ),
            acknowledged: false,
        });
    }
    let denied = audit
        .records()
        .iter()
        .filter(|r| r.outcome.starts_with("denied") || r.outcome.starts_with("rejected"))
        .count();
    if denied > 0 {
        let severity = if denied >= 5 {
            Severity::High
        } else {
            Severity::Medium
        };
        findings.push(Finding {
            id: "denied".into(),
            severity,
            title: format!("{denied} request(s) were refused by policy"),
            detail: "Look at the Audit screen for who asked and what was refused. A burst of refusals can mean someone is probing what they may do.".into(),
            acknowledged: false,
        });
    }
    let failed = audit
        .records()
        .iter()
        .filter(|r| r.outcome.starts_with("failed"))
        .count();
    if failed > 0 {
        findings.push(Finding {
            id: "failures".into(),
            severity: Severity::Low,
            title: format!("{failed} action(s) failed"),
            detail: "These ended in an error rather than a refusal; see the Audit screen.".into(),
            acknowledged: false,
        });
    }
    for user in ws.locked_out() {
        findings.push(Finding {
            id: format!("lockout-{user}"),
            severity: Severity::High,
            title: format!("{user} is locked out after repeated refusals"),
            detail: format!(
                "{user} was refused {} times in a row and cannot act until someone who can approve types /unlock {user}.",
                mouli_interface::LOCKOUT_REFUSALS
            ),
            acknowledged: false,
        });
    }
    // Anomaly: one actor refused many times within a short stretch of the log, even if they
    // were not locked out because something succeeded in between. (The log records order, not
    // wall-clock time, so "odd hours" cannot be judged.)
    let locked = ws.locked_out();
    let recent = audit.records();
    let mut burst: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for w in recent.windows(BURST_WINDOW.min(recent.len().max(1))) {
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for r in w.iter().filter(|r| r.outcome.starts_with("denied")) {
            *counts.entry(r.actor.as_str()).or_default() += 1;
        }
        burst.extend(
            counts
                .into_iter()
                .filter(|(_, n)| *n >= BURST_REFUSALS)
                .map(|(a, _)| a),
        );
    }
    for actor in burst.into_iter().filter(|a| !locked.iter().any(|l| l == a)) {
        findings.push(Finding {
            id: format!("burst-{actor}"),
            severity: Severity::High,
            title: format!("{actor} was refused {BURST_REFUSALS} or more times in a short stretch"),
            detail: format!(
                "Within {BURST_WINDOW} consecutive log records {actor} hit the policy {BURST_REFUSALS} times. That can mean someone is probing what they may do. Look at the Audit screen and search for {actor}."
            ),
            acknowledged: false,
        });
    }
    let waiting = ws.pending_approvals().len();
    if waiting > 0 {
        findings.push(Finding {
            id: "approvals".into(),
            severity: Severity::Medium,
            title: format!("{waiting} request(s) waiting for approval"),
            detail: "Risky actions are held until a second person decides. Open Approvals (F3)."
                .into(),
            acknowledged: false,
        });
    }
    // A finding someone acknowledged stays acknowledged while it says the same thing; if it
    // changes (more refusals, say) it comes back unacknowledged. The record lives in the audit
    // chain, so it survives a restart. A critical finding can be acknowledged but still counts.
    for f in &mut findings {
        f.acknowledged = acknowledged_as(audit.records(), &f.id).is_some_and(|t| t == f.title);
    }
    let mut score: u32 = 0;
    for f in findings
        .iter()
        .filter(|f| !f.acknowledged || f.severity == Severity::Critical)
    {
        score += match f.severity {
            Severity::Low => 5,
            Severity::Medium => 20,
            Severity::High => 40,
            Severity::Critical => 70,
        };
    }
    (score.min(100) as u8, findings)
}
