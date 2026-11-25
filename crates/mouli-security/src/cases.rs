//! Security cases: a strict state machine, deterministic triage and escalation, merging, and a
//! hash-chained history so any later edit of the record is detectable.

use crate::scoring::Level;
use sha2::{Digest, Sha256};
use std::fmt;

/// Where a case is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Just raised.
    New,
    /// Accepted for handling.
    Open,
    /// Priority has been set.
    Triaged,
    /// Owned by someone.
    Assigned,
    /// Being investigated.
    Investigating,
    /// Waiting for more information.
    PendingInformation,
    /// Waiting for an approval.
    PendingApproval,
    /// Fixes are being applied.
    Remediating,
    /// Resolved, awaiting closure.
    Resolved,
    /// Closed.
    Closed,
    /// Reopened after resolution or closure.
    Reopened,
    /// Cancelled (final).
    Cancelled,
    /// Merged into another case (final).
    Duplicate,
}

impl Status {
    /// The states this one may move to.
    pub fn next(self) -> &'static [Status] {
        use Status::*;
        match self {
            New => &[Open, Triaged, Cancelled, Duplicate],
            Open => &[Triaged, Assigned, Cancelled, Duplicate],
            Triaged => &[Assigned, Open, PendingApproval, Cancelled],
            Assigned => &[
                Investigating,
                PendingInformation,
                PendingApproval,
                Remediating,
                Cancelled,
            ],
            Investigating => &[PendingInformation, Remediating, Resolved, Cancelled],
            PendingInformation => &[Investigating, Assigned],
            PendingApproval => &[Remediating, Assigned, Resolved],
            Remediating => &[Resolved, Closed, PendingApproval],
            Resolved => &[Closed, Reopened],
            Closed => &[Reopened],
            Reopened => &[Open, Assigned, Investigating],
            Cancelled | Duplicate => &[],
        }
    }

    /// Whether this state may move to `to`.
    pub fn can_move_to(self, to: Status) -> bool {
        self.next().contains(&to)
    }

    /// True for states that accept no further changes at all.
    pub fn is_final(self) -> bool {
        matches!(self, Status::Cancelled | Status::Duplicate)
    }

    /// True when work on the case has ended (final, or closed).
    pub fn is_ended(self) -> bool {
        self.is_final() || self == Status::Closed
    }
}

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseError {
    /// A required text field is empty.
    EmptyField(&'static str),
    /// The state machine does not allow this move.
    InvalidTransition {
        /// Current state.
        from: Status,
        /// Requested state.
        to: Status,
    },
    /// This move needs a written reason.
    ReasonRequired,
    /// This move needs the case to have an assignee.
    NeedsAssignee,
    /// The case has ended and cannot take this action.
    Ended(Status),
    /// A case cannot be merged into itself.
    SameCase,
}

impl fmt::Display for CaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaseError::EmptyField(n) => write!(f, "{n} must not be empty"),
            CaseError::InvalidTransition { from, to } => {
                write!(f, "cannot move a case from {from:?} to {to:?}")
            }
            CaseError::ReasonRequired => write!(f, "a reason is required for this change"),
            CaseError::NeedsAssignee => write!(f, "the case needs an assignee first"),
            CaseError::Ended(s) => write!(f, "the case has ended ({s:?})"),
            CaseError::SameCase => write!(f, "a case cannot be merged into itself"),
        }
    }
}

impl std::error::Error for CaseError {}

/// What a history entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// The case was created.
    Created,
    /// The state changed.
    StatusChanged,
    /// An assignee was set.
    Assigned,
    /// Priority was set by triage.
    Triaged,
    /// Priority was raised to critical.
    Escalated,
    /// Cases were merged.
    Merged,
    /// A comment was added.
    Commented,
}

/// One tamper-evident history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Position in the history, from 1.
    pub seq: u64,
    /// When it happened (epoch seconds).
    pub at: i64,
    /// Who did it.
    pub actor: String,
    /// What kind of entry.
    pub kind: EventKind,
    /// Details.
    pub detail: String,
    /// Hash of the previous entry (empty for the first).
    pub prev_hash: String,
    /// Hash of this entry, covering all other fields and `prev_hash`.
    pub hash: String,
}

fn entry_hash(prev: &str, seq: u64, at: i64, actor: &str, kind: EventKind, detail: &str) -> String {
    let mut h = Sha256::new();
    // Length-prefixed fields so no field can bleed into the next.
    for part in [
        prev,
        &seq.to_string(),
        &at.to_string(),
        actor,
        &format!("{kind:?}"),
        detail,
    ] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    crate::evidence::hex(&h.finalize())
}

