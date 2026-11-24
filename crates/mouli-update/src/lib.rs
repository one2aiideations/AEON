//! MOULI OS update system: signed manifests, resumable verified downloads, A/B slots with boot
//! counting and rollback, a power-loss-safe journal and a hash-chained audit log.
//!
//! Everything that touches the outside world goes through a trait (`Transport`, `Storage`,
//! `Switcher`, `Health`), so the logic is deterministic and testable. Time is always passed in.
#![forbid(unsafe_code)]
// Release tooling takes the manifest fields as separate arguments.
#![allow(clippy::too_many_arguments)]

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::{Read, Write};

/// Directory on the data partition that holds slots, state, audit log and the grub block.
pub const UPDATE_DIR: &str = "/mouli/data/updates";

// ==== MODULE: version ====

/// Represents a semantic version adhering to MAJOR.MINOR.PATCH[-prerelease] rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    /// The major version number.
    pub major: u64,
    /// The minor version number.
    pub minor: u64,
    /// The patch version number.
    pub patch: u64,
    /// Optional pre-release identifier string.
    pub pre_release: Option<String>,
}

/// Represents the update distribution channels.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    /// Stable release channel.
    Stable,
    /// Beta testing channel.
    Beta,
    /// Nightly development channel.
    Nightly,
}

impl SemVer {
    /// Parses a semantic version string.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        let mut parts = s.splitn(2, '-');
        let main = parts.next().ok_or(UpdateError::MalformedVersion)?;
        let pre = parts.next().map(|p| p.to_string());

        let digits: Vec<&str> = main.split('.').collect();
        let [major, minor, patch] = digits.as_slice() else {
            return Err(UpdateError::MalformedVersion);
        };
        let num = |d: &str| -> Result<u64, UpdateError> {
            canonical_int::<u64>(d)
                .filter(|_| d.bytes().all(|b| b.is_ascii_digit()))
                .ok_or(UpdateError::MalformedVersion)
        };
        let (major, minor, patch) = (num(major)?, num(minor)?, num(patch)?);

        if let Some(ref pre_val) = pre {
            if pre_val.is_empty()
                || !pre_val
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            {
                return Err(UpdateError::MalformedVersion);
            }
        }

        Ok(SemVer {
            major,
            minor,
            patch,
            pre_release: pre,
        })
    }

    /// Renders the semantic version as a String.
    pub fn render(&self) -> String {
        match &self.pre_release {
            Some(pre) => format!("{}.{}.{}-{}", self.major, self.minor, self.patch, pre),
            None => format!("{}.{}.{}", self.major, self.minor, self.patch),
        }
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (&self.pre_release, &other.pre_release) {
                (None, None) => std::cmp::Ordering::Equal,
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(a), Some(b)) => compare_prerelease_strings(a, b),
            })
    }
}

fn compare_prerelease_strings(a: &str, b: &str) -> std::cmp::Ordering {
    let parts_a: Vec<&str> = a.split('.').collect();
    let parts_b: Vec<&str> = b.split('.').collect();

    for (pa, pb) in parts_a.iter().zip(parts_b.iter()) {
        let is_num_a = !pa.is_empty() && pa.bytes().all(|c| c.is_ascii_digit());
        let is_num_b = !pb.is_empty() && pb.bytes().all(|c| c.is_ascii_digit());

        let ord = match (is_num_a, is_num_b) {
            (true, true) => {
                // Compare as numbers without overflow: shorter digit string is smaller.
                let (ta, tb) = (pa.trim_start_matches('0'), pb.trim_start_matches('0'));
                ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb))
            }
            (false, false) => pa.cmp(pb),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    parts_a.len().cmp(&parts_b.len())
}

impl Channel {
    /// Parses the distribution channel from a string.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        match s {
            "stable" => Ok(Channel::Stable),
            "beta" => Ok(Channel::Beta),
            "nightly" => Ok(Channel::Nightly),
            _ => Err(UpdateError::MalformedChannel),
        }
    }

    /// Renders the channel as a string slice.
    pub fn render(&self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Nightly => "nightly",
        }
    }
}

// ==== MODULE: manifest ====

/// Represents an artifact payload inside an update manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The canonical, safe filename.
    pub name: String,
    /// Size of the artifact in bytes.
    pub size: u64,
    /// Expected SHA-256 hash.
    pub sha256: [u8; 32],
    /// Remote URL.
    pub url: String,
}

/// A parsed, structured update manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Targeted system product name.
    pub product: String,
    /// Channel context.
    pub channel: Channel,
    /// Payload version details.
    pub version: SemVer,
    /// Sequential rollback avoidance indicator.
    pub rollback_index: u64,
    /// The oldest supported version required to proceed.
    pub min_current_version: SemVer,
    /// Absolute epoch release date.
    pub released_at: i64,
    /// Absolute expiration threshold.
    pub expires_at: i64,
    /// Target percentage for staged rollouts.
    pub rollout_percent: u8,
    /// Operator release notes.
    pub notes: String,
    /// Complete artifact package lists.
    pub artifacts: Vec<Artifact>,
    /// Authorized key ID signature prefix identifier.
    pub signed_by: String,
}

const MANIFEST_KEYS: [&str; 9] = [
    "product",
    "channel",
    "version",
    "rollback_index",
    "min_current_version",
    "released_at",
    "expires_at",
    "rollout_percent",
    "notes",
];

impl Manifest {
    /// Parses raw bytes into a validated manifest. Parsing is strict: wrong order, unknown or
    /// duplicate keys, CRLF, uppercase hex and trailing data are all errors.
    pub fn parse_raw(bytes: &[u8]) -> Result<Self, UpdateError> {
        if bytes.len() > 65536 {
            return Err(UpdateError::ManifestTooLarge);
        }
        let content = std::str::from_utf8(bytes).map_err(|_| UpdateError::InvalidUtf8)?;
        if content.contains('\r') || !content.ends_with('\n') {
            return Err(UpdateError::MalformedManifest);
        }

        let mut lines = content.lines();
        if lines.next() != Some("MOULI-UPDATE-1") {
            return Err(UpdateError::MalformedManifest);
        }

        let mut values: Vec<&str> = Vec::with_capacity(MANIFEST_KEYS.len());
        for expected in MANIFEST_KEYS {
            let line = lines.next().ok_or(UpdateError::MalformedManifest)?;
            let (key, val) = line.split_once('=').ok_or(UpdateError::MalformedManifest)?;
            if key != expected {
                return Err(UpdateError::MalformedManifest);
            }
            values.push(val);
        }
        let bad = || UpdateError::MalformedManifest;
        let [product, channel, version, rollback_index, min_current, released_at, expires_at, rollout, notes] =
            values.as_slice()
        else {
            return Err(bad());
        };
        if product.is_empty() || product.len() > 64 {
            return Err(bad());
        }
        let rollout_percent = canonical_int::<u8>(rollout).ok_or_else(bad)?;
        if rollout_percent > 100 {
            return Err(bad());
        }
        if notes.len() > 500 || notes.chars().any(|c| c.is_ascii_control()) {
            return Err(bad());
        }

        let mut artifacts = Vec::new();
        let mut signed_by = None;
        for line in lines {
            let (key, val) = line.split_once('=').ok_or_else(bad)?;
            match key {
                "artifact" if signed_by.is_none() => {
                    if artifacts.len() >= 16 {
                        return Err(bad());
                    }
                    let fields: Vec<&str> = val.split(';').collect();
                    let [name, size, sha, url] = fields.as_slice() else {
                        return Err(bad());
                    };
                    validate_artifact_name(name)?;
                    if url.is_empty() {
                        return Err(bad());
                    }
                    artifacts.push(Artifact {
                        name: (*name).to_string(),
                        size: canonical_int::<u64>(size).ok_or_else(bad)?,
                        sha256: parse_hex_sha256(sha)?,
                        url: (*url).to_string(),
                    });
                }
                "signed_by" if signed_by.is_none() && !artifacts.is_empty() => {
                    signed_by = Some(val.to_string());
                }
                _ => return Err(bad()),
            }
        }

        Ok(Manifest {
            product: (*product).to_string(),
            channel: Channel::parse(channel)?,
            version: SemVer::parse(version)?,
            rollback_index: canonical_int::<u64>(rollback_index).ok_or_else(bad)?,
            min_current_version: SemVer::parse(min_current)?,
            released_at: canonical_int::<i64>(released_at).ok_or_else(bad)?,
            expires_at: canonical_int::<i64>(expires_at).ok_or_else(bad)?,
            rollout_percent,
            notes: (*notes).to_string(),
            artifacts,
            signed_by: signed_by.ok_or_else(bad)?,
        })
    }

