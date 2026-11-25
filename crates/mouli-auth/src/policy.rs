//! Deny-by-default policy evaluation (RBAC with ABAC conditions and org scope).
//!
//! A request is allowed only when the principal is active, the resource sits in the principal's
//! tenant, at least one allow rule matches, and no deny rule matches. Deny always wins.

use crate::org::Org;
use crate::principal::Directory;
use std::collections::BTreeMap;

/// Rule effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Permit.
    Allow,
    /// Forbid (overrides any allow).
    Deny,
}

/// Extra condition on a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// Principal attribute equals value.
    PrincipalAttr(String, String),
    /// Resource attribute equals value.
    ResourceAttr(String, String),
    /// The resource amount (integer minor units) is present and at most this.
    AmountAtMost(u64),
}

/// A policy rule, always inside one tenant.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Rule id, quoted in decisions.
    pub id: String,
    /// Tenant the rule belongs to.
    pub tenant: String,
    /// Allow or deny.
    pub effect: Effect,
    /// Role the principal must hold, or `*`.
    pub role: String,
    /// Action: exact, `app.*`, or `*`.
    pub action: String,
    /// Resource kind or `*`.
    pub resource_kind: String,
    /// Org node the rule is limited to (covers its descendants); `None` means the whole tenant.
    pub scope: Option<String>,
    /// All must hold.
    pub conditions: Vec<Cond>,
}

/// What is being acted on.
#[derive(Debug, Clone)]
pub struct Resource {
    /// Tenant the caller claims the resource is in.
    pub tenant: String,
    /// Org node the resource belongs to.
    pub node: String,
    /// Kind, for example `invoice`.
    pub kind: String,
    /// Attributes.
    pub attrs: BTreeMap<String, String>,
    /// Amount in minor units, for limit checks.
    pub amount: Option<u64>,
}

/// An authorisation question.
#[derive(Debug, Clone)]
pub struct Request {
    /// Who.
    pub principal: String,
    /// What they want to do.
    pub action: String,
    /// To what.
    pub resource: Resource,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Whether the action may proceed.
    pub allowed: bool,
    /// Human-readable reason.
    pub reason: String,
    /// Rule that decided, if any.
    pub rule: Option<String>,
}

