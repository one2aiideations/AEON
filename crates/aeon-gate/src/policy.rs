#![forbid(unsafe_code)]
//! Deterministic capability policy gate for AI-initiated AEON actions.
//!
//! This module does not parse natural language and never executes an action.
//! Callers are responsible for extracting a typed [`Action`] from a model
//! response or tool call, then asking [`Policy::evaluate`] whether the action
//! is allowed, denied, or needs an interactive confirmation prompt.

use std::collections::hash_map::RandomState;
use std::error::Error;
use std::fmt;
use std::hash::BuildHasher;

/// Maximum accepted length for the raw operator or model utterance being reviewed.
pub const MAX_INPUT_BYTES: usize = 4096;
/// Maximum accepted length for a workspace-relative path.
pub const MAX_PATH_BYTES: usize = 512;
/// Maximum accepted length for a general single argument.
pub const MAX_ARG_BYTES: usize = 1024;
/// Maximum accepted size for a single workspace write or patch.
pub const MAX_WRITE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_COMMAND_ARGS: usize = 32;
const MAX_COMMAND_ARG_BYTES: usize = 256;
const MAX_COMMAND_TOTAL_BYTES: usize = 2048;
const MAX_PACKAGE_BYTES: usize = 128;
const MAX_SYMBOL_BYTES: usize = 64;
const MAX_QUERY_BYTES: usize = 256;

/// Coarse capability required to perform an action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Capability {
    /// Observe read-only status without reading private workspace content.
    Observe,
    /// Show suggestions, explanations, or proposed diffs without touching state.
    Suggest,
    /// Read user workspace files and local indexes.
    ReadWorkspace,
    /// Create, modify, delete, or patch user workspace files.
    WriteWorkspace,
    /// Change network configuration or connectivity.
    Network,
    /// Change installed software, services, or system packages.
    ModifySystem,
    /// Reboot or shut down the machine.
    Power,
}

impl Capability {
    /// All capabilities, in stable declaration order.
    pub const ALL: [Capability; 7] = [
        Capability::Observe,
        Capability::Suggest,
        Capability::ReadWorkspace,
        Capability::WriteWorkspace,
        Capability::Network,
        Capability::ModifySystem,
        Capability::Power,
    ];
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Capability::Observe => f.write_str("observe"),
            Capability::Suggest => f.write_str("suggest"),
            Capability::ReadWorkspace => f.write_str("read workspace"),
            Capability::WriteWorkspace => f.write_str("write workspace"),
            Capability::Network => f.write_str("network"),
            Capability::ModifySystem => f.write_str("modify system"),
            Capability::Power => f.write_str("power"),
        }
    }
}

const fn capability_bit(capability: Capability) -> u16 {
    match capability {
        Capability::Observe => 1 << 0,
        Capability::Suggest => 1 << 1,
        Capability::ReadWorkspace => 1 << 2,
        Capability::WriteWorkspace => 1 << 3,
        Capability::Network => 1 << 4,
        Capability::ModifySystem => 1 << 5,
        Capability::Power => 1 << 6,
    }
}

/// Compact immutable set of granted capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CapabilitySet(u16);

impl CapabilitySet {
    /// Creates a set granting no capabilities.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Creates a set granting every capability.
    pub const fn all() -> Self {
        Self(0b111_1111)
    }

    /// Builds a set from a slice of capabilities.
    pub fn from_slice(capabilities: &[Capability]) -> Self {
        let mut bits = 0u16;
        for &capability in capabilities {
            bits |= capability_bit(capability);
        }
        Self(bits)
    }

    /// Returns true if `capability` is granted.
    pub fn contains(&self, capability: Capability) -> bool {
        self.0 & capability_bit(capability) != 0
    }

    /// Returns a new set with `capability` added.
    pub fn with(mut self, capability: Capability) -> Self {
        self.0 |= capability_bit(capability);
        self
    }

