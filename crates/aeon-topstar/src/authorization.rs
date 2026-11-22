use serde::{Deserialize, Serialize};

use crate::{CapabilityId, Identity, Policy, ResponsibilityContract};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorizationDecision {
    Allow,
    Deny(String),
}

pub fn authorize(
    identity: &Identity,
    capability: CapabilityId,
    responsibility: &ResponsibilityContract,
    policy: &Policy,
    now_unix: i64,
) -> AuthorizationDecision {
    if responsibility.actor != identity.id {
        return AuthorizationDecision::Deny("responsibility actor mismatch".into());
    }
    if !responsibility.allows(capability, now_unix) {
        return AuthorizationDecision::Deny("responsibility contract denies capability".into());
    }
    if !policy.applies_to(identity) || !policy.permits(capability) {
        return AuthorizationDecision::Deny("policy denies capability".into());
    }
    AuthorizationDecision::Allow
}