/// How two cases relate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relation {
    /// This case was merged into, or received, the other.
    MergedWith(String),
}

/// A security case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Case identifier.
    pub id: String,
    /// Short title.
    pub title: String,
    /// How bad the underlying problem is.
    pub severity: Level,
    /// How urgently it is handled.
    pub priority: Level,
    /// Current state.
    pub status: Status,
    /// Who owns it.
    pub assignee: Option<String>,
    /// Links to other cases.
    pub related: Vec<Relation>,
    events: Vec<Event>,
}

fn need(text: &str, name: &'static str) -> Result<(), CaseError> {
    if text.trim().is_empty() {
        Err(CaseError::EmptyField(name))
    } else {
        Ok(())
    }
}

impl Case {
    /// Opens a case in state [`Status::New`] with Medium priority.
    pub fn new(
        id: &str,
        title: &str,
        severity: Level,
        actor: &str,
        now: i64,
    ) -> Result<Case, CaseError> {
        need(id, "id")?;
        need(title, "title")?;
        need(actor, "actor")?;
        let mut c = Case {
            id: id.to_string(),
            title: title.to_string(),
            severity,
            priority: Level::Medium,
            status: Status::New,
            assignee: None,
            related: Vec::new(),
            events: Vec::new(),
        };
        c.record(
            now,
            actor,
            EventKind::Created,
            format!("created with severity {severity:?}"),
        );
        Ok(c)
    }

    /// Opens a case from a detection signal: critical signals start at High priority.
    pub fn from_signal(
        id: &str,
        source: &str,
        title: &str,
        severity: Level,
        actor: &str,
        now: i64,
    ) -> Result<Case, CaseError> {
        need(source, "source")?;
        let mut c = Case::new(id, title, severity, actor, now)?;
        if severity == Level::Critical {
            c.priority = Level::High;
        }
        c.record(
            now,
            actor,
            EventKind::Created,
            format!("raised from signal source {source}"),
        );
        Ok(c)
    }

    fn record(&mut self, at: i64, actor: &str, kind: EventKind, detail: String) {
        let seq = self.events.len() as u64 + 1;
        let prev = self.events.last().map_or(String::new(), |e| e.hash.clone());
        let hash = entry_hash(&prev, seq, at, actor, kind, &detail);
        self.events.push(Event {
            seq,
            at,
            actor: actor.to_string(),
            kind,
            detail,
            prev_hash: prev,
            hash,
        });
    }

    /// The history, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Checks the history chain. On failure returns the `seq` of the first bad entry.
    pub fn verify_history(&self) -> Result<(), u64> {
        let mut prev = String::new();
        for (i, e) in self.events.iter().enumerate() {
            let ok = e.seq == i as u64 + 1
                && e.prev_hash == prev
                && e.hash == entry_hash(&e.prev_hash, e.seq, e.at, &e.actor, e.kind, &e.detail);
            if !ok {
                return Err(e.seq);
            }
            prev = e.hash.clone();
        }
        Ok(())
    }

    fn guard_open(&self) -> Result<(), CaseError> {
        if self.status.is_ended() {
            Err(CaseError::Ended(self.status))
        } else {
            Ok(())
        }
    }

    /// Moves the case to `to`. Cancelling or reopening needs a reason; moving into Assigned,
    /// Investigating or Remediating needs an assignee.
    pub fn change_status(
        &mut self,
        to: Status,
        actor: &str,
        reason: &str,
        now: i64,
    ) -> Result<(), CaseError> {
        need(actor, "actor")?;
        if !self.status.can_move_to(to) {
            return Err(CaseError::InvalidTransition {
                from: self.status,
                to,
            });
        }
        if matches!(to, Status::Cancelled | Status::Reopened) && reason.trim().is_empty() {
            return Err(CaseError::ReasonRequired);
        }
        if matches!(
            to,
            Status::Assigned | Status::Investigating | Status::Remediating
        ) && self.assignee.is_none()
        {
            return Err(CaseError::NeedsAssignee);
        }
        let from = self.status;
        self.status = to;
        self.record(
            now,
            actor,
            EventKind::StatusChanged,
            format!("{from:?} -> {to:?}: {}", reason.trim()),
        );
        Ok(())
    }

