//! Opportunity pipeline: stage moves, approval for large wins, forecast.

use crate::model::{Opportunity, Provenance, Stage};
use crate::{Approval, ApprovalState, Crm, Ctx, Error, Outcome, Result};
use std::collections::BTreeMap;

/// Forecast for one currency, in minor units.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forecast {
    /// Total value of open opportunities.
    pub open: i128,
    /// Open value weighted by stage probability.
    pub weighted: i128,
    /// Value already won.
    pub won: i128,
}

impl Crm {
    /// An opportunity of the caller's tenant.
    ///
    /// # Errors
    /// [`Error::NotFound`].
    pub fn opportunity(&self, ctx: &Ctx, id: &str) -> Result<&Opportunity> {
        self.opps
            .get(id)
            .filter(|o| o.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("opportunity {id}")))
    }

    fn close(&mut self, ctx: &Ctx, id: &str, stage: Stage, reason: Option<&str>) -> Result<()> {
        let acct = {
            let o = self
                .opps
                .get_mut(id)
                .filter(|o| o.tenant == ctx.tenant)
                .ok_or_else(|| Error::NotFound(format!("opportunity {id}")))?;
            o.stage = stage;
            o.closed = Some(ctx.now);
            o.lost_reason = reason.map(str::to_string);
            o.account.clone()
        };
        self.event(
            ctx,
            Some(&acct),
            "stage",
            &format!("{id} closed {stage:?}"),
            Provenance::Human,
        );
        Ok(())
    }

    /// Move an opportunity one stage forward, or close it as lost (from any open stage, with a
    /// `reason`). Closing as won needs an amount above zero and, at or above the configured
    /// threshold, another person's approval: the call then returns [`Outcome::Pending`].
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.opportunity.advance`; [`Error::NotFound`];
    /// [`Error::State`] for a closed opportunity or a skipped stage; [`Error::Invalid`] for a
    /// lost move without a reason or a won move with a zero amount.
    pub fn advance(
        &mut self,
        ctx: &Ctx,
        id: &str,
        to: Stage,
        reason: Option<&str>,
    ) -> Result<Outcome<Stage>> {
        self.guard(ctx, "crm.opportunity.advance", id)?;
        let o = self.opportunity(ctx, id)?.clone();
        if o.stage.is_closed() {
            return Err(Error::State(format!("{id} is already closed")));
        }
        if to == Stage::ClosedLost {
            let r = reason.map(str::trim).filter(|r| !r.is_empty());
            let Some(r) = r else {
                return Err(Error::Invalid("a lost reason is required".into()));
            };
            self.close(ctx, id, Stage::ClosedLost, Some(r))?;
            self.note(ctx, "crm.opportunity.advance", id, "lost");
            return Ok(Outcome::Done(Stage::ClosedLost));
        }
        if o.stage.next() != Some(to) {
            return Err(Error::State(format!("cannot move {:?} to {to:?}", o.stage)));
        }
        if to == Stage::ClosedWon {
            if o.amount.minor == 0 {
                return Err(Error::Invalid("a won opportunity needs an amount".into()));
            }
            if o.amount.minor >= self.cfg.approval_threshold_minor {
                return Ok(Outcome::Pending {
                    approval: self.request_close(ctx, id),
                });
            }
            self.close(ctx, id, Stage::ClosedWon, None)?;
            self.note(ctx, "crm.opportunity.advance", id, "won");
            return Ok(Outcome::Done(Stage::ClosedWon));
        }
        let acct = o.account.clone();
        if let Some(op) = self.opps.get_mut(id) {
            op.stage = to;
        }
        self.event(
            ctx,
            Some(&acct),
            "stage",
            &format!("{id} moved to {to:?}"),
            Provenance::Human,
        );
        self.note(ctx, "crm.opportunity.advance", id, &format!("{to:?}"));
        Ok(Outcome::Done(to))
    }

    fn request_close(&mut self, ctx: &Ctx, opp: &str) -> String {
        if let Some((id, _)) = self.approvals.iter().find(|(_, a)| {
            a.tenant == ctx.tenant
                && a.opportunity == opp
                && matches!(a.state, ApprovalState::Pending)
        }) {
            return id.clone();
        }
        let id = self.next_id("appr");
        self.approvals.insert(
            id.clone(),
            Approval {
                tenant: ctx.tenant.clone(),
                requester: ctx.user.clone(),
                opportunity: opp.into(),
                state: ApprovalState::Pending,
            },
        );
        self.note(
            ctx,
            "crm.opportunity.advance",
            opp,
            &format!("held for approval {id}"),
        );
        id
    }

    /// Approve or refuse a held close. The approver must hold `crm.approve`, must not be the
    /// requester, and a request can be decided once.
    ///
    /// # Errors
    /// [`Error::Denied`]; [`Error::NotFound`] (also for another tenant's request);
    /// [`Error::Conflict`] when already decided; [`Error::State`] if the opportunity changed.
    pub fn decide(&mut self, ctx: &Ctx, approval: &str, approve: bool) -> Result<Outcome<Stage>> {
        self.guard(ctx, "crm.approve", approval)?;
        let (requester, opp, pending) = {
            let a = self
                .approvals
                .get(approval)
                .filter(|a| a.tenant == ctx.tenant)
                .ok_or_else(|| Error::NotFound(format!("approval {approval}")))?;
            (
                a.requester.clone(),
                a.opportunity.clone(),
                matches!(a.state, ApprovalState::Pending),
            )
        };
        if !pending {
            self.note(ctx, "crm.approve", approval, "rejected: already decided");
            return Err(Error::Conflict("already decided".into()));
        }
        if requester == ctx.user {
            self.note(ctx, "crm.approve", approval, "rejected: self approval");
            return Err(Error::Denied(
                "the requester cannot approve their own request".into(),
            ));
        }
        if let Some(a) = self.approvals.get_mut(approval) {
            a.state = if approve {
                ApprovalState::Approved
            } else {
                ApprovalState::Refused
            };
        }
        if !approve {
            self.note(ctx, "crm.approve", approval, "refused");
            return Ok(Outcome::Done(self.opportunity(ctx, &opp)?.stage));
        }
        if self.opportunity(ctx, &opp)?.stage != Stage::Negotiation {
            return Err(Error::State(
                "opportunity is no longer in negotiation".into(),
            ));
        }
        self.close(ctx, &opp, Stage::ClosedWon, None)?;
        self.note(
            ctx,
            "crm.approve",
            approval,
            &format!("approved; {opp} won"),
        );
        Ok(Outcome::Done(Stage::ClosedWon))
    }

    /// Pipeline forecast per currency.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.forecast.read`.
    pub fn forecast(&mut self, ctx: &Ctx) -> Result<BTreeMap<String, Forecast>> {
        self.guard(ctx, "crm.forecast.read", "forecast")?;
        let mut out: BTreeMap<String, Forecast> = BTreeMap::new();
        for o in self.opps.values().filter(|o| o.tenant == ctx.tenant) {
            let f = out.entry(o.amount.currency.clone()).or_default();
            let amt = i128::from(o.amount.minor);
            match o.stage {
                Stage::ClosedWon => f.won += amt,
                Stage::ClosedLost => {}
                s => {
                    f.open += amt;
                    f.weighted += amt * i128::from(s.probability()) / 100;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::LeadInput;
    use crate::model::Money;
    use crate::Config;

    fn deal(crm: &mut Crm, c: &Ctx, email: &str, minor: i64, cur: &str) -> String {
        let l = crm
            .capture_lead(
                c,
                LeadInput {
                    name: "P".into(),
                    email: email.into(),
                    company: Some(email.into()),
                    source: "web".into(),
                    ..LeadInput::default()
                },
                "",
            )
            .unwrap();
        crm.qualify_lead(c, &l, None).unwrap();
        crm.convert_lead(c, &l, "D", Money::new(minor, cur).unwrap())
            .unwrap()
            .2
    }

    fn walk(crm: &mut Crm, c: &Ctx, o: &str, to: Stage) {
        let mut s = crm.opportunity(c, o).unwrap().stage;
        while s != to {
            s = s.next().unwrap();
            if s == Stage::ClosedWon {
                break;
            }
            crm.advance(c, o, s, None).unwrap();
        }
    }

    #[test]
    fn stages_cannot_be_skipped_and_lost_needs_reason() {
        let mut crm = Crm::new(Config::default());
        let c = Ctx::new("t", "ann", &["*"], 1);
        let o = deal(&mut crm, &c, "a@x.com", 1000, "USD");
        assert!(matches!(
            crm.advance(&c, &o, Stage::Proposal, None),
            Err(Error::State(_))
        ));
        assert_eq!(
            crm.advance(&c, &o, Stage::Qualified, None).unwrap(),
            Outcome::Done(Stage::Qualified)
        );
        assert!(matches!(
            crm.advance(&c, &o, Stage::ClosedLost, Some("  ")),
            Err(Error::Invalid(_))
        ));
        crm.advance(&c, &o, Stage::ClosedLost, Some("no budget"))
            .unwrap();
        assert!(matches!(
            crm.advance(&c, &o, Stage::Proposal, None),
            Err(Error::State(_))
        ));
        assert_eq!(
            crm.opportunity(&c, &o).unwrap().lost_reason.as_deref(),
            Some("no budget")
        );
    }

    #[test]
    fn small_deal_closes_large_needs_a_second_person() {
        let mut crm = Crm::new(Config {
            approval_threshold_minor: 100_000,
        });
        let ann = Ctx::new("t", "ann", &["crm.*"], 1);
        let boss = Ctx::new("t", "bob", &["crm.approve"], 2);
        let small = deal(&mut crm, &ann, "s@x.com", 99_999, "USD");
        walk(&mut crm, &ann, &small, Stage::Negotiation);
        assert_eq!(
            crm.advance(&ann, &small, Stage::ClosedWon, None).unwrap(),
            Outcome::Done(Stage::ClosedWon)
        );

        let big = deal(&mut crm, &ann, "b@x.com", 100_000, "USD");
        walk(&mut crm, &ann, &big, Stage::Negotiation);
        let Outcome::Pending { approval } =
            crm.advance(&ann, &big, Stage::ClosedWon, None).unwrap()
        else {
            panic!("should be held")
        };
        // asking again reuses the request
        assert_eq!(
            crm.advance(&ann, &big, Stage::ClosedWon, None).unwrap(),
            Outcome::Pending {
                approval: approval.clone()
            }
        );
        assert_eq!(
            crm.opportunity(&ann, &big).unwrap().stage,
            Stage::Negotiation
        );
        // requester (even with crm.*) cannot approve; a stranger tenant cannot see it
        assert!(matches!(
            crm.decide(&ann, &approval, true),
            Err(Error::Denied(_))
        ));
        let alien = Ctx::new("t2", "bob", &["crm.approve"], 2);
        assert!(matches!(
            crm.decide(&alien, &approval, true),
            Err(Error::NotFound(_))
        ));
        let nobody = Ctx::new("t", "cy", &["crm.lead.create"], 2);
        assert!(matches!(
            crm.decide(&nobody, &approval, true),
            Err(Error::Denied(_))
        ));
        assert_eq!(
            crm.decide(&boss, &approval, true).unwrap(),
            Outcome::Done(Stage::ClosedWon)
        );
        assert!(matches!(
            crm.decide(&boss, &approval, true),
            Err(Error::Conflict(_))
        ));
        assert!(crm.audit().verify().is_ok());
    }

    #[test]
    fn refusal_leaves_the_deal_open_and_zero_amount_cannot_win() {
        let mut crm = Crm::new(Config {
            approval_threshold_minor: 10,
        });
        let ann = Ctx::new("t", "ann", &["crm.*"], 1);
        let boss = Ctx::new("t", "bob", &["crm.approve"], 2);
        let o = deal(&mut crm, &ann, "a@x.com", 50, "USD");
        walk(&mut crm, &ann, &o, Stage::Negotiation);
        let Outcome::Pending { approval } = crm.advance(&ann, &o, Stage::ClosedWon, None).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            crm.decide(&boss, &approval, false).unwrap(),
            Outcome::Done(Stage::Negotiation)
        );
        let z = deal(&mut crm, &ann, "z@x.com", 0, "USD");
        walk(&mut crm, &ann, &z, Stage::Negotiation);
        assert!(matches!(
            crm.advance(&ann, &z, Stage::ClosedWon, None),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn forecast_is_exact_per_currency_and_tenant_scoped() {
        let mut crm = Crm::new(Config::default());
        let c = Ctx::new("t", "ann", &["*"], 1);
        let a = deal(&mut crm, &c, "a@x.com", 1001, "USD"); // prospecting 10%
        let b = deal(&mut crm, &c, "b@x.com", 2000, "EUR");
        walk(&mut crm, &c, &b, Stage::Proposal); // 50%
        let _ = a;
        let other = Ctx::new("t2", "z", &["*"], 1);
        deal(&mut crm, &other, "o@x.com", 777, "USD");
        let f = crm.forecast(&c).unwrap();
        assert_eq!(
            f["USD"],
            Forecast {
                open: 1001,
                weighted: 100,
                won: 0
            }
        );
        assert_eq!(
            f["EUR"],
            Forecast {
                open: 2000,
                weighted: 1000,
                won: 0
            }
        );
        assert!(Money::new(-1, "USD").is_err() && Money::new(1, "usd").is_err());
    }
}
