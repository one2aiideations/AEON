//! Tests for the update system. Nothing here touches the network or the real data partition.

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

// ---------- test doubles ----------

#[derive(Default)]
struct MemStorage {
    files: Mutex<HashMap<String, Vec<u8>>>,
    fail_at: Mutex<Option<usize>>,
    writes: Mutex<usize>,
    locked: Mutex<bool>,
}

impl MemStorage {
    fn new() -> Self {
        Self::default()
    }

    /// Fail (as if power was lost) at the n-th write from now on; later writes also fail.
    fn fail_from(&self, n: usize) {
        *self.fail_at.lock().unwrap() = Some(n);
        *self.writes.lock().unwrap() = 0;
    }

    fn heal(&self) {
        *self.fail_at.lock().unwrap() = None;
    }

    fn tick(&self) -> Result<(), UpdateError> {
        let mut w = self.writes.lock().unwrap();
        *w += 1;
        match *self.fail_at.lock().unwrap() {
            Some(limit) if *w >= limit => Err(UpdateError::Io("power lost".into())),
            _ => Ok(()),
        }
    }
}

impl Storage for MemStorage {
    fn read(&self, path: &str) -> Result<Vec<u8>, UpdateError> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| UpdateError::Io(format!("{path} not found")))
    }
    fn write_atomic(&self, path: &str, data: &[u8]) -> Result<(), UpdateError> {
        self.tick()?;
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), data.to_vec());
        Ok(())
    }
    fn append(&self, path: &str, data: &[u8]) -> Result<(), UpdateError> {
        self.tick()?;
        self.files
            .lock()
            .unwrap()
            .entry(path.to_string())
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }
    fn remove(&self, path: &str) -> Result<(), UpdateError> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, UpdateError> {
        let mut out: Vec<String> = self
            .files
            .lock()
            .unwrap()
            .keys()
            .filter_map(|k| k.strip_prefix(prefix))
            .map(|s| s.trim_start_matches('/'))
            .filter(|s| !s.is_empty() && !s.contains('/'))
            .map(str::to_string)
            .collect();
        out.sort();
        Ok(out)
    }
    fn free_space(&self) -> Result<u64, UpdateError> {
        Ok(1_000_000)
    }
    fn create_dir_all(&self, _path: &str) -> Result<(), UpdateError> {
        Ok(())
    }
    fn lock(&self) -> Result<(), UpdateError> {
        let mut l = self.locked.lock().unwrap();
        if *l {
            return Err(UpdateError::Io("locked".into()));
        }
        *l = true;
        Ok(())
    }
    fn unlock(&self) -> Result<(), UpdateError> {
        *self.locked.lock().unwrap() = false;
        Ok(())
    }
}

struct FakeTransport {
    payloads: HashMap<String, Vec<u8>>,
    /// Always answer 200 with the whole body, ignoring ranges.
    ignore_ranges: bool,
    /// Cut every response after this many bytes (to force resuming).
    cut_after: Option<usize>,
    /// Report a wrong Content-Range start.
    wrong_start: bool,
    /// Omit the Content-Range header on 206.
    no_range_header: bool,
    /// Send this many extra bytes beyond the declared size.
    extra: usize,
}

impl FakeTransport {
    fn new(url: &str, data: &[u8]) -> Self {
        FakeTransport {
            payloads: HashMap::from([(url.to_string(), data.to_vec())]),
            ignore_ranges: false,
            cut_after: None,
            wrong_start: false,
            no_range_header: false,
            extra: 0,
        }
    }
}

impl Transport for FakeTransport {
    fn get(&self, url: &str, range_from: u64) -> Result<Response, UpdateError> {
        let data = self
            .payloads
            .get(url)
            .ok_or_else(|| UpdateError::Io("404".into()))?;
        let from = if self.ignore_ranges {
            0
        } else {
            range_from as usize
        };
        if from > data.len() {
            return Ok(Response {
                status: 416,
                content_range: Some(format!("bytes */{}", data.len())),
                content_length: Some(0),
                body: Box::new(std::io::Cursor::new(Vec::new())),
            });
        }
        let mut body = data.get(from..).unwrap_or_default().to_vec();
        body.extend(std::iter::repeat_n(b'X', self.extra));
        if let Some(cut) = self.cut_after {
            body.truncate(cut);
        }
        let (status, content_range) = if from > 0 && !self.no_range_header {
            let start = if self.wrong_start { from + 1 } else { from };
            (
                206,
                Some(format!("bytes {}-{}/{}", start, data.len() - 1, data.len())),
            )
        } else if from > 0 {
            (206, None)
        } else {
            (200, None)
        };
        Ok(Response {
            status,
            content_range,
            content_length: Some(body.len() as u64),
            body: Box::new(std::io::Cursor::new(body)),
        })
    }
}

struct FakeSwitcher {
    slot: Mutex<(Slot, u8)>,
}

impl FakeSwitcher {
    fn new(slot: Slot) -> Self {
        FakeSwitcher {
            slot: Mutex::new((slot, 3)),
        }
    }
}

impl Switcher for FakeSwitcher {
    fn activate(&self, slot: Slot, tries: u8) -> Result<(), UpdateError> {
        *self.slot.lock().unwrap() = (slot, tries);
        Ok(())
    }
    fn active(&self) -> Result<(Slot, u8), UpdateError> {
        Ok(*self.slot.lock().unwrap())
    }
}

struct FakeHealth(bool);

impl Health for FakeHealth {
    fn checks(&self) -> Vec<(String, bool)> {
        vec![("engine".to_string(), self.0)]
    }
}

// ---------- helpers ----------

const SEED_A: [u8; 32] = [42u8; 32];
const SEED_B: [u8; 32] = [7u8; 32];
const AUDIT: &str = "/mouli/data/updates/audit.log";

fn ver(s: &str) -> SemVer {
    SemVer::parse(s).unwrap()
}

fn vk(seed: [u8; 32]) -> VerifyingKey {
    SigningKey::from_bytes(&seed).verifying_key()
}

