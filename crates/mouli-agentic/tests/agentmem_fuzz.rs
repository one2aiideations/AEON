//! Deterministic, structure-aware fuzzing of the agent-memory codecs: snapshot, wire,
//! checkpoint and checkpoint delta.
//!
//! Every codec ends in a checksum, so mutating bytes blindly gets rejected at the checksum and
//! never reaches the parser behind it. Each codec is therefore fuzzed in two phases:
//!
//! * raw: mutate a valid frame and decode it unchanged (checks the outer defences);
//! * sealed: mutate the body, fix the length field, and recompute the checksum, so the parser sees
//!   corrupted but correctly checksummed input.
//!
//! Required: decoding never panics (the failing input is printed as hex); a decoded value
//! re-encodes and decodes back to itself; and under sealing the checksum is never what rejects the
//! input (which also proves the sealing here matches the real checksum). This is not
//! coverage-guided fuzzing like cargo-fuzz; it is a fixed-seed regression harness.

use mouli_agentic::agentmem::checkpoint_codec::{
    self, CodecLimits as CpLimits, ReplayCheckpoint, StreamCheckpoint,
};
use mouli_agentic::agentmem::snapshot::{
    self, CodecLimits as SnapLimits, MemorySnapshot, SnapshotRecord,
};
use mouli_agentic::agentmem::wire::{self, Limits as WireLimits, MemoryRecord, Message};
use mouli_agentic::agentmem::x055::{CheckpointDelta, StreamChange, StreamKey};
use mouli_agentic::agentmem::x056::{self, DeltaCodecLimits};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::panic::{catch_unwind, AssertUnwindSafe};