    /// Serializes the manifest into its canonical byte form.
    pub fn render_manifest(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str("MOULI-UPDATE-1\n");
        out.push_str(&format!("product={}\n", self.product));
        out.push_str(&format!("channel={}\n", self.channel.render()));
        out.push_str(&format!("version={}\n", self.version.render()));
        out.push_str(&format!("rollback_index={}\n", self.rollback_index));
        out.push_str(&format!(
            "min_current_version={}\n",
            self.min_current_version.render()
        ));
        out.push_str(&format!("released_at={}\n", self.released_at));
        out.push_str(&format!("expires_at={}\n", self.expires_at));
        out.push_str(&format!("rollout_percent={}\n", self.rollout_percent));
        out.push_str(&format!("notes={}\n", self.notes));
        for art in &self.artifacts {
            out.push_str(&format!(
                "artifact={};{};{};{}\n",
                art.name,
                art.size,
                hex_encode(&art.sha256),
                art.url
            ));
        }
        out.push_str(&format!("signed_by={}\n", self.signed_by));
        out.into_bytes()
    }
}

/// Validates an artifact file name so a hostile manifest cannot escape the slot directory.
pub fn validate_artifact_name(name: &str) -> Result<(), UpdateError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(UpdateError::InvalidArtifactName)
    }
}

/// Parses an integer only when its text is the canonical form (no `+`, no leading zeros).
fn canonical_int<T: std::str::FromStr + fmt::Display>(s: &str) -> Option<T> {
    let v = s.parse::<T>().ok()?;
    (v.to_string() == s).then_some(v)
}

/// Decodes exactly `N` lowercase hex byte pairs. Rejects uppercase, non-ASCII and wrong lengths.
fn parse_hex_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        let hi = hex_digit(*pair.first()?)?;
        let lo = hex_digit(*pair.get(1)?)?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn parse_hex_sha256(s: &str) -> Result<[u8; 32], UpdateError> {
    parse_hex_array::<32>(s).ok_or(UpdateError::MalformedManifest)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ==== MODULE: trust ====

/// Identity mapping of key references to public key material.
#[derive(Debug, Clone)]
pub struct TrustKey {
    /// Friendly name.
    pub name: String,
    /// Key id: first 8 bytes of SHA-256 of the public key, hex.
    pub id: String,
    /// Public verification key.
    pub key: VerifyingKey,
}

/// Trusted public keys, a revocation list and the signature threshold.
#[derive(Debug, Clone)]
pub struct TrustStore {
    /// Registered verification keys.
    pub keys: Vec<TrustKey>,
    /// Revoked key ids.
    pub revoked_ids: Vec<String>,
    /// Number of distinct valid signatures required (1 or 2).
    pub threshold: usize,
}

/// The id of a public key (first 8 bytes of its SHA-256, hex).
pub fn key_id(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.to_bytes());
    hex_encode(digest.get(..8).unwrap_or_default())
}

impl TrustStore {
    /// Creates an empty trust store with the given signature threshold.
    pub fn new(threshold: usize) -> Self {
        TrustStore {
            keys: Vec::new(),
            revoked_ids: Vec::new(),
            threshold,
        }
    }

    /// Registers a public key and returns its id.
    pub fn add_key(&mut self, name: &str, key: VerifyingKey) -> String {
        let id = key_id(&key);
        self.keys.push(TrustKey {
            name: name.to_string(),
            id: id.clone(),
            key,
        });
        id
    }

    /// Revokes a key id.
    pub fn revoke_key(&mut self, id: &str) {
        if !self.revoked_ids.iter().any(|r| r == id) {
            self.revoked_ids.push(id.to_string());
        }
    }

    /// Parses a trust file: `threshold N`, `key <name> <64 hex chars>`, `revoke <key id>`.
    pub fn parse_file(text: &str) -> Result<Self, UpdateError> {
        let mut store = TrustStore::new(1);
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["threshold", n] => {
                    store.threshold = n
                        .parse::<usize>()
                        .map_err(|_| UpdateError::MalformedState)?;
                    if !(1..=2).contains(&store.threshold) {
                        return Err(UpdateError::MalformedState);
                    }
                }
                ["key", name, hex] => {
                    let raw = parse_hex_array::<32>(hex).ok_or(UpdateError::MalformedState)?;
                    let key =
                        VerifyingKey::from_bytes(&raw).map_err(|_| UpdateError::MalformedState)?;
                    store.add_key(name, key);
                }
                ["revoke", id] => store.revoke_key(id),
                _ => return Err(UpdateError::MalformedState),
            }
        }
        Ok(store)
    }
}

/// A parsed detached signature file.
pub struct DetachedSig {
    /// Format header (`MOULI-SIG-1`).
    pub header: String,
    /// Id of the signing key.
    pub key_id: String,
    /// Ed25519 signature bytes.
    pub sig_bytes: [u8; 64],
}

impl DetachedSig {
    /// Parses a signature file: header line, 16-hex key id line, 128-hex signature line.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        if s.contains('\r') {
            return Err(UpdateError::MalformedSignature);
        }
        let lines: Vec<&str> = s.lines().collect();
        let [header, key_id, sig] = lines.as_slice() else {
            return Err(UpdateError::MalformedSignature);
        };
        if *header != "MOULI-SIG-1" || key_id.len() != 16 || parse_hex_array::<8>(key_id).is_none()
        {
            return Err(UpdateError::MalformedSignature);
        }
        let sig_bytes = parse_hex_array::<64>(sig).ok_or(UpdateError::MalformedSignature)?;
        Ok(DetachedSig {
            header: (*header).to_string(),
            key_id: (*key_id).to_string(),
            sig_bytes,
        })
    }

    /// Renders the signature file.
    pub fn render(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.header,
            self.key_id,
            hex_encode(&self.sig_bytes)
        )
    }
}

/// Compares two byte slices in constant time (for equal lengths).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut res = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        res |= x ^ y;
    }
    res == 0
}

/// Verifies a manifest: signatures first, then expiry, then structure and the signer.
pub fn verify_manifest(
    bytes: &[u8],
    signatures: &[DetachedSig],
    trust: &TrustStore,
    now: i64,
) -> Result<Manifest, UpdateError> {
    let manifest = Manifest::parse_raw(bytes)?;

    let mut valid_keys: Vec<&str> = Vec::new();
    for sig in signatures {
        if trust.revoked_ids.contains(&sig.key_id) {
            continue;
        }
        let Some(trust_key) = trust
            .keys
            .iter()
            .find(|k| constant_time_eq(k.id.as_bytes(), sig.key_id.as_bytes()))
        else {
            continue;
        };
        let ed_sig = Signature::from_bytes(&sig.sig_bytes);
        if trust_key.key.verify(bytes, &ed_sig).is_ok()
            && !valid_keys.contains(&sig.key_id.as_str())
        {
            valid_keys.push(sig.key_id.as_str());
        }
    }

    if valid_keys.len() < trust.threshold.max(1)
        || !valid_keys.contains(&manifest.signed_by.as_str())
    {
        return Err(UpdateError::BadSignature);
    }
    if now >= manifest.expires_at {
        return Err(UpdateError::ManifestExpired);
    }
    Ok(manifest)
}

// ==== MODULE: planner ====

/// User configuration policy for controlling device updating conditions.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Channels this device accepts.
    pub channels: Vec<Channel>,
    /// Bytes that must remain free after the update is staged.
    pub reserve_bytes: u64,
    /// Versions that are never installed.
    pub skip_versions: Vec<SemVer>,
    /// Whether updates may use a metered connection.
    pub allow_metered: bool,
    /// The product name this device accepts.
    pub expected_product: String,
}

