//! Delegating permission decisions to the enterprise authority (`aeon-auth`).

use aeon_auth::policy::{Request, Resource};
use aeon_auth::Authority;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Decides whether a user may perform a CRM action in a tenant.
pub trait Authorizer {
    /// `Ok` to allow, `Err(reason)` to deny.
    ///
    /// # Errors
    /// The reason for the denial.
    fn check(&mut self, tenant: &str, user: &str, action: &str, now: u64) -> Result<(), String>;
}

/// An [`Authorizer`] backed by an [`Authority`]. The authority is shared, so the code that
/// administers principals and rules keeps a handle while the CRM asks questions.
///
/// Each CRM action (for example `crm.lead.create`) is asked as an `aeon-auth` action on a
/// resource of kind `crm` at the tenant's CRM org node (the tenant root unless set with
/// [`AuthorityGate::node`]). Every decision lands in the authority's audit log.
pub struct AuthorityGate {
    authority: Arc<Mutex<Authority>>,
    nodes: BTreeMap<String, String>,
}

impl AuthorityGate {
    /// Wrap a shared authority.
    #[must_use]
    pub fn new(authority: Arc<Mutex<Authority>>) -> Self {
        Self {
            authority,
            nodes: BTreeMap::new(),
        }
    }

    /// Use `node` (a company or plant in the org tree) as the CRM's scope for `tenant`.
    #[must_use]
    pub fn node(mut self, tenant: &str, node: &str) -> Self {
        self.nodes.insert(tenant.into(), node.into());
        self
    }
}

impl Authorizer for AuthorityGate {
    fn check(&mut self, tenant: &str, user: &str, action: &str, now: u64) -> Result<(), String> {
        let node = self
            .nodes
            .get(tenant)
            .cloned()
            .unwrap_or_else(|| tenant.to_string());
        let req = Request {
            principal: user.into(),
            action: action.into(),
            resource: Resource {
                tenant: tenant.into(),
                node,
                kind: "crm".into(),
                attrs: BTreeMap::new(),
                amount: None,
            },
        };
        // A poisoned lock means another thread panicked mid-decision: fail closed.
        let mut a = self
            .authority
            .lock()
            .map_err(|_| "authority unavailable".to_string())?;
        let d = a.authorize(&req, now).decision;
        if d.allowed {
            Ok(())
        } else {
            Err(d.reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::LeadInput;
    use crate::{Config, Crm, Ctx, Error};
    use aeon_auth::org::Level;
    use aeon_auth::policy::{Effect, Rule};
    use aeon_auth::principal::Kind;

    fn rule(id: &str, effect: Effect, role: &str, action: &str) -> Rule {
        Rule {
            id: id.into(),
            tenant: "acme".into(),
            effect,
            role: role.into(),
            action: action.into(),
            resource_kind: "crm".into(),
            scope: Some("sales".into()),
            conditions: vec![],
        }
    }

    fn setup() -> (Crm, Arc<Mutex<Authority>>) {
        let mut a = Authority::new(&[7u8; 32]).unwrap();
        a.org.create_tenant("acme").unwrap();
        a.org.add("co", Level::Company, "acme").unwrap();
        a.org.add("sales", Level::Branch, "co").unwrap();
        a.directory
            .create("ann", "acme", Kind::Human, &["rep"], &[])
            .unwrap();
        a.directory
            .create("eve", "acme", Kind::Human, &["guest"], &[])
            .unwrap();
        a.add_rule(rule("rep-write", Effect::Allow, "rep", "crm.*"), 1)
            .unwrap();
        a.add_rule(rule("no-approve", Effect::Deny, "rep", "crm.approve"), 1)
            .unwrap();
        let a = Arc::new(Mutex::new(a));
        let mut crm = Crm::new(Config::default());
        crm.set_authorizer(Box::new(
            AuthorityGate::new(a.clone()).node("acme", "sales"),
        ));
        (crm, a)
    }

    fn lead(email: &str) -> LeadInput {
        LeadInput {
            name: "Bo".into(),
            email: email.into(),
            source: "web".into(),
            ..LeadInput::default()
        }
    }

    #[test]
    fn the_authority_decides_not_the_context() {
        let (mut crm, a) = setup();
        // no local permissions at all, still allowed by the authority's rule
        let ann = Ctx::new("acme", "ann", &[], 10);
        assert!(crm.capture_lead(&ann, lead("a@x.com"), "").is_ok());
        // wildcard local permissions do not help a user the authority does not allow
        let eve = Ctx::new("acme", "eve", &["*"], 10);
        assert!(matches!(
            crm.capture_lead(&eve, lead("b@x.com"), ""),
            Err(Error::Denied(_))
        ));
        // an explicit deny rule beats the allow
        assert!(matches!(
            crm.decide(&ann, "appr-1", true),
            Err(Error::Denied(_))
        ));
        // unknown principal and other tenant are refused
        let ghost = Ctx::new("acme", "ghost", &["*"], 10);
        assert!(matches!(
            crm.capture_lead(&ghost, lead("c@x.com"), ""),
            Err(Error::Denied(_))
        ));
        let alien = Ctx::new("globex", "ann", &["*"], 10);
        assert!(matches!(
            crm.capture_lead(&alien, lead("d@x.com"), ""),
            Err(Error::Denied(_))
        ));
        // deactivating the principal takes effect on the next call
        a.lock()
            .unwrap()
            .directory
            .deactivate("acme", "ann")
            .unwrap();
        assert!(matches!(
            crm.capture_lead(&ann, lead("e@x.com"), ""),
            Err(Error::Denied(_))
        ));
        // both audit trails recorded it and are intact
        assert!(a.lock().unwrap().verify_audit().is_ok());
        assert!(a.lock().unwrap().audit().len() > 5);
        assert!(crm.audit().verify().is_ok());
    }
}