// ---------------------------------------------------------------- random numbers and mutation

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const INTERESTING_U32: [u32; 9] = [
    0,
    1,
    2,
    0x7F,
    0xFF,
    0xFFFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

/// Applies one random structural mutation to `bytes` within `[lo, hi)` (or anywhere if empty).
fn mutate(bytes: &mut Vec<u8>, lo: usize, rng: &mut Rng) {
    let hi = bytes.len();
    if hi <= lo {
        bytes.push(rng.next() as u8);
        return;
    }
    let pos = lo + rng.below(hi - lo);
    match rng.below(8) {
        0 => bytes[pos] ^= 1 << rng.below(8),
        1 => bytes[pos] = rng.next() as u8,
        2 => bytes[pos] = [0x00, 0xFF, 0x7F, 0x80][rng.below(4)],
        3 => {
            let v = INTERESTING_U32[rng.below(INTERESTING_U32.len())].to_be_bytes();
            for (i, b) in v.iter().enumerate() {
                if pos + i < hi {
                    bytes[pos + i] = *b;
                }
            }
        }
        4 => {
            let n = 1 + rng.below(8);
            let at = pos.min(bytes.len());
            for _ in 0..n {
                bytes.insert(at, rng.next() as u8);
            }
        }
        5 => {
            let n = (1 + rng.below(8)).min(hi - pos);
            bytes.drain(pos..pos + n);
        }
        6 => {
            let n = (1 + rng.below(16)).min(hi - pos);
            let chunk: Vec<u8> = bytes[pos..pos + n].to_vec();
            let at = lo + rng.below(hi - lo + 1);
            for (i, b) in chunk.into_iter().enumerate() {
                bytes.insert((at + i).min(bytes.len()), b);
            }
        }
        _ => {
            let n = rng.below(bytes.len() - lo + 1);
            bytes.truncate(lo + n);
        }
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// How a codec frames and checksums its bytes.
#[derive(Clone, Copy)]
enum Seal {
    /// `magic(4) version(1) flags(1) body_len(u32 BE) body crc32(u32 BE)`.
    Crc32,
    /// `... body fnv1a64(u64 LE)`; no length field.
    Fnv64,
}

impl Seal {
    /// Where the body starts (mutations stay at or after this).
    const fn body_start(self) -> usize {
        match self {
            Seal::Crc32 => 10,
            Seal::Fnv64 => 8, // after the magic; the rest is header fields and body alike
        }
    }

    /// Makes `frame` structurally valid again: fixes the length field, then the checksum.
    fn reseal(self, frame: &mut Vec<u8>) {
        match self {
            Seal::Crc32 => {
                // `frame` currently holds header + body, without a checksum.
                let body = frame.len().saturating_sub(10) as u32;
                if frame.len() >= 10 {
                    frame[6..10].copy_from_slice(&body.to_be_bytes());
                }
                let c = crc32(frame);
                frame.extend_from_slice(&c.to_be_bytes());
            }
            Seal::Fnv64 => {
                let c = fnv1a64(frame);
                frame.extend_from_slice(&c.to_le_bytes());
            }
        }
    }

    /// The frame without its checksum.
    fn unsealed(self, frame: &[u8]) -> Vec<u8> {
        let tail = match self {
            Seal::Crc32 => 4,
            Seal::Fnv64 => 8,
        };
        frame[..frame.len() - tail].to_vec()
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn kind_of<E: Debug>(e: &E) -> String {
    let s = format!("{e:?}");
    s.split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
        .to_string()
}

/// Statistics of one fuzz run.
#[derive(Default, Debug)]
struct Stats {
    ok: usize,
    errors: BTreeMap<String, usize>,
}

fn decode_guarded<T, E: Debug>(
    name: &str,
    input: &[u8],
    decode: &dyn Fn(&[u8]) -> Result<T, E>,
) -> Result<T, E> {
    match catch_unwind(AssertUnwindSafe(|| decode(input))) {
        Ok(r) => r,
        Err(_) => panic!(
            "{name}: decode PANICKED on {} bytes: {}",
            input.len(),
            hex(input)
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn fuzz<T: PartialEq + Debug, E: Debug>(
    name: &str,
    seeds: &[Vec<u8>],
    seal: Seal,
    iterations: usize,
    decode: &dyn Fn(&[u8]) -> Result<T, E>,
    reencode: &dyn Fn(&T) -> Result<Vec<u8>, E>,
    rng_seed: u64,
) -> (Stats, Stats) {
    let mut rng = Rng(rng_seed);
    // The harness's own sealing must reproduce the real thing exactly.
    for s in seeds {
        let mut again = seal.unsealed(s);
        seal.reseal(&mut again);
        assert_eq!(
            &again, s,
            "{name}: the test's sealing does not match the codec's checksum"
        );
        assert!(
            decode_guarded(name, s, decode).is_ok(),
            "{name}: a seed frame must decode"
        );
    }
    let (mut raw, mut sealed) = (Stats::default(), Stats::default());
    for i in 0..iterations {
        let base = &seeds[i % seeds.len()];
        // Raw phase: mutate the whole frame, leave the checksum as it is.
        let mut frame = base.clone();
        for _ in 0..1 + rng.below(3) {
            mutate(&mut frame, 0, &mut rng);
        }
        check(name, &frame, decode, reencode, &mut raw);
        // Sealed phase: mutate the body only, then make the frame consistent again.
        let mut body = seal.unsealed(base);
        for _ in 0..1 + rng.below(4) {
            mutate(&mut body, seal.body_start(), &mut rng);
        }
        seal.reseal(&mut body);
        check(name, &body, decode, reencode, &mut sealed);
    }
    (raw, sealed)
}

fn check<T: PartialEq + Debug, E: Debug>(
    name: &str,
    input: &[u8],
    decode: &dyn Fn(&[u8]) -> Result<T, E>,
    reencode: &dyn Fn(&T) -> Result<Vec<u8>, E>,
    stats: &mut Stats,
) {
    match decode_guarded(name, input, decode) {
        Ok(v) => {
            stats.ok += 1;
            // A decoded value must be one the codec itself can encode, and must survive a round trip.
            let again = reencode(&v).unwrap_or_else(|e| {
                panic!(
                    "{name}: decoded a value it cannot re-encode ({e:?}); input {}",
                    hex(input)
                )
            });
            let back = decode_guarded(name, &again, decode);
            assert!(
                back.is_ok_and(|b| b == v),
                "{name}: round trip changed the value; input {}",
                hex(input)
            );
        }
        Err(e) => *stats.errors.entry(kind_of(&e)).or_insert(0) += 1,
    }
}

fn report(name: &str, raw: &Stats, sealed: &Stats) {
    eprintln!("{name}: raw ok {} errors {:?}", raw.ok, raw.errors);
    eprintln!("{name}: sealed ok {} errors {:?}", sealed.ok, sealed.errors);
}

/// Common assertions: the checksum never rejects a correctly sealed frame, and the sealed phase
/// gets past the checksum into several different parser checks.
fn expect_deep(name: &str, sealed: &Stats, min_kinds: usize) {
    assert!(
        !sealed.errors.contains_key("ChecksumMismatch"),
        "{name}: sealed frames were rejected by the checksum"
    );
    assert!(
        sealed.errors.len() + usize::from(sealed.ok > 0) >= min_kinds,
        "{name}: the sealed phase only reached {} distinct outcomes: {:?}",
        sealed.errors.len(),
        sealed.errors
    );
}

const ITER: usize = 30_000;

// ---------------------------------------------------------------- the codecs

fn wire_seeds() -> Vec<Vec<u8>> {
    let l = WireLimits::default();
    let rec = |id: &str, v: u64, del: bool, p: &[u8]| MemoryRecord {
        memory_id: id.into(),
        version: v,
        updated_at_ms: v * 10,
        deleted: del,
        digest: [v as u8; 32],
        payload: p.to_vec(),
    };
    [
        Message::Hello {
            tenant_id: "t".into(),
            replica_id: "r1".into(),
            protocol_min: 1,
            protocol_max: 3,
        },
        Message::Inventory {
            tenant_id: "tenant-é".into(),
            replica_id: "r".into(),
            high_watermark: 99,
            inventory_digest: [7; 32],
        },
        Message::Fetch {
            tenant_id: "t".into(),
            request_id: 5,
            after_sequence: 10,
            max_records: 100,
            max_bytes: 4096,
        },
        Message::Records {
            tenant_id: "t".into(),
            request_id: 1,
            first_sequence: 1,
            records: vec![],
        },
        Message::Records {
            tenant_id: "t".into(),
            request_id: 2,
            first_sequence: 3,
            records: vec![
                rec("a", 3, false, b"hello"),
                rec("b", 4, true, b""),
                rec("c", 5, false, &[0u8; 300]),
            ],
        },
        Message::Acknowledge {
            tenant_id: "t".into(),
            request_id: 9,
            through_sequence: 12,
        },
        Message::Reject {
            tenant_id: "t".into(),
            request_id: 9,
            code: 400,
            message: "no \u{1F600}".into(),
        },
    ]
    .iter()
    .map(|m| wire::encode(m, &l).unwrap())
    .collect()
}

#[test]
fn the_wire_codec_survives_fuzzing() {
    let limits = WireLimits {
        max_frame_bytes: 64 * 1024,
        max_string_bytes: 256,
        max_records: 64,
        max_record_bytes: 4096,
    };
    let seeds = wire_seeds();
    let (raw, sealed) = fuzz(
        "wire",
        &seeds,
        Seal::Crc32,
        ITER,
        &|b| wire::decode(b, &limits),
        &|m| wire::encode(m, &limits),
        0xA11CE,
    );
    report("wire", &raw, &sealed);
    expect_deep("wire", &sealed, 4);
}

fn snapshot_seeds() -> Vec<Vec<u8>> {
    let l = SnapLimits::default();
    let rec = |id: u128, text: &str, tags: &[&str]| SnapshotRecord {
        id,
        agent_id: 3,
        created_at_ns: 1_700_000_000_000_000_000_u64.wrapping_add(id as u64),
        text: text.into(),
        tags: tags.iter().map(|t| (*t).into()).collect(),
    };
    [
        MemorySnapshot {
            tenant_id: 1,
            generation: 0,
            records: vec![],
        },
        MemorySnapshot {
            tenant_id: 1,
            generation: 2,
            records: vec![rec(1, "one", &[])],
        },
        MemorySnapshot {
            tenant_id: 9,
            generation: 7,
            records: vec![
                rec(1, "naïve café \u{1F600}", &["a", "bb"]),
                rec(2, "", &["x"]),
                rec(u128::MAX, &"long ".repeat(50), &[]),
            ],
        },
    ]
    .iter()
    .map(|s| snapshot::encode(s, &l).unwrap())
    .collect()
}

#[test]
fn the_snapshot_codec_survives_fuzzing() {
    let limits = SnapLimits {
        max_snapshot_bytes: 64 * 1024,
        max_records: 64,
        max_text_bytes: 1024,
        max_tags_per_record: 8,
        max_tag_bytes: 64,
    };
    let seeds = snapshot_seeds();
    let (raw, sealed) = fuzz(
        "snapshot",
        &seeds,
        Seal::Fnv64,
        ITER,
        &|b| snapshot::decode(b, None, &limits),
        &|s| snapshot::encode(s, &limits),
        0xB0B,
    );
    report("snapshot", &raw, &sealed);
    expect_deep("snapshot", &sealed, 4);
    // A tenant check: the same bytes decoded for another tenant are refused, never returned.
    let ok = snapshot::decode(&seeds[1], Some(1), &limits);
    assert!(ok.is_ok());
    assert!(snapshot::decode(&seeds[1], Some(2), &limits).is_err());
}

fn stream(t: &str, p: &str, highest: u64, seqs: &[u64]) -> StreamCheckpoint {
    StreamCheckpoint {
        tenant_id: t.into(),
        peer_id: p.into(),
        highest_sequence: highest,
        accepted_sequences: seqs.iter().copied().collect::<BTreeSet<_>>(),
    }
}

fn checkpoint_seeds() -> Vec<Vec<u8>> {
    let l = CpLimits::default();
    [
        ReplayCheckpoint {
            window_size: 16,
            streams: vec![],
        },
        ReplayCheckpoint {
            window_size: 16,
            streams: vec![stream("t", "p", 100, &[95, 97, 100])],
        },
        ReplayCheckpoint {
            window_size: 64,
            streams: vec![
                stream("a", "x", 10, &[10]),
                stream("a", "y", 70, &[8, 9, 70]),
                stream("b", "x", 1, &[1]),
            ],
        },
    ]
    .iter()
    .map(|c| checkpoint_codec::encode(c, &l).unwrap())
    .collect()
}

#[test]
fn the_checkpoint_codec_survives_fuzzing() {
    let limits = CpLimits {
        max_snapshot_bytes: 64 * 1024,
        max_streams: 32,
        max_identity_bytes: 64,
        max_sequences_per_stream: 64,
    };
    let seeds = checkpoint_seeds();
    let (raw, sealed) = fuzz(
        "checkpoint",
        &seeds,
        Seal::Crc32,
        ITER,
        &|b| checkpoint_codec::decode(b, &limits),
        &|c| checkpoint_codec::encode(c, &limits),
        0xC0FFEE,
    );
    report("checkpoint", &raw, &sealed);
    expect_deep("checkpoint", &sealed, 4);
}

fn delta_seeds() -> Vec<Vec<u8>> {
    let l = DeltaCodecLimits::default();
    let key = |t: &str, p: &str| StreamKey {
        tenant_id: t.into(),
        peer_id: p.into(),
    };
    [
        CheckpointDelta {
            base_digest: 0,
            target_window_size: 16,
            changes: vec![],
        },
        CheckpointDelta {
            base_digest: 0xDEAD_BEEF,
            target_window_size: 16,
            changes: vec![StreamChange::Remove { key: key("t", "p") }],
        },
        CheckpointDelta {
            base_digest: 42,
            target_window_size: 64,
            changes: vec![
                StreamChange::Remove { key: key("a", "x") },
                StreamChange::Replace {
                    stream: stream("a", "y", 70, &[8, 9, 70]),
                },
                StreamChange::Replace {
                    stream: stream("b", "x", 1, &[1]),
                },
            ],
        },
    ]
    .iter()
    .map(|d| x056::encode(d, &l).unwrap())
    .collect()
}

#[test]
fn the_checkpoint_delta_codec_survives_fuzzing() {
    let limits = DeltaCodecLimits {
        max_frame_bytes: 64 * 1024,
        max_changes: 32,
        max_identity_bytes: 64,
        max_sequences_per_stream: 64,
    };
    let seeds = delta_seeds();
    let (raw, sealed) = fuzz(
        "delta",
        &seeds,
        Seal::Crc32,
        ITER,
        &|b| x056::decode(b, &limits),
        &|d| x056::encode(d, &limits),
        0xD317A,
    );
    report("delta", &raw, &sealed);
    expect_deep("delta", &sealed, 4);
}

#[test]
fn purely_random_bytes_and_hostile_lengths_are_refused_quickly_without_panicking() {
    let started = std::time::Instant::now();
    let mut rng = Rng(0xBAD5EED);
    let wl = WireLimits {
        max_frame_bytes: 4096,
        max_string_bytes: 64,
        max_records: 8,
        max_record_bytes: 256,
    };
    let sl = SnapLimits {
        max_snapshot_bytes: 4096,
        max_records: 8,
        max_text_bytes: 128,
        max_tags_per_record: 4,
        max_tag_bytes: 16,
    };
    let cl = CpLimits {
        max_snapshot_bytes: 4096,
        max_streams: 8,
        max_identity_bytes: 32,
        max_sequences_per_stream: 16,
    };
    let dl = DeltaCodecLimits {
        max_frame_bytes: 4096,
        max_changes: 8,
        max_identity_bytes: 32,
        max_sequences_per_stream: 16,
    };
    for _ in 0..20_000 {
        let len = rng.below(200);
        let junk: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = decode_guarded("wire", &junk, &|b| wire::decode(b, &wl));
        let _ = decode_guarded("snapshot", &junk, &|b| snapshot::decode(b, None, &sl));
        let _ = decode_guarded("checkpoint", &junk, &|b| checkpoint_codec::decode(b, &cl));
        let _ = decode_guarded("delta", &junk, &|b| x056::decode(b, &dl));
    }
    // A valid header declaring a huge body, with a correct checksum and almost no bytes: every
    // codec must refuse it from the declared length, not by trying to read or allocate that much.
    for (magic, seal) in [
        (b"AMRP", Seal::Crc32),
        (b"ARCP", Seal::Crc32),
        (b"ARDL", Seal::Crc32),
    ] {
        for declared in [u32::MAX, 0x7FFF_FFFF, 1 << 24, 4097] {
            let mut f = magic.to_vec();
            f.extend_from_slice(&[1, 0]);
            f.extend_from_slice(&declared.to_be_bytes());
            f.extend_from_slice(&[0; 5]);
            let c = crc32(&f);
            f.extend_from_slice(&c.to_be_bytes());
            let _ = seal; // (framing is the same for these three)
            assert!(decode_guarded("wire", &f, &|b| wire::decode(b, &wl)).is_err());
            assert!(
                decode_guarded("checkpoint", &f, &|b| checkpoint_codec::decode(b, &cl)).is_err()
            );
            assert!(decode_guarded("delta", &f, &|b| x056::decode(b, &dl)).is_err());
        }
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "decoding junk took {:?}",
        started.elapsed()
    );
}