/// What the device knows about its surroundings right now.
#[derive(Debug, Clone, Copy)]
pub struct Environment {
    /// The current connection is metered.
    pub metered: bool,
    /// Free bytes on the data partition.
    pub free_bytes: u64,
}

/// Decision results computed from scanning a manifest against local state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// System requires no modifications.
    UpToDate,
    /// An update should be performed.
    Update(Plan),
    /// An update is available but must not run now.
    Blocked(BlockedReason),
}

/// Why an update is blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedReason {
    /// Wrong product.
    ProductMismatch,
    /// Channel not accepted by policy.
    ChannelMismatch,
    /// Rollback protection.
    Downgrade,
    /// An intermediate version must be installed first.
    NeedsIntermediate(String),
    /// This device is not yet in the rollout.
    StagedRolloutIneligible,
    /// Metered connection and policy forbids it.
    Metered,
    /// Not enough free space.
    NoSpace,
    /// Version is on the skip list.
    Skipped,
}

/// An actionable update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The verified manifest to install.
    pub manifest: Manifest,
}

/// The device's rollout bucket (0..=99) for a version: stable for a given device and version.
pub fn rollout_bucket(device_id: &str, version: &SemVer) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(device_id.as_bytes());
    hasher.update(version.render().as_bytes());
    let h = hasher.finalize();
    let word = [
        h.first().copied().unwrap_or(0),
        h.get(1).copied().unwrap_or(0),
        h.get(2).copied().unwrap_or(0),
        h.get(3).copied().unwrap_or(0),
    ];
    u32::from_be_bytes(word) % 100
}

/// Decides what to do with a verified manifest.
pub fn decide(
    state: &State,
    manifest: &Manifest,
    policy: &Policy,
    env: &Environment,
    device_id: &str,
) -> Decision {
    if manifest.product != policy.expected_product {
        return Decision::Blocked(BlockedReason::ProductMismatch);
    }
    if !policy.channels.contains(&manifest.channel) {
        return Decision::Blocked(BlockedReason::ChannelMismatch);
    }
    let current_version = &state.get_slot(state.current).version;
    if &manifest.version <= current_version {
        return Decision::UpToDate;
    }
    if manifest.rollback_index < state.rollback_floor {
        return Decision::Blocked(BlockedReason::Downgrade);
    }
    if current_version < &manifest.min_current_version {
        return Decision::Blocked(BlockedReason::NeedsIntermediate(
            manifest.min_current_version.render(),
        ));
    }
    if policy.skip_versions.contains(&manifest.version) {
        return Decision::Blocked(BlockedReason::Skipped);
    }
    if rollout_bucket(device_id, &manifest.version) >= u32::from(manifest.rollout_percent) {
        return Decision::Blocked(BlockedReason::StagedRolloutIneligible);
    }
    if env.metered && !policy.allow_metered {
        return Decision::Blocked(BlockedReason::Metered);
    }
    let total: u64 = manifest
        .artifacts
        .iter()
        .fold(0u64, |a, x| a.saturating_add(x.size));
    if env.free_bytes < total.saturating_add(policy.reserve_bytes) {
        return Decision::Blocked(BlockedReason::NoSpace);
    }
    Decision::Update(Plan {
        manifest: manifest.clone(),
    })
}

// ==== MODULE: journal ====

/// Logical partitions of the running platform.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Slot {
    /// Slot A.
    A,
    /// Slot B.
    B,
}

impl Slot {
    /// The other slot.
    pub fn alternate(&self) -> Self {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    /// Renders the slot name.
    pub fn render(&self) -> &'static str {
        match self {
            Slot::A => "A",
            Slot::B => "B",
        }
    }

    /// Parses a slot name.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        match s {
            "A" => Ok(Slot::A),
            "B" => Ok(Slot::B),
            _ => Err(UpdateError::MalformedState),
        }
    }
}

/// Health rating of a slot.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SlotStatus {
    /// No payload.
    Empty,
    /// Staged, not yet confirmed.
    Unverified,
    /// Confirmed good.
    Good,
    /// Failed to boot.
    Bad,
}

impl SlotStatus {
    /// Renders the status name.
    pub fn render(&self) -> &'static str {
        match self {
            SlotStatus::Empty => "Empty",
            SlotStatus::Unverified => "Unverified",
            SlotStatus::Good => "Good",
            SlotStatus::Bad => "Bad",
        }
    }

    /// Parses a status name.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        match s {
            "Empty" => Ok(SlotStatus::Empty),
            "Unverified" => Ok(SlotStatus::Unverified),
            "Good" => Ok(SlotStatus::Good),
            "Bad" => Ok(SlotStatus::Bad),
            _ => Err(UpdateError::MalformedState),
        }
    }
}

/// Per-slot record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotDetails {
    /// Version in the slot.
    pub version: SemVer,
    /// Rollback index of the payload.
    pub rollback_index: u64,
    /// Health rating.
    pub status: SlotStatus,
    /// Boot attempts left before the slot is declared bad.
    pub tries_left: u8,
}

/// Lifecycle phase of the updater.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing in progress.
    Idle,
    /// Downloading artifacts.
    Downloading,
    /// Verifying artifacts.
    Verifying,
    /// Artifacts staged in the inactive slot.
    Staged,
    /// New slot activated, awaiting confirmation.
    Switched,
    /// New slot confirmed good.
    Confirmed,
    /// Went back to the previous slot.
    RolledBack,
    /// Something failed.
    Failed,
}

impl Phase {
    /// Renders the phase name.
    pub fn render(&self) -> &'static str {
        match self {
            Phase::Idle => "Idle",
            Phase::Downloading => "Downloading",
            Phase::Verifying => "Verifying",
            Phase::Staged => "Staged",
            Phase::Switched => "Switched",
            Phase::Confirmed => "Confirmed",
            Phase::RolledBack => "RolledBack",
            Phase::Failed => "Failed",
        }
    }

    /// Parses a phase name.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        match s {
            "Idle" => Ok(Phase::Idle),
            "Downloading" => Ok(Phase::Downloading),
            "Verifying" => Ok(Phase::Verifying),
            "Staged" => Ok(Phase::Staged),
            "Switched" => Ok(Phase::Switched),
            "Confirmed" => Ok(Phase::Confirmed),
            "RolledBack" => Ok(Phase::RolledBack),
            "Failed" => Ok(Phase::Failed),
            _ => Err(UpdateError::MalformedState),
        }
    }
}

/// Events that drive phase transitions.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// Update started.
    StartUpdate,
    /// All artifacts downloaded.
    DownloadComplete,
    /// Artifacts verified.
    VerifySuccess,
    /// New slot activated.
    SwitchComplete,
    /// New slot confirmed.
    SystemConfirmed,
    /// Rolled back.
    RollBackTriggered,
    /// Something failed.
    ErrorOccurred,
}

/// Durable state record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Monotonic write counter; the higher valid copy wins on load.
    pub generation: u64,
    /// The slot the system boots.
    pub current: Slot,
    /// Slot A.
    pub slot_a: SlotDetails,
    /// Slot B.
    pub slot_b: SlotDetails,
    /// Updates with a lower rollback index than this are refused.
    pub rollback_floor: u64,
    /// Current phase.
    pub phase: Phase,
    /// Artifacts fully downloaded so far.
    pub artifact_progress: usize,
}

impl State {
    /// State of a freshly installed system running `version` from slot A.
    pub fn initial(version: SemVer, rollback_index: u64) -> Self {
        State {
            generation: 1,
            current: Slot::A,
            slot_a: SlotDetails {
                version,
                rollback_index,
                status: SlotStatus::Good,
                tries_left: 3,
            },
            slot_b: SlotDetails {
                version: SemVer {
                    major: 0,
                    minor: 0,
                    patch: 0,
                    pre_release: None,
                },
                rollback_index: 0,
                status: SlotStatus::Empty,
                tries_left: 0,
            },
            rollback_floor: rollback_index,
            phase: Phase::Idle,
            artifact_progress: 0,
        }
    }

