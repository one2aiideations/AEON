//! Customer service cases with SLAs, renewals and account health.

use crate::model::{
    Band, Case, Health, Money, Priority, Provenance, Renewal, RenewalStatus, SlaStatus,
};
use crate::{Crm, Ctx, Error, Result};

const DAY: u64 = 86_400;

fn sla_of(c: &Case, now: u64) -> SlaStatus {
    let (first, resolve) = c.priority.targets();
    let mut worst = SlaStatus::Ok;
    let mut check = |used: u64, target: u64| {
        let s = if used > target {
            SlaStatus::Breached
        } else if used * 10 >= target * 8 {
            SlaStatus::AtRisk
        } else {
            SlaStatus::Ok
        };
        if (s == SlaStatus::Breached) || (s == SlaStatus::AtRisk && worst == SlaStatus::Ok) {
            worst = s;
        }
    };
    match c.first_response {
        Some(t) => check(t.saturating_sub(c.opened), first),
        None => check(now.saturating_sub(c.opened), first),
    }
    match c.resolved {
        Some(t) => check(t.saturating_sub(c.opened), resolve),
        None => check(now.saturating_sub(c.opened), resolve),
    }
    worst
}

impl Crm {
    /// Open a case for an account.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.case.open`; [`Error::NotFound`]; [`Error::Invalid`] for a
    /// blank subject.
    pub fn open_case(
        &mut self,
        ctx: &Ctx,
        account: &str,
        subject: &str,
        priority: Priority,
    ) -> Result<String> {
        self.guard(ctx, "crm.case.open", account)?;
        self.account_of(ctx, account)?;
        if subject.trim().is_empty() {
            return Err(Error::Invalid("subject is required".into()));
        }
        let id = self.next_id("case");
        self.cases.insert(
            id.clone(),
            Case {
                id: id.clone(),
                tenant: ctx.tenant.clone(),
                account: account.into(),
                subject: subject.trim().into(),
                priority,
                opened: ctx.now,
                first_response: None,
                resolved: None,
            },
        );
        self.event(
            ctx,
            Some(account),
            "case",
            &format!("{id} opened: {}", subject.trim()),
            Provenance::Human,
        );
        self.note(ctx, "crm.case.open", &id, &format!("{priority:?}"));
        Ok(id)
    }