fn artifact(name: &str, data: &[u8]) -> Artifact {
    Artifact {
        name: name.to_string(),
        size: data.len() as u64,
        sha256: Sha256::digest(data).into(),
        url: format!("https://u.example/{name}"),
    }
}

fn manifest(version: &str, rollback: u64, artifacts: Vec<Artifact>) -> Manifest {
    Manifest {
        product: "MOULI".to_string(),
        channel: Channel::Stable,
        version: ver(version),
        rollback_index: rollback,
        min_current_version: ver("1.0.0"),
        released_at: 1000,
        expires_at: 5000,
        rollout_percent: 100,
        notes: "release notes".to_string(),
        artifacts,
        signed_by: String::new(),
    }
}

fn signed(mut m: Manifest, seed: [u8; 32]) -> (Vec<u8>, DetachedSig, Manifest) {
    let (bytes, sig) = ReleaseTool::sign(&mut m, seed);
    (bytes, sig, m)
}

fn trust_with(seeds: &[[u8; 32]], threshold: usize) -> TrustStore {
    let mut t = TrustStore::new(threshold);
    for (i, s) in seeds.iter().enumerate() {
        t.add_key(&format!("k{i}"), vk(*s));
    }
    t
}

fn policy() -> Policy {
    Policy {
        channels: vec![Channel::Stable],
        reserve_bytes: 100,
        skip_versions: vec![],
        allow_metered: true,
        expected_product: "MOULI".to_string(),
    }
}

fn roomy() -> Environment {
    Environment {
        metered: false,
        free_bytes: 1_000_000,
    }
}

fn seeded_storage(version: &str, rollback: u64) -> (MemStorage, State) {
    let s = MemStorage::new();
    let st = State::initial(ver(version), rollback);
    save_state(&s, &st).unwrap();
    (s, st)
}

// ---------- versions ----------

#[test]
fn semver_parses_and_renders() {
    assert_eq!(ver("1.2.3").render(), "1.2.3");
    let v = ver("2.0.1-rc.1");
    assert_eq!(
        (v.major, v.minor, v.patch, v.pre_release.as_deref()),
        (2, 0, 1, Some("rc.1"))
    );
    assert_eq!(v.render(), "2.0.1-rc.1");
}

