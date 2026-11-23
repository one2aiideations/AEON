//! Maker-checker approvals with amount tiers, segregation of duties and escalation.

use crate::principal::Principal;
use aeon_interface::ids::IdGen;
use aeon_interface::{Error, Result};
use std::collections::BTreeMap;

/// One approval tier. Amounts are integer minor units.
#[derive(Debug, Clone)]
pub struct Tier {
    /// Highest amount this tier covers; `None` means no upper bound (the last tier).
    pub up_to: Option<u64>,
    /// Role whose holders may approve at this tier (higher tiers' roles may too).
    pub role: String,
    /// Distinct approvals needed.
    pub approvals: u8,
}

/// State of a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseState {
    /// Waiting for approvals.
    Open,
    /// Enough approvals.
    Approved,
    /// Rejected by an eligible checker.
    Rejected,
}

/// A change awaiting checking.
#[derive(Debug, Clone)]
pub struct Case {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Who made the change.
    pub maker: String,
    /// Action to be performed once approved.
    pub action: String,
    /// Amount in minor units.
    pub amount: u64,
    /// Current tier index.
    pub tier: usize,
    /// Time of submission or last escalation.
    pub since: u64,
    /// Approvers so far.
    pub approved_by: Vec<String>,
    /// State.
    pub state: CaseState,
}

/// The workflow engine.
pub struct MakerChecker {
    tiers: Vec<Tier>,
    escalate_after: u64,
    cases: BTreeMap<String, Case>,
}

impl MakerChecker {
    /// Create an engine. Tiers must ascend by `up_to`, the last must be unbounded, and every
    /// tier needs at least one approval.
    ///
    /// # Errors
    /// [`Error::Invalid`] when those rules are broken.
    pub fn new(tiers: Vec<Tier>, escalate_after: u64) -> Result<Self> {
        let ascending = tiers.windows(2).all(|w| {
            matches!((w[0].up_to, w[1].up_to), (Some(a), Some(b)) if a < b)
                || matches!((w[0].up_to, w[1].up_to), (Some(_), None))
        });
        if tiers.is_empty()
            || !ascending
            || tiers.last().is_some_and(|t| t.up_to.is_some())
            || tiers.iter().any(|t| t.approvals == 0)
        {
            return Err(Error::Invalid(
                "tiers must ascend, end unbounded and need approvals".into(),
            ));
        }
        Ok(Self {
            tiers,
            escalate_after,
            cases: BTreeMap::new(),
        })
    }

    /// Submit a change for checking.
    pub fn submit(
        &mut self,
        tenant: &str,
        maker: &str,
        action: &str,
        amount: u64,
        now: u64,
        ids: &mut IdGen,
    ) -> String {
        let tier = self
            .tiers
            .iter()
            .position(|t| t.up_to.is_none_or(|m| amount <= m))
            .unwrap_or(self.tiers.len() - 1);
        let id = ids.next("case");
        self.cases.insert(
            id.clone(),
            Case {
                id: id.clone(),
                tenant: tenant.into(),
                maker: maker.into(),
                action: action.into(),
                amount,
                tier,
                since: now,
                approved_by: vec![],
                state: CaseState::Open,
            },
        );
        id
    }

    /// A case, if it belongs to `tenant`.
    #[must_use]
    pub fn get(&self, tenant: &str, id: &str) -> Option<&Case> {
        self.cases.get(id).filter(|c| c.tenant == tenant)
    }

    fn escalate(&mut self, id: &str, now: u64) {
        let last = self.tiers.len() - 1;
        if let Some(c) = self.cases.get_mut(id) {
            if c.state == CaseState::Open
                && c.tier < last
                && now.saturating_sub(c.since) > self.escalate_after
            {
                c.tier += 1;
                c.since = now;
                c.approved_by.clear();
            }
        }
    }

    fn check_eligible(&self, c: &Case, who: &Principal) -> Result<()> {
        if who.tenant != c.tenant || !who.active {
            return Err(Error::Denied(
                "approver is not an active member of this tenant".into(),
            ));
        }
        if who.id == c.maker {
            return Err(Error::Denied(
                "the maker cannot check their own change".into(),
            ));
        }
        if !self.tiers[c.tier..]
            .iter()
            .any(|t| who.roles.contains(&t.role))
        {
            return Err(Error::Denied(format!(
                "approver lacks the {} role or above",
                self.tiers[c.tier].role
            )));
        }
        Ok(())
    }