    /// The record of a slot.
    pub fn get_slot(&self, slot: Slot) -> &SlotDetails {
        match slot {
            Slot::A => &self.slot_a,
            Slot::B => &self.slot_b,
        }
    }

    /// The mutable record of a slot.
    pub fn get_slot_mut(&mut self, slot: Slot) -> &mut SlotDetails {
        match slot {
            Slot::A => &mut self.slot_a,
            Slot::B => &mut self.slot_b,
        }
    }

    /// Renders the state with a trailing CRC-32 line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("MOULI-STATE-1\n");
        out.push_str(&format!("generation={}\n", self.generation));
        out.push_str(&format!("current={}\n", self.current.render()));
        for (tag, d) in [("a", &self.slot_a), ("b", &self.slot_b)] {
            out.push_str(&format!("slot_{tag}_version={}\n", d.version.render()));
            out.push_str(&format!("slot_{tag}_rollback_index={}\n", d.rollback_index));
            out.push_str(&format!("slot_{tag}_status={}\n", d.status.render()));
            out.push_str(&format!("slot_{tag}_tries_left={}\n", d.tries_left));
        }
        out.push_str(&format!("rollback_floor={}\n", self.rollback_floor));
        out.push_str(&format!("phase={}\n", self.phase.render()));
        out.push_str(&format!("artifact_progress={}\n", self.artifact_progress));
        let crc = crc32_compute(out.as_bytes());
        out.push_str(&format!("crc={:08x}\n", crc));
        out
    }

    /// Parses a state block, rejecting any block whose CRC does not match.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        let bad = || UpdateError::MalformedState;
        let body_end = s.rfind("crc=").ok_or_else(bad)?;
        let (body, crc_line) = s.split_at(body_end);
        let crc_hex = crc_line
            .strip_prefix("crc=")
            .ok_or_else(bad)?
            .trim_end_matches('\n');
        if crc_hex.len() != 8 || !body.ends_with('\n') {
            return Err(bad());
        }
        let expected = u32::from_str_radix(crc_hex, 16).map_err(|_| bad())?;
        if crc32_compute(body.as_bytes()) != expected {
            return Err(bad());
        }

        let mut lines = body.lines();
        if lines.next() != Some("MOULI-STATE-1") {
            return Err(bad());
        }
        let mut map = std::collections::BTreeMap::new();
        for line in lines {
            let (k, v) = line.split_once('=').ok_or_else(bad)?;
            if map.insert(k, v).is_some() {
                return Err(bad());
            }
        }
        let get = |k: &str| map.get(k).copied().ok_or_else(bad);
        let u64_of = |k: &str| get(k)?.parse::<u64>().map_err(|_| bad());
        let slot_of = |tag: &str| -> Result<SlotDetails, UpdateError> {
            Ok(SlotDetails {
                version: SemVer::parse(get(&format!("slot_{tag}_version"))?)?,
                rollback_index: u64_of(&format!("slot_{tag}_rollback_index"))?,
                status: SlotStatus::parse(get(&format!("slot_{tag}_status"))?)?,
                tries_left: get(&format!("slot_{tag}_tries_left"))?
                    .parse::<u8>()
                    .map_err(|_| bad())?,
            })
        };
        if map.len() != 13 {
            return Err(bad());
        }
        Ok(State {
            generation: u64_of("generation")?,
            current: Slot::parse(get("current")?)?,
            slot_a: slot_of("a")?,
            slot_b: slot_of("b")?,
            rollback_floor: u64_of("rollback_floor")?,
            phase: Phase::parse(get("phase")?)?,
            artifact_progress: get("artifact_progress")?
                .parse::<usize>()
                .map_err(|_| bad())?,
        })
    }
}

/// CRC-32 (IEEE) of a byte slice.
pub fn crc32_compute(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffff_ffff
}

/// Applies an event to the state; illegal moves are errors.
pub fn advance(mut state: State, event: UpdateEvent) -> Result<State, UpdateError> {
    let next = match (state.phase, &event) {
        (
            Phase::Idle | Phase::Confirmed | Phase::RolledBack | Phase::Failed,
            UpdateEvent::StartUpdate,
        ) => {
            state.artifact_progress = 0;
            Phase::Downloading
        }
        (Phase::Downloading, UpdateEvent::DownloadComplete) => Phase::Verifying,
        (Phase::Verifying, UpdateEvent::VerifySuccess) => Phase::Staged,
        (Phase::Staged, UpdateEvent::SwitchComplete) => Phase::Switched,
        (Phase::Switched, UpdateEvent::SystemConfirmed) => Phase::Confirmed,
        (
            Phase::Idle | Phase::Switched | Phase::Staged | Phase::Confirmed,
            UpdateEvent::RollBackTriggered,
        ) => Phase::RolledBack,
        (_, UpdateEvent::ErrorOccurred) => Phase::Failed,
        (p, e) => {
            return Err(UpdateError::InvalidStateTransition(format!(
                "phase {:?} cannot take event {:?}",
                p, e
            )))
        }
    };
    state.phase = next;
    state.generation = state.generation.saturating_add(1);
    Ok(state)
}

// ==== MODULE: fetch ====

/// Source of artifact bytes (the caller supplies the real HTTPS client).
pub trait Transport {
    /// Requests `url` from byte offset `range_from`.
    fn get(&self, url: &str, range_from: u64) -> Result<Response, UpdateError>;
}

/// Source of the current time.
pub trait Clock {
    /// Unix seconds.
    fn now(&self) -> i64;
}

/// A transfer response.
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// `Content-Range` header, if any.
    pub content_range: Option<String>,
    /// `Content-Length` header, if any.
    pub content_length: Option<u64>,
    /// The body.
    pub body: Box<dyn Read>,
}

/// Resumable artifact downloader.
pub struct DownloadTracker<'a> {
    storage: &'a dyn Storage,
    progress_callback: &'a dyn Fn(u64),
}

impl<'a> DownloadTracker<'a> {
    /// Creates a downloader writing through `storage`.
    pub fn new(storage: &'a dyn Storage, progress_callback: &'a dyn Fn(u64)) -> Self {
        DownloadTracker {
            storage,
            progress_callback,
        }
    }

    fn promote(
        &self,
        part_path: &str,
        final_path: &str,
        art: &Artifact,
    ) -> Result<(), UpdateError> {
        let bytes = self.storage.read(part_path)?;
        if bytes.len() as u64 == art.size && constant_time_eq(&Sha256::digest(&bytes), &art.sha256)
        {
            self.storage.write_atomic(final_path, &bytes)?;
            self.storage.remove(part_path).ok();
            Ok(())
        } else {
            self.storage.remove(part_path).ok();
            Err(UpdateError::HashMismatch {
                artifact: art.name.clone(),
            })
        }
    }