#[test]
fn semver_rejects_malformed_text() {
    for bad in [
        "1.2",
        "1.2.3.4",
        "a.b.c",
        "1.2.3-",
        "1.2.3-bad#char",
        "",
        "1..3",
        "+1.2.3",
        "1.2.-3",
        "99999999999999999999.0.0",
    ] {
        assert!(SemVer::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn semver_order_follows_the_rules() {
    let order = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.2",
        "1.0.0-alpha.10",
        "1.0.0-beta",
        "1.0.0-rc.1",
        "1.0.0",
        "1.0.1",
        "1.1.0",
        "2.0.0",
    ];
    for pair in order.windows(2) {
        assert!(ver(pair[0]) < ver(pair[1]), "{} < {}", pair[0], pair[1]);
    }
    assert!(
        ver("1.0.0-1") < ver("1.0.0-alpha"),
        "numeric identifiers sort below text"
    );
    assert_eq!(ver("1.2.3").cmp(&ver("1.2.3")), std::cmp::Ordering::Equal);
}

#[test]
fn huge_numeric_prerelease_identifiers_do_not_overflow() {
    let a = ver("1.0.0-99999999999999999999999");
    let b = ver("1.0.0-100000000000000000000000");
    assert!(a < b);
}

#[test]
fn channels_round_trip() {
    for c in [Channel::Stable, Channel::Beta, Channel::Nightly] {
        assert_eq!(Channel::parse(c.render()).unwrap(), c);
    }
    assert!(Channel::parse("Stable").is_err());
}

// ---------- manifest ----------

fn sample_manifest() -> Manifest {
    manifest(
        "1.2.3",
        10,
        vec![
            artifact("bzImage", b"kernel"),
            artifact("initramfs.cpio.gz", b"rootfs"),
        ],
    )
}

#[test]
fn manifest_round_trips() {
    let mut m = sample_manifest();
    m.signed_by = "0123456789abcdef".into();
    assert_eq!(Manifest::parse_raw(&m.render_manifest()).unwrap(), m);
}

#[test]
fn manifest_parsing_is_strict() {
    let mut m = sample_manifest();
    m.signed_by = "k".into();
    let good = String::from_utf8(m.render_manifest()).unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("crlf", good.replace('\n', "\r\n")),
        ("missing newline", good.trim_end().to_string()),
        ("unknown key", good.replace("notes=", "comment=")),
        (
            "wrong order",
            good.replace(
                "product=MOULI\nchannel=stable\n",
                "channel=stable\nproduct=MOULI\n",
            ),
        ),
        ("duplicate signed_by", format!("{good}signed_by=k\n")),
        ("trailing line", format!("{good}extra=1\n")),
        (
            "artifact after signature",
            format!("{good}artifact=a;1;{};https://x\n", "0".repeat(64)),
        ),
        (
            "uppercase hex",
            good.replace(
                &hex_encode(&Sha256::digest(b"kernel")),
                &hex_encode(&Sha256::digest(b"kernel")).to_uppercase(),
            ),
        ),
        (
            "rollout over 100",
            good.replace("rollout_percent=100", "rollout_percent=101"),
        ),
        (
            "no artifacts",
            good.lines()
                .filter(|l| !l.starts_with("artifact="))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        ),
        (
            "bad header",
            good.replacen("MOULI-UPDATE-1", "MOULI-UPDATE-2", 1),
        ),
        (
            "control char in notes",
            good.replace("release notes", "release\tnotes"),
        ),
        ("empty line", good.replacen("\nproduct", "\n\nproduct", 1)),
    ];
    for (name, text) in cases {
        assert!(Manifest::parse_raw(text.as_bytes()).is_err(), "{name}");
    }
}

#[test]
fn manifest_size_and_artifact_limits() {
    assert_eq!(
        Manifest::parse_raw(&vec![b'a'; 70_000]).err(),
        Some(UpdateError::ManifestTooLarge)
    );
    assert_eq!(
        Manifest::parse_raw(&[0xff, 0xfe]).err(),
        Some(UpdateError::InvalidUtf8)
    );
    let arts: Vec<Artifact> = (0..17).map(|i| artifact(&format!("a{i}"), b"x")).collect();
    let mut m = manifest("1.2.3", 1, arts);
    m.signed_by = "k".into();
    assert!(
        Manifest::parse_raw(&m.render_manifest()).is_err(),
        "17 artifacts"
    );
}

#[test]
fn multibyte_hex_never_panics() {
    // 64 bytes long but made of two-byte characters: slicing by offsets would panic.
    let hostile = "é".repeat(32);
    assert_eq!(hostile.len(), 64);
    let mut m = sample_manifest();
    m.signed_by = "k".into();
    let text = String::from_utf8(m.render_manifest()).unwrap();
    let broken = text.replace(&hex_encode(&Sha256::digest(b"kernel")), &hostile);
    assert!(Manifest::parse_raw(broken.as_bytes()).is_err());
    let sig = format!("MOULI-SIG-1\n{}\n{}\n", "é".repeat(8), "é".repeat(64));
    assert!(DetachedSig::parse(&sig).is_err());
}

#[test]
fn artifact_names_are_validated() {
    for ok in ["kernel-v1_image.bin", "a", "initramfs.cpio.gz"] {
        assert!(validate_artifact_name(ok).is_ok(), "{ok}");
    }
    for bad in [
        "../../etc/passwd",
        ".hidden",
        "a..b",
        "",
        "a/b",
        "a b",
        &"x".repeat(65),
    ] {
        assert_eq!(
            validate_artifact_name(bad),
            Err(UpdateError::InvalidArtifactName),
            "{bad}"
        );
    }
    assert!(validate_artifact_name(&"x".repeat(64)).is_ok());
}

#[test]
fn mutated_manifests_never_panic_and_never_parse_unless_they_round_trip() {
    let mut m = sample_manifest();
    m.signed_by = "0123456789abcdef".into();
    let good = m.render_manifest();
    for i in 0..good.len() {
        for flip in [0x01u8, 0x20, 0x80, 0xff] {
            let mut bytes = good.clone();
            bytes[i] ^= flip;
            if let Ok(parsed) = Manifest::parse_raw(&bytes) {
                assert_eq!(
                    parsed.render_manifest(),
                    bytes,
                    "accepted bytes must be canonical (byte {i})"
                );
            }
        }
    }
    for len in 0..good.len() {
        let _ = Manifest::parse_raw(&good[..len]);
    }
}

// ---------- trust and signatures ----------

#[test]
fn a_valid_signature_verifies() {
    let (bytes, sig, m) = signed(sample_manifest(), SEED_A);
    let trust = trust_with(&[SEED_A], 1);
    let got = verify_manifest(&bytes, &[sig], &trust, 2000).unwrap();
    assert_eq!(got, m);
}

#[test]
fn every_kind_of_bad_signature_is_refused() {
    let (bytes, sig, _) = signed(sample_manifest(), SEED_A);
    let trust = trust_with(&[SEED_A], 1);
    let other_sig = |s: &DetachedSig| DetachedSig {
        header: s.header.clone(),
        key_id: s.key_id.clone(),
        sig_bytes: s.sig_bytes,
    };

    // A tampered byte.
    let mut tampered = bytes.clone();
    let pos = tampered
        .windows(13)
        .position(|w| w == b"release notes")
        .unwrap();
    tampered[pos] = b'R';
    assert_eq!(
        verify_manifest(&tampered, &[other_sig(&sig)], &trust, 2000).err(),
        Some(UpdateError::BadSignature)
    );

    // A key the store has never heard of.
    let (b2, sig2, _) = signed(sample_manifest(), SEED_B);
    assert_eq!(
        verify_manifest(&b2, &[sig2], &trust, 2000).err(),
        Some(UpdateError::BadSignature)
    );

    // A revoked key.
    let mut revoked = trust_with(&[SEED_A], 1);
    revoked.revoke_key(&sig.key_id);
    assert_eq!(
        verify_manifest(&bytes, &[other_sig(&sig)], &revoked, 2000).err(),
        Some(UpdateError::BadSignature)
    );

    // The same signature reused on a different manifest.
    let mut other = sample_manifest();
    other.notes = "different".into();
    other.signed_by = sig.key_id.clone();
    assert_eq!(
        verify_manifest(&other.render_manifest(), &[other_sig(&sig)], &trust, 2000).err(),
        Some(UpdateError::BadSignature)
    );

    // No signatures at all.
    assert_eq!(
        verify_manifest(&bytes, &[], &trust, 2000).err(),
        Some(UpdateError::BadSignature)
    );
}

#[test]
fn a_signature_by_a_different_key_than_the_manifest_names_is_refused() {
    let mut m = sample_manifest();
    m.signed_by = key_id(&vk(SEED_B));
    let bytes = m.render_manifest();
    let sig = SigningKey::from_bytes(&SEED_A).sign(&bytes);
    let detached = DetachedSig {
        header: "MOULI-SIG-1".into(),
        key_id: key_id(&vk(SEED_A)),
        sig_bytes: sig.to_bytes(),
    };
    let trust = trust_with(&[SEED_A, SEED_B], 1);
    assert_eq!(
        verify_manifest(&bytes, &[detached], &trust, 2000).err(),
        Some(UpdateError::BadSignature)
    );
}

#[test]
fn an_expired_manifest_is_refused_at_the_boundary() {
    let (bytes, sig, _) = signed(sample_manifest(), SEED_A);
    let trust = trust_with(&[SEED_A], 1);
    let again = |s: &DetachedSig| DetachedSig {
        header: s.header.clone(),
        key_id: s.key_id.clone(),
        sig_bytes: s.sig_bytes,
    };
    assert!(verify_manifest(&bytes, &[again(&sig)], &trust, 4999).is_ok());
    assert_eq!(
        verify_manifest(&bytes, &[again(&sig)], &trust, 5000).err(),
        Some(UpdateError::ManifestExpired)
    );
}

#[test]
fn threshold_two_needs_two_distinct_valid_signatures() {
    let mut m = sample_manifest();
    m.signed_by = key_id(&vk(SEED_A));
    let bytes = m.render_manifest();
    let make = |seed: [u8; 32]| {
        let s = SigningKey::from_bytes(&seed);
        DetachedSig {
            header: "MOULI-SIG-1".into(),
            key_id: key_id(&s.verifying_key()),
            sig_bytes: s.sign(&bytes).to_bytes(),
        }
    };
    let trust = trust_with(&[SEED_A, SEED_B], 2);
    assert_eq!(
        verify_manifest(&bytes, &[make(SEED_A)], &trust, 2000).err(),
        Some(UpdateError::BadSignature)
    );
    assert_eq!(
        verify_manifest(&bytes, &[make(SEED_A), make(SEED_A)], &trust, 2000).err(),
        Some(UpdateError::BadSignature),
        "the same key twice is one signature"
    );
    assert!(verify_manifest(&bytes, &[make(SEED_A), make(SEED_B)], &trust, 2000).is_ok());
}

#[test]
fn signature_files_round_trip_and_reject_damage() {
    let (_, sig, _) = signed(sample_manifest(), SEED_A);
    let text = sig.render();
    let parsed = DetachedSig::parse(&text).unwrap();
    assert_eq!(parsed.sig_bytes, sig.sig_bytes);
    for bad in [
        text.replace("MOULI-SIG-1", "MOULI-SIG-2"),
        text.to_uppercase(),
        text.replacen('\n', "\r\n", 1),
        format!("{text}extra\n"),
        String::new(),
    ] {
        assert!(DetachedSig::parse(&bad).is_err());
    }
}

#[test]
fn trust_files_parse() {
    let id = key_id(&vk(SEED_B));
    let text = format!(
        "# keys\nthreshold 2\nkey alpha {}\nkey beta {}\nrevoke {id}\n",
        hex_encode(&vk(SEED_A).to_bytes()),
        hex_encode(&vk(SEED_B).to_bytes())
    );
    let t = TrustStore::parse_file(&text).unwrap();
    assert_eq!((t.keys.len(), t.threshold, t.revoked_ids.len()), (2, 2, 1));
    for bad in [
        "threshold 3\n",
        "threshold x\n",
        "key a zz\n",
        "frobnicate\n",
        "key a 00\n",
    ] {
        assert!(TrustStore::parse_file(bad).is_err(), "{bad}");
    }
}

// ---------- planner ----------

fn decide_for(m: &Manifest, state: &State) -> Decision {
    decide(state, m, &policy(), &roomy(), "device-1")
}

#[test]
fn newer_versions_update_and_current_ones_do_not() {
    let state = State::initial(ver("1.0.0"), 5);
    assert!(matches!(
        decide_for(&manifest("1.1.0", 6, vec![artifact("a", b"x")]), &state),
        Decision::Update(_)
    ));
    assert_eq!(
        decide_for(&manifest("1.0.0", 6, vec![]), &state),
        Decision::UpToDate
    );
    assert_eq!(
        decide_for(&manifest("0.9.0", 6, vec![]), &state),
        Decision::UpToDate
    );
}

#[test]
fn rollback_protection_beats_a_valid_signature() {
    let state = State::initial(ver("2.0.0"), 20);
    assert_eq!(
        decide_for(&manifest("2.1.0", 15, vec![]), &state),
        Decision::Blocked(BlockedReason::Downgrade)
    );
    assert!(matches!(
        decide_for(&manifest("2.1.0", 20, vec![artifact("a", b"x")]), &state),
        Decision::Update(_)
    ));
}

#[test]
fn old_systems_are_told_which_version_to_install_first() {
    let state = State::initial(ver("1.0.0"), 5);
    let mut m = manifest("3.0.0", 6, vec![artifact("a", b"x")]);
    m.min_current_version = ver("2.0.0");
    assert_eq!(
        decide_for(&m, &state),
        Decision::Blocked(BlockedReason::NeedsIntermediate("2.0.0".into()))
    );
}

#[test]
fn product_channel_skip_list_metered_and_space_are_respected() {
    let state = State::initial(ver("1.0.0"), 5);
    let m = manifest("1.1.0", 6, vec![artifact("a", &[0u8; 500])]);
    let mut wrong_product = m.clone();
    wrong_product.product = "OTHER".into();
    assert_eq!(
        decide_for(&wrong_product, &state),
        Decision::Blocked(BlockedReason::ProductMismatch)
    );
    let mut beta = m.clone();
    beta.channel = Channel::Beta;
    assert_eq!(
        decide_for(&beta, &state),
        Decision::Blocked(BlockedReason::ChannelMismatch)
    );
    let mut skipping = policy();
    skipping.skip_versions = vec![ver("1.1.0")];
    assert_eq!(
        decide(&state, &m, &skipping, &roomy(), "d"),
        Decision::Blocked(BlockedReason::Skipped)
    );
    let mut strict = policy();
    strict.allow_metered = false;
    assert_eq!(
        decide(
            &state,
            &m,
            &strict,
            &Environment {
                metered: true,
                free_bytes: 1_000_000
            },
            "d"
        ),
        Decision::Blocked(BlockedReason::Metered)
    );
    assert!(matches!(
        decide(&state, &m, &strict, &roomy(), "d"),
        Decision::Update(_)
    ));
    // 500 bytes of artifacts plus the 100 byte reserve need 600 free.
    assert_eq!(
        decide(
            &state,
            &m,
            &policy(),
            &Environment {
                metered: false,
                free_bytes: 599
            },
            "d"
        ),
        Decision::Blocked(BlockedReason::NoSpace)
    );
    assert!(matches!(
        decide(
            &state,
            &m,
            &policy(),
            &Environment {
                metered: false,
                free_bytes: 600
            },
            "d"
        ),
        Decision::Update(_)
    ));
}

#[test]
fn staged_rollout_is_stable_and_proportional() {
    let state = State::initial(ver("1.0.0"), 5);
    let mut m = manifest("2.0.0", 6, vec![artifact("a", b"x")]);
    m.rollout_percent = 10;
    let eligible = |m: &Manifest, id: &str| {
        matches!(
            decide(&state, m, &policy(), &roomy(), id),
            Decision::Update(_)
        )
    };
    let n = 10_000;
    let count = (0..n)
        .filter(|i| eligible(&m, &format!("device-{i}")))
        .count();
    assert!((900..=1100).contains(&count), "got {count} of {n}");
    for i in 0..200 {
        let id = format!("device-{i}");
        assert_eq!(
            eligible(&m, &id),
            eligible(&m, &id),
            "same device, same answer"
        );
    }
    m.rollout_percent = 0;
    assert!(!eligible(&m, "device-1"));
    m.rollout_percent = 100;
    assert!((0..200).all(|i| eligible(&m, &format!("device-{i}"))));
}

// ---------- journal ----------

#[test]
fn state_round_trips_and_detects_damage() {
    let mut s = State::initial(ver("1.2.3"), 9);
    s.generation = 77;
    s.phase = Phase::Switched;
    s.slot_b.status = SlotStatus::Unverified;
    s.slot_b.tries_left = 2;
    let text = s.render();
    assert_eq!(State::parse(&text).unwrap(), s);
    for i in 0..text.len() {
        let mut bytes = text.clone().into_bytes();
        bytes[i] ^= 0x01;
        if let Ok(t) = String::from_utf8(bytes) {
            if let Ok(parsed) = State::parse(&t) {
                assert_eq!(
                    parsed.render(),
                    t,
                    "accepted text must be canonical (byte {i})"
                );
            }
        }
    }
    assert!(State::parse(&text.replace("crc=", "crd=")).is_err());
    assert!(State::parse("").is_err());
}

#[test]
fn the_newer_valid_copy_wins_and_a_torn_copy_falls_back() {
    let s = MemStorage::new();
    let older = State::initial(ver("1.0.0"), 5);
    let mut newer = older.clone();
    newer.generation = 12;
    newer.phase = Phase::Downloading;
    s.write_atomic("/mouli/data/updates/state.0", newer.render().as_bytes())
        .unwrap();
    s.write_atomic("/mouli/data/updates/state.1", older.render().as_bytes())
        .unwrap();
    assert_eq!(load_state(&s).unwrap().generation, 12);
    // Tear the newer copy: the older one is used.
    let rendered = newer.render();
    let torn = &rendered.as_bytes()[..40];
    s.write_atomic("/mouli/data/updates/state.0", torn).unwrap();
    assert_eq!(load_state(&s).unwrap().generation, 1);
    s.write_atomic("/mouli/data/updates/state.1", b"garbage")
        .unwrap();
    assert!(load_state(&s).is_err());
}

#[test]
fn illegal_phase_moves_are_refused() {
    let s = State::initial(ver("1.0.0"), 1);
    assert!(advance(s.clone(), UpdateEvent::DownloadComplete).is_err());
    assert!(advance(s.clone(), UpdateEvent::SystemConfirmed).is_err());
    let d = advance(s.clone(), UpdateEvent::StartUpdate).unwrap();
    assert_eq!(d.phase, Phase::Downloading);
    assert!(d.generation > s.generation);
    assert!(advance(d.clone(), UpdateEvent::StartUpdate).is_err());
    assert_eq!(
        advance(d, UpdateEvent::ErrorOccurred).unwrap().phase,
        Phase::Failed
    );
}

// ---------- download ----------

const URL: &str = "https://u.example/app.img";

fn tracker_fetch(
    storage: &MemStorage,
    transport: &FakeTransport,
    art: &Artifact,
) -> Result<(), UpdateError> {
    DownloadTracker::new(storage, &|_| {}).fetch_artifact(transport, art, "/slot")
}

#[test]
fn a_download_completes_and_leaves_only_the_final_file() {
    let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &data)
    };
    let s = MemStorage::new();
    tracker_fetch(&s, &FakeTransport::new(URL, &data), &art).unwrap();
    assert_eq!(s.read("/slot/app.img").unwrap(), data);
    assert!(s.read("/slot/app.img.part").is_err());
}

