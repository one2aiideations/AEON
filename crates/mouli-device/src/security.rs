//! The security view, derived only from the audit chain and the approval queue. It reports what
//! the log shows; it does not guess about anything the log does not record.

use crate::rulepack;
use mouli_interface::Workspace;
use mouli_screen::screens::{Finding, Severity};

/// How many refusals by one actor within [`BURST_WINDOW`] records count as a burst.
const BURST_REFUSALS: usize = 4;
/// How many consecutive audit records make up a stretch.
const BURST_WINDOW: usize = 20;

/// Local hours (from, up to but not including) that count as the small hours.
const ODD_FROM_HOUR: u64 = 0;
const ODD_TO_HOUR: u64 = 5;
/// How many records by one person in the small hours, within the day before the newest record,
/// make a finding.
const ODD_MIN_RECORDS: usize = 3;
/// The look-back, in seconds.
const ODD_LOOKBACK: u64 = 24 * 3600;

/// The local hour of day (0 to 23) for a Unix time and a zone.
fn local_hour(at: u64, utc_offset_min: i32) -> u64 {
    let local = i64::try_from(at).unwrap_or(i64::MAX / 2) + i64::from(utc_offset_min) * 60;
    u64::try_from(local.rem_euclid(86_400)).unwrap_or(0) / 3600
}

/// People who acted in the small hours (00:00 to 04:59 local) at least [`ODD_MIN_RECORDS`] times
/// in the day up to the newest timed record. Only records that carry a wall-clock time count
/// (older records and records from a machine whose clock was not set have none), the host's own
/// events do not, and the newest record sets "now" so the result does not depend on when it is
/// worked out. It says what the log shows, not that anything is wrong: a person may simply work
/// at night, so it is a low-severity finding that can be acknowledged.
fn odd_hours(records: &[mouli_interface::audit::AuditRecord], utc_offset_min: i32) -> Vec<Finding> {
    let Some(newest) = records.iter().map(|r| r.at).max().filter(|&t| t != 0) else {
        return Vec::new();
    };
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for r in records.iter().filter(|r| {
        r.at != 0
            && r.at + ODD_LOOKBACK >= newest
            && r.actor != "host"
            && r.actor != "-"
            && !r.action.starts_with("finding.")
            && (ODD_FROM_HOUR..ODD_TO_HOUR).contains(&local_hour(r.at, utc_offset_min))
    }) {
        *counts.entry(r.actor.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= ODD_MIN_RECORDS)
        .map(|(actor, n)| Finding {
            id: format!("odd-hours-{actor}"),
            severity: Severity::Low,
            title: format!("{actor} was active in the small hours (00:00 to 05:00)"),
            detail: format!(
                "{n} records by {actor} in the last day were written between 00:00 and 05:00 local time. That may be normal for them; if not, look at the Audit screen and search for {actor}. The time zone is set in Settings."
            ),
            acknowledged: false,
        })
        .collect()
}

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

pub(crate) fn assess(ws: &Workspace, utc_offset_min: i32) -> (u8, Vec<Finding>) {
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
    // Findings that are rules over the audit records (see rulepack.rs).
    let pack = rulepack::default_pack();
    let mut pack_matches: std::collections::BTreeMap<String, usize> = Default::default();
    if let Ok(hits) = rulepack::evaluate_pack(&pack, audit.records()) {
        for hit in hits {
            pack_matches.insert(hit.rule.id.to_string(), hit.count);
            findings.push(Finding {
                id: hit.rule.id.into(),
                severity: hit.severity(),
                title: hit.title(),
                detail: hit.detail(),
                acknowledged: false,
            });
        }
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
    // were not locked out because something succeeded in between.
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
    findings.extend(odd_hours(audit.records(), utc_offset_min));
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
    // Each finding that still counts gets an inherent risk from the shared scoring module; the
    // device score combines them. Acknowledged findings stop counting unless they are critical.
    let scores: Vec<f64> = findings
        .iter()
        .filter(|f| !f.acknowledged || f.severity == Severity::Critical)
        .map(|f| {
            let matches = pack_matches.get(&f.id).copied().unwrap_or(1);
            rulepack::finding_score(f.severity, matches)
        })
        .collect();
    let score = rulepack::combine(&scores).round() as u32;
    (score.min(100) as u8, findings)
}

/// A plain-text evidence file for one finding: the finding, the audit records that led to it,
/// and the head of the chain at the time.
pub(crate) fn evidence(ws: &Workspace, f: &Finding, by: &str) -> String {
    let records = ws.audit().records();
    let id = f.id.as_str();
    let related = |r: &mouli_interface::audit::AuditRecord| -> bool {
        if let Some(actor) = id.strip_prefix("odd-hours-") {
            // Everything the person did that carries a time, so the hours can be read off.
            r.actor == actor && r.at != 0
        } else if let Some(actor) = id
            .strip_prefix("lockout-")
            .or_else(|| id.strip_prefix("burst-"))
        {
            r.actor == actor && (r.outcome.starts_with("denied") || r.action == "lockout.clear")
        } else {
            match id {
                "denied" => r.outcome.starts_with("denied") || r.outcome.starts_with("rejected"),
                "failures" => r.outcome.starts_with("failed"),
                "checkpoint" => r.action == "checkpoint.failed",
                "approvals" => r.action.contains("approv"),
                _ => false,
            }
        }
    };
    let mut picked: Vec<&mouli_interface::audit::AuditRecord> =
        records.iter().filter(|r| related(r)).collect();
    // A finding with no obvious rows (the broken chain, say) gets the last few records.
    let general = picked.is_empty();
    if general {
        picked = records.iter().rev().take(20).rev().collect();
    }
    let mut out = String::new();
    out.push_str("MOULI evidence for a security finding\n");
    out.push_str(&format!("finding:   {} ({})\n", f.id, f.severity.label()));
    out.push_str(&format!("title:     {}\n", f.title));
    out.push_str(&format!("detail:    {}\n", f.detail));
    out.push_str(&format!("exported by: {by}\n"));
    out.push_str(&format!("records in the log: {}\n", records.len()));
    out.push_str(&format!(
        "chain head: {}\n",
        records.last().map_or("(empty)", |r| r.hash.as_str())
    ));
    out.push_str(&match ws.audit().verify() {
        Ok(()) => "chain check: every record matches the one before it\n".to_string(),
        Err(i) => format!("chain check: BROKEN at record {}\n", i + 1),
    });
    out.push_str(&format!(
        "\n{} ({} records)\nseq\ttick\tunix_time\tactor\taction\tsubject\toutcome\thash\n",
        if general {
            "the last records"
        } else {
            "records behind this finding"
        },
        picked.len()
    ));
    for r in picked {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.seq + 1,
            r.tick,
            if r.at == 0 {
                "-".to_string()
            } else {
                r.at.to_string()
            },
            r.actor,
            r.action,
            r.subject.replace(['\n', '\t'], " "),
            r.outcome,
            r.hash
        ));
    }
    out
}