    /// Downloads one artifact into `dest_dir`, resuming from a `.part` file. The final file only
    /// appears after both size and SHA-256 match.
    pub fn fetch_artifact(
        &self,
        transport: &dyn Transport,
        art: &Artifact,
        dest_dir: &str,
    ) -> Result<(), UpdateError> {
        validate_artifact_name(&art.name)?;
        let final_path = format!("{}/{}", dest_dir, art.name);
        let part_path = format!("{}.part", final_path);

        let mut have = self
            .storage
            .read(&part_path)
            .map(|b| b.len() as u64)
            .unwrap_or(0);
        if have > art.size {
            self.storage.remove(&part_path).ok();
            have = 0;
        } else if have == art.size && have > 0 {
            if self.promote(&part_path, &final_path, art).is_ok() {
                return Ok(());
            }
            have = 0;
        }

        let mut attempts = 0;
        while attempts < 8 {
            attempts += 1;
            let Ok(mut resp) = transport.get(&art.url, have) else {
                continue;
            };
            match resp.status {
                206 => {
                    let range = resp
                        .content_range
                        .as_deref()
                        .ok_or(UpdateError::Interrupted)?;
                    if !validate_range_header(range, have) {
                        return Err(UpdateError::Interrupted);
                    }
                }
                200 => {
                    // The server ignored the range: start again from zero.
                    have = 0;
                    self.storage.remove(&part_path).ok();
                }
                416 => {
                    if self.promote(&part_path, &final_path, art).is_ok() {
                        return Ok(());
                    }
                    have = 0;
                    continue;
                }
                _ => return Err(UpdateError::Interrupted),
            }

            let mut buffer = [0u8; 8192];
            let mut since_report = 0u64;
            while have < art.size {
                let want = usize::try_from((art.size - have).min(buffer.len() as u64))
                    .unwrap_or(buffer.len());
                let n = resp
                    .body
                    .read(buffer.get_mut(..want).unwrap_or_default())
                    .map_err(|e| UpdateError::Io(e.to_string()))?;
                if n == 0 {
                    break;
                }
                let block = buffer.get(..n).unwrap_or_default();
                if have == 0 {
                    self.storage.write_atomic(&part_path, block)?;
                } else {
                    self.storage.append(&part_path, block)?;
                }
                have += n as u64;
                since_report += n as u64;
                if since_report >= 262_144 {
                    (self.progress_callback)(have);
                    since_report = 0;
                }
            }
            if have == art.size {
                return self.promote(&part_path, &final_path, art);
            }
        }
        Err(UpdateError::Interrupted)
    }
}

fn validate_range_header(header: &str, expected_start: u64) -> bool {
    let Some(rest) = header.strip_prefix("bytes ") else {
        return false;
    };
    let Some((range, _total)) = rest.split_once('/') else {
        return false;
    };
    let Some((start, end)) = range.split_once('-') else {
        return false;
    };
    match (start.parse::<u64>(), end.parse::<u64>()) {
        (Ok(s), Ok(e)) => s == expected_start && e >= s,
        _ => false,
    }
}

/// Backoff before retry number `attempt` (0-based), or `None` after eight attempts.
pub fn delay_ms(attempt: usize, jitter: u64) -> Option<u64> {
    if attempt >= 8 {
        return None;
    }
    let base = 250u64
        .checked_shl(attempt as u32)
        .unwrap_or(30_000)
        .min(30_000);
    Some(base + jitter % 500)
}

// ==== MODULE: slots ====

/// What to do at boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootDecision {
    /// Boot the active slot.
    BootActive,
    /// The active slot ran out of tries; the other slot was activated.
    RollBack,
}

/// Downloads and verifies the update into the inactive slot.
pub fn stage(
    plan: &Plan,
    transport: &dyn Transport,
    storage: &dyn Storage,
    mut state: State,
    audit_log: &mut AuditLog,
    now: i64,
) -> Result<State, UpdateError> {
    let inactive = state.current.alternate();
    let slot_dir = format!("{}/slots/{}", UPDATE_DIR, inactive.render());
    storage.create_dir_all(&slot_dir)?;

    state = advance(state, UpdateEvent::StartUpdate)?;
    save_state(storage, &state)?;

    let tracker = DownloadTracker::new(storage, &|_| {});
    for (idx, art) in plan.manifest.artifacts.iter().enumerate() {
        tracker.fetch_artifact(transport, art, &slot_dir)?;
        state.artifact_progress = idx + 1;
        save_state(storage, &state)?;
    }
    state = advance(state, UpdateEvent::DownloadComplete)?;
    save_state(storage, &state)?;

    for art in &plan.manifest.artifacts {
        let bytes = storage.read(&format!("{}/{}", slot_dir, art.name))?;
        if !constant_time_eq(&Sha256::digest(&bytes), &art.sha256) {
            return Err(UpdateError::HashMismatch {
                artifact: art.name.clone(),
            });
        }
    }
    // The marker is written last: a slot without it is never bootable.
    storage.write_atomic(&format!("{}/slot.ok", slot_dir), b"OK")?;

    state = advance(state, UpdateEvent::VerifySuccess)?;
    let details = state.get_slot_mut(inactive);
    details.version = plan.manifest.version.clone();
    details.rollback_index = plan.manifest.rollback_index;
    details.status = SlotStatus::Unverified;
    details.tries_left = 3;
    save_state(storage, &state)?;
    audit_log.append(
        now,
        "staged",
        &format!("staged update {}", plan.manifest.version.render()),
    )?;
    Ok(state)
}

/// Activates the staged slot (writes the loader variable) and records the switch.
pub fn switch_to_staged(
    storage: &dyn Storage,
    mut state: State,
    switcher: &dyn Switcher,
    audit_log: &mut AuditLog,
    now: i64,
) -> Result<State, UpdateError> {
    if state.phase != Phase::Staged {
        return Err(UpdateError::InvalidStateTransition(format!(
            "cannot switch from phase {:?}",
            state.phase
        )));
    }
    let target = state.current.alternate();
    let tries = state.get_slot(target).tries_left;
    // Persist the intent first; if power is lost before activation the old slot still boots.
    state = advance(state, UpdateEvent::SwitchComplete)?;
    state.current = target;
    save_state(storage, &state)?;
    switcher.activate(target, tries)?;
    audit_log.append(
        now,
        "switched",
        &format!("activated slot {}", target.render()),
    )?;
    Ok(state)
}

/// Runs at every boot: counts attempts of an unconfirmed slot and rolls back when they run out.
pub fn on_boot(
    storage: &dyn Storage,
    mut state: State,
    switcher: &dyn Switcher,
    audit_log: &mut AuditLog,
    now: i64,
) -> Result<(State, BootDecision), UpdateError> {
    let active = state.current;
    let details = state.get_slot_mut(active);
    if details.status != SlotStatus::Unverified {
        return Ok((state, BootDecision::BootActive));
    }
    if details.tries_left > 0 {
        // Persist BEFORE booting, so a crash during boot still counts.
        details.tries_left -= 1;
        state.generation = state.generation.saturating_add(1);
        save_state(storage, &state)?;
        return Ok((state, BootDecision::BootActive));
    }
    details.status = SlotStatus::Bad;
    let fallback = active.alternate();
    if state.get_slot(fallback).status == SlotStatus::Good {
        state.current = fallback;
        state = advance(state, UpdateEvent::RollBackTriggered)?;
        save_state(storage, &state)?;
        switcher.activate(fallback, 3)?;
        audit_log.append(
            now,
            "rollback",
            "rolled back after the new slot failed to confirm",
        )?;
        Ok((state, BootDecision::RollBack))
    } else {
        state = advance(state, UpdateEvent::ErrorOccurred)?;
        save_state(storage, &state)?;
        Err(UpdateError::Interrupted)
    }
}

/// Marks the running slot good, raises the rollback floor and records it.
pub fn confirm(
    storage: &dyn Storage,
    mut state: State,
    switcher: &dyn Switcher,
    audit_log: &mut AuditLog,
    now: i64,
) -> Result<State, UpdateError> {
    let active = state.current;
    let details = state.get_slot_mut(active);
    if details.status != SlotStatus::Unverified && details.status != SlotStatus::Good {
        return Err(UpdateError::Interrupted);
    }
    details.status = SlotStatus::Good;
    details.tries_left = 3;
    let index = details.rollback_index;
    state.rollback_floor = state.rollback_floor.max(index);
    if state.phase == Phase::Switched {
        state = advance(state, UpdateEvent::SystemConfirmed)?;
    } else {
        state.generation = state.generation.saturating_add(1);
    }
    save_state(storage, &state)?;
    storage
        .remove(&format!(
            "{}/slots/{}/slot.ok",
            UPDATE_DIR,
            active.alternate().render()
        ))
        .ok();
    switcher.activate(active, 3)?;
    audit_log.append(now, "confirmed", "update confirmed good")?;
    Ok(state)
}

/// Switches back to the other slot if it is good.
pub fn rollback(
    storage: &dyn Storage,
    mut state: State,
    switcher: &dyn Switcher,
    audit_log: &mut AuditLog,
    now: i64,
) -> Result<State, UpdateError> {
    let active = state.current;
    let fallback = active.alternate();
    if state.get_slot(fallback).status != SlotStatus::Good {
        return Err(UpdateError::Interrupted);
    }
    state.get_slot_mut(active).status = SlotStatus::Bad;
    state.current = fallback;
    state = advance(state, UpdateEvent::RollBackTriggered)?;
    save_state(storage, &state)?;
    switcher.activate(fallback, 3)?;
    audit_log.append(now, "rollback", "manual rollback applied")?;
    Ok(state)
}