    /// Returns true if no capabilities are granted.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut wrote = false;
        for capability in Capability::ALL {
            if self.contains(capability) {
                if wrote {
                    f.write_str(", ")?;
                }
                write!(f, "{capability}")?;
                wrote = true;
            }
        }
        if !wrote {
            f.write_str("none")?;
        }
        Ok(())
    }
}

/// Kind of real-world consequence a confirmed action may have.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Consequence {
    /// Changes user workspace content or metadata.
    WorkspaceWrite,
    /// Permanently removes workspace content.
    EraseData,
    /// Changes network configuration or connectivity state.
    NetworkChange,
    /// Installs or removes system software or services.
    SystemChange,
    /// Starts a process with effects outside this policy gate.
    SpawnProcess,
    /// Reboots or shuts down the machine.
    Power,
}

impl fmt::Display for Consequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Consequence::WorkspaceWrite => f.write_str("workspace write"),
            Consequence::EraseData => f.write_str("erase data"),
            Consequence::NetworkChange => f.write_str("network change"),
            Consequence::SystemChange => f.write_str("system change"),
            Consequence::SpawnProcess => f.write_str("spawn process"),
            Consequence::Power => f.write_str("power"),
        }
    }
}

/// A typed, non-executing description of an AI-proposed action.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Action {
    /// Read a named read-only status source such as `cpu`, `memory`, or `thermal`.
    ReadStatus {
        /// Status identifier.
        name: String,
    },
    /// Search the user's local file or memory index.
    SearchLocalIndex {
        /// Search query text.
        query: String,
    },
    /// Read a workspace-relative file.
    ReadWorkspaceFile {
        /// Workspace-relative path using `/` separators.
        path: String,
    },
    /// Write bytes to a workspace-relative file.
    WriteWorkspaceFile {
        /// Workspace-relative path using `/` separators.
        path: String,
        /// Number of bytes to write.
        bytes: u64,
    },
    /// Apply a text patch to a workspace-relative file.
    ApplyWorkspacePatch {
        /// Workspace-relative path using `/` separators.
        path: String,
        /// Number of patch bytes.
        bytes: u64,
    },
    /// Delete a workspace-relative file.
    DeleteWorkspaceFile {
        /// Workspace-relative path using `/` separators.
        path: String,
    },
    /// Install a system package by lowercase package name.
    InstallPackage {
        /// Distribution package name.
        name: String,
    },
    /// Enable or disable an interface.
    ConfigureNetwork {
        /// Interface name, such as `eth0` or `wlan0`.
        interface: String,
        /// True to enable networking, false to disable it.
        enable: bool,
    },
    /// Reboot the machine.
    Reboot,
    /// Shut down the machine.
    Shutdown,
    /// Run a sandboxed argv vector. This still requires explicit confirmation.
    RunSandboxedCommand {
        /// Argument vector; argv\[0\] is the executable.
        argv: Vec<String>,
    },
}

impl Action {
    /// Returns the capability required to attempt this action.
    pub fn required(&self) -> Capability {
        match self {
            Action::ReadStatus { .. } => Capability::Observe,
            Action::SearchLocalIndex { .. } | Action::ReadWorkspaceFile { .. } => {
                Capability::ReadWorkspace
            }
            Action::WriteWorkspaceFile { .. }
            | Action::ApplyWorkspacePatch { .. }
            | Action::DeleteWorkspaceFile { .. } => Capability::WriteWorkspace,
            Action::InstallPackage { .. } | Action::RunSandboxedCommand { .. } => {
                Capability::ModifySystem
            }
            Action::ConfigureNetwork { .. } => Capability::Network,
            Action::Reboot | Action::Shutdown => Capability::Power,
        }
    }

    /// Returns the confirmation consequence, if any, for this action.
    pub fn consequence(&self) -> Option<Consequence> {
        match self {
            Action::ReadStatus { .. }
            | Action::SearchLocalIndex { .. }
            | Action::ReadWorkspaceFile { .. } => None,
            Action::WriteWorkspaceFile { .. } | Action::ApplyWorkspacePatch { .. } => {
                Some(Consequence::WorkspaceWrite)
            }
            Action::DeleteWorkspaceFile { .. } => Some(Consequence::EraseData),
            Action::ConfigureNetwork { .. } => Some(Consequence::NetworkChange),
            Action::InstallPackage { .. } => Some(Consequence::SystemChange),
            Action::RunSandboxedCommand { .. } => Some(Consequence::SpawnProcess),
            Action::Reboot | Action::Shutdown => Some(Consequence::Power),
        }
    }

