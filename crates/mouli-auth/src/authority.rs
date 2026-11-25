//! The authority: one object that decides, audits and issues envelopes. Consumers enforce what
//! it returns; they never decide for themselves.

use crate::envelope::{sign, Claims, MAX_TTL_SECS, MIN_KEY_LEN};
use crate::org::Org;
use crate::policy::{evaluate, Decision, Request, Rule};
use crate::principal::Directory;
use mouli_interface::audit::{AuditLog, AuditRecord};
use mouli_interface::ids::IdGen;
use mouli_interface::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// An authorisation result with its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    /// Decision id (quote it in audit and envelopes).
    pub id: String,
    /// The decision.
    pub decision: Decision,
}

struct Granted {
    subject: String,
    tenant: String,
    node: String,
    action: String,
}

/// The enterprise authority.
pub struct Authority {
    /// Tenant and org hierarchy.
    pub org: Org,
    /// Principals.
    pub directory: Directory,
    rules: Vec<Rule>,
    ids: IdGen,
    audit: AuditLog,
    granted: BTreeMap<String, Granted>,
    revoked: BTreeSet<String>,
    key: Vec<u8>,
}

impl Authority {
    /// Create an authority that signs envelopes with `key`.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the key is shorter than 32 bytes.
    pub fn new(key: &[u8]) -> Result<Self> {
        if key.len() < MIN_KEY_LEN {
            return Err(Error::Invalid(format!(
                "signing key must be at least {MIN_KEY_LEN} bytes"
            )));
        }
        Ok(Self {
            org: Org::new(),
            directory: Directory::new(),
            rules: Vec::new(),
            ids: IdGen::new(),
            audit: AuditLog::new(),
            granted: BTreeMap::new(),
            revoked: BTreeSet::new(),
            key: key.to_vec(),
        })
    }

    /// Add a policy rule.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the rule's tenant or scope node does not exist, or the scope
    /// belongs to another tenant.
    pub fn add_rule(&mut self, rule: Rule, now: u64) -> Result<()> {
        if self.org.tenant_of(&rule.tenant) != Some(rule.tenant.as_str()) {
            return Err(Error::Invalid(format!("unknown tenant {}", rule.tenant)));
        }
        if let Some(s) = &rule.scope {
            if self.org.tenant_of(s) != Some(rule.tenant.as_str()) {
                return Err(Error::Invalid(format!(
                    "scope {s} is not in tenant {}",
                    rule.tenant
                )));
            }
        }
        self.audit
            .append(now, "authority", "policy.add", &rule.id, "ok");
        self.rules.push(rule);
        Ok(())
    }

    /// Decide a request and write the audit record.
    pub fn authorize(&mut self, req: &Request, now: u64) -> Authorization {
        let decision = evaluate(&self.rules, &self.org, &self.directory, req);
        let id = self.ids.next("dec");
        let outcome = format!(
            "{}: {}",
            if decision.allowed { "allow" } else { "deny" },
            decision.reason
        );
        self.audit.append(
            now,
            &req.principal,
            &req.action,
            &req.resource.node,
            &format!("{id} {outcome}"),
        );
        if decision.allowed {
            self.granted.insert(
                id.clone(),
                Granted {
                    subject: req.principal.clone(),
                    tenant: req.resource.tenant.clone(),
                    node: req.resource.node.clone(),
                    action: req.action.clone(),
                },
            );
        }
        Authorization { id, decision }
    }

    /// Issue a signed envelope for an allowed decision, to be accepted only by `audience`.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown or denied decision, [`Error::Denied`] when it was
    /// revoked or the principal has since been deactivated, [`Error::Invalid`] for a bad
    /// lifetime or fields.
    pub fn issue_envelope(
        &mut self,
        decision_id: &str,
        audience: &str,
        correlation_id: &str,
        ttl_secs: u64,
        now: u64,
    ) -> Result<String> {
        let g = self
            .granted
            .get(decision_id)
            .ok_or_else(|| Error::NotFound(format!("allowed decision {decision_id}")))?;
        if self.revoked.contains(decision_id) {
            return Err(Error::Denied("the decision was revoked".into()));
        }
        if !self
            .directory
            .get(&g.tenant, &g.subject)
            .is_some_and(|p| p.active)
        {
            return Err(Error::Denied("the principal is no longer active".into()));
        }
        if ttl_secs == 0 || ttl_secs > MAX_TTL_SECS {
            return Err(Error::Invalid(format!(
                "ttl must be 1..={MAX_TTL_SECS} seconds"
            )));
        }
        let claims = Claims {
            subject: g.subject.clone(),
            tenant: g.tenant.clone(),
            node: g.node.clone(),
            scopes: vec![g.action.clone()],
            audience: audience.into(),
            decision_id: decision_id.into(),
            correlation_id: correlation_id.into(),
            token_id: self.ids.next("jti"),
            issued_at: now,
            expires_at: now + ttl_secs,
        };
        let token = sign(&self.key, &claims)?;
        let subject = claims.subject.clone();
        self.audit.append(
            now,
            &subject,
            "envelope.issue",
            decision_id,
            &format!("audience {audience}, ttl {ttl_secs}s"),
        );
        Ok(token)
    }