fn action_matches(pattern: &str, action: &str) -> bool {
    pattern == "*"
        || pattern == action
        || pattern.strip_suffix(".*").is_some_and(|app| {
            action
                .strip_prefix(app)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

fn deny(reason: &str, rule: Option<&str>) -> Decision {
    Decision {
        allowed: false,
        reason: reason.into(),
        rule: rule.map(str::to_string),
    }
}

/// Evaluate `req` against `rules`.
#[must_use]
pub fn evaluate(rules: &[Rule], org: &Org, dir: &Directory, req: &Request) -> Decision {
    let tenant = &req.resource.tenant;
    let Some(p) = dir.get(tenant, &req.principal) else {
        return deny("unknown principal in this tenant", None);
    };
    if !p.active {
        return deny("principal is inactive", None);
    }
    if org.tenant_of(&req.resource.node) != Some(tenant.as_str()) {
        return deny("resource node is not in the principal's tenant", None);
    }
    let mut allow: Option<&Rule> = None;
    for r in rules.iter().filter(|r| &r.tenant == tenant) {
        let hit = (r.role == "*" || p.roles.contains(&r.role))
            && action_matches(&r.action, &req.action)
            && (r.resource_kind == "*" || r.resource_kind == req.resource.kind)
            && r.scope
                .as_deref()
                .is_none_or(|s| org.within(&req.resource.node, s))
            && r.conditions.iter().all(|c| match c {
                Cond::PrincipalAttr(k, v) => p.attrs.get(k) == Some(v),
                Cond::ResourceAttr(k, v) => req.resource.attrs.get(k) == Some(v),
                Cond::AmountAtMost(max) => req.resource.amount.is_some_and(|a| a <= *max),
            });
        if !hit {
            continue;
        }
        if r.effect == Effect::Deny {
            return deny(&format!("denied by rule {}", r.id), Some(&r.id));
        }
        allow.get_or_insert(r);
    }
    match allow {
        Some(r) => Decision {
            allowed: true,
            reason: format!("allowed by rule {}", r.id),
            rule: Some(r.id.clone()),
        },
        None => deny("no rule allows this action", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::Level;
    use crate::principal::Kind;

    fn world() -> (Org, Directory) {
        let mut o = Org::new();
        o.create_tenant("acme").unwrap();
        o.add("co", Level::Company, "acme").unwrap();
        o.add("pune", Level::Plant, "co").unwrap();
        o.add("goa", Level::Plant, "co").unwrap();
        o.create_tenant("beta").unwrap();
        o.add("beta-co", Level::Company, "beta").unwrap();
        let mut d = Directory::new();
        d.create("ann", "acme", Kind::Human, &["clerk"], &[("dept", "press")])
            .unwrap();
        d.create("bob", "beta", Kind::Human, &["clerk"], &[])
            .unwrap();
        (o, d)
    }

    fn rule(id: &str, effect: Effect, action: &str) -> Rule {
        Rule {
            id: id.into(),
            tenant: "acme".into(),
            effect,
            role: "clerk".into(),
            action: action.into(),
            resource_kind: "*".into(),
            scope: None,
            conditions: vec![],
        }
    }

    fn req(who: &str, tenant: &str, action: &str, node: &str, amount: Option<u64>) -> Request {
        Request {
            principal: who.into(),
            action: action.into(),
            resource: Resource {
                tenant: tenant.into(),
                node: node.into(),
                kind: "invoice".into(),
                attrs: BTreeMap::new(),
                amount,
            },
        }
    }

    #[test]
    fn default_deny_and_wildcards() {
        let (o, d) = world();
        let rules = [rule("r1", Effect::Allow, "erp.*")];
        assert!(
            evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "erp.read", "pune", None)
            )
            .allowed
        );
        assert!(
            !evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "mail.send", "pune", None)
            )
            .allowed
        );
        assert!(
            !evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "erpx.read", "pune", None)
            )
            .allowed,
            "prefix is not a wildcard"
        );
        assert!(!evaluate(&[], &o, &d, &req("ann", "acme", "erp.read", "pune", None)).allowed);
    }

    #[test]
    fn deny_overrides_and_scope_limits() {
        let (o, d) = world();
        let mut scoped = rule("pune-only", Effect::Allow, "*");
        scoped.scope = Some("pune".into());
        let block = rule("no-delete", Effect::Deny, "erp.delete");
        let rules = [scoped, block, rule("all", Effect::Allow, "erp.delete")];
        assert!(
            evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "erp.read", "pune", None)
            )
            .allowed
        );
        let r = evaluate(
            &rules,
            &o,
            &d,
            &req("ann", "acme", "erp.delete", "pune", None),
        );
        assert!(!r.allowed && r.rule.as_deref() == Some("no-delete"));
        let only = [rules[0].clone()];
        assert!(
            !evaluate(&only, &o, &d, &req("ann", "acme", "erp.read", "goa", None)).allowed,
            "other plant"
        );
    }

    #[test]
    fn conditions_and_amount_limits() {
        let (o, d) = world();
        let mut r = rule("small", Effect::Allow, "erp.pay");
        r.conditions = vec![
            Cond::AmountAtMost(10_000),
            Cond::PrincipalAttr("dept".into(), "press".into()),
        ];
        let rules = [r];
        assert!(
            evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "erp.pay", "pune", Some(10_000))
            )
            .allowed
        );
        assert!(
            !evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "erp.pay", "pune", Some(10_001))
            )
            .allowed
        );
        assert!(
            !evaluate(&rules, &o, &d, &req("ann", "acme", "erp.pay", "pune", None)).allowed,
            "no amount, no allow"
        );
    }

    #[test]
    fn tenant_isolation_and_inactive() {
        let (o, mut d) = world();
        let rules = [rule("r", Effect::Allow, "*")];
        assert!(
            !evaluate(
                &rules,
                &o,
                &d,
                &req("bob", "acme", "erp.read", "pune", None)
            )
            .allowed,
            "bob is beta"
        );
        assert!(
            !evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "acme", "erp.read", "beta-co", None)
            )
            .allowed,
            "node of another tenant"
        );
        assert!(
            !evaluate(
                &rules,
                &o,
                &d,
                &req("ann", "beta", "erp.read", "beta-co", None)
            )
            .allowed,
            "claimed wrong tenant"
        );
        d.deactivate("acme", "ann").unwrap();
        let r = evaluate(
            &rules,
            &o,
            &d,
            &req("ann", "acme", "erp.read", "pune", None),
        );
        assert!(!r.allowed && r.reason.contains("inactive"));
    }
}