    /// Returns a short human-readable summary suitable for confirmation UI.
    pub fn summary(&self) -> String {
        match self {
            Action::ReadStatus { name } => format!("read status `{name}`"),
            Action::SearchLocalIndex { query } => {
                format!("search local index with a {}-byte query", query.len())
            }
            Action::ReadWorkspaceFile { path } => format!("read workspace file `{path}`"),
            Action::WriteWorkspaceFile { path, bytes } => {
                format!("write {bytes} bytes to workspace file `{path}`")
            }
            Action::ApplyWorkspacePatch { path, bytes } => {
                format!("apply a {bytes}-byte patch to workspace file `{path}`")
            }
            Action::DeleteWorkspaceFile { path } => format!("delete workspace file `{path}`"),
            Action::InstallPackage { name } => format!("install package `{name}`"),
            Action::ConfigureNetwork { interface, enable } => {
                if *enable {
                    format!("enable networking on interface `{interface}`")
                } else {
                    format!("disable networking on interface `{interface}`")
                }
            }
            Action::Reboot => "reboot the system".to_string(),
            Action::Shutdown => "shut down the system".to_string(),
            Action::RunSandboxedCommand { argv } => {
                format!("run a sandboxed command with {} argument(s)", argv.len())
            }
        }
    }
}

/// Typed error produced before an action may be considered by policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyError {
    /// The raw input contains no non-whitespace characters.
    EmptyInput,
    /// The raw input exceeds the configured byte budget.
    InputTooLong {
        /// Maximum allowed bytes.
        max: usize,
        /// Actual bytes.
        actual: usize,
    },
    /// A typed field exceeds its byte budget.
    TooLong {
        /// Field name.
        field: &'static str,
        /// Maximum allowed bytes.
        max: usize,
        /// Actual bytes.
        actual: usize,
    },
    /// A field contains a control or otherwise invalid character.
    InvalidCharacter {
        /// Field name.
        field: &'static str,
    },
    /// A workspace path is not confined to an AEON workspace.
    InvalidPath {
        /// Offending path.
        path: String,
        /// Short reason.
        reason: &'static str,
    },
    /// A field is structurally invalid.
    InvalidArgument {
        /// Field name.
        field: &'static str,
        /// Short reason.
        reason: &'static str,
    },
    /// A confirmation token was supplied that was not issued by this policy
    /// for this exact action.
    ConfirmationMismatch,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyError::EmptyInput => f.write_str("input is empty"),
            PolicyError::InputTooLong { max, actual } => {
                write!(f, "input is {actual} bytes, exceeds {max} bytes")
            }
            PolicyError::TooLong { field, max, actual } => {
                write!(f, "{field} is {actual} bytes, exceeds {max} bytes")
            }
            PolicyError::InvalidCharacter { field } => {
                write!(
                    f,
                    "{field} contains a control or otherwise invalid character"
                )
            }
            PolicyError::InvalidPath { path, reason } => {
                write!(f, "invalid workspace path `{path}`: {reason}")
            }
            PolicyError::InvalidArgument { field, reason } => {
                write!(f, "invalid {field}: {reason}")
            }
            PolicyError::ConfirmationMismatch => {
                f.write_str("confirmation does not match this action")
            }
        }
    }
}

impl Error for PolicyError {}

/// Result of reviewing an action against a granted capability set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// The action may proceed without prompting.
    Allow {
        /// Human-readable action summary.
        summary: String,
    },
    /// The action is well-formed and authorized but needs explicit user confirmation.
    Confirm {
        /// Human-readable action summary.
        summary: String,
        /// Kind of consequence being confirmed.
        consequence: Consequence,
        /// Token to pass back to [`Policy::evaluate`] once the user approves.
        /// It is valid only for this exact action and this policy instance.
        token: ConfirmToken,
    },
    /// The action is well-formed but not authorized by the granted capabilities.
    Deny {
        /// Capabilities missing from the grant.
        missing: CapabilitySet,
        /// Human-readable action summary.
        summary: String,
    },
}