// ==== MODULE: switcher ====

/// Activates a slot for the next boot.
pub trait Switcher {
    /// Make `slot` the one to boot, allowing `tries` attempts.
    fn activate(&self, slot: Slot, tries: u8) -> Result<(), UpdateError>;
    /// The slot and tries currently set.
    fn active(&self) -> Result<(Slot, u8), UpdateError>;
}

/// A `kexec` command line (never executed here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KexecPlan {
    /// Argument vector, program name first.
    pub args: Vec<String>,
}

/// Writes the GRUB environment block.
pub struct GrubEnvSwitcher<'a> {
    storage: &'a dyn Storage,
    path: String,
}

impl<'a> GrubEnvSwitcher<'a> {
    /// Uses the block at `path`.
    pub fn new(storage: &'a dyn Storage, path: String) -> Self {
        GrubEnvSwitcher { storage, path }
    }
}

const GRUB_HEADER: &[u8] = b"# GRUB Environment Block\n";

impl Switcher for GrubEnvSwitcher<'_> {
    fn activate(&self, slot: Slot, tries: u8) -> Result<(), UpdateError> {
        let mut block = Vec::with_capacity(1024);
        block.extend_from_slice(GRUB_HEADER);
        block.extend_from_slice(
            format!("mouli_slot={}\nmouli_tries={}\n", slot.render(), tries).as_bytes(),
        );
        block.resize(1024, b'#');
        self.storage.write_atomic(&self.path, &block)
    }

    fn active(&self) -> Result<(Slot, u8), UpdateError> {
        let data = self.storage.read(&self.path)?;
        if data.len() != 1024 || !data.starts_with(GRUB_HEADER) {
            return Err(UpdateError::MalformedState);
        }
        let text = std::str::from_utf8(&data).map_err(|_| UpdateError::MalformedState)?;
        let (mut slot, mut tries) = (None, None);
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            match line.split_once('=') {
                Some(("mouli_slot", v)) => slot = Some(Slot::parse(v)?),
                Some(("mouli_tries", v)) => {
                    tries = Some(v.parse::<u8>().map_err(|_| UpdateError::MalformedState)?)
                }
                _ => {}
            }
        }
        Ok((
            slot.ok_or(UpdateError::MalformedState)?,
            tries.ok_or(UpdateError::MalformedState)?,
        ))
    }
}

/// Builds the `kexec` argument vector for a slot directory.
pub fn make_kexec_plan(slot_dir: &str) -> KexecPlan {
    KexecPlan {
        args: vec![
            "kexec".to_string(),
            "-l".to_string(),
            format!("{}/bzImage", slot_dir),
            format!("--initrd={}/initramfs.cpio.gz", slot_dir),
            "--command-line=root=/dev/ram0 console=ttyS0".to_string(),
        ],
    }
}

// ==== MODULE: health ====

/// Post-boot health checks.
pub trait Health {
    /// Named checks and whether each passed.
    fn checks(&self) -> Vec<(String, bool)>;
}

/// Confirms the update only when every health check passes.
pub fn confirm_if_healthy(
    storage: &dyn Storage,
    state: State,
    switcher: &dyn Switcher,
    audit_log: &mut AuditLog,
    health: &dyn Health,
    now: i64,
) -> Result<State, UpdateError> {
    for (name, passed) in health.checks() {
        if !passed {
            return Err(UpdateError::Io(format!("health check failed: {}", name)));
        }
    }
    confirm(storage, state, switcher, audit_log, now)
}

// ==== MODULE: audit ====

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Sequence number from 0.
    pub seq: u64,
    /// Unix time.
    pub at: i64,
    /// Event kind.
    pub kind: String,
    /// Description.
    pub detail: String,
    /// Hash of the previous entry (`GENESIS` padded for the first).
    pub prev_hash: [u8; 32],
    /// Hash over this entry's fields.
    pub entry_hash: [u8; 32],
}

/// Append-only, hash-chained event log.
pub struct AuditLog<'a> {
    storage: &'a dyn Storage,
    path: String,
}

fn genesis() -> [u8; 32] {
    let mut h = [0u8; 32];
    let word = b"GENESIS";
    if let Some(dst) = h.get_mut(..word.len()) {
        dst.copy_from_slice(word);
    }
    h
}

fn entry_digest(seq: u64, at: i64, kind: &str, detail: &str, prev: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(seq.to_be_bytes());
    hasher.update(at.to_be_bytes());
    hasher.update(kind.as_bytes());
    hasher.update(detail.as_bytes());
    hasher.update(prev);
    hasher.finalize().into()
}

impl<'a> AuditLog<'a> {
    /// Opens the log at `path`.
    pub fn new(storage: &'a dyn Storage, path: String) -> Self {
        AuditLog { storage, path }
    }

    /// Appends an entry. `kind` and `detail` may not contain `;` or newlines.
    pub fn append(&mut self, at: i64, kind: &str, detail: &str) -> Result<(), UpdateError> {
        let clean = |s: &str| {
            s.chars()
                .map(|c| {
                    if c == ';' || c == '\n' || c == '\r' {
                        ' '
                    } else {
                        c
                    }
                })
                .collect::<String>()
        };
        let (kind, detail) = (clean(kind), clean(detail));
        let entries = self.load_entries()?;
        let seq = entries.len() as u64;
        let prev_hash = entries.last().map_or_else(genesis, |e| e.entry_hash);
        let entry_hash = entry_digest(seq, at, &kind, &detail, &prev_hash);
        let line = format!(
            "{};{};{};{};{};{}\n",
            seq,
            at,
            kind,
            detail,
            hex_encode(&prev_hash),
            hex_encode(&entry_hash)
        );
        self.storage.append(&self.path, line.as_bytes())
    }

    fn load_entries(&self) -> Result<Vec<AuditEntry>, UpdateError> {
        let Ok(bytes) = self.storage.read(&self.path) else {
            return Ok(Vec::new());
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| UpdateError::InvalidUtf8)?;
        let mut entries = Vec::new();
        for line in text.lines().filter(|l| !l.is_empty()) {
            let parts: Vec<&str> = line.split(';').collect();
            let [seq, at, kind, detail, prev, hash] = parts.as_slice() else {
                return Err(UpdateError::MalformedState);
            };
            entries.push(AuditEntry {
                seq: seq.parse().map_err(|_| UpdateError::MalformedState)?,
                at: at.parse().map_err(|_| UpdateError::MalformedState)?,
                kind: (*kind).to_string(),
                detail: (*detail).to_string(),
                prev_hash: parse_hex_sha256(prev).map_err(|_| UpdateError::MalformedState)?,
                entry_hash: parse_hex_sha256(hash).map_err(|_| UpdateError::MalformedState)?,
            });
        }
        Ok(entries)
    }

    /// Verifies the chain; `Err(seq)` names the first bad entry.
    pub fn verify(&self) -> Result<(), u64> {
        let entries = self.load_entries().map_err(|_| 0u64)?;
        let mut expected_prev = genesis();
        for (idx, e) in entries.iter().enumerate() {
            let computed = entry_digest(e.seq, e.at, &e.kind, &e.detail, &e.prev_hash);
            if e.seq != idx as u64
                || !constant_time_eq(&e.prev_hash, &expected_prev)
                || !constant_time_eq(&e.entry_hash, &computed)
            {
                return Err(e.seq);
            }
            expected_prev = e.entry_hash;
        }
        Ok(())
    }
}

// ==== MODULE: storage ====

