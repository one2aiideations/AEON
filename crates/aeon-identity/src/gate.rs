//! I/O gates: submit flags, per-process accounting, and network hook policy.

use crate::registry::DidRegistry;
use crate::semantic::{Frame, Verdict};
use crate::IdError;
use std::collections::HashMap;

/// Extra requirements a submitter may demand on top of the full check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubmitFlags {
    /// Require a human-presence attestation.
    pub require_human: bool,
    /// Require the sender to claim human verification.
    pub require_did: bool,
    /// Require a credential hash.
    pub require_vc: bool,
    /// Require a content address.
    pub require_cid: bool,
}

/// Kind of I/O being gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoClass {
    /// Local inter-process message.
    Ipc,
    /// Outgoing network.
    NetTx,
    /// Incoming network.
    NetRx,
}

/// Check a frame for submission: fast flag checks, then the full gate.
pub fn submit(registry: &DidRegistry, flags: SubmitFlags, f: &Frame) -> Result<(), IdError> {
    if flags.require_did && !f.auth.human_verified {
        return Err(IdError::NotEligible);
    }
    if flags.require_cid && f.cid.is_empty() {
        return Err(IdError::NoCid);
    }
    if flags.require_vc && f.auth.vc_hash == [0; 32] {
        return Err(IdError::NoCredential);
    }
    // The full check always verifies friction, so `require_human` is implied.
    match f.check(registry) {
        Verdict::Allow => Ok(()),
        Verdict::Malformed => Err(IdError::Malformed),
        Verdict::Digest => Err(IdError::Digest),
        Verdict::Identity(e) => Err(e),
    }
}

/// Per-process gate state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessState {
    /// Last submission was accepted.
    pub authenticated: bool,
    /// Accepted submissions.
    pub accepted: u64,
    /// Denied submissions.
    pub denied: u64,
}

/// Tracks gate outcomes per process id.
#[derive(Debug, Clone)]
pub struct ProcessGate {
    states: HashMap<u32, ProcessState>,
    max_processes: usize,
    generation: u32,
}

impl ProcessGate {
    /// Gate tracking at most `max_processes` processes.
    #[must_use]
    pub fn new(max_processes: usize) -> Self {
        Self {
            states: HashMap::new(),
            max_processes,
            generation: 1,
        }
    }

    /// Submit on behalf of `pid`; records the outcome.
    pub fn submit(
        &mut self,
        registry: &DidRegistry,
        pid: u32,
        _class: IoClass,
        flags: SubmitFlags,
        f: &Frame,
    ) -> Result<(), IdError> {
        if pid == 0 {
            return Err(IdError::BadArg);
        }
        if !self.states.contains_key(&pid) && self.states.len() >= self.max_processes {
            return Err(IdError::TooManyProcesses);
        }
        let result = submit(registry, flags, f);
        let st = self.states.entry(pid).or_default();
        match result {
            Ok(()) => {
                st.authenticated = true;
                st.accepted += 1;
            }
            Err(_) => {
                st.authenticated = false;
                st.denied += 1;
            }
        }
        result
    }

    /// State of `pid`, if seen.
    #[must_use]
    pub fn query(&self, pid: u32) -> Option<ProcessState> {
        self.states.get(&pid).copied()
    }

    /// Policy generation (bumped when policy changes).
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Forget all process state and bump the generation.
    pub fn reset_policy(&mut self) {
        self.states.clear();
        self.generation = self.generation.wrapping_add(1).max(1);
    }
}

/// Network hook point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetHook {
    /// Before transmit.
    PreTx,
    /// Before receive.
    PreRx,
}

/// Packet decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetAction {
    /// Drop.
    Drop,
    /// Pass.
    Accept,
    /// Hold until the semantic frame is available.
    DeferIdentity,
}

/// Decide a packet. Non-semantic traffic passes; semantic traffic must
/// carry a frame that passes the gate.
#[must_use]
pub fn net_decide(
    registry: &DidRegistry,
    _hook: NetHook,
    semantic_candidate: bool,
    frame: Option<&Frame>,
) -> NetAction {
    if !semantic_candidate {
        return NetAction::Accept;
    }
    match frame {
        None => NetAction::DeferIdentity,
        Some(f) if f.check(registry) == Verdict::Allow => NetAction::Accept,
        Some(_) => NetAction::Drop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::tests::signed_frame;

    #[test]
    fn process_accounting() {
        let (f, reg) = signed_frame();
        let mut g = ProcessGate::new(1);
        g.submit(&reg, 10, IoClass::NetTx, SubmitFlags::default(), &f)
            .unwrap();
        let mut bad = f.clone();
        bad.digest[0] ^= 1;
        assert_eq!(
            g.submit(&reg, 10, IoClass::Ipc, SubmitFlags::default(), &bad),
            Err(IdError::Digest)
        );
        let st = g.query(10).unwrap();
        assert_eq!((st.accepted, st.denied, st.authenticated), (1, 1, false));
        assert_eq!(
            g.submit(&reg, 11, IoClass::Ipc, SubmitFlags::default(), &f),
            Err(IdError::TooManyProcesses)
        );
        g.reset_policy();
        assert_eq!(g.generation(), 2);
        assert!(g.query(10).is_none());
    }

    #[test]
    fn net_policy() {
        let (f, reg) = signed_frame();
        assert_eq!(net_decide(&reg, NetHook::PreRx, false, None), NetAction::Accept);
        assert_eq!(
            net_decide(&reg, NetHook::PreRx, true, None),
            NetAction::DeferIdentity
        );
        assert_eq!(
            net_decide(&reg, NetHook::PreTx, true, Some(&f)),
            NetAction::Accept
        );
        let mut bad = f;
        bad.cid.clear();
        assert_eq!(
            net_decide(&reg, NetHook::PreTx, true, Some(&bad)),
            NetAction::Drop
        );
    }

    #[test]
    fn submit_flags() {
        let (mut f, reg) = signed_frame();
        f.auth.vc_hash = [0; 32];
        let flags = SubmitFlags {
            require_vc: true,
            ..SubmitFlags::default()
        };
        assert_eq!(submit(&reg, flags, &f), Err(IdError::NoCredential));
    }
}