/// Proof that the user approved one specific action shown by [`Decision::Confirm`].
///
/// The value is a keyed hash of the action under a per-policy random key, so a
/// token for one action cannot be reused for another, and a caller without the
/// policy instance cannot forge one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ConfirmToken(u64);

/// Configuration for the policy gate.
#[derive(Clone, Debug)]
pub struct Policy {
    max_input_bytes: usize,
    confirm_key: RandomState,
}

impl Policy {
    /// Creates a policy with production defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the configured raw-input byte budget.
    pub fn max_input_bytes(&self) -> usize {
        self.max_input_bytes
    }

    /// Creates a policy with a custom raw-input byte budget.
    ///
    /// The budget must be between 64 and [`MAX_INPUT_BYTES`].
    pub fn with_max_input_bytes(mut self, max_input_bytes: usize) -> Result<Self, PolicyError> {
        if !(64..=MAX_INPUT_BYTES).contains(&max_input_bytes) {
            return Err(PolicyError::InvalidArgument {
                field: "max_input_bytes",
                reason: "must be between 64 and MAX_INPUT_BYTES",
            });
        }
        self.max_input_bytes = max_input_bytes;
        Ok(self)
    }

    /// Reviews `action` derived from `raw` under `granted`.
    ///
    /// `confirmation` must be the token from a prior [`Decision::Confirm`] for
    /// this exact action, passed only after the user explicitly approved it.
    /// A token issued for a different action, or by a different policy
    /// instance, returns [`PolicyError::ConfirmationMismatch`].
    pub fn evaluate(
        &self,
        raw: &str,
        action: &Action,
        granted: CapabilitySet,
        confirmation: Option<ConfirmToken>,
    ) -> Result<Decision, PolicyError> {
        validate_raw(raw, self.max_input_bytes)?;
        validate_action(action)?;

        let required = action.required();
        if !granted.contains(required) {
            return Ok(Decision::Deny {
                missing: CapabilitySet::from_slice(&[required]),
                summary: action.summary(),
            });
        }

        if let Some(consequence) = action.consequence() {
            let expected = self.token_for(action, consequence);
            match confirmation {
                None => {
                    return Ok(Decision::Confirm {
                        summary: action.summary(),
                        consequence,
                        token: expected,
                    })
                }
                Some(token) if token != expected => return Err(PolicyError::ConfirmationMismatch),
                Some(_) => {}
            }
        }

        Ok(Decision::Allow {
            summary: action.summary(),
        })
    }

    fn token_for(&self, action: &Action, consequence: Consequence) -> ConfirmToken {
        ConfirmToken(self.confirm_key.hash_one((action, consequence)))
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_input_bytes: MAX_INPUT_BYTES,
            confirm_key: RandomState::new(),
        }
    }
}

fn validate_len(field: &'static str, value: &str, max: usize) -> Result<(), PolicyError> {
    let actual = value.len();
    if actual > max {
        return Err(PolicyError::TooLong { field, max, actual });
    }
    Ok(())
}

fn validate_raw(raw: &str, max_input_bytes: usize) -> Result<(), PolicyError> {
    if raw.trim().is_empty() {
        return Err(PolicyError::EmptyInput);
    }
    let actual = raw.len();
    if actual > max_input_bytes {
        return Err(PolicyError::InputTooLong {
            max: max_input_bytes,
            actual,
        });
    }
    for byte in raw.bytes() {
        let allowed_ws = byte == b' ' || byte == b'\n' || byte == b'\t';
        if (byte < 0x20 && !allowed_ws) || byte == 0x7f {
            return Err(PolicyError::InvalidCharacter { field: "input" });
        }
    }
    Ok(())
}

