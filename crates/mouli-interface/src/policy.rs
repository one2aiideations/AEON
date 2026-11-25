//! Roles, policy and the decision to allow, hold for approval, or deny a capability call.

use crate::capability::{Resource, Risk};
use std::collections::BTreeSet;

/// A named set of permissions. A permission is a capability name (`mail.send`), an
/// application wildcard (`mail.*`) or `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    /// Role name.
    pub name: String,
    perms: BTreeSet<String>,
}

impl Role {
    /// Build a role.
    #[must_use]
    pub fn new(name: &str, perms: &[&str]) -> Self {
        Self {
            name: name.into(),
            perms: perms.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    /// Whether the role holds `permission`.
    #[must_use]
    pub fn can(&self, permission: &str) -> bool {
        if self.perms.contains("*") || self.perms.contains(permission) {
            return true;
        }
        permission
            .split_once('.')
            .is_some_and(|(app, _)| self.perms.contains(&format!("{app}.*")))
    }
}

/// Outcome of a policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run now.
    Allow,
    /// Hold until someone else approves.
    NeedsApproval,
    /// Refuse, with the reason.
    Deny(String),
}

/// Workspace-wide policy.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Calls that use an external (hosted) model are allowed.
    pub allow_external_models: bool,
    /// Calls that use the network are allowed.
    pub allow_network: bool,
    /// Calls that start host processes are allowed.
    pub allow_host_processes: bool,
    /// Capabilities that always need approval, whatever their risk.
    pub require_approval: BTreeSet<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_external_models: false,
            allow_network: true,
            allow_host_processes: false,
            require_approval: BTreeSet::new(),
        }
    }
}

impl Policy {
    /// Decide what happens to a call of `capability` made by `role`.
    #[must_use]
    pub fn check(&self, capability: &str, risk: Risk, uses: &[Resource], role: &Role) -> Verdict {
        if !role.can(capability) {
            return Verdict::Deny(format!("role {} lacks permission {capability}", role.name));
        }
        for r in uses {
            let (allowed, what) = match r {
                Resource::ExternalModel => (self.allow_external_models, "external models"),
                Resource::Network => (self.allow_network, "network access"),
                Resource::HostProcess => (self.allow_host_processes, "host processes"),
            };
            if !allowed {
                return Verdict::Deny(format!("policy forbids {what}"));
            }
        }
        if risk == Risk::Consequential || self.require_approval.contains(capability) {
            Verdict::NeedsApproval
        } else {
            Verdict::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_wildcards() {
        let r = Role::new("clerk", &["mail.*", "sheet.read"]);
        assert!(r.can("mail.send") && r.can("sheet.read"));
        assert!(!r.can("sheet.write") && !r.can("shell"));
        assert!(Role::new("root", &["*"]).can("anything.at.all"));
    }

    #[test]
    fn verdicts() {
        let p = Policy::default();
        let r = Role::new("u", &["*"]);
        assert_eq!(p.check("a.read", Risk::Read, &[], &r), Verdict::Allow);
        assert_eq!(
            p.check("a.send", Risk::Consequential, &[], &r),
            Verdict::NeedsApproval
        );
        assert!(matches!(
            p.check("a.x", Risk::Read, &[Resource::ExternalModel], &r),
            Verdict::Deny(_)
        ));
        assert!(matches!(
            p.check("a.x", Risk::Read, &[Resource::HostProcess], &r),
            Verdict::Deny(_)
        ));
        assert_eq!(
            p.check("a.x", Risk::Read, &[Resource::Network], &r),
            Verdict::Allow
        );
        let none = Role::new("guest", &[]);
        assert!(matches!(
            p.check("a.read", Risk::Read, &[], &none),
            Verdict::Deny(_)
        ));
        let mut p2 = Policy::default();
        p2.require_approval.insert("a.read".into());
        assert_eq!(
            p2.check("a.read", Risk::Read, &[], &r),
            Verdict::NeedsApproval
        );
    }
}
