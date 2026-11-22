//! Kernel-side hook accounting and the fast eBPF pre-classifier.
//!
//! The eBPF program cannot verify signatures, so it only sorts events into
//! "not semantic, pass" and "send to the identity gate". [`HookTable`]
//! wraps [`net_decide`] with counters for the attached hooks.

use crate::gate::{net_decide, IoClass, NetAction, NetHook};
use crate::registry::DidRegistry;
use crate::semantic::Frame;
use crate::IdError;

/// Hook counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookStats {
    /// Transmit events seen.
    pub tx: u64,
    /// Receive events seen.
    pub rx: u64,
    /// Events held for identity.
    pub deferred: u64,
    /// Events dropped.
    pub dropped: u64,
}

/// Attached network hooks.
#[derive(Debug, Clone, Default)]
pub struct HookTable {
    attached: bool,
    stats: HookStats,
}

impl HookTable {
    /// Attach (resets counters).
    pub fn attach(&mut self) {
        self.attached = true;
        self.stats = HookStats::default();
    }

    /// Detach.
    pub fn detach(&mut self) -> Result<(), IdError> {
        if !self.attached {
            return Err(IdError::BadArg);
        }
        self.attached = false;
        Ok(())
    }

    /// Decide and count one event.
    pub fn event(
        &mut self,
        registry: &DidRegistry,
        hook: NetHook,
        semantic_candidate: bool,
        frame: Option<&Frame>,
    ) -> Result<NetAction, IdError> {
        if !self.attached {
            return Err(IdError::BadArg);
        }
        let action = net_decide(registry, hook, semantic_candidate, frame);
        match hook {
            NetHook::PreTx => self.stats.tx += 1,
            NetHook::PreRx => self.stats.rx += 1,
        }
        match action {
            NetAction::DeferIdentity => self.stats.deferred += 1,
            NetAction::Drop => self.stats.dropped += 1,
            NetAction::Accept => {}
        }
        Ok(action)
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> HookStats {
        self.stats
    }
}

/// eBPF pre-classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classify {
    /// Not semantic traffic: pass without the identity gate.
    NotSemantic,
    /// Semantic traffic: must go through the identity gate.
    IdentityGate,
}

/// Classify an event for process `pid` (0 is invalid).
pub fn classify(pid: u32, _class: IoClass, semantic_candidate: bool) -> Result<Classify, IdError> {
    if pid == 0 {
        return Err(IdError::BadArg);
    }
    Ok(if semantic_candidate {
        Classify::IdentityGate
    } else {
        Classify::NotSemantic
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::tests::signed_frame;

    #[test]
    fn counts_events() {
        let (f, reg) = signed_frame();
        let mut h = HookTable::default();
        assert!(h.event(&reg, NetHook::PreTx, false, None).is_err());
        h.attach();
        h.event(&reg, NetHook::PreTx, true, Some(&f)).unwrap();
        h.event(&reg, NetHook::PreRx, true, None).unwrap();
        let mut bad = f;
        bad.digest[1] ^= 1;
        h.event(&reg, NetHook::PreRx, true, Some(&bad)).unwrap();
        assert_eq!(
            h.stats(),
            HookStats {
                tx: 1,
                rx: 2,
                deferred: 1,
                dropped: 1
            }
        );
        h.detach().unwrap();
        assert!(h.detach().is_err());
    }

    #[test]
    fn classifier() {
        assert_eq!(classify(1, IoClass::Ipc, false), Ok(Classify::NotSemantic));
        assert_eq!(classify(1, IoClass::NetTx, true), Ok(Classify::IdentityGate));
        assert!(classify(0, IoClass::NetRx, true).is_err());
    }
}