/// Everything the updater needs from the file system.
pub trait Storage {
    /// Reads a whole file.
    fn read(&self, path: &str) -> Result<Vec<u8>, UpdateError>;
    /// Replaces a file atomically (write temp, sync, rename, sync directory).
    fn write_atomic(&self, path: &str, data: &[u8]) -> Result<(), UpdateError>;
    /// Appends to a file and syncs it.
    fn append(&self, path: &str, data: &[u8]) -> Result<(), UpdateError>;
    /// Removes a file.
    fn remove(&self, path: &str) -> Result<(), UpdateError>;
    /// Lists the file names directly under a directory.
    fn list(&self, prefix: &str) -> Result<Vec<String>, UpdateError>;
    /// Free bytes on the data partition.
    fn free_space(&self) -> Result<u64, UpdateError>;
    /// Creates a directory and its parents.
    fn create_dir_all(&self, path: &str) -> Result<(), UpdateError>;
    /// Takes the exclusive updater lock; fails if it is already held.
    fn lock(&self) -> Result<(), UpdateError>;
    /// Releases the updater lock.
    fn unlock(&self) -> Result<(), UpdateError>;
}

/// The real file system.
#[derive(Default)]
pub struct StdStorage {
    lock_path: String,
}

impl StdStorage {
    /// Uses the lock file inside [`UPDATE_DIR`].
    pub fn new() -> Self {
        StdStorage {
            lock_path: format!("{}/updater.lock", UPDATE_DIR),
        }
    }

    /// Uses a lock file at `path` (for tests and non-standard layouts).
    pub fn with_lock_path(path: &str) -> Self {
        StdStorage {
            lock_path: path.to_string(),
        }
    }
}

fn io(e: std::io::Error) -> UpdateError {
    UpdateError::Io(e.to_string())
}

impl Storage for StdStorage {
    fn read(&self, path: &str) -> Result<Vec<u8>, UpdateError> {
        std::fs::read(path).map_err(io)
    }

    fn write_atomic(&self, path: &str, data: &[u8]) -> Result<(), UpdateError> {
        let tmp = format!("{}.tmp", path);
        {
            let mut f = std::fs::File::create(&tmp).map_err(io)?;
            f.write_all(data).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        std::fs::rename(&tmp, path).map_err(io)?;
        if let Some(dir) = std::path::Path::new(path).parent() {
            if let Ok(d) = std::fs::File::open(dir) {
                d.sync_all().ok();
            }
        }
        Ok(())
    }

    fn append(&self, path: &str, data: &[u8]) -> Result<(), UpdateError> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io)?;
        file.write_all(data).map_err(io)?;
        file.sync_all().map_err(io)
    }

    fn remove(&self, path: &str) -> Result<(), UpdateError> {
        std::fs::remove_file(path).map_err(io)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, UpdateError> {
        let mut out: Vec<String> = std::fs::read_dir(prefix)
            .map_err(io)?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        out.sort();
        Ok(out)
    }

    fn free_space(&self) -> Result<u64, UpdateError> {
        // No libc access in safe Rust: ask `df` for the data partition (1 KiB blocks).
        let out = std::process::Command::new("df")
            .args(["-Pk", UPDATE_DIR])
            .output()
            .map_err(io)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .nth(1)
            .ok_or_else(|| UpdateError::Io("df gave no data".into()))?;
        let kib = line
            .split_whitespace()
            .nth(3)
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| UpdateError::Io("cannot read free space from df".into()))?;
        Ok(kib.saturating_mul(1024))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), UpdateError> {
        std::fs::create_dir_all(path).map_err(io)
    }

    fn lock(&self) -> Result<(), UpdateError> {
        // `create_new` is atomic: exactly one process can create the file.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.lock_path)
            .map(|_| ())
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    UpdateError::Io("another update is already running (lock file exists)".into())
                } else {
                    io(e)
                }
            })
    }

    fn unlock(&self) -> Result<(), UpdateError> {
        std::fs::remove_file(&self.lock_path).map_err(io)
    }
}

/// Loads the newest valid copy of the state from the two redundant files.
pub fn load_state(storage: &dyn Storage) -> Result<State, UpdateError> {
    let read = |name: &str| -> Result<State, UpdateError> {
        let bytes = storage.read(&format!("{}/{}", UPDATE_DIR, name))?;
        State::parse(std::str::from_utf8(&bytes).map_err(|_| UpdateError::InvalidUtf8)?)
    };
    match (read("state.0"), read("state.1")) {
        (Ok(a), Ok(b)) => Ok(if a.generation >= b.generation { a } else { b }),
        (Ok(a), Err(_)) => Ok(a),
        (Err(_), Ok(b)) => Ok(b),
        (Err(_), Err(_)) => Err(UpdateError::MalformedState),
    }
}

/// Saves the state alternately to `state.0` and `state.1` by generation parity, so a torn write
/// can only damage the older copy.
pub fn save_state(storage: &dyn Storage, state: &State) -> Result<(), UpdateError> {
    let name = if state.generation.is_multiple_of(2) {
        "state.0"
    } else {
        "state.1"
    };
    storage.write_atomic(
        &format!("{}/{}", UPDATE_DIR, name),
        state.render().as_bytes(),
    )
}

// ==== MODULE: tools ====

/// Offline release tooling.
pub struct ReleaseTool;

impl ReleaseTool {
    /// Builds a manifest from every artifact file in `dir` (size and SHA-256 are measured).
    pub fn make_manifest(
        storage: &dyn Storage,
        dir: &str,
        channel: Channel,
        version: SemVer,
        rollback_index: u64,
        min_current_version: SemVer,
        released_at: i64,
        expires_at: i64,
        notes: &str,
    ) -> Result<Manifest, UpdateError> {
        let mut artifacts = Vec::new();
        for name in storage.list(dir)? {
            if name.ends_with(".part")
                || name.starts_with('.')
                || name.ends_with(".sig")
                || name == "manifest"
            {
                continue;
            }
            validate_artifact_name(&name)?;
            let data = storage.read(&format!("{}/{}", dir, name))?;
            artifacts.push(Artifact {
                size: data.len() as u64,
                sha256: Sha256::digest(&data).into(),
                url: format!("https://updates.mouli.local/artifacts/{}", name),
                name,
            });
        }
        Ok(Manifest {
            product: "MOULI".to_string(),
            channel,
            version,
            rollback_index,
            min_current_version,
            released_at,
            expires_at,
            rollout_percent: 100,
            notes: notes.to_string(),
            artifacts,
            signed_by: String::new(),
        })
    }

    /// Sets `signed_by` to the key's id, renders the manifest and signs the exact bytes.
    pub fn sign(manifest: &mut Manifest, seed: [u8; 32]) -> (Vec<u8>, DetachedSig) {
        let signing = SigningKey::from_bytes(&seed);
        let id = key_id(&signing.verifying_key());
        manifest.signed_by = id.clone();
        let bytes = manifest.render_manifest();
        let sig = signing.sign(&bytes);
        (
            bytes,
            DetachedSig {
                header: "MOULI-SIG-1".to_string(),
                key_id: id,
                sig_bytes: sig.to_bytes(),
            },
        )
    }
}

// ==== MODULE: cli ====

/// Errors of the update system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// Bad version text.
    MalformedVersion,
    /// Unknown channel.
    MalformedChannel,
    /// Manifest does not follow the canonical format.
    MalformedManifest,
    /// Input is not UTF-8.
    InvalidUtf8,
    /// Manifest larger than 64 KiB.
    ManifestTooLarge,
    /// Unsafe artifact name.
    InvalidArtifactName,
    /// Bad signature file.
    MalformedSignature,
    /// Signature check failed.
    BadSignature,
    /// The manifest has expired.
    ManifestExpired,
    /// A phase transition that is not allowed.
    InvalidStateTransition(String),
    /// State or trust data is damaged.
    MalformedState,
    /// File system error.
    Io(String),
    /// Transfer or boot interrupted.
    Interrupted,
    /// Downgrade refused.
    Downgrade,
    /// Not enough free space.
    NoSpace {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        free: u64,
    },
    /// An artifact did not match its digest.
    HashMismatch {
        /// Name of the artifact.
        artifact: String,
    },
}