    /// Sets the assignee and moves the case to Assigned (when the state machine allows it).
    pub fn assign(&mut self, assignee: &str, actor: &str, now: i64) -> Result<(), CaseError> {
        need(assignee, "assignee")?;
        need(actor, "actor")?;
        if !self.status.can_move_to(Status::Assigned) {
            return Err(CaseError::InvalidTransition {
                from: self.status,
                to: Status::Assigned,
            });
        }
        self.assignee = Some(assignee.to_string());
        self.record(
            now,
            actor,
            EventKind::Assigned,
            format!("assigned to {assignee}"),
        );
        self.change_status(Status::Assigned, actor, "assigned", now)
    }

    /// Deterministic triage: High and Critical severities lift the priority to at least their own
    /// level; lower severities leave the priority alone.
    pub fn triage(&mut self, actor: &str, now: i64) -> Result<Level, CaseError> {
        need(actor, "actor")?;
        self.guard_open()?;
        if self.severity >= Level::High && self.priority < self.severity {
            self.priority = self.severity;
        }
        self.record(
            now,
            actor,
            EventKind::Triaged,
            format!("priority {:?}", self.priority),
        );
        Ok(self.priority)
    }

    /// Raises the priority to Critical. Needs a reason, and an open case.
    pub fn escalate(&mut self, reason: &str, actor: &str, now: i64) -> Result<(), CaseError> {
        need(actor, "actor")?;
        if reason.trim().is_empty() {
            return Err(CaseError::ReasonRequired);
        }
        self.guard_open()?;
        self.priority = Level::Critical;
        self.record(now, actor, EventKind::Escalated, reason.trim().to_string());
        Ok(())
    }

    /// Adds a comment to the history.
    pub fn comment(&mut self, text: &str, actor: &str, now: i64) -> Result<(), CaseError> {
        need(text, "comment")?;
        need(actor, "actor")?;
        self.record(now, actor, EventKind::Commented, text.trim().to_string());
        Ok(())
    }