#[cfg(test)]
mod odd_hours_tests {
    use super::*;
    use mouli_interface::audit::AuditRecord;

    fn rec(seq: u64, actor: &str, at: u64) -> AuditRecord {
        AuditRecord {
            seq,
            tick: seq,
            at,
            actor: actor.into(),
            action: "chat".into(),
            subject: "-".into(),
            outcome: "ok".into(),
            prev: String::new(),
            hash: String::new(),
        }
    }

    /// Unix time for 2026-11-27 at `h:m` UTC.
    fn at(h: u64, m: u64) -> u64 {
        1_795_824_000 + h * 3600 + m * 60
    }

    #[test]
    fn the_local_hour_follows_the_zone() {
        assert_eq!(local_hour(at(2, 30), 0), 2);
        assert_eq!(local_hour(at(2, 30), 330), 8, "UTC+05:30");
        assert_eq!(
            local_hour(at(2, 30), -180),
            23,
            "UTC-03:00 wraps to the day before"
        );
        assert_eq!(local_hour(at(23, 59), 120), 1, "UTC+02:00 wraps forward");
        assert_eq!(local_hour(0, 0), 0);
    }

    #[test]
    fn three_small_hours_records_in_a_day_make_a_finding_and_fewer_do_not() {
        let night: Vec<_> = (0..3).map(|i| rec(i, "gus", at(2, i))).collect();
        let f = odd_hours(&night, 0);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].id, "odd-hours-gus");
        assert_eq!(f[0].severity, Severity::Low);
        assert!(odd_hours(&night[..2], 0).is_empty(), "two is not enough");
        // The same instants are daytime in a zone eight hours ahead.
        assert!(odd_hours(&night, 480).is_empty());
        // And the boundaries: 05:00 is not small hours, 00:00 is.
        let edge = [
            rec(0, "a", at(5, 0)),
            rec(1, "a", at(5, 1)),
            rec(2, "a", at(5, 2)),
        ];
        assert!(odd_hours(&edge, 0).is_empty());
        let edge = [
            rec(0, "a", at(0, 0)),
            rec(1, "a", at(0, 1)),
            rec(2, "a", at(4, 59)),
        ];
        assert_eq!(odd_hours(&edge, 0).len(), 1);
    }

    #[test]
    fn records_without_a_time_old_records_and_host_events_do_not_count() {
        let untimed: Vec<_> = (0..5).map(|i| rec(i, "gus", 0)).collect();
        assert!(odd_hours(&untimed, 0).is_empty());
        assert!(odd_hours(&[], 0).is_empty());
        let mut host: Vec<_> = (0..4).map(|i| rec(i, "host", at(2, i))).collect();
        assert!(odd_hours(&host, 0).is_empty());
        host.iter_mut().for_each(|r| r.actor = "gus".into());
        host[0].action = "finding.ack".into();
        assert_eq!(odd_hours(&host, 0).len(), 1, "three real records remain");
        assert!(
            odd_hours(&host[..3], 0).is_empty(),
            "the finding.ack record is not counted"
        );
        // Nights more than a day before the newest record are history, not a finding.
        let mut old: Vec<_> = (0..3).map(|i| rec(i, "gus", at(2, i))).collect();
        old.push(rec(3, "ann", at(2, 0) + 3 * 86_400));
        assert!(odd_hours(&old, 0).is_empty());
        // Each person is judged on their own records.
        let mixed = [
            rec(0, "gus", at(1, 0)),
            rec(1, "ann", at(1, 1)),
            rec(2, "gus", at(1, 2)),
            rec(3, "ann", at(1, 3)),
            rec(4, "gus", at(1, 4)),
        ];
        let f = odd_hours(&mixed, 0);
        assert_eq!(
            f.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["odd-hours-gus"]
        );
    }
}
