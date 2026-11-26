//! End to end: change log -> wire codec -> replay guard -> merge -> acknowledgement -> checkpoint.
//!
//! Replica 1 owns a change log; replica 2 pulls batches over the wire protocol, rejects replays,
//! merges, and acknowledges. Transport, authentication and durable storage are outside these
//! primitives, so the "network" here is a byte vector that the tests duplicate, corrupt and reorder.

use mouli_agentic::agentmem::repl_checkpoint::{
    CheckpointConfig, CheckpointKey, CheckpointManager,
};
use mouli_agentic::agentmem::repl_log::{
    ChangeKind, MemoryChange, ReplicationConfig, ReplicationError, ReplicationLog,
};
use mouli_agentic::agentmem::repl_merge::{
    ApplyOutcome, MergeConfig, MutationKind, ReplicatedMutation, ReplicationMerge,
};
use mouli_agentic::agentmem::replay_guard::{ReplayDecision, ReplayGuard};
use mouli_agentic::agentmem::wire::{decode, encode, CodecError, Limits, MemoryRecord, Message};
use sha2::{Digest, Sha256};

const TENANT: u64 = 7;
const SOURCE: u64 = 1;
const DEST: u64 = 2;
const TENANT_NAME: &str = "tenant-7";

fn digest(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

fn to_record(c: &MemoryChange) -> MemoryRecord {
    MemoryRecord {
        memory_id: format!("{:032x}", c.memory_id),
        version: c.sequence,
        updated_at_ms: c.sequence * 10,
        deleted: c.kind == ChangeKind::Delete,
        digest: digest(&c.payload),
        payload: c.payload.clone(),
    }
}

/// The receiver turns a record back into a mutation, refusing one whose payload does not match its
/// digest (the frame checksum guards the transport; this guards the record itself).
fn to_mutation(r: &MemoryRecord) -> Result<ReplicatedMutation, &'static str> {
    if digest(&r.payload) != r.digest {
        return Err("digest mismatch");
    }
    let memory_id = u128::from_str_radix(&r.memory_id, 16).map_err(|_| "bad memory id")?;
    Ok(ReplicatedMutation {
        tenant_id: TENANT,
        memory_id,
        origin_replica: SOURCE,
        origin_sequence: r.version,
        logical_timestamp_ns: r.updated_at_ms * 1_000_000,
        kind: if r.deleted {
            MutationKind::Delete
        } else {
            MutationKind::Upsert
        },
        payload: r.payload.clone(),
    })
}

/// A receiving replica.
struct Receiver {
    guard: ReplayGuard,
    merge: ReplicationMerge,
}

impl Receiver {
    fn new() -> Self {
        Self {
            guard: ReplayGuard::new(64, 16).unwrap(),
            merge: ReplicationMerge::new(MergeConfig {
                max_records: 1000,
                max_payload_bytes: 1 << 20,
                max_origins: 16,
            })
            .unwrap(),
        }
    }

    /// Handles one frame of records: returns how many were applied. Replays are skipped.
    fn receive(&mut self, frame: &[u8]) -> Result<usize, String> {
        let msg = decode(frame, &Limits::default()).map_err(|e| format!("{e:?}"))?;
        let Message::Records { records, .. } = msg else {
            return Err("not a records frame".into());
        };
        let mut applied = 0;
        for r in &records {
            match self
                .guard
                .check_and_record(TENANT_NAME, "replica-1", r.version)
                .map_err(|e| format!("{e:?}"))?
            {
                ReplayDecision::Accepted => {}
                ReplayDecision::Duplicate | ReplayDecision::TooOld => continue,
            }
            let m = to_mutation(r).map_err(str::to_string)?;
            match self.merge.apply(m).map_err(|e| format!("{e:?}"))? {
                ApplyOutcome::Applied | ApplyOutcome::Replaced => applied += 1,
                ApplyOutcome::IgnoredStale | ApplyOutcome::Duplicate => {}
            }
        }
        Ok(applied)
    }
}

