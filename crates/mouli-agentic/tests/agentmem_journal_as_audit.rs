//! Evidence for the evaluation "is the agent-memory journal usable as the device's audit file
//! format?" (the answer is no, for the reasons these tests pin down). The journal is a good
//! in-memory record of memory operations; the device's audit trail is the hash-chained log.

use mouli_agentic::agentmem::journal::{AuditJournal, AuditOutcome, JournalError, MemoryOperation};

fn append(j: &mut AuditJournal, t: u64) -> Result<u64, JournalError> {
    j.append(
        t,
        1,
        7,
        MemoryOperation::Read,
        Some(1),
        AuditOutcome::Allowed,
        "ok",
    )
}

#[test]
fn a_full_journal_silently_drops_its_oldest_events() {
    // For an audit trail, losing the oldest events without a trace is the wrong default: the
    // record of an early action disappears once enough later ones are written, and nothing in
    // the journal says that anything was lost (only `oldest_sequence` moves).
    let mut j = AuditJournal::new(3, 32).unwrap();
    for t in 1..=5 {
        append(&mut j, t).unwrap();
    }
    assert_eq!(j.len(), 3);
    assert_eq!(j.oldest_sequence(), Some(3), "sequences 1 and 2 are gone");
    assert_eq!(j.newest_sequence(), Some(5));
    let kept = j.read_range(1, 1, 5).unwrap();
    assert_eq!(
        kept.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [3, 4, 5]
    );
    // Asking for the lost range returns an empty list, not an error: a reader cannot tell
    // "nothing happened" from "it was evicted".
    assert!(j.read_range(1, 1, 2).unwrap().is_empty());
}

#[test]
fn an_event_is_refused_when_the_clock_goes_backwards_so_the_action_would_go_unrecorded() {
    // A device whose clock is stepped back (or set after boot) would have its audit writes
    // rejected, so a caller that must record every action cannot use `append` as it stands.
    let mut j = AuditJournal::new(8, 32).unwrap();
    append(&mut j, 1_000).unwrap();
    assert_eq!(append(&mut j, 999), Err(JournalError::InvalidTimeOrder));
    assert_eq!(j.len(), 1, "the later action left no record");
    // The same instant is fine, and time moving forward again resumes normally.
    append(&mut j, 1_000).unwrap();
    append(&mut j, 2_000).unwrap();
    assert_eq!(j.len(), 3);
}

#[test]
fn an_event_names_the_principal_only_as_a_number_and_cannot_carry_other_actions() {
    // `principal_id` is a u64 and the operation is one of eight memory verbs, so the device's
    // sign-in, patch, account and approval events do not fit without extending the type, and the
    // account name would have to be kept elsewhere.
    let mut j = AuditJournal::new(8, 32).unwrap();
    j.append(
        1,
        1,
        0xDEAD_BEEF,
        MemoryOperation::Export,
        None,
        AuditOutcome::Denied,
        "no permission",
    )
    .unwrap();
    let e = &j.read_range(1, 1, 1).unwrap()[0];
    assert_eq!(e.principal_id, 0xDEAD_BEEF);
    assert_eq!(e.operation, MemoryOperation::Export);
    // The reason is bounded; a longer one is refused, not shortened.
    assert_eq!(
        j.append(
            2,
            1,
            1,
            MemoryOperation::Read,
            None,
            AuditOutcome::Failed,
            &"x".repeat(33)
        ),
        Err(JournalError::ReasonTooLarge)
    );
}

#[test]
fn events_are_scoped_by_tenant_and_never_hold_memory_contents() {
    // The good parts, kept: reads are tenant-scoped, and an event has no field for the content
    // of a memory, so the journal cannot leak what it records.
    let mut j = AuditJournal::new(8, 32).unwrap();
    j.append(
        1,
        1,
        1,
        MemoryOperation::Read,
        Some(10),
        AuditOutcome::Allowed,
        "a",
    )
    .unwrap();
    j.append(
        2,
        2,
        1,
        MemoryOperation::Read,
        Some(20),
        AuditOutcome::Allowed,
        "b",
    )
    .unwrap();
    assert_eq!(j.read_range(1, 1, 10).unwrap().len(), 1);
    assert_eq!(j.read_range(2, 1, 10).unwrap()[0].memory_id, Some(20));
    assert_eq!(j.read_range(3, 1, 10).unwrap().len(), 0);
}