impl std::error::Error for UpdateError {}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::MalformedVersion => write!(f, "malformed version"),
            UpdateError::MalformedChannel => write!(f, "unknown distribution channel"),
            UpdateError::MalformedManifest => write!(f, "manifest is not in the canonical format"),
            UpdateError::InvalidUtf8 => write!(f, "input is not valid UTF-8"),
            UpdateError::ManifestTooLarge => write!(f, "manifest is larger than 64 KiB"),
            UpdateError::InvalidArtifactName => write!(f, "artifact name is not allowed"),
            UpdateError::MalformedSignature => write!(f, "malformed signature file"),
            UpdateError::BadSignature => write!(f, "signature check failed"),
            UpdateError::ManifestExpired => write!(f, "the manifest has expired"),
            UpdateError::InvalidStateTransition(s) => write!(f, "illegal state change: {}", s),
            UpdateError::MalformedState => write!(f, "state or trust data is damaged"),
            UpdateError::Io(s) => write!(f, "I/O error: {}", s),
            UpdateError::Interrupted => {
                write!(f, "interrupted: the operation stopped before it finished")
            }
            UpdateError::Downgrade => write!(f, "downgrade refused by rollback protection"),
            UpdateError::NoSpace { needed, free } => {
                write!(f, "not enough space: need {} bytes, {} free", needed, free)
            }
            UpdateError::HashMismatch { artifact } => {
                write!(f, "{} does not match its digest", artifact)
            }
        }
    }
}

/// Services a command needs.
pub struct CliContext<'a> {
    /// File system.
    pub storage: &'a dyn Storage,
    /// Boot loader switch.
    pub switcher: &'a dyn Switcher,
    /// Trusted keys.
    pub trust: &'a TrustStore,
    /// Health checks.
    pub health: &'a dyn Health,
}

impl CliContext<'_> {
    /// Runs `status`, `confirm`, `rollback` or `verify-audit`. Returns the exit code.
    pub fn execute(&self, args: &[String], now: i64) -> Result<i32, UpdateError> {
        let audit_path = format!("{}/audit.log", UPDATE_DIR);
        match args.first().map(String::as_str) {
            Some("status") => {
                let state = load_state(self.storage)?;
                println!("running slot: {}", state.current.render());
                println!("phase: {}", state.phase.render());
                println!("rollback floor: {}", state.rollback_floor);
                for slot in [Slot::A, Slot::B] {
                    let d = state.get_slot(slot);
                    println!(
                        "slot {}: {} {} (tries left {})",
                        slot.render(),
                        d.version.render(),
                        d.status.render(),
                        d.tries_left
                    );
                }
                Ok(0)
            }
            Some("confirm") => {
                let mut log = AuditLog::new(self.storage, audit_path);
                confirm_if_healthy(
                    self.storage,
                    load_state(self.storage)?,
                    self.switcher,
                    &mut log,
                    self.health,
                    now,
                )?;
                println!("update confirmed");
                Ok(0)
            }
            Some("rollback") => {
                let mut log = AuditLog::new(self.storage, audit_path);
                rollback(
                    self.storage,
                    load_state(self.storage)?,
                    self.switcher,
                    &mut log,
                    now,
                )?;
                println!("rolled back; reboot to use the previous slot");
                Ok(0)
            }
            Some("verify-audit") => match AuditLog::new(self.storage, audit_path).verify() {
                Ok(()) => {
                    println!("audit log verified");
                    Ok(0)
                }
                Err(seq) => {
                    println!("audit log FAILED at entry {}", seq);
                    Ok(1)
                }
            },
            _ => Ok(1),
        }
    }
}

/// Health check used by the command line: the updater itself is running.
struct BasicHealth;

impl Health for BasicHealth {
    fn checks(&self) -> Vec<(String, bool)> {
        vec![("updater runs".to_string(), true)]
    }
}

fn read_file(path: &str) -> Result<Vec<u8>, UpdateError> {
    std::fs::read(path).map_err(io)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Command-line entry point. Exit codes: 0 success, 2 update refused, 1 error.
pub fn run_cli(args: &[String]) -> i32 {
    match run_cli_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mouli-update: {}", e);
            1
        }
    }
}

fn run_cli_inner(args: &[String]) -> Result<i32, UpdateError> {
    let arg = |i: usize| {
        args.get(i)
            .map(String::as_str)
            .ok_or(UpdateError::MalformedState)
    };
    match arg(0).unwrap_or("") {
        "verify" => {
            let manifest = read_file(arg(1)?)?;
            let sig_text =
                String::from_utf8(read_file(arg(2)?)?).map_err(|_| UpdateError::InvalidUtf8)?;
            let trust_text =
                String::from_utf8(read_file(arg(3)?)?).map_err(|_| UpdateError::InvalidUtf8)?;
            let trust = TrustStore::parse_file(&trust_text)?;
            let sig = DetachedSig::parse(&sig_text)?;
            let m = verify_manifest(&manifest, &[sig], &trust, now_unix())?;
            println!(
                "valid: {} {} {}",
                m.product,
                m.channel.render(),
                m.version.render()
            );
            Ok(0)
        }
        "make-manifest" => {
            // make-manifest <dir> <channel> <version> <rollback_index> <min_current> <released_at> <expires_at> <notes>
            let m = ReleaseTool::make_manifest(
                &StdStorage::new(),
                arg(1)?,
                Channel::parse(arg(2)?)?,
                SemVer::parse(arg(3)?)?,
                arg(4)?
                    .parse()
                    .map_err(|_| UpdateError::MalformedManifest)?,
                SemVer::parse(arg(5)?)?,
                arg(6)?
                    .parse()
                    .map_err(|_| UpdateError::MalformedManifest)?,
                arg(7)?
                    .parse()
                    .map_err(|_| UpdateError::MalformedManifest)?,
                arg(8)?,
            )?;
            std::io::stdout()
                .write_all(&m.render_manifest())
                .map_err(io)?;
            Ok(0)
        }
        "sign" => {
            // sign <manifest-file> <seed-file (64 hex chars)>; writes <manifest-file> and <manifest-file>.sig
            let text =
                String::from_utf8(read_file(arg(1)?)?).map_err(|_| UpdateError::InvalidUtf8)?;
            let seed_text =
                String::from_utf8(read_file(arg(2)?)?).map_err(|_| UpdateError::InvalidUtf8)?;
            let seed =
                parse_hex_array::<32>(seed_text.trim()).ok_or(UpdateError::MalformedSignature)?;
            // The signer sets `signed_by`; accept a manifest that has it empty.
            let mut manifest = Manifest::parse_raw(text.as_bytes())?;
            let (bytes, sig) = ReleaseTool::sign(&mut manifest, seed);
            std::fs::write(arg(1)?, bytes).map_err(io)?;
            std::fs::write(format!("{}.sig", arg(1)?), sig.render()).map_err(io)?;
            println!("signed by key {}", sig.key_id);
            Ok(0)
        }
        "keyid" => {
            let seed_text =
                String::from_utf8(read_file(arg(1)?)?).map_err(|_| UpdateError::InvalidUtf8)?;
            let seed =
                parse_hex_array::<32>(seed_text.trim()).ok_or(UpdateError::MalformedSignature)?;
            let vk = SigningKey::from_bytes(&seed).verifying_key();
            println!("key {} {}", key_id(&vk), hex_encode(&vk.to_bytes()));
            Ok(0)
        }
        "status" | "confirm" | "rollback" | "verify-audit" => {
            let storage = StdStorage::new();
            let switcher = GrubEnvSwitcher::new(&storage, format!("{}/grubenv", UPDATE_DIR));
            let trust = TrustStore::new(1);
            let ctx = CliContext {
                storage: &storage,
                switcher: &switcher,
                trust: &trust,
                health: &BasicHealth,
            };
            ctx.execute(args, now_unix())
        }
        "check" | "download" | "apply" => {
            eprintln!(
                "mouli-update: `{}` needs the HTTPS transport, which the OS image supplies; use verify/sign/make-manifest here",
                args.first().map_or("", String::as_str)
            );
            Ok(1)
        }
        _ => {
            eprintln!("usage: mouli-update status|confirm|rollback|verify-audit|verify|make-manifest|sign|keyid");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests;