fn source_log(n: u64) -> ReplicationLog {
    let mut log = ReplicationLog::new(ReplicationConfig {
        max_events_per_tenant: 1000,
        max_payload_bytes: 1 << 20,
    })
    .unwrap();
    for i in 1..=n {
        let memory_id = u128::from(i % 3) + 1; // three memories, updated repeatedly
        let kind = if i % 7 == 0 {
            ChangeKind::Delete
        } else {
            ChangeKind::Upsert
        };
        let payload = if kind == ChangeKind::Delete {
            Vec::new()
        } else {
            format!("value {i}").into_bytes()
        };
        assert_eq!(log.append(TENANT, memory_id, kind, payload).unwrap(), i);
    }
    log
}

fn records_frame(changes: &[MemoryChange], request_id: u64) -> Vec<u8> {
    let msg = Message::Records {
        tenant_id: TENANT_NAME.into(),
        request_id,
        first_sequence: changes.first().map_or(0, |c| c.sequence),
        records: changes.iter().map(to_record).collect(),
    };
    encode(&msg, &Limits::default()).unwrap()
}

#[test]
fn a_replica_catches_up_in_batches_and_ends_with_the_same_state() {
    let log = source_log(23);
    let mut rx = Receiver::new();
    let mut checkpoints = CheckpointManager::new(CheckpointConfig { max_checkpoints: 8 }).unwrap();
    let key = CheckpointKey {
        tenant_id: TENANT,
        source_replica: SOURCE,
        destination_replica: DEST,
    };
    checkpoints.acknowledge(key, 0, 0).unwrap();

    let mut request_id = 0;
    let mut frames = 0;
    while let Some((start, end)) = checkpoints
        .next_batch(key, log.latest_sequence(TENANT), 5)
        .unwrap()
    {
        request_id += 1;
        // Source side: read the batch and put it on the wire.
        let batch = log
            .read_after(TENANT, start - 1, (end - start + 1) as usize)
            .unwrap();
        assert_eq!(batch.changes.first().map(|c| c.sequence), Some(start));
        let frame = records_frame(&batch.changes, request_id);
        // Destination side: decode, guard, merge.
        assert!(rx.receive(&frame).unwrap() > 0);
        // Destination acknowledges over the wire; the source decodes it and moves the checkpoint.
        let ack = encode(
            &Message::Acknowledge {
                tenant_id: TENANT_NAME.into(),
                request_id,
                through_sequence: batch.next_cursor,
            },
            &Limits::default(),
        )
        .unwrap();
        let Message::Acknowledge {
            through_sequence, ..
        } = decode(&ack, &Limits::default()).unwrap()
        else {
            panic!()
        };
        checkpoints
            .acknowledge(key, through_sequence, request_id)
            .unwrap();
        frames += 1;
    }
    assert_eq!(frames, 5, "23 changes in batches of 5");
    assert_eq!(checkpoints.get(key).unwrap().applied_sequence, 23);

    // The truth, computed independently from the log: the last change for each memory id wins.
    let all = log.read_after(TENANT, 0, 1000).unwrap().changes;
    for id in 1u128..=3 {
        let last = all.iter().rev().find(|c| c.memory_id == id).unwrap();
        let got = rx.merge.get(TENANT, id).expect("replicated");
        assert_eq!(got.origin_sequence, last.sequence, "memory {id}");
        assert_eq!(got.payload, last.payload, "memory {id}");
        assert_eq!(
            got.kind == MutationKind::Delete,
            last.kind == ChangeKind::Delete,
            "memory {id}"
        );
    }
    assert_eq!(rx.merge.record_count(), 3);
    assert_eq!(rx.merge.watermark(TENANT, SOURCE), 23);
}