    fn case_mut(&mut self, ctx: &Ctx, id: &str) -> Result<&mut Case> {
        self.cases
            .get_mut(id)
            .filter(|c| c.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("case {id}")))
    }

    /// Record the first response to a case.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.case.respond`; [`Error::NotFound`]; [`Error::Conflict`]
    /// when already answered.
    pub fn respond_case(&mut self, ctx: &Ctx, id: &str) -> Result<()> {
        self.guard(ctx, "crm.case.respond", id)?;
        let now = ctx.now;
        let c = self.case_mut(ctx, id)?;
        if c.first_response.is_some() {
            return Err(Error::Conflict("already answered".into()));
        }
        c.first_response = Some(now);
        self.note(ctx, "crm.case.respond", id, "ok");
        Ok(())
    }

    /// Resolve a case (an unanswered case counts as answered at that moment).
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.case.resolve`; [`Error::NotFound`]; [`Error::State`] when
    /// already resolved.
    pub fn resolve_case(&mut self, ctx: &Ctx, id: &str) -> Result<()> {
        self.guard(ctx, "crm.case.resolve", id)?;
        let now = ctx.now;
        let c = self.case_mut(ctx, id)?;
        if c.resolved.is_some() {
            return Err(Error::State("already resolved".into()));
        }
        c.resolved = Some(now);
        c.first_response.get_or_insert(now);
        let acct = c.account.clone();
        self.event(
            ctx,
            Some(&acct),
            "case",
            &format!("{id} resolved"),
            Provenance::Human,
        );
        self.note(ctx, "crm.case.resolve", id, "ok");
        Ok(())
    }

    /// A case's SLA status at the caller's current time.
    ///
    /// # Errors
    /// [`Error::NotFound`].
    pub fn sla_status(&self, ctx: &Ctx, id: &str) -> Result<SlaStatus> {
        let c = self
            .cases
            .get(id)
            .filter(|c| c.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("case {id}")))?;
        Ok(sla_of(c, ctx.now))
    }

    /// Ids of cases whose SLA is breached now, oldest first.
    #[must_use]
    pub fn breached_cases(&self, ctx: &Ctx) -> Vec<String> {
        let mut v: Vec<&Case> = self
            .cases
            .values()
            .filter(|c| c.tenant == ctx.tenant && sla_of(c, ctx.now) == SlaStatus::Breached)
            .collect();
        v.sort_by_key(|c| c.opened);
        v.into_iter().map(|c| c.id.clone()).collect()
    }

    /// Add an upcoming renewal.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.renewal.create`; [`Error::NotFound`].
    pub fn add_renewal(
        &mut self,
        ctx: &Ctx,
        account: &str,
        due: u64,
        amount: Money,
    ) -> Result<String> {
        self.guard(ctx, "crm.renewal.create", account)?;
        self.account_of(ctx, account)?;
        let id = self.next_id("ren");
        self.renewals.insert(
            id.clone(),
            Renewal {
                id: id.clone(),
                tenant: ctx.tenant.clone(),
                account: account.into(),
                due,
                amount,
                status: RenewalStatus::Open,
            },
        );
        self.note(ctx, "crm.renewal.create", &id, "ok");
        Ok(id)
    }

    /// Settle a renewal as renewed or churned.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.renewal.close`; [`Error::NotFound`]; [`Error::State`] when
    /// already settled.
    pub fn close_renewal(&mut self, ctx: &Ctx, id: &str, renewed: bool) -> Result<()> {
        self.guard(ctx, "crm.renewal.close", id)?;
        let r = self
            .renewals
            .get_mut(id)
            .filter(|r| r.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("renewal {id}")))?;
        if r.status != RenewalStatus::Open {
            return Err(Error::State("renewal already settled".into()));
        }
        r.status = if renewed {
            RenewalStatus::Renewed
        } else {
            RenewalStatus::Churned
        };
        let acct = r.account.clone();
        self.event(
            ctx,
            Some(&acct),
            "renewal",
            &format!("{id} {}", if renewed { "renewed" } else { "churned" }),
            Provenance::Human,
        );
        self.note(
            ctx,
            "crm.renewal.close",
            id,
            if renewed { "renewed" } else { "churned" },
        );
        Ok(())
    }

    /// Open renewals due within `within` seconds of now, including overdue ones, soonest first.
    #[must_use]
    pub fn renewals_due(&self, ctx: &Ctx, within: u64) -> Vec<&Renewal> {
        let mut v: Vec<&Renewal> = self
            .renewals
            .values()
            .filter(|r| {
                r.tenant == ctx.tenant
                    && r.status == RenewalStatus::Open
                    && r.due <= ctx.now.saturating_add(within)
            })
            .collect();
        v.sort_by_key(|r| r.due);
        v
    }

    /// Account health from open deals, case load and SLA, recent contact and renewals. Every
    /// factor is listed, so the score can be challenged.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.health.read`; [`Error::NotFound`].
    pub fn account_health(&mut self, ctx: &Ctx, account: &str) -> Result<Health> {
        self.guard(ctx, "crm.health.read", account)?;
        self.account_of(ctx, account)?;
        let mut score: i32 = 70;
        let mut reasons = vec!["base 70".to_string()];
        let mut adj = |n: i32, why: String| {
            score += n;
            reasons.push(format!("{n:+} {why}"));
        };
        let open_deals = self
            .opps
            .values()
            .filter(|o| o.tenant == ctx.tenant && o.account == account && !o.stage.is_closed())
            .count();
        if open_deals > 0 {
            adj(
                i32::try_from(open_deals.min(3) * 5).unwrap_or(15),
                format!("{open_deals} open opportunities"),
            );
        }
        let cases: Vec<&Case> = self
            .cases
            .values()
            .filter(|c| c.tenant == ctx.tenant && c.account == account)
            .collect();
        let urgent = cases
            .iter()
            .filter(|c| c.resolved.is_none() && matches!(c.priority, Priority::P1 | Priority::P2))
            .count();
        if urgent > 0 {
            adj(
                -15 * i32::try_from(urgent.min(3)).unwrap_or(3),
                format!("{urgent} open urgent cases"),
            );
        }
        let breached = cases
            .iter()
            .filter(|c| sla_of(c, ctx.now) == SlaStatus::Breached)
            .count();
        if breached > 0 {
            adj(
                -10 * i32::try_from(breached.min(3)).unwrap_or(3),
                format!("{breached} SLA breaches"),
            );
        }
        let last = self
            .events
            .iter()
            .filter(|e| {
                e.tenant == ctx.tenant
                    && e.account.as_deref() == Some(account)
                    && e.provenance == Provenance::Human
            })
            .map(|e| e.at)
            .max();
        match last.map(|t| ctx.now.saturating_sub(t) / DAY) {
            None => adj(-20, "no recorded contact".into()),
            Some(d) if d > 90 => adj(-30, format!("no contact for {d} days")),
            Some(d) if d > 30 => adj(-15, format!("no contact for {d} days")),
            Some(_) => {}
        }
        for r in self.renewals.values().filter(|r| {
            r.tenant == ctx.tenant && r.account == account && r.status == RenewalStatus::Open
        }) {
            if r.due < ctx.now {
                adj(-20, format!("renewal {} overdue", r.id));
            } else if r.due - ctx.now <= 60 * DAY {
                adj(-5, format!("renewal {} due within 60 days", r.id));
            }
        }
        let score = u8::try_from(score.clamp(0, 100)).unwrap_or(0);
        let band = match score {
            70..=100 => Band::Healthy,
            40..=69 => Band::Watch,
            _ => Band::AtRisk,
        };
        Ok(Health {
            score,
            band,
            reasons,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::LeadInput;
    use crate::Config;

    fn setup() -> (Crm, Ctx, String) {
        let mut crm = Crm::new(Config::default());
        let c = Ctx::new("t", "ann", &["*"], 0);
        let l = crm
            .capture_lead(
                &c,
                LeadInput {
                    name: "Bo".into(),
                    email: "bo@acme.com".into(),
                    company: Some("Acme".into()),
                    source: "web".into(),
                    ..LeadInput::default()
                },
                "",
            )
            .unwrap();
        crm.qualify_lead(&c, &l, None).unwrap();
        let (a, _, _) = crm
            .convert_lead(&c, &l, "D", Money::new(1, "USD").unwrap())
            .unwrap();
        (crm, c, a)
    }

    #[test]
    fn sla_progression() {
        let (mut crm, c, a) = setup();
        let id = crm.open_case(&c, &a, "down", Priority::P1).unwrap(); // 1h / 4h
        assert_eq!(crm.sla_status(&c.at(60 * 30), &id).unwrap(), SlaStatus::Ok);
        assert_eq!(crm.sla_status(&c.at(3000), &id).unwrap(), SlaStatus::AtRisk);
        assert_eq!(
            crm.sla_status(&c.at(3601), &id).unwrap(),
            SlaStatus::Breached
        );
        assert_eq!(crm.breached_cases(&c.at(3601)), vec![id.clone()]);
        crm.respond_case(&c.at(600), &id).unwrap();
        assert!(matches!(
            crm.respond_case(&c.at(700), &id),
            Err(Error::Conflict(_))
        ));
        assert_eq!(crm.sla_status(&c.at(1000), &id).unwrap(), SlaStatus::Ok);
        assert_eq!(
            crm.sla_status(&c.at(3 * 3600 + 1000), &id).unwrap(),
            SlaStatus::AtRisk
        );
        crm.resolve_case(&c.at(2 * 3600), &id).unwrap();
        assert!(matches!(
            crm.resolve_case(&c.at(2 * 3600), &id),
            Err(Error::State(_))
        ));
        assert_eq!(
            crm.sla_status(&c.at(10 * 86_400), &id).unwrap(),
            SlaStatus::Ok
        );
        // resolved late stays breached
        let late = crm.open_case(&c, &a, "slow", Priority::P1).unwrap();
        crm.resolve_case(&c.at(5 * 3600), &late).unwrap();
        assert_eq!(crm.sla_status(&c, &late).unwrap(), SlaStatus::Breached);
        let other = Ctx::new("t2", "x", &["*"], 0);
        assert!(matches!(
            crm.sla_status(&other, &id),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            crm.open_case(&c, &a, " ", Priority::P3),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn renewals_sorted_and_settled_once() {
        let (mut crm, c, a) = setup();
        let c = c.at(1000);
        let r2 = crm
            .add_renewal(&c, &a, 5000, Money::new(1, "USD").unwrap())
            .unwrap();
        let r1 = crm
            .add_renewal(&c, &a, 500, Money::new(1, "USD").unwrap())
            .unwrap();
        let r3 = crm
            .add_renewal(&c, &a, 900_000, Money::new(1, "USD").unwrap())
            .unwrap();
        let due: Vec<_> = crm
            .renewals_due(&c, 10_000)
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(due, vec![r1.clone(), r2]);
        crm.close_renewal(&c, &r1, false).unwrap();
        assert!(matches!(
            crm.close_renewal(&c, &r1, true),
            Err(Error::State(_))
        ));
        assert!(!crm.renewals_due(&c, 10_000).iter().any(|r| r.id == r1));
        assert!(!crm.renewals_due(&c, 10_000).iter().any(|r| r.id == r3));
    }

    #[test]
    fn health_is_explained_and_bounded() {
        let (mut crm, c, a) = setup();
        // fresh: converted lead event just now, one open opportunity: 70 + 5
        let h = crm.account_health(&c, &a).unwrap();
        assert_eq!((h.score, h.band), (75, Band::Healthy));
        // 100 days later: no contact (-30)
        let late = c.at(100 * DAY);
        let h = crm.account_health(&late, &a).unwrap();
        assert_eq!(h.score, 70 + 5 - 30);
        assert!(h.reasons.iter().any(|r| r.contains("100 days")));
        // add an urgent unanswered case and overdue renewal
        crm.open_case(&late, &a, "down", Priority::P1).unwrap();
        crm.add_renewal(&late, &a, 1, Money::new(1, "USD").unwrap())
            .unwrap();
        let h = crm.account_health(&late.at(101 * DAY), &a).unwrap();
        assert_eq!(h.band, Band::AtRisk);
        assert!(h.score < 40 && h.reasons.len() >= 5);
        let weak = Ctx::new("t", "x", &[], 0);
        assert!(matches!(
            crm.account_health(&weak, &a),
            Err(Error::Denied(_))
        ));
    }
}