    /// Revoke a decision. Pass the id to every verifier (`Verifier::revoke`).
    pub fn revoke(&mut self, decision_id: &str, now: u64) {
        self.revoked.insert(decision_id.into());
        self.audit
            .append(now, "authority", "decision.revoke", decision_id, "ok");
    }

    /// Audit records, hash-chained.
    #[must_use]
    pub fn audit(&self) -> &[AuditRecord] {
        self.audit.records()
    }

    /// Verify the audit chain.
    ///
    /// # Errors
    /// Index of the first bad record.
    pub fn verify_audit(&self) -> std::result::Result<(), usize> {
        self.audit.verify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Verifier;
    use crate::org::Level;
    use crate::policy::{Cond, Effect, Resource};
    use crate::principal::Kind;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn authority() -> Authority {
        let mut a = Authority::new(KEY).unwrap();
        a.org.create_tenant("acme").unwrap();
        a.org.add("co", Level::Company, "acme").unwrap();
        a.org.add("pune", Level::Plant, "co").unwrap();
        a.directory
            .create("ann", "acme", Kind::Human, &["clerk"], &[])
            .unwrap();
        a.add_rule(
            Rule {
                id: "agents".into(),
                tenant: "acme".into(),
                effect: Effect::Allow,
                role: "clerk".into(),
                action: "agent.run".into(),
                resource_kind: "agent".into(),
                scope: Some("co".into()),
                conditions: vec![Cond::AmountAtMost(0)],
            },
            1,
        )
        .unwrap();
        a
    }

    fn req(action: &str) -> Request {
        Request {
            principal: "ann".into(),
            action: action.into(),
            resource: Resource {
                tenant: "acme".into(),
                node: "pune".into(),
                kind: "agent".into(),
                attrs: BTreeMap::new(),
                amount: Some(0),
            },
        }
    }

    #[test]
    fn decide_issue_verify_end_to_end() {
        let mut a = authority();
        let auth = a.authorize(&req("agent.run"), 10);
        assert!(auth.decision.allowed, "{}", auth.decision.reason);
        let token = a
            .issue_envelope(&auth.id, "agentbox", "req-1", 300, 10)
            .unwrap();
        let mut v = Verifier::new(KEY, "agentbox").unwrap();
        let c = v.verify(&token, 20, "acme", "agent.run").unwrap();
        assert_eq!(
            (c.subject.as_str(), c.node.as_str(), c.decision_id.as_str()),
            ("ann", "pune", auth.id.as_str())
        );
        assert!(v.verify(&token, 21, "acme", "agent.run").is_err(), "replay");
        assert!(a.verify_audit().is_ok());
        assert!(a.audit().iter().any(|r| r.action == "envelope.issue"));
    }

    #[test]
    fn denied_decisions_cannot_get_envelopes() {
        let mut a = authority();
        let auth = a.authorize(&req("agent.delete"), 10);
        assert!(!auth.decision.allowed);
        assert!(matches!(
            a.issue_envelope(&auth.id, "agentbox", "r", 60, 10),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            a.issue_envelope("dec-forged", "agentbox", "r", 60, 10),
            Err(Error::NotFound(_))
        ));
        assert!(a.audit().last().unwrap().outcome.contains("deny"));
    }

    #[test]
    fn revocation_and_deactivation_stop_issuing() {
        let mut a = authority();
        let auth = a.authorize(&req("agent.run"), 10);
        a.revoke(&auth.id, 11);
        assert!(matches!(
            a.issue_envelope(&auth.id, "agentbox", "r", 60, 12),
            Err(Error::Denied(_))
        ));
        let second = a.authorize(&req("agent.run"), 13);
        a.directory.deactivate("acme", "ann").unwrap();
        assert!(matches!(
            a.issue_envelope(&second.id, "agentbox", "r", 60, 14),
            Err(Error::Denied(_))
        ));
        assert!(!a.authorize(&req("agent.run"), 15).decision.allowed);
    }

    #[test]
    fn ttl_and_key_and_rule_validation() {
        let mut a = authority();
        let auth = a.authorize(&req("agent.run"), 10);
        assert!(a.issue_envelope(&auth.id, "agentbox", "r", 0, 10).is_err());
        assert!(a
            .issue_envelope(&auth.id, "agentbox", "r", MAX_TTL_SECS + 1, 10)
            .is_err());
        assert!(Authority::new(b"short").is_err());
        let mut bad = Rule {
            id: "x".into(),
            tenant: "nope".into(),
            effect: Effect::Allow,
            role: "*".into(),
            action: "*".into(),
            resource_kind: "*".into(),
            scope: None,
            conditions: vec![],
        };
        assert!(a.add_rule(bad.clone(), 1).is_err());
        bad.tenant = "acme".into();
        a.org.create_tenant("beta").unwrap();
        bad.scope = Some("beta".into());
        assert!(a.add_rule(bad, 1).is_err(), "scope in another tenant");
    }
}