fn validate_printable(field: &'static str, value: &str, max: usize) -> Result<(), PolicyError> {
    validate_len(field, value, max)?;
    if value
        .bytes()
        .any(|byte| (byte < 0x20 && byte != b' ') || byte == 0x7f)
    {
        return Err(PolicyError::InvalidCharacter { field });
    }
    Ok(())
}

fn validate_symbol(field: &'static str, value: &str) -> Result<(), PolicyError> {
    validate_len(field, value, MAX_SYMBOL_BYTES)?;
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(byte) if byte.is_ascii_alphanumeric() => {}
        _ => {
            return Err(PolicyError::InvalidArgument {
                field,
                reason: "must start with an ASCII letter or digit",
            })
        }
    }
    if !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')) {
        return Err(PolicyError::InvalidArgument {
            field,
            reason: "only ASCII letters, digits, `_`, `-`, and `.` are allowed",
        });
    }
    Ok(())
}

fn validate_package_name(value: &str) -> Result<(), PolicyError> {
    validate_len("package", value, MAX_PACKAGE_BYTES)?;
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(byte) if byte.is_ascii_lowercase() || byte.is_ascii_digit() => {}
        _ => {
            return Err(PolicyError::InvalidArgument {
                field: "package",
                reason: "must start with a lowercase ASCII letter or digit",
            })
        }
    }
    if !bytes.all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'-' | b'_' | b'.' | b'+')
    }) {
        return Err(PolicyError::InvalidArgument {
            field: "package",
            reason: "only lowercase ASCII letters, digits, `-`, `_`, `.`, and `+` are allowed",
        });
    }
    Ok(())
}

fn validate_workspace_path(path: &str) -> Result<(), PolicyError> {
    validate_len("path", path, MAX_PATH_BYTES)?;
    if path.starts_with('/') {
        return Err(PolicyError::InvalidPath {
            path: path.to_string(),
            reason: "absolute paths are not allowed",
        });
    }
    if path.contains('\\') {
        return Err(PolicyError::InvalidPath {
            path: path.to_string(),
            reason: "backslashes are not workspace path separators",
        });
    }

    let mut segments = 0usize;
    for segment in path.split('/') {
        segments += 1;
        if segments > 32 {
            return Err(PolicyError::InvalidPath {
                path: path.to_string(),
                reason: "path has too many segments",
            });
        }
        if segment.is_empty() {
            return Err(PolicyError::InvalidPath {
                path: path.to_string(),
                reason: "empty path segment",
            });
        }
        if segment == "." || segment == ".." {
            return Err(PolicyError::InvalidPath {
                path: path.to_string(),
                reason: "path traversal segments are not allowed",
            });
        }
        if segment
            .bytes()
            .any(|byte| byte < 0x20 || byte == 0x7f || byte == b'\\')
        {
            return Err(PolicyError::InvalidPath {
                path: path.to_string(),
                reason: "control characters are not allowed",
            });
        }
    }
    Ok(())
}

fn validate_write_bytes(field: &'static str, bytes: u64) -> Result<(), PolicyError> {
    if bytes > MAX_WRITE_BYTES {
        return Err(PolicyError::InvalidArgument {
            field,
            reason: "exceeds the maximum single-operation write budget",
        });
    }
    Ok(())
}

fn validate_command_argv(argv: &[String]) -> Result<(), PolicyError> {
    if argv.is_empty() {
        return Err(PolicyError::InvalidArgument {
            field: "argv",
            reason: "must contain at least one argument",
        });
    }
    if argv.len() > MAX_COMMAND_ARGS {
        return Err(PolicyError::InvalidArgument {
            field: "argv",
            reason: "too many arguments",
        });
    }
    if let Some(first) = argv.first() {
        if first.starts_with('-') {
            return Err(PolicyError::InvalidArgument {
                field: "argv",
                reason: "executable must not start with `-`",
            });
        }
    }

    let mut total = 0usize;
    for argument in argv {
        validate_len("argv", argument, MAX_COMMAND_ARG_BYTES)?;
        if argument.is_empty() {
            return Err(PolicyError::InvalidArgument {
                field: "argv",
                reason: "empty argument",
            });
        }
        if argument.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
            return Err(PolicyError::InvalidCharacter { field: "argv" });
        }
        total = total
            .checked_add(argument.len())
            .ok_or(PolicyError::InvalidArgument {
                field: "argv",
                reason: "argument byte count overflow",
            })?;
        if total > MAX_COMMAND_TOTAL_BYTES {
            return Err(PolicyError::InvalidArgument {
                field: "argv",
                reason: "total command bytes exceed the sandbox budget",
            });
        }
    }
    Ok(())
}

