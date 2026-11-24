//! Delegating permission decisions to the enterprise authority (`mouli-auth`).

use mouli_auth::policy::{Request, Resource};
use mouli_auth::Authority;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Decides whether a user may perform a CRM action in a tenant.
pub trait Authorizer {
    /// `Ok` to allow, `Err(reason)` to deny. `amount` is the money involved in minor units,
    /// when the action moves any, so limit rules can be enforced.
    ///
    /// # Errors
    /// The reason for the denial.
    fn check(
        &mut self,
        tenant: &str,
        user: &str,
        action: &str,
        amount: Option<u64>,
        now: u64,
    ) -> Result<(), String>;
}

/// An [`Authorizer`] backed by an [`Authority`]. The authority is shared, so the code that
/// administers principals and rules keeps a handle while the CRM asks questions.
///
/// Each CRM action (for example `crm.lead.create`) is asked as an `mouli-auth` action on a
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
    fn check(
        &mut self,
        tenant: &str,
        user: &str,
        action: &str,
        amount: Option<u64>,
        now: u64,
    ) -> Result<(), String> {
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
                amount,
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
    use crate::model::{Money, Stage};
    use crate::{Config, Crm, Ctx, Error, Outcome};
    use mouli_auth::org::Level;
    use mouli_auth::policy::{Effect, Rule};
    use mouli_auth::principal::Kind;

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
        // A rule without an amount condition matches only calls that carry no amount.
        for (id, action) in [
            ("rep-lead", "crm.lead.create"),
            ("rep-qual", "crm.lead.qualify"),
            ("rep-adv", "crm.opportunity.advance"),
        ] {
            a.add_rule(rule(id, Effect::Allow, "rep", action), 1)
                .unwrap();
        }
        a.add_rule(rule("no-approve", Effect::Deny, "rep", "crm.approve"), 1)
            .unwrap();
        for (id, action, max) in [
            ("rep-conv", "crm.lead.convert", 1_000_000),
            ("rep-close", "crm.opportunity.close", 100_000),
        ] {
            let mut r = rule(id, Effect::Allow, "rep", action);
            r.conditions = vec![mouli_auth::policy::Cond::AmountAtMost(max)];
            a.add_rule(r, 1).unwrap();
        }
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

    fn deal(crm: &mut Crm, c: &Ctx, email: &str, minor: i64) -> Result<String, Error> {
        let l = crm.capture_lead(c, lead(email), "")?;
        crm.qualify_lead(c, &l, None)?;
        Ok(crm
            .convert_lead(c, &l, "D", Money::new(minor, "USD").unwrap())?
            .2)
    }

    #[test]
    fn amount_limits_reach_the_authority() {
        let (mut crm, _a) = setup();
        let ann = Ctx::new("acme", "ann", &[], 10);
        // conversion above the limit is refused, at the limit it passes
        let r = deal(&mut crm, &ann, "big@x.com", 1_000_001);
        assert!(matches!(r, Err(Error::Denied(_))), "{r:?}");
        let small = deal(&mut crm, &ann, "s@x.com", 100_000).unwrap();
        let mid = deal(&mut crm, &ann, "m@x.com", 100_001).unwrap();
        for o in [&small, &mid] {
            for s in [Stage::Qualified, Stage::Proposal, Stage::Negotiation] {
                crm.advance(&ann, o, s, None).unwrap();
            }
        }
        // closing is limited to 100_000 by the rule, whatever the CRM's own threshold
        assert!(matches!(
            crm.advance(&ann, &mid, Stage::ClosedWon, None),
            Err(Error::Denied(_))
        ));
        assert_eq!(
            crm.advance(&ann, &small, Stage::ClosedWon, None).unwrap(),
            Outcome::Done(Stage::ClosedWon)
        );
        assert_eq!(
            crm.opportunity(&ann, &mid).unwrap().stage,
            Stage::Negotiation
        );
    }
}