#[test]
fn a_replayed_frame_changes_nothing_and_the_merge_would_also_refuse_it() {
    let log = source_log(6);
    let changes = log.read_after(TENANT, 0, 100).unwrap().changes;
    let frame = records_frame(&changes, 1);
    let mut rx = Receiver::new();
    assert_eq!(
        rx.receive(&frame).unwrap(),
        changes.iter().filter(|_| true).count().min(6)
    );
    let before = (rx.merge.watermark(TENANT, SOURCE), rx.merge.record_count());
    // The same bytes again: the guard sees every sequence a second time.
    assert_eq!(rx.receive(&frame).unwrap(), 0);
    assert_eq!(
        (rx.merge.watermark(TENANT, SOURCE), rx.merge.record_count()),
        before
    );
    for c in &changes {
        assert_eq!(
            rx.guard
                .check_and_record(TENANT_NAME, "replica-1", c.sequence)
                .unwrap(),
            ReplayDecision::Duplicate
        );
    }
    // Defence in depth: with the guard bypassed, the merge's watermark still refuses a replay.
    let m = to_mutation(&to_record(&changes[0])).unwrap();
    assert_eq!(rx.merge.apply(m).unwrap(), ApplyOutcome::Duplicate);
}

#[test]
fn flipping_any_single_bit_of_a_frame_is_never_applied() {
    let log = source_log(3);
    let changes = log.read_after(TENANT, 0, 10).unwrap().changes;
    let frame = records_frame(&changes, 1);
    let limits = Limits::default();
    let original = decode(&frame, &limits).unwrap();
    let mut accepted_unchanged = 0;
    for byte in 0..frame.len() {
        for bit in 0..8 {
            let mut bad = frame.clone();
            bad[byte] ^= 1 << bit;
            match decode(&bad, &limits) {
                Err(_) => {}
                // If a corrupted frame still decodes, it must decode to exactly the original.
                Ok(m) => {
                    assert_eq!(
                        m, original,
                        "bit {bit} of byte {byte} changed the message without an error"
                    );
                    accepted_unchanged += 1;
                }
            }
        }
    }
    // Report, do not assume: how many flips were not noticed at all (and so changed nothing).
    assert!(
        accepted_unchanged <= 8,
        "{accepted_unchanged} bit flips went unnoticed"
    );
    // Truncation and trailing garbage are refused as well.
    for cut in 0..frame.len() {
        assert!(
            decode(&frame[..cut], &limits).is_err(),
            "truncated at {cut}"
        );
    }
    let mut longer = frame.clone();
    longer.push(0);
    assert!(decode(&longer, &limits).is_err());
}

#[test]
fn a_record_whose_payload_does_not_match_its_digest_is_refused_even_in_a_valid_frame() {
    let log = source_log(2);
    let mut changes = log.read_after(TENANT, 0, 10).unwrap().changes;
    let mut record = to_record(&changes[0]);
    record.payload = b"tampered after the digest was made".to_vec(); // frame is re-encoded, so its checksum is fine
    changes.truncate(1);
    let msg = Message::Records {
        tenant_id: TENANT_NAME.into(),
        request_id: 1,
        first_sequence: 1,
        records: vec![record],
    };
    let frame = encode(&msg, &Limits::default()).unwrap();
    let mut rx = Receiver::new();
    assert_eq!(rx.receive(&frame).unwrap_err(), "digest mismatch");
    assert_eq!(rx.merge.record_count(), 0, "nothing was applied");
}