#[test]
fn an_interrupted_download_resumes_from_every_offset() {
    let data: Vec<u8> = (0..300u32).map(|i| (i % 253) as u8).collect();
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &data)
    };
    for held in 0..data.len() {
        let s = MemStorage::new();
        if held > 0 {
            s.write_atomic("/slot/app.img.part", &data[..held]).unwrap();
        }
        tracker_fetch(&s, &FakeTransport::new(URL, &data), &art).unwrap();
        assert_eq!(
            s.read("/slot/app.img").unwrap(),
            data,
            "resuming from {held}"
        );
    }
}

#[test]
fn a_connection_that_keeps_dropping_still_finishes() {
    let data = vec![7u8; 5_000];
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &data)
    };
    let mut t = FakeTransport::new(URL, &data);
    t.cut_after = Some(700);
    let s = MemStorage::new();
    tracker_fetch(&s, &t, &art).unwrap();
    assert_eq!(s.read("/slot/app.img").unwrap(), data);
}

#[test]
fn a_server_that_ignores_ranges_restarts_cleanly() {
    let data = b"abcdefghij".to_vec();
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &data)
    };
    let s = MemStorage::new();
    s.write_atomic("/slot/app.img.part", b"abcde").unwrap();
    let mut t = FakeTransport::new(URL, &data);
    t.ignore_ranges = true;
    tracker_fetch(&s, &t, &art).unwrap();
    assert_eq!(s.read("/slot/app.img").unwrap(), data);
}

