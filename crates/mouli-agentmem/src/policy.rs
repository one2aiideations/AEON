//! M033 — Agent Capability Policy Engine.
//! Deterministic tenant-scoped authorization with explicit deny precedence,
//! bounded policy storage, capability budgets and append-only decision records.
//! Authentication and trusted identity issuance remain upstream responsibilities.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub type TenantId = u64;
pub type AgentId = u128;
pub type PolicyId = u64;

const MAX_POLICIES: usize = 100_000;
const MAX_AUDIT_RECORDS: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 256;
const MAX_CONTEXT_FIELDS: usize = 64;
const MAX_CONTEXT_VALUE_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchValue {
    Any,
    Exact(String),
    Prefix(String),
    OneOf(BTreeSet<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityBudget {
    pub max_calls: Option<u64>,
    pub max_bytes_in: Option<u64>,
    pub max_bytes_out: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub id: PolicyId,
    pub tenant_id: TenantId,
    pub agent: MatchValue,
    pub resource: MatchValue,
    pub action: MatchValue,
    pub effect: Effect,
    pub priority: i32,
    pub expires_at_unix_ms: Option<u64>,
    pub required_context: BTreeMap<String, MatchValue>,
    pub budget: Option<CapabilityBudget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RequestContext {
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationRequest {
    pub tenant_id: TenantId,
    pub agent_id: AgentId,
    pub resource: String,
    pub action: String,
    pub context: RequestContext,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionReason {
    ExplicitDeny,
    AllowedByPolicy,
    NoMatchingPolicy,
    PolicyExpired,
    BudgetExceeded,
    InvalidRequest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationDecision {
    pub allowed: bool,
    pub policy_id: Option<PolicyId>,
    pub reason: DecisionReason,
    pub remaining_calls: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRecord {
    pub sequence: u64,
    pub timestamp_unix_ms: u64,
    pub tenant_id: TenantId,
    pub agent_id: AgentId,
    pub resource: String,
    pub action: String,
    pub allowed: bool,
    pub policy_id: Option<PolicyId>,
    pub reason: DecisionReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    InvalidPolicy(&'static str),
    PolicyLimit,
    AuditLimit,
    IdExhausted,
    DuplicatePolicy,
    NotFound,
    Poisoned,
}
impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PolicyError {}

#[derive(Clone, Debug, Default)]
struct Usage {
    calls: u64,
    bytes_in: u64,
    bytes_out: u64,
}

#[derive(Default)]
struct EngineState {
    policies: BTreeMap<PolicyId, Policy>,
    usage: BTreeMap<(PolicyId, TenantId, AgentId), Usage>,
    audit: Vec<AuditRecord>,
    next_policy_id: PolicyId,
    next_audit_sequence: u64,
}

#[derive(Clone)]
pub struct PolicyEngine {
    state: Arc<Mutex<EngineState>>,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_BYTES && !value.chars().any(char::is_control)
}

fn match_value(rule: &MatchValue, value: &str) -> bool {
    match rule {
        MatchValue::Any => true,
        MatchValue::Exact(expected) => expected == value,
        MatchValue::Prefix(prefix) => value.starts_with(prefix),
        MatchValue::OneOf(values) => values.contains(value),
    }
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_policy(&self, mut policy: Policy) -> Result<PolicyId, PolicyError> {
        validate_policy(&policy)?;
        let mut state = self.state.lock().map_err(|_| PolicyError::Poisoned)?;
        if state.policies.len() >= MAX_POLICIES {
            return Err(PolicyError::PolicyLimit);
        }
        if policy.id != 0 && state.policies.contains_key(&policy.id) {
            return Err(PolicyError::DuplicatePolicy);
        }
        if policy.id == 0 {
            state.next_policy_id = state
                .next_policy_id
                .checked_add(1)
                .ok_or(PolicyError::IdExhausted)?;
            policy.id = state.next_policy_id;
        } else if policy.id > state.next_policy_id {
            state.next_policy_id = policy.id;
        }
        let id = policy.id;
        state.policies.insert(id, policy);
        Ok(id)
    }

    pub fn remove_policy(&self, id: PolicyId) -> Result<Policy, PolicyError> {
        let mut state = self.state.lock().map_err(|_| PolicyError::Poisoned)?;
        state.policies.remove(&id).ok_or(PolicyError::NotFound)
    }

    pub fn list_policies(&self, tenant_id: TenantId) -> Result<Vec<Policy>, PolicyError> {
        let state = self.state.lock().map_err(|_| PolicyError::Poisoned)?;
        Ok(state
            .policies
            .values()
            .filter(|p| p.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    pub fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizationDecision, PolicyError> {
        validate_request(&request).map_err(PolicyError::InvalidPolicy)?;
        let now = now_unix_ms();
        let mut state = self.state.lock().map_err(|_| PolicyError::Poisoned)?;
        if state.audit.len() >= MAX_AUDIT_RECORDS {
            return Err(PolicyError::AuditLimit);
        }
        let mut candidates: Vec<&Policy> = state
            .policies
            .values()
            .filter(|policy| policy.tenant_id == request.tenant_id)
            .filter(|policy| {
                policy
                    .expires_at_unix_ms
                    .map(|expiry| expiry > now)
                    .unwrap_or(true)
            })
            .filter(|policy| match_value(&policy.agent, &request.agent_id.to_string()))
            .filter(|policy| match_value(&policy.resource, &request.resource))
            .filter(|policy| match_value(&policy.action, &request.action))
            .filter(|policy| {
                policy.required_context.iter().all(|(key, matcher)| {
                    request
                        .context
                        .fields
                        .get(key)
                        .map(|value| match_value(matcher, value))
                        .unwrap_or(false)
                })
            })
            .collect();

        // Explicit deny wins regardless of priority. Within an effect, highest
        // priority wins; policy ID is the deterministic tie breaker.
        candidates.sort_by(|a, b| {
            let effect_rank = |effect: &Effect| if *effect == Effect::Deny { 0 } else { 1 };
            effect_rank(&a.effect)
                .cmp(&effect_rank(&b.effect))
                .then_with(|| b.priority.cmp(&a.priority))
                .then_with(|| a.id.cmp(&b.id))
        });

        let selected = candidates.first().copied().cloned();
        let decision = match selected {
            None => AuthorizationDecision {
                allowed: false,
                policy_id: None,
                reason: DecisionReason::NoMatchingPolicy,
                remaining_calls: None,
            },
            Some(policy) if policy.effect == Effect::Deny => AuthorizationDecision {
                allowed: false,
                policy_id: Some(policy.id),
                reason: DecisionReason::ExplicitDeny,
                remaining_calls: None,
            },
            Some(policy) => {
                let key = (policy.id, request.tenant_id, request.agent_id);
                let usage = state.usage.entry(key).or_default();
                let exceeded = policy
                    .budget
                    .as_ref()
                    .map(|budget| {
                        budget
                            .max_calls
                            .map(|limit| usage.calls >= limit)
                            .unwrap_or(false)
                            || budget
                                .max_bytes_in
                                .map(|limit| {
                                    request.bytes_in > limit.saturating_sub(usage.bytes_in)
                                })
                                .unwrap_or(false)
                            || budget
                                .max_bytes_out
                                .map(|limit| {
                                    request.bytes_out > limit.saturating_sub(usage.bytes_out)
                                })
                                .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if exceeded {
                    AuthorizationDecision {
                        allowed: false,
                        policy_id: Some(policy.id),
                        reason: DecisionReason::BudgetExceeded,
                        remaining_calls: policy
                            .budget
                            .as_ref()
                            .and_then(|b| b.max_calls)
                            .map(|limit| limit.saturating_sub(usage.calls)),
                    }
                } else {
                    usage.calls = usage.calls.saturating_add(1);
                    usage.bytes_in = usage.bytes_in.saturating_add(request.bytes_in);
                    usage.bytes_out = usage.bytes_out.saturating_add(request.bytes_out);
                    AuthorizationDecision {
                        allowed: true,
                        policy_id: Some(policy.id),
                        reason: DecisionReason::AllowedByPolicy,
                        remaining_calls: policy
                            .budget
                            .as_ref()
                            .and_then(|b| b.max_calls)
                            .map(|limit| limit.saturating_sub(usage.calls)),
                    }
                }
            }
        };
        append_audit(&mut state, &request, &decision)?;
        Ok(decision)
    }

    pub fn audit_records(
        &self,
        tenant_id: TenantId,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, PolicyError> {
        let state = self.state.lock().map_err(|_| PolicyError::Poisoned)?;
        Ok(state
            .audit
            .iter()
            .rev()
            .filter(|record| record.tenant_id == tenant_id)
            .take(limit.min(MAX_AUDIT_RECORDS))
            .cloned()
            .collect())
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(EngineState::default())),
        }
    }
}

fn validate_policy(policy: &Policy) -> Result<(), PolicyError> {
    if !valid_name(&policy.resource_name_for_validation()) {
        return Err(PolicyError::InvalidPolicy("invalid resource matcher"));
    }
    if policy.required_context.len() > MAX_CONTEXT_FIELDS {
        return Err(PolicyError::InvalidPolicy("too many context fields"));
    }
    for (key, matcher) in &policy.required_context {
        if !valid_name(key) {
            return Err(PolicyError::InvalidPolicy("invalid context key"));
        }
        if let MatchValue::Exact(value) | MatchValue::Prefix(value) = matcher {
            if value.len() > MAX_CONTEXT_VALUE_BYTES {
                return Err(PolicyError::InvalidPolicy("context matcher too large"));
            }
        }
        if let MatchValue::OneOf(values) = matcher {
            if values.len() > 256 || values.iter().any(|v| v.len() > MAX_CONTEXT_VALUE_BYTES) {
                return Err(PolicyError::InvalidPolicy("context set too large"));
            }
        }
    }
    for matcher in [&policy.agent, &policy.resource, &policy.action] {
        match matcher {
            MatchValue::Exact(value) | MatchValue::Prefix(value)
                if value.len() > MAX_NAME_BYTES =>
            {
                return Err(PolicyError::InvalidPolicy("matcher too large"))
            }
            MatchValue::OneOf(values)
                if values.len() > 1024 || values.iter().any(|v| v.len() > MAX_NAME_BYTES) =>
            {
                return Err(PolicyError::InvalidPolicy("matcher set too large"))
            }
            _ => {}
        }
    }
    Ok(())
}

impl Policy {
    fn resource_name_for_validation(&self) -> String {
        match &self.resource {
            MatchValue::Exact(value) | MatchValue::Prefix(value) => value.clone(),
            MatchValue::OneOf(values) => values.iter().next().cloned().unwrap_or_default(),
            MatchValue::Any => "*".to_owned(),
        }
    }
}

fn validate_request(request: &AuthorizationRequest) -> Result<(), &'static str> {
    if !valid_name(&request.resource) || !valid_name(&request.action) {
        return Err("resource/action name invalid");
    }
    if request.context.fields.len() > MAX_CONTEXT_FIELDS {
        return Err("too many context fields");
    }
    for (key, value) in &request.context.fields {
        if !valid_name(key) || value.len() > MAX_CONTEXT_VALUE_BYTES {
            return Err("context field invalid or too large");
        }
    }
    Ok(())
}

fn append_audit(
    state: &mut EngineState,
    request: &AuthorizationRequest,
    decision: &AuthorizationDecision,
) -> Result<(), PolicyError> {
    if state.audit.len() >= MAX_AUDIT_RECORDS {
        return Err(PolicyError::AuditLimit);
    }
    state.next_audit_sequence = state
        .next_audit_sequence
        .checked_add(1)
        .ok_or(PolicyError::IdExhausted)?;
    state.audit.push(AuditRecord {
        sequence: state.next_audit_sequence,
        timestamp_unix_ms: now_unix_ms(),
        tenant_id: request.tenant_id,
        agent_id: request.agent_id,
        resource: request.resource.clone(),
        action: request.action.clone(),
        allowed: decision.allowed,
        policy_id: decision.policy_id,
        reason: decision.reason.clone(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(tenant: u64, effect: Effect, priority: i32) -> Policy {
        Policy {
            id: 0,
            tenant_id: tenant,
            agent: MatchValue::Any,
            resource: MatchValue::Exact("mcp.tool.text.echo".into()),
            action: MatchValue::Exact("invoke".into()),
            effect,
            priority,
            expires_at_unix_ms: None,
            required_context: BTreeMap::new(),
            budget: None,
        }
    }
    fn request(tenant: u64) -> AuthorizationRequest {
        AuthorizationRequest {
            tenant_id: tenant,
            agent_id: 77,
            resource: "mcp.tool.text.echo".into(),
            action: "invoke".into(),
            context: RequestContext::default(),
            bytes_in: 10,
            bytes_out: 20,
        }
    }

    #[test]
    fn deny_overrides_allow_and_is_audited() {
        let engine = PolicyEngine::new();
        engine
            .add_policy(policy(4, Effect::Allow, 100))
            .expect("allow");
        engine
            .add_policy(policy(4, Effect::Deny, -10))
            .expect("deny");
        let decision = engine.authorize(request(4)).expect("authorize");
        assert!(!decision.allowed);
        assert_eq!(decision.reason, DecisionReason::ExplicitDeny);
        assert_eq!(engine.audit_records(4, 10).expect("audit").len(), 1);
    }

    #[test]
    fn tenant_scoping_prevents_cross_tenant_grants() {
        let engine = PolicyEngine::new();
        engine
            .add_policy(policy(4, Effect::Allow, 1))
            .expect("policy");
        assert!(!engine.authorize(request(5)).expect("authorize").allowed);
    }

    #[test]
    fn call_budget_is_enforced() {
        let engine = PolicyEngine::new();
        let mut p = policy(4, Effect::Allow, 1);
        p.budget = Some(CapabilityBudget {
            max_calls: Some(1),
            max_bytes_in: None,
            max_bytes_out: None,
        });
        engine.add_policy(p).expect("policy");
        assert!(engine.authorize(request(4)).expect("first").allowed);
        let second = engine.authorize(request(4)).expect("second");
        assert!(!second.allowed);
        assert_eq!(second.reason, DecisionReason::BudgetExceeded);
    }
}