    /// Merges this case into `target`: this case becomes a Duplicate (only possible while the
    /// state machine allows it) and both link to each other. `target` must still be open.
    pub fn merge_into(
        &mut self,
        target: &mut Case,
        actor: &str,
        now: i64,
    ) -> Result<(), CaseError> {
        need(actor, "actor")?;
        if self.id == target.id {
            return Err(CaseError::SameCase);
        }
        target.guard_open()?;
        if !self.status.can_move_to(Status::Duplicate) {
            return Err(CaseError::InvalidTransition {
                from: self.status,
                to: Status::Duplicate,
            });
        }
        self.change_status(
            Status::Duplicate,
            actor,
            &format!("merged into {}", target.id),
            now,
        )?;
        self.related.push(Relation::MergedWith(target.id.clone()));
        target.related.push(Relation::MergedWith(self.id.clone()));
        let src = self.id.clone();
        self.record(
            now,
            actor,
            EventKind::Merged,
            format!("merged into {}", target.id),
        );
        target.record(now, actor, EventKind::Merged, format!("merged from {src}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Status::*;

    const ALL: [Status; 13] = [
        New,
        Open,
        Triaged,
        Assigned,
        Investigating,
        PendingInformation,
        PendingApproval,
        Remediating,
        Resolved,
        Closed,
        Reopened,
        Cancelled,
        Duplicate,
    ];

    fn case(sev: Level) -> Case {
        Case::new("C-1", "Suspicious login", sev, "sec-bot", 100).unwrap()
    }

    #[test]
    fn transition_table_is_exactly_as_specified() {
        let allowed = |s: Status| s.next().to_vec();
        assert_eq!(allowed(New), vec![Open, Triaged, Cancelled, Duplicate]);
        assert_eq!(allowed(Resolved), vec![Closed, Reopened]);
        assert_eq!(allowed(Closed), vec![Reopened]);
        assert!(allowed(Cancelled).is_empty() && allowed(Duplicate).is_empty());
        let edges: usize = ALL.iter().map(|s| s.next().len()).sum();
        assert_eq!(edges, 4 + 4 + 4 + 5 + 4 + 2 + 3 + 3 + 2 + 1 + 3);
    }

    #[test]
    fn no_state_moves_to_itself_or_lists_a_target_twice() {
        for s in ALL {
            assert!(!s.can_move_to(s), "{s:?}");
            let mut seen = std::collections::HashSet::new();
            assert!(s.next().iter().all(|t| seen.insert(*t)), "{s:?}");
        }
    }

    #[test]
    fn every_state_is_reachable_from_new() {
        let mut seen = std::collections::HashSet::from([New]);
        let mut stack = vec![New];
        while let Some(s) = stack.pop() {
            for t in s.next() {
                if seen.insert(*t) {
                    stack.push(*t);
                }
            }
        }
        assert_eq!(seen.len(), ALL.len());
    }

    #[test]
    fn illegal_moves_are_refused_and_change_nothing() {
        let mut c = case(Level::Low);
        let before = c.clone();
        assert_eq!(
            c.change_status(Closed, "a", "x", 1),
            Err(CaseError::InvalidTransition {
                from: New,
                to: Closed
            })
        );
        assert_eq!(c, before);
    }

    #[test]
    fn assignee_is_required_for_working_states() {
        let mut c = case(Level::Low);
        c.change_status(Open, "a", "", 1).unwrap();
        assert_eq!(
            c.change_status(Assigned, "a", "", 2),
            Err(CaseError::NeedsAssignee)
        );
        c.assign("alice", "a", 3).unwrap();
        assert_eq!((c.status, c.assignee.as_deref()), (Assigned, Some("alice")));
        c.change_status(Investigating, "alice", "", 4).unwrap();
    }

    #[test]
    fn full_lifecycle_with_reopen() {
        let mut c = case(Level::High);
        c.change_status(Open, "a", "", 1).unwrap();
        c.assign("alice", "a", 2).unwrap();
        c.change_status(Investigating, "alice", "", 3).unwrap();
        c.change_status(Resolved, "alice", "fixed", 4).unwrap();
        assert_eq!(
            c.change_status(Reopened, "bob", "  ", 5),
            Err(CaseError::ReasonRequired)
        );
        c.change_status(Closed, "alice", "", 5).unwrap();
        assert!(c.status.is_ended() && !c.status.is_final());
        c.change_status(Reopened, "bob", "recurred", 6).unwrap();
        c.change_status(Investigating, "alice", "", 7).unwrap();
        assert_eq!(c.verify_history(), Ok(()));
        assert_eq!(c.events().len(), 1 + 1 + 2 + 1 + 1 + 1 + 1 + 1);
    }

    #[test]
    fn cancel_needs_a_reason_and_is_final() {
        let mut c = case(Level::Low);
        assert_eq!(
            c.change_status(Cancelled, "a", "", 1),
            Err(CaseError::ReasonRequired)
        );
        c.change_status(Cancelled, "a", "false alarm", 1).unwrap();
        for to in ALL {
            assert!(c.change_status(to, "a", "r", 2).is_err());
        }
        assert_eq!(c.triage("a", 3), Err(CaseError::Ended(Cancelled)));
        assert_eq!(c.escalate("why", "a", 3), Err(CaseError::Ended(Cancelled)));
    }

    #[test]
    fn triage_lifts_only_high_severity() {
        let mut crit = case(Level::Critical);
        assert_eq!(crit.triage("a", 1), Ok(Level::Critical));
        let mut high = case(Level::High);
        assert_eq!(high.triage("a", 1), Ok(Level::High));
        let mut med = case(Level::Medium);
        assert_eq!(med.triage("a", 1), Ok(Level::Medium));
        let mut low = case(Level::Low);
        assert_eq!(low.triage("a", 1), Ok(Level::Medium));
        // Never lowers an already-raised priority.
        let mut raised = case(Level::High);
        raised.escalate("boss said so", "a", 1).unwrap();
        assert_eq!(raised.triage("a", 2), Ok(Level::Critical));
    }

    #[test]
    fn escalation_needs_a_reason_and_sets_critical() {
        let mut c = case(Level::Low);
        assert_eq!(c.escalate(" ", "a", 1), Err(CaseError::ReasonRequired));
        c.escalate("data leaving the network", "a", 1).unwrap();
        assert_eq!(c.priority, Level::Critical);
        assert_eq!(c.events().last().unwrap().kind, EventKind::Escalated);
    }

    #[test]
    fn signals_start_at_the_right_priority() {
        let crit = Case::from_signal(
            "C-2",
            "anomaly-engine",
            "Odd payment",
            Level::Critical,
            "bot",
            1,
        )
        .unwrap();
        assert_eq!(
            (crit.priority, crit.severity),
            (Level::High, Level::Critical)
        );
        let low = Case::from_signal("C-3", "anomaly-engine", "Odd payment", Level::Low, "bot", 1)
            .unwrap();
        assert_eq!(low.priority, Level::Medium);
        assert_eq!(
            Case::from_signal("C-4", " ", "t", Level::Low, "bot", 1),
            Err(CaseError::EmptyField("source"))
        );
    }

    #[test]
    fn empty_fields_are_refused() {
        assert_eq!(
            Case::new("", "t", Level::Low, "a", 1),
            Err(CaseError::EmptyField("id"))
        );
        assert_eq!(
            Case::new("i", " ", Level::Low, "a", 1),
            Err(CaseError::EmptyField("title"))
        );
        assert_eq!(
            Case::new("i", "t", Level::Low, "", 1),
            Err(CaseError::EmptyField("actor"))
        );
        let mut c = case(Level::Low);
        assert_eq!(c.comment("", "a", 1), Err(CaseError::EmptyField("comment")));
        assert_eq!(c.assign("", "a", 1), Err(CaseError::EmptyField("assignee")));
    }

    #[test]
    fn merge_links_both_cases_and_marks_the_source() {
        let mut a = Case::new("A", "dup", Level::Low, "x", 1).unwrap();
        let mut b = Case::new("B", "main", Level::Low, "x", 1).unwrap();
        a.merge_into(&mut b, "x", 2).unwrap();
        assert_eq!(a.status, Duplicate);
        assert_eq!(a.related, vec![Relation::MergedWith("B".into())]);
        assert_eq!(b.related, vec![Relation::MergedWith("A".into())]);
        assert_eq!(b.status, New);
        assert_eq!(a.verify_history(), Ok(()));
        assert_eq!(b.verify_history(), Ok(()));
    }

    #[test]
    fn merge_refusals_leave_both_cases_untouched() {
        let mut a = Case::new("A", "x", Level::Low, "x", 1).unwrap();
        let mut same = a.clone();
        assert_eq!(a.merge_into(&mut same, "x", 2), Err(CaseError::SameCase));
        let mut deep = Case::new("D", "x", Level::Low, "x", 1).unwrap();
        deep.change_status(Open, "x", "", 1).unwrap();
        deep.assign("al", "x", 2).unwrap();
        let mut b = Case::new("B", "x", Level::Low, "x", 1).unwrap();
        let (deep0, b0) = (deep.clone(), b.clone());
        // Assigned cases can no longer be marked Duplicate.
        assert!(matches!(
            deep.merge_into(&mut b, "x", 3),
            Err(CaseError::InvalidTransition { .. })
        ));
        assert_eq!((&deep, &b), (&deep0, &b0));
        let mut closed = Case::new("Z", "x", Level::Low, "x", 1).unwrap();
        closed.change_status(Cancelled, "x", "no", 1).unwrap();
        let mut src = Case::new("S", "x", Level::Low, "x", 1).unwrap();
        assert_eq!(
            src.merge_into(&mut closed, "x", 2),
            Err(CaseError::Ended(Cancelled))
        );
        assert_eq!(src.status, New);
    }

    #[test]
    fn history_chain_detects_every_kind_of_tampering() {
        let mut c = case(Level::Low);
        c.change_status(Open, "a", "", 1).unwrap();
        c.comment("looked at logs", "a", 2).unwrap();
        c.comment("nothing found", "a", 3).unwrap();
        assert_eq!(c.verify_history(), Ok(()));
        let orig = c.clone();
        let mut edited = orig.clone();
        edited.events[1].detail = "Open -> Closed".into();
        assert_eq!(edited.verify_history(), Err(2));
        let mut actor = orig.clone();
        actor.events[2].actor = "mallory".into();
        assert_eq!(actor.verify_history(), Err(3));
        let mut time = orig.clone();
        time.events[3].at += 1;
        assert_eq!(time.verify_history(), Err(4));
        let mut removed = orig.clone();
        removed.events.remove(1);
        // Entry 3 is now the first one that does not follow its predecessor.
        assert_eq!(removed.verify_history(), Err(3));
        let mut reordered = orig.clone();
        reordered.events.swap(1, 2);
        assert!(reordered.verify_history().is_err());
        let mut truncated_tail_ok = orig.clone();
        truncated_tail_ok.events.pop();
        // Dropping the newest entries is not detectable from the chain alone.
        assert_eq!(truncated_tail_ok.verify_history(), Ok(()));
    }

    #[test]
    fn identical_histories_hash_identically_and_fields_cannot_bleed() {
        let a = Case::new("C", "t", Level::Low, "ab", 5).unwrap();
        let b = Case::new("C", "t", Level::Low, "ab", 5).unwrap();
        assert_eq!(a.events()[0].hash, b.events()[0].hash);
        assert_ne!(
            entry_hash("", 1, 1, "ab", EventKind::Commented, "c"),
            entry_hash("", 1, 1, "a", EventKind::Commented, "bc")
        );
    }
}