#[test]
fn bad_range_answers_are_rejected() {
    let data = b"abcdefghij".to_vec();
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &data)
    };
    let s = MemStorage::new();
    s.write_atomic("/slot/app.img.part", b"abcde").unwrap();
    let mut wrong = FakeTransport::new(URL, &data);
    wrong.wrong_start = true;
    assert_eq!(
        tracker_fetch(&s, &wrong, &art).err(),
        Some(UpdateError::Interrupted)
    );
    let mut missing = FakeTransport::new(URL, &data);
    missing.no_range_header = true;
    assert_eq!(
        tracker_fetch(&s, &missing, &art).err(),
        Some(UpdateError::Interrupted)
    );
    assert!(s.read("/slot/app.img").is_err());
}

#[test]
fn a_flipped_byte_fails_the_digest_and_leaves_no_final_file() {
    let good = b"abcdefghij".to_vec();
    let mut bad = good.clone();
    bad[3] ^= 1;
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &good)
    };
    let s = MemStorage::new();
    let err = tracker_fetch(&s, &FakeTransport::new(URL, &bad), &art).unwrap_err();
    assert_eq!(
        err,
        UpdateError::HashMismatch {
            artifact: "app.img".into()
        }
    );
    assert!(s.read("/slot/app.img").is_err());
    assert!(
        s.read("/slot/app.img.part").is_err(),
        "the bad partial file is removed"
    );
}