#[test]
fn out_of_order_delivery_loses_updates_unless_the_receiver_reorders() {
    // The guard accepts out-of-order sequences inside its window, but the merge treats any
    // sequence at or below the origin's watermark as a duplicate. So a frame delivered late is
    // accepted by the guard and then silently dropped by the merge. Documented here; the receiver
    // must hold frames back until they are contiguous.
    let mut log = ReplicationLog::new(ReplicationConfig::default()).unwrap();
    // Six different memories, so nothing later overwrites what the early frame carries.
    for (i, id) in [10u128, 11, 12, 20, 21, 22].into_iter().enumerate() {
        log.append(TENANT, id, ChangeKind::Upsert, format!("v{i}").into_bytes())
            .unwrap();
    }
    let all = log.read_after(TENANT, 0, 100).unwrap().changes;
    let (first, second) = (records_frame(&all[..3], 1), records_frame(&all[3..], 2));

    let mut naive = Receiver::new();
    assert_eq!(naive.receive(&second).unwrap(), 3); // sequences 4..6 arrive first
    assert_eq!(
        naive.receive(&first).unwrap(),
        0,
        "1..3 arrive late and the merge drops them"
    );
    assert_eq!(
        naive
            .guard
            .retained_sequence_count(TENANT_NAME, "replica-1"),
        6,
        "the guard accepted all six"
    );
    assert_eq!(naive.merge.record_count(), 3, "but only three were applied");
    for id in [10u128, 11, 12] {
        assert!(
            naive.merge.get(TENANT, id).is_none(),
            "memory {id} was lost"
        );
    }

    // With a reorder buffer keyed by first sequence, the same two frames converge.
    let mut ordered = Receiver::new();
    let mut pending: std::collections::BTreeMap<u64, &Vec<u8>> = std::collections::BTreeMap::new();
    pending.insert(4, &second); // arrived first
    pending.insert(1, &first);
    let mut next = 1;
    while let Some((&start, _)) = pending.iter().next() {
        assert_eq!(start, next, "frames are applied only when contiguous");
        let frame = pending.remove(&start).unwrap();
        ordered.receive(frame).unwrap();
        next = ordered.merge.watermark(TENANT, SOURCE) + 1;
    }
    assert_eq!(ordered.merge.record_count(), 6);
    for c in &all {
        assert_eq!(
            ordered
                .merge
                .get(TENANT, c.memory_id)
                .unwrap()
                .origin_sequence,
            c.sequence
        );
    }
}

#[test]
fn a_cursor_that_fell_behind_retained_history_is_reported_not_papered_over() {
    let mut log = ReplicationLog::new(ReplicationConfig {
        max_events_per_tenant: 3,
        max_payload_bytes: 64,
    })
    .unwrap();
    for i in 1..=8u128 {
        log.append(TENANT, i, ChangeKind::Upsert, vec![i as u8])
            .unwrap();
    }
    assert_eq!(log.retained_count(TENANT), 3);
    // A replica that has seen up to 2 needs 3..; the log only has 6..8.
    assert_eq!(
        log.read_after(TENANT, 2, 10).unwrap_err(),
        ReplicationError::CursorExpired {
            oldest_available: 6
        }
    );
    // One that has seen up to 5 can continue.
    let b = log.read_after(TENANT, 5, 10).unwrap();
    assert_eq!(
        b.changes.iter().map(|c| c.sequence).collect::<Vec<_>>(),
        [6, 7, 8]
    );
    // An oversize payload is refused at the source.
    assert_eq!(
        log.append(TENANT, 9, ChangeKind::Upsert, vec![0; 65])
            .unwrap_err(),
        ReplicationError::PayloadTooLarge
    );
}

#[test]
fn the_wire_limits_hold_in_both_directions() {
    let log = source_log(4);
    let changes = log.read_after(TENANT, 0, 10).unwrap().changes;
    let tight = Limits {
        max_records: 2,
        ..Limits::default()
    };
    let msg = Message::Records {
        tenant_id: TENANT_NAME.into(),
        request_id: 1,
        first_sequence: 1,
        records: changes.iter().map(to_record).collect(),
    };
    assert_eq!(
        encode(&msg, &tight).unwrap_err(),
        CodecError::TooManyRecords
    );
    let frame = encode(&msg, &Limits::default()).unwrap();
    assert_eq!(
        decode(&frame, &tight).unwrap_err(),
        CodecError::TooManyRecords
    );
    let small = Limits {
        max_frame_bytes: 16,
        ..Limits::default()
    };
    assert_eq!(
        decode(&frame, &small).unwrap_err(),
        CodecError::FrameTooLarge
    );
}