    /// Record an approval. Returns the state afterwards.
    ///
    /// # Errors
    /// [`Error::NotFound`] (also for another tenant's case), [`Error::State`] when not open,
    /// [`Error::Denied`] for ineligible approvers, [`Error::Conflict`] for a second approval by
    /// the same person.
    pub fn approve(
        &mut self,
        tenant: &str,
        id: &str,
        who: &Principal,
        now: u64,
    ) -> Result<CaseState> {
        self.get(tenant, id)
            .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
        self.escalate(id, now);
        let c = &self.cases[id];
        if c.state != CaseState::Open {
            return Err(Error::State(format!("case {id} is not open")));
        }
        self.check_eligible(c, who)?;
        if c.approved_by.contains(&who.id) {
            return Err(Error::Conflict("this person already approved".into()));
        }
        let need = usize::from(self.tiers[c.tier].approvals);
        let c = self
            .cases
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
        c.approved_by.push(who.id.clone());
        if c.approved_by.len() >= need {
            c.state = CaseState::Approved;
        }
        Ok(c.state)
    }

    /// Reject a case.
    ///
    /// # Errors
    /// As [`MakerChecker::approve`], except duplicates.
    pub fn reject(&mut self, tenant: &str, id: &str, who: &Principal, now: u64) -> Result<()> {
        self.get(tenant, id)
            .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
        self.escalate(id, now);
        let c = &self.cases[id];
        if c.state != CaseState::Open {
            return Err(Error::State(format!("case {id} is not open")));
        }
        self.check_eligible(c, who)?;
        if let Some(c) = self.cases.get_mut(id) {
            c.state = CaseState::Rejected;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::{Directory, Kind};

    fn tiers() -> Vec<Tier> {
        vec![
            Tier {
                up_to: Some(1_000),
                role: "supervisor".into(),
                approvals: 1,
            },
            Tier {
                up_to: Some(100_000),
                role: "manager".into(),
                approvals: 1,
            },
            Tier {
                up_to: None,
                role: "director".into(),
                approvals: 2,
            },
        ]
    }

    fn people() -> Directory {
        let mut d = Directory::new();
        for (id, role) in [
            ("maker", "clerk"),
            ("sup", "supervisor"),
            ("mgr", "manager"),
            ("dir1", "director"),
            ("dir2", "director"),
        ] {
            d.create(id, "acme", Kind::Human, &[role], &[]).unwrap();
        }
        d.create("other", "beta", Kind::Human, &["director"], &[])
            .unwrap();
        d
    }

    #[test]
    fn tier_selection_and_role_hierarchy() {
        let (d, mut ids) = (people(), IdGen::new());
        let mut m = MakerChecker::new(tiers(), 3600).unwrap();
        let small = m.submit("acme", "maker", "erp.pay", 500, 0, &mut ids);
        let mid = m.submit("acme", "maker", "erp.pay", 50_000, 0, &mut ids);
        assert_eq!(m.get("acme", &small).unwrap().tier, 0);
        assert_eq!(m.get("acme", &mid).unwrap().tier, 1);
        assert!(
            matches!(
                m.approve("acme", &mid, d.get("acme", "sup").unwrap(), 1),
                Err(Error::Denied(_))
            ),
            "supervisor below tier"
        );
        assert_eq!(
            m.approve("acme", &small, d.get("acme", "mgr").unwrap(), 1)
                .unwrap(),
            CaseState::Approved,
            "higher role may approve"
        );
        assert!(matches!(
            m.approve("acme", &small, d.get("acme", "mgr").unwrap(), 2),
            Err(Error::State(_))
        ));
    }

    #[test]
    fn segregation_of_duties_and_dual_approval() {
        let (d, mut ids) = (people(), IdGen::new());
        let mut m = MakerChecker::new(tiers(), 3600).unwrap();
        let big = m.submit("acme", "dir1", "erp.pay", 5_000_000, 0, &mut ids);
        assert!(
            matches!(
                m.approve("acme", &big, d.get("acme", "dir1").unwrap(), 1),
                Err(Error::Denied(_))
            ),
            "maker is a director"
        );
        assert_eq!(
            m.approve("acme", &big, d.get("acme", "dir2").unwrap(), 1)
                .unwrap(),
            CaseState::Open
        );
        assert!(matches!(
            m.approve("acme", &big, d.get("acme", "dir2").unwrap(), 2),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            m.approve("acme", &big, d.get("acme", "mgr").unwrap(), 2),
            Err(Error::Denied(_))
        ));
        let c3 = m.submit("acme", "maker", "erp.pay", 5_000_000, 0, &mut ids);
        m.approve("acme", &c3, d.get("acme", "dir1").unwrap(), 1)
            .unwrap();
        assert_eq!(
            m.approve("acme", &c3, d.get("acme", "dir2").unwrap(), 2)
                .unwrap(),
            CaseState::Approved
        );
    }

    #[test]
    fn tenant_isolation_and_inactive_approvers() {
        let (mut d, mut ids) = (people(), IdGen::new());
        let mut m = MakerChecker::new(tiers(), 3600).unwrap();
        let c = m.submit("acme", "maker", "erp.pay", 10, 0, &mut ids);
        assert!(matches!(
            m.approve("beta", &c, d.get("beta", "other").unwrap(), 1),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            m.approve("acme", &c, d.get("beta", "other").unwrap(), 1),
            Err(Error::Denied(_))
        ));
        d.deactivate("acme", "sup").unwrap();
        assert!(matches!(
            m.approve("acme", &c, d.get("acme", "sup").unwrap(), 1),
            Err(Error::Denied(_))
        ));
    }

    #[test]
    fn escalation_moves_to_the_next_tier() {
        let (d, mut ids) = (people(), IdGen::new());
        let mut m = MakerChecker::new(tiers(), 100).unwrap();
        let c = m.submit("acme", "maker", "erp.pay", 500, 0, &mut ids);
        assert_eq!(
            m.approve("acme", &c, d.get("acme", "sup").unwrap(), 50)
                .unwrap(),
            CaseState::Approved,
            "in time"
        );
        let c2 = m.submit("acme", "maker", "erp.pay", 500, 0, &mut ids);
        assert!(
            matches!(
                m.approve("acme", &c2, d.get("acme", "sup").unwrap(), 101),
                Err(Error::Denied(_))
            ),
            "escalated past supervisor"
        );
        assert_eq!(m.get("acme", &c2).unwrap().tier, 1);
        assert_eq!(
            m.approve("acme", &c2, d.get("acme", "mgr").unwrap(), 102)
                .unwrap(),
            CaseState::Approved
        );
    }

    #[test]
    fn rejection_and_config_validation() {
        let (d, mut ids) = (people(), IdGen::new());
        let mut m = MakerChecker::new(tiers(), 3600).unwrap();
        let c = m.submit("acme", "maker", "erp.pay", 10, 0, &mut ids);
        assert!(m
            .reject("acme", &c, d.get("acme", "maker").unwrap(), 1)
            .is_err());
        m.reject("acme", &c, d.get("acme", "sup").unwrap(), 1)
            .unwrap();
        assert_eq!(m.get("acme", &c).unwrap().state, CaseState::Rejected);
        assert!(m
            .approve("acme", &c, d.get("acme", "mgr").unwrap(), 2)
            .is_err());
        assert!(MakerChecker::new(vec![], 1).is_err());
        assert!(
            MakerChecker::new(
                vec![Tier {
                    up_to: Some(5),
                    role: "r".into(),
                    approvals: 1
                }],
                1
            )
            .is_err(),
            "last must be unbounded"
        );
        assert!(MakerChecker::new(
            vec![Tier {
                up_to: None,
                role: "r".into(),
                approvals: 0
            }],
            1
        )
        .is_err());
        let desc = vec![
            Tier {
                up_to: Some(10),
                role: "a".into(),
                approvals: 1,
            },
            Tier {
                up_to: Some(5),
                role: "b".into(),
                approvals: 1,
            },
            Tier {
                up_to: None,
                role: "c".into(),
                approvals: 1,
            },
        ];
        assert!(MakerChecker::new(desc, 1).is_err());
    }
}