#[test]
fn an_oversize_body_is_cut_at_the_declared_size() {
    let data = b"abcdefghij".to_vec();
    let art = Artifact {
        url: URL.into(),
        ..artifact("app.img", &data)
    };
    let mut t = FakeTransport::new(URL, &data);
    t.extra = 4096;
    let s = MemStorage::new();
    tracker_fetch(&s, &t, &art).unwrap();
    assert_eq!(s.read("/slot/app.img").unwrap(), data);
}

#[test]
fn hostile_artifact_names_never_reach_the_file_system() {
    let data = b"x".to_vec();
    let s = MemStorage::new();
    for name in ["../../etc/passwd", ".hidden", "", &"y".repeat(65)] {
        let art = Artifact {
            name: name.into(),
            url: URL.into(),
            size: 1,
            sha256: Sha256::digest(&data).into(),
        };
        assert_eq!(
            tracker_fetch(&s, &FakeTransport::new(URL, &data), &art).err(),
            Some(UpdateError::InvalidArtifactName)
        );
    }
    assert!(s.files.lock().unwrap().is_empty());
}

#[test]
fn retry_delays_grow_are_capped_and_stop() {
    let base: Vec<u64> = (0..8).map(|i| delay_ms(i, 0).unwrap()).collect();
    assert_eq!(base, [250, 500, 1000, 2000, 4000, 8000, 16000, 30000]);
    assert_eq!(delay_ms(8, 0), None);
    assert_eq!(delay_ms(0, 499), Some(749));
    assert_eq!(delay_ms(0, 500), Some(250), "jitter stays below 500 ms");
}

// ---------- staging, switching, boot counting ----------

fn staged_world() -> (MemStorage, FakeTransport, Plan, Vec<(String, Vec<u8>)>) {
    let kernel = vec![1u8; 3000];
    let initrd = vec![2u8; 5000];
    let m = manifest(
        "1.1.0",
        6,
        vec![
            Artifact {
                url: "https://u.example/bzImage".into(),
                ..artifact("bzImage", &kernel)
            },
            Artifact {
                url: "https://u.example/initramfs.cpio.gz".into(),
                ..artifact("initramfs.cpio.gz", &initrd)
            },
        ],
    );
    let mut t = FakeTransport::new("https://u.example/bzImage", &kernel);
    t.payloads
        .insert("https://u.example/initramfs.cpio.gz".into(), initrd.clone());
    let (s, _) = seeded_storage("1.0.0", 5);
    (
        s,
        t,
        Plan { manifest: m },
        vec![
            ("bzImage".into(), kernel),
            ("initramfs.cpio.gz".into(), initrd),
        ],
    )
}

#[test]
fn a_full_update_stages_switches_boots_and_confirms() {
    let (s, t, plan, files) = staged_world();
    let switcher = FakeSwitcher::new(Slot::A);
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    let state = load_state(&s).unwrap();
    let state = stage(&plan, &t, &s, state, &mut log, 100).unwrap();
    assert_eq!(state.phase, Phase::Staged);
    assert_eq!(state.slot_b.status, SlotStatus::Unverified);
    for (name, data) in &files {
        assert_eq!(
            &s.read(&format!("{UPDATE_DIR}/slots/B/{name}")).unwrap(),
            data
        );
    }
    assert!(s.read(&format!("{UPDATE_DIR}/slots/B/slot.ok")).is_ok());
    let state = switch_to_staged(&s, state, &switcher, &mut log, 110).unwrap();
    assert_eq!((state.current, state.phase), (Slot::B, Phase::Switched));
    assert_eq!(switcher.active().unwrap(), (Slot::B, 3));
    let (state, decision) = on_boot(&s, state, &switcher, &mut log, 120).unwrap();
    assert_eq!(
        (decision, state.slot_b.tries_left),
        (BootDecision::BootActive, 2)
    );
    let state = confirm(&s, state, &switcher, &mut log, 130).unwrap();
    assert_eq!(
        (state.slot_b.status, state.rollback_floor, state.phase),
        (SlotStatus::Good, 6, Phase::Confirmed)
    );
    assert_eq!(load_state(&s).unwrap(), state);
    assert!(log.verify().is_ok());
}

