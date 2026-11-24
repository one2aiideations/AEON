//! Tenant- and principal-scoped access control for agent memory records.
//!
//! Pure authorization policy: authentication and durable policy storage are
//! supplied by the host. Deny rules take precedence over allow rules.

pub type TenantId = u64;
pub type PrincipalId = u64;
pub type MemoryId = u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryAction {
    Read,
    Search,
    Append,
    Update,
    Delete,
    Share,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRule {
    pub tenant_id: TenantId,
    /// `None` means any authenticated principal in the tenant.
    pub principal_id: Option<PrincipalId>,
    /// `None` means every memory record in the tenant.
    pub memory_id: Option<MemoryId>,
    pub action: MemoryAction,
    pub effect: Effect,
    pub priority: i32,
    pub expires_at_ns: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessRequest {
    pub tenant_id: TenantId,
    pub principal_id: PrincipalId,
    pub memory_id: MemoryId,
    pub action: MemoryAction,
    pub now_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionReason {
    ExplicitDeny,
    ExplicitAllow,
    DefaultDeny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessDecision {
    pub allowed: bool,
    pub reason: DecisionReason,
    pub matched_rule: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyError {
    CapacityReached,
    InvalidExpiry,
}

pub struct MemoryAccessPolicy {
    rules: Vec<AccessRule>,
    max_rules: usize,
}

impl MemoryAccessPolicy {
    pub fn new(max_rules: usize) -> Self {
        Self {
            rules: Vec::new(),
            max_rules,
        }
    }

    pub fn add_rule(&mut self, rule: AccessRule) -> Result<usize, PolicyError> {
        if self.rules.len() >= self.max_rules {
            return Err(PolicyError::CapacityReached);
        }
        if rule.expires_at_ns.is_some_and(|expiry| expiry == 0) {
            return Err(PolicyError::InvalidExpiry);
        }
        let index = self.rules.len();
        self.rules.push(rule);
        Ok(index)
    }

    pub fn remove_rule(&mut self, index: usize) -> bool {
        if index >= self.rules.len() {
            return false;
        }
        self.rules.remove(index);
        true
    }

    pub fn authorize(&self, request: AccessRequest) -> AccessDecision {
        let mut matching: Vec<(usize, &AccessRule)> = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule_matches(rule, request))
            .collect();

        // Any matching deny overrides all allows, independent of priority.
        if let Some((index, _)) = matching
            .iter()
            .find(|(_, rule)| rule.effect == Effect::Deny)
        {
            return AccessDecision {
                allowed: false,
                reason: DecisionReason::ExplicitDeny,
                matched_rule: Some(*index),
            };
        }

        matching.sort_by(|(index_a, a), (index_b, b)| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| index_a.cmp(index_b))
        });

        if let Some((index, _)) = matching.first() {
            AccessDecision {
                allowed: true,
                reason: DecisionReason::ExplicitAllow,
                matched_rule: Some(*index),
            }
        } else {
            AccessDecision {
                allowed: false,
                reason: DecisionReason::DefaultDeny,
                matched_rule: None,
            }
        }
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

fn rule_matches(rule: &AccessRule, request: AccessRequest) -> bool {
    rule.tenant_id == request.tenant_id
        && rule
            .principal_id
            .is_none_or(|id| id == request.principal_id)
        && rule.memory_id.is_none_or(|id| id == request.memory_id)
        && rule.action == request.action
        && rule
            .expires_at_ns
            .is_none_or(|expiry| request.now_ns < expiry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(effect: Effect, principal_id: Option<u64>, priority: i32) -> AccessRule {
        AccessRule {
            tenant_id: 5,
            principal_id,
            memory_id: None,
            action: MemoryAction::Read,
            effect,
            priority,
            expires_at_ns: None,
        }
    }

    fn request() -> AccessRequest {
        AccessRequest {
            tenant_id: 5,
            principal_id: 42,
            memory_id: 100,
            action: MemoryAction::Read,
            now_ns: 10,
        }
    }

    #[test]
    fn deny_overrides_allow() {
        let mut policy = MemoryAccessPolicy::new(10);
        assert!(policy.add_rule(rule(Effect::Allow, None, 100)).is_ok());
        assert!(policy.add_rule(rule(Effect::Deny, Some(42), -100)).is_ok());
        let decision = policy.authorize(request());
        assert!(!decision.allowed);
        assert_eq!(decision.reason, DecisionReason::ExplicitDeny);
    }

    #[test]
    fn tenant_and_expiry_are_enforced() {
        let mut policy = MemoryAccessPolicy::new(10);
        let mut allow = rule(Effect::Allow, None, 1);
        allow.tenant_id = 6;
        assert!(policy.add_rule(allow).is_ok());
        let mut expired = rule(Effect::Allow, Some(42), 2);
        expired.expires_at_ns = Some(10);
        assert!(policy.add_rule(expired).is_ok());
        assert_eq!(
            policy.authorize(request()).reason,
            DecisionReason::DefaultDeny
        );
    }

    #[test]
    fn capacity_and_rule_removal_are_bounded() {
        let mut policy = MemoryAccessPolicy::new(1);
        assert!(policy.add_rule(rule(Effect::Allow, None, 1)).is_ok());
        assert_eq!(
            policy.add_rule(rule(Effect::Deny, None, 2)),
            Err(PolicyError::CapacityReached)
        );
        assert!(policy.remove_rule(0));
        assert_eq!(policy.rule_count(), 0);
    }
}
