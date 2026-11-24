//! The security view, derived only from the audit chain and the approval queue. It reports what
//! the log shows; it does not guess about anything the log does not record.

use mouli_interface::Workspace;
use mouli_screen::screens::{Finding, Severity};

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
    let mut score: u32 = 0;
    for f in &findings {
        score += match f.severity {
            Severity::Low => 5,
            Severity::Medium => 20,
            Severity::High => 40,
            Severity::Critical => 70,
        };
    }
    (score.min(100) as u8, findings)
}