#[test]
fn an_unconfirmed_slot_is_tried_three_times_then_rolled_back() {
    let (s, _) = seeded_storage("1.0.0", 5);
    let switcher = FakeSwitcher::new(Slot::B);
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    let mut state = State::initial(ver("1.0.0"), 5);
    state.current = Slot::B;
    state.phase = Phase::Switched;
    state.slot_b = SlotDetails {
        version: ver("1.1.0"),
        rollback_index: 6,
        status: SlotStatus::Unverified,
        tries_left: 3,
    };
    for expected_left in [2u8, 1, 0] {
        let (next, d) = on_boot(&s, state, &switcher, &mut log, 1).unwrap();
        assert_eq!(
            (d, next.slot_b.tries_left),
            (BootDecision::BootActive, expected_left)
        );
        state = next;
    }
    let (state, d) = on_boot(&s, state, &switcher, &mut log, 2).unwrap();
    assert_eq!(d, BootDecision::RollBack);
    assert_eq!(
        (state.current, state.slot_b.status),
        (Slot::A, SlotStatus::Bad)
    );
    assert_eq!(switcher.active().unwrap().0, Slot::A);
    assert_eq!(
        state.rollback_floor, 5,
        "a failed update never raises the floor"
    );
}

#[test]
fn a_bad_slot_with_no_good_fallback_reports_failure() {
    let (s, _) = seeded_storage("1.0.0", 5);
    let switcher = FakeSwitcher::new(Slot::B);
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    let mut state = State::initial(ver("1.0.0"), 5);
    state.current = Slot::B;
    state.phase = Phase::Switched;
    state.slot_a.status = SlotStatus::Bad;
    state.slot_b = SlotDetails {
        version: ver("1.1.0"),
        rollback_index: 6,
        status: SlotStatus::Unverified,
        tries_left: 0,
    };
    assert_eq!(
        on_boot(&s, state, &switcher, &mut log, 1).err(),
        Some(UpdateError::Interrupted)
    );
}

#[test]
fn a_confirmed_slot_boots_without_counting() {
    let (s, state) = seeded_storage("1.0.0", 5);
    let switcher = FakeSwitcher::new(Slot::A);
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    let (after, d) = on_boot(&s, state.clone(), &switcher, &mut log, 1).unwrap();
    assert_eq!((d, after), (BootDecision::BootActive, state));
}

#[test]
fn manual_rollback_needs_a_good_other_slot() {
    let (s, state) = seeded_storage("1.0.0", 5);
    let switcher = FakeSwitcher::new(Slot::A);
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    assert_eq!(
        rollback(&s, state.clone(), &switcher, &mut log, 1).err(),
        Some(UpdateError::Interrupted)
    );
    let mut two = state;
    two.slot_b = SlotDetails {
        version: ver("0.9.0"),
        rollback_index: 4,
        status: SlotStatus::Good,
        tries_left: 3,
    };
    let after = rollback(&s, two, &switcher, &mut log, 2).unwrap();
    assert_eq!(
        (after.current, after.slot_a.status),
        (Slot::B, SlotStatus::Bad)
    );
}

#[test]
fn an_unhealthy_system_is_not_confirmed() {
    let (s, mut state) = seeded_storage("1.0.0", 5);
    state.slot_a.status = SlotStatus::Unverified;
    let switcher = FakeSwitcher::new(Slot::A);
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    assert!(confirm_if_healthy(
        &s,
        state.clone(),
        &switcher,
        &mut log,
        &FakeHealth(false),
        1
    )
    .is_err());
    assert!(confirm_if_healthy(&s, state, &switcher, &mut log, &FakeHealth(true), 1).is_ok());
}

#[test]
fn cutting_power_at_every_write_during_staging_never_damages_the_running_slot() {
    for n in 1..80 {
        let (s, t, plan, _) = staged_world();
        let mut log_storage_failed = false;
        s.fail_from(n);
        {
            let mut log = AuditLog::new(&s, AUDIT.to_string());
            let state = load_state(&s).unwrap();
            if stage(&plan, &t, &s, state, &mut log, 100).is_err() {
                log_storage_failed = true;
            }
        }
        s.heal();
        // Whatever happened, the state on disk is valid and the running slot is untouched.
        let after = load_state(&s).expect("state must always load after a power cut");
        assert_eq!(after.current, Slot::A, "cut at write {n}");
        assert_eq!(after.slot_a.status, SlotStatus::Good, "cut at write {n}");
        if log_storage_failed {
            assert_ne!(
                after.slot_b.status,
                SlotStatus::Good,
                "an interrupted update is never marked good (cut {n})"
            );
        }
        // A rerun after power returns converges to a fully staged slot.
        let mut log = AuditLog::new(&s, AUDIT.to_string());
        let state = advance_to_idle(load_state(&s).unwrap());
        let done = stage(&plan, &t, &s, state, &mut log, 200).expect("rerun must succeed");
        assert_eq!(done.phase, Phase::Staged, "after cut at write {n}");
        assert_eq!(done.slot_b.status, SlotStatus::Unverified);
    }
}

fn advance_to_idle(mut s: State) -> State {
    // After an interrupted run the phase may be mid-way; a restart begins a new attempt.
    s.phase = Phase::Idle;
    s
}

#[test]
fn cutting_power_during_the_switch_leaves_a_bootable_system() {
    for n in 1..12 {
        let (s, t, plan, _) = staged_world();
        let switcher = FakeSwitcher::new(Slot::A);
        let mut log = AuditLog::new(&s, AUDIT.to_string());
        let state = stage(&plan, &t, &s, load_state(&s).unwrap(), &mut log, 100).unwrap();
        s.fail_from(n);
        let _ = switch_to_staged(&s, state, &switcher, &mut log, 110);
        s.heal();
        let after = load_state(&s).unwrap();
        // Either the intent was recorded (slot B active, unconfirmed with tries) or nothing changed.
        match after.current {
            Slot::A => assert_eq!(after.slot_a.status, SlotStatus::Good),
            Slot::B => assert_eq!(after.slot_b.status, SlotStatus::Unverified),
        }
    }
}

// ---------- loader variable ----------

#[test]
fn the_grub_block_is_exactly_1024_bytes_and_round_trips() {
    let s = MemStorage::new();
    let sw = GrubEnvSwitcher::new(&s, "/mouli/data/updates/grubenv".into());
    sw.activate(Slot::B, 3).unwrap();
    let raw = s.read("/mouli/data/updates/grubenv").unwrap();
    assert_eq!(raw.len(), 1024);
    assert!(raw.starts_with(b"# GRUB Environment Block\n"));
    assert_eq!(sw.active().unwrap(), (Slot::B, 3));
    sw.activate(Slot::A, 0).unwrap();
    assert_eq!(sw.active().unwrap(), (Slot::A, 0));
}