fn validate_action(action: &Action) -> Result<(), PolicyError> {
    match action {
        Action::ReadStatus { name } => validate_symbol("status", name),
        Action::SearchLocalIndex { query } => {
            validate_len("query", query, MAX_QUERY_BYTES)?;
            if query.trim().is_empty() {
                return Err(PolicyError::InvalidArgument {
                    field: "query",
                    reason: "must not be blank",
                });
            }
            validate_printable("query", query, MAX_QUERY_BYTES)
        }
        Action::ReadWorkspaceFile { path } | Action::DeleteWorkspaceFile { path } => {
            validate_workspace_path(path)
        }
        Action::WriteWorkspaceFile { path, bytes } => {
            validate_workspace_path(path)?;
            validate_write_bytes("bytes", *bytes)
        }
        Action::ApplyWorkspacePatch { path, bytes } => {
            validate_workspace_path(path)?;
            validate_write_bytes("bytes", *bytes)
        }
        Action::InstallPackage { name } => validate_package_name(name),
        Action::ConfigureNetwork { interface, .. } => validate_symbol("interface", interface),
        Action::Reboot | Action::Shutdown => Ok(()),
        Action::RunSandboxedCommand { argv } => validate_command_argv(argv),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn granted(capabilities: &[Capability]) -> CapabilitySet {
        CapabilitySet::from_slice(capabilities)
    }

    #[test]
    fn allows_read_only_status_when_observed() {
        let action = Action::ReadStatus {
            name: "cpu".to_string(),
        };
        let decision = Policy::new()
            .evaluate("show cpu", &action, granted(&[Capability::Observe]), None)
            .expect("read-only action should be well-formed");
        assert!(matches!(decision, Decision::Allow { .. }));
    }

    #[test]
    fn denies_missing_capability_without_prompting() {
        let action = Action::ConfigureNetwork {
            interface: "wlan0".to_string(),
            enable: true,
        };
        let decision = Policy::new()
            .evaluate("go online", &action, granted(&[Capability::Observe]), None)
            .expect("action is well-formed but unauthorized");
        assert!(
            matches!(decision, Decision::Deny { missing, .. } if missing.contains(Capability::Network))
        );
    }

    #[test]
    fn confirmation_is_required_for_destructive_action() {
        let action = Action::DeleteWorkspaceFile {
            path: "notes/tmp.txt".to_string(),
        };
        let decision = Policy::new()
            .evaluate(
                "delete the temp note",
                &action,
                granted(&[Capability::WriteWorkspace]),
                None,
            )
            .expect("delete action is well-formed");
        assert!(matches!(
            decision,
            Decision::Confirm {
                ref summary,
                consequence: Consequence::EraseData,
                ..
            } if summary == "delete workspace file `notes/tmp.txt`"
        ));
    }

    fn confirm_token(policy: &Policy, action: &Action, caps: CapabilitySet) -> ConfirmToken {
        match policy.evaluate("approve", action, caps, None) {
            Ok(Decision::Confirm { token, .. }) => token,
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn token_for_one_action_is_rejected_for_another() {
        let policy = Policy::new();
        let caps = granted(&[Capability::WriteWorkspace]);
        let shown = Action::DeleteWorkspaceFile {
            path: "notes/tmp.txt".to_string(),
        };
        let swapped = Action::DeleteWorkspaceFile {
            path: "notes/important.txt".to_string(),
        };
        let token = confirm_token(&policy, &shown, caps);
        assert_eq!(
            policy.evaluate("delete", &swapped, caps, Some(token)),
            Err(PolicyError::ConfirmationMismatch)
        );
        assert!(matches!(
            policy.evaluate("delete", &shown, caps, Some(token)),
            Ok(Decision::Allow { .. })
        ));
    }

    #[test]
    fn token_from_another_policy_instance_is_rejected() {
        let caps = granted(&[Capability::Power]);
        let token = confirm_token(&Policy::new(), &Action::Reboot, caps);
        assert_eq!(
            Policy::new().evaluate("restart", &Action::Reboot, caps, Some(token)),
            Err(PolicyError::ConfirmationMismatch)
        );
    }

    #[test]
    fn confirmed_write_is_allowed() {
        let action = Action::WriteWorkspaceFile {
            path: "src/main.rs".to_string(),
            bytes: 32,
        };
        let policy = Policy::new();
        let caps = granted(&[Capability::WriteWorkspace]);
        let token = confirm_token(&policy, &action, caps);
        let decision = policy
            .evaluate("write main", &action, caps, Some(token))
            .expect("write action is well-formed");
        assert!(matches!(decision, Decision::Allow { .. }));
    }

    #[test]
    fn workspace_patch_requires_confirmation_even_when_small() {
        let action = Action::ApplyWorkspacePatch {
            path: "README.md".to_string(),
            bytes: 12,
        };
        let decision = Policy::new()
            .evaluate(
                "apply readme patch",
                &action,
                granted(&[Capability::WriteWorkspace]),
                None,
            )
            .expect("patch is well-formed");
        assert!(matches!(
            decision,
            Decision::Confirm {
                consequence: Consequence::WorkspaceWrite,
                ..
            }
        ));
    }

    #[test]
    fn rejects_empty_and_control_laden_raw_input() {
        let action = Action::ReadStatus {
            name: "cpu".to_string(),
        };
        assert_eq!(
            Policy::new().evaluate("   \n\t", &action, CapabilitySet::all(), None),
            Err(PolicyError::EmptyInput)
        );
        assert_eq!(
            Policy::new().evaluate("show\x07cpu", &action, CapabilitySet::all(), None),
            Err(PolicyError::InvalidCharacter { field: "input" })
        );
    }

    #[test]
    fn rejects_overlong_raw_input() {
        let raw = "a".repeat(MAX_INPUT_BYTES + 1);
        let action = Action::ReadStatus {
            name: "cpu".to_string(),
        };
        assert_eq!(
            Policy::new().evaluate(&raw, &action, CapabilitySet::all(), None),
            Err(PolicyError::InputTooLong {
                max: MAX_INPUT_BYTES,
                actual: MAX_INPUT_BYTES + 1,
            })
        );
    }

    #[test]
    fn rejects_path_traversal_and_absolute_paths() {
        let traversal = Action::ReadWorkspaceFile {
            path: "../secrets".to_string(),
        };
        let absolute = Action::ReadWorkspaceFile {
            path: "/etc/shadow".to_string(),
        };
        assert!(matches!(
            Policy::new().evaluate(
                "read",
                &traversal,
                granted(&[Capability::ReadWorkspace]),
                None
            ),
            Err(PolicyError::InvalidPath { .. })
        ));
        assert!(matches!(
            Policy::new().evaluate(
                "read",
                &absolute,
                granted(&[Capability::ReadWorkspace]),
                None
            ),
            Err(PolicyError::InvalidPath { .. })
        ));
    }

    #[test]
    fn rejects_empty_segments_backslashes_and_dot_segments() {
        for path in ["a//b", "a/./b", "./b", "a/b/", "a\\b", ""] {
            let action = Action::ReadWorkspaceFile {
                path: path.to_string(),
            };
            assert!(matches!(
                Policy::new().evaluate(
                    "read",
                    &action,
                    granted(&[Capability::ReadWorkspace]),
                    None
                ),
                Err(PolicyError::InvalidPath { .. })
            ));
        }
    }

    #[test]
    fn accepts_nested_unix_style_workspace_paths() {
        let action = Action::ReadWorkspaceFile {
            path: "docs/notes/äon.txt".to_string(),
        };
        let decision = Policy::new()
            .evaluate(
                "read note",
                &action,
                granted(&[Capability::ReadWorkspace]),
                None,
            )
            .expect("nested UTF-8 workspace path should be valid");
        assert!(matches!(decision, Decision::Allow { .. }));
    }

    #[test]
    fn rejects_uppercase_package_names() {
        let action = Action::InstallPackage {
            name: "Vim".to_string(),
        };
        assert_eq!(
            Policy::new().evaluate(
                "install vim",
                &action,
                granted(&[Capability::ModifySystem]),
                None
            ),
            Err(PolicyError::InvalidArgument {
                field: "package",
                reason: "must start with a lowercase ASCII letter or digit",
            })
        );
    }

    #[test]
    fn rejects_empty_sandbox_argv() {
        let action = Action::RunSandboxedCommand { argv: Vec::new() };
        assert_eq!(
            Policy::new().evaluate(
                "run command",
                &action,
                granted(&[Capability::ModifySystem]),
                None
            ),
            Err(PolicyError::InvalidArgument {
                field: "argv",
                reason: "must contain at least one argument",
            })
        );
    }

    #[test]
    fn rejects_leading_dash_executable() {
        let action = Action::RunSandboxedCommand {
            argv: vec!["-rm".to_string()],
        };
        assert_eq!(
            Policy::new().evaluate(
                "run command",
                &action,
                granted(&[Capability::ModifySystem]),
                None
            ),
            Err(PolicyError::InvalidArgument {
                field: "argv",
                reason: "executable must not start with `-`",
            })
        );
    }

    #[test]
    fn rejects_oversized_workspace_write() {
        let action = Action::WriteWorkspaceFile {
            path: "big.bin".to_string(),
            bytes: MAX_WRITE_BYTES + 1,
        };
        assert_eq!(
            Policy::new().evaluate(
                "write",
                &action,
                granted(&[Capability::WriteWorkspace]),
                None
            ),
            Err(PolicyError::InvalidArgument {
                field: "bytes",
                reason: "exceeds the maximum single-operation write budget",
            })
        );
    }

    #[test]
    fn policy_budget_must_be_sane() {
        assert!(matches!(
            Policy::new().with_max_input_bytes(10),
            Err(PolicyError::InvalidArgument {
                field: "max_input_bytes",
                ..
            })
        ));
        assert!(Policy::new().with_max_input_bytes(64).is_ok());
        let policy = Policy::new()
            .with_max_input_bytes(64)
            .expect("64-byte budget is valid");
        let action = Action::ReadStatus {
            name: "cpu".to_string(),
        };
        let raw = "a".repeat(65);
        assert_eq!(
            policy.evaluate(&raw, &action, granted(&[Capability::Observe]), None),
            Err(PolicyError::InputTooLong {
                max: 64,
                actual: 65
            })
        );
    }

    #[test]
    fn capability_set_contains_union_and_empty() {
        let set = CapabilitySet::empty()
            .with(Capability::Observe)
            .with(Capability::Power);
        assert!(!set.is_empty());
        assert!(set.contains(Capability::Observe));
        assert!(set.contains(Capability::Power));
        assert!(!set.contains(Capability::Network));
        assert!(CapabilitySet::all().contains(Capability::ModifySystem));
        assert!(CapabilitySet::empty().is_empty());
    }

    #[test]
    fn power_actions_require_power_capability_and_confirmation() {
        let action = Action::Reboot;
        let denied = Policy::new()
            .evaluate("restart", &action, granted(&[Capability::Observe]), None)
            .expect("reboot is well-formed");
        assert!(
            matches!(denied, Decision::Deny { missing, .. } if missing.contains(Capability::Power))
        );

        let unconfirmed = Policy::new()
            .evaluate("restart", &action, granted(&[Capability::Power]), None)
            .expect("reboot is well-formed");
        assert!(matches!(
            unconfirmed,
            Decision::Confirm {
                consequence: Consequence::Power,
                ..
            }
        ));
    }
}