#[test]
fn a_damaged_grub_block_is_rejected_not_guessed() {
    let s = MemStorage::new();
    let sw = GrubEnvSwitcher::new(&s, "/g".into());
    sw.activate(Slot::B, 3).unwrap();
    let good = s.read("/g").unwrap();
    let mut short = good.clone();
    short.truncate(1000);
    let mut headless = good.clone();
    headless[0] = b'X';
    let mut no_slot = b"# GRUB Environment Block\n".to_vec();
    no_slot.resize(1024, b'#');
    for (name, bytes) in [
        ("short", short),
        ("no header", headless),
        ("no keys", no_slot),
    ] {
        s.write_atomic("/g", &bytes).unwrap();
        assert!(sw.active().is_err(), "{name}");
    }
}

#[test]
fn the_kexec_plan_is_only_a_command_line() {
    let p = make_kexec_plan("/mouli/data/updates/slots/B");
    assert_eq!(p.args[0], "kexec");
    assert!(p
        .args
        .contains(&"/mouli/data/updates/slots/B/bzImage".to_string()));
    assert!(p.args.iter().any(|a| a.starts_with("--initrd=")));
}

// ---------- audit log ----------

#[test]
fn the_audit_chain_names_the_first_altered_entry() {
    let s = MemStorage::new();
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    for i in 0..5 {
        log.append(100 + i, "event", &format!("detail {i}"))
            .unwrap();
    }
    assert_eq!(log.verify(), Ok(()));
    let text = String::from_utf8(s.read(AUDIT).unwrap()).unwrap();
    let tampered = text.replacen("detail 2", "detail X", 1);
    s.write_atomic(AUDIT, tampered.as_bytes()).unwrap();
    assert_eq!(log.verify(), Err(2));
    // Deleting an entry breaks the chain at the gap.
    let lines: Vec<&str> = text.lines().collect();
    let removed = [lines[0], lines[1], lines[3], lines[4]].join("\n") + "\n";
    s.write_atomic(AUDIT, removed.as_bytes()).unwrap();
    assert_eq!(log.verify(), Err(3));
}

#[test]
fn audit_text_cannot_inject_extra_records() {
    let s = MemStorage::new();
    let mut log = AuditLog::new(&s, AUDIT.to_string());
    log.append(1, "kind;with;semicolons", "line one\nline two")
        .unwrap();
    assert_eq!(log.verify(), Ok(()));
    assert_eq!(
        String::from_utf8(s.read(AUDIT).unwrap())
            .unwrap()
            .lines()
            .count(),
        1
    );
}

// ---------- locking and tools ----------

#[test]
fn only_one_updater_can_hold_the_lock() {
    let path = std::env::temp_dir().join(format!("mouli-update-lock-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let a = StdStorage::with_lock_path(path.to_str().unwrap());
    let b = StdStorage::with_lock_path(path.to_str().unwrap());
    a.lock().unwrap();
    assert!(b
        .lock()
        .unwrap_err()
        .to_string()
        .contains("already running"));
    a.unlock().unwrap();
    b.lock().unwrap();
    b.unlock().unwrap();
}

#[test]
fn the_real_file_system_replaces_files_atomically() {
    let dir = std::env::temp_dir().join(format!("mouli-update-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s = StdStorage::with_lock_path("/dev/null/unused");
    let d = dir.to_str().unwrap();
    s.create_dir_all(d).unwrap();
    let f = format!("{d}/state");
    s.write_atomic(&f, b"one").unwrap();
    s.write_atomic(&f, b"two").unwrap();
    s.append(&f, b"+more").unwrap();
    assert_eq!(s.read(&f).unwrap(), b"two+more");
    assert!(!std::path::Path::new(&format!("{f}.tmp")).exists());
    assert_eq!(s.list(d).unwrap(), vec!["state".to_string()]);
    s.remove(&f).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_release_is_built_signed_and_verified_end_to_end() {
    let s = MemStorage::new();
    s.write_atomic("/rel/bzImage", b"kernel bytes").unwrap();
    s.write_atomic("/rel/initramfs.cpio.gz", b"rootfs bytes")
        .unwrap();
    s.write_atomic("/rel/.hidden", b"ignored").unwrap();
    s.write_atomic("/rel/old.part", b"ignored").unwrap();
    let mut m = ReleaseTool::make_manifest(
        &s,
        "/rel",
        Channel::Beta,
        ver("2.0.0-rc.1"),
        9,
        ver("1.5.0"),
        10,
        5000,
        "notes",
    )
    .unwrap();
    assert_eq!(m.artifacts.len(), 2);
    assert_eq!(m.artifacts[0].size, 12);
    let (bytes, sig) = ReleaseTool::sign(&mut m, SEED_A);
    let trust = trust_with(&[SEED_A], 1);
    let verified = verify_manifest(&bytes, &[sig], &trust, 100).unwrap();
    assert_eq!(verified.channel, Channel::Beta);
    assert_eq!(verified.version.render(), "2.0.0-rc.1");
}

#[test]
fn command_line_commands_work_against_the_traits() {
    let (s, _) = seeded_storage("1.0.0", 5);
    let switcher = FakeSwitcher::new(Slot::A);
    let trust = TrustStore::new(1);
    let ctx = CliContext {
        storage: &s,
        switcher: &switcher,
        trust: &trust,
        health: &FakeHealth(true),
    };
    let run = |args: &[&str]| {
        ctx.execute(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>(), 1)
            .unwrap()
    };
    assert_eq!(run(&["status"]), 0);
    assert_eq!(run(&["verify-audit"]), 0);
    assert_eq!(run(&["nonsense"]), 1);
    assert_eq!(run(&[]), 1);
    assert!(
        ctx.execute(&["rollback".to_string()], 1).is_err(),
        "rollback with no good other slot is an error"
    );
}
