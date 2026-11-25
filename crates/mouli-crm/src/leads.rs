//! Leads: capture, duplicate detection, scoring, assignment, qualification, conversion.

use crate::model::{
    Account, Contact, Lead, LeadStatus, Money, Opportunity, Provenance, Score, Stage,
};
use crate::{normalise, Crm, Ctx, Error, Result};

/// Input for [`Crm::capture_lead`].
#[derive(Debug, Clone, Default)]
pub struct LeadInput {
    /// Person's name.
    pub name: String,
    /// Email address.
    pub email: String,
    /// Phone number, any formatting.
    pub phone: Option<String>,
    /// Company name.
    pub company: Option<String>,
    /// Acquisition source.
    pub source: String,
    /// Marketing consent given at capture: (purpose, granted).
    pub consent: Option<(String, bool)>,
}

const FREE_MAIL: [&str; 5] = [
    "gmail.com",
    "yahoo.com",
    "outlook.com",
    "hotmail.com",
    "icloud.com",
];

fn valid_email(e: &str) -> bool {
    let mut parts = e.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(l), Some(d), None) => {
            !l.is_empty()
                && d.contains('.')
                && !d.starts_with('.')
                && !d.ends_with('.')
                && !e.chars().any(char::is_whitespace)
        }
        _ => false,
    }
}

fn digits(p: &str) -> Option<String> {
    let d: String = p.chars().filter(char::is_ascii_digit).collect();
    (d.len() >= 6).then_some(d)
}

fn score(l: &Lead, consented: bool) -> Score {
    let mut pts = 0u32;
    let mut reasons = Vec::new();
    let mut add = |n: u32, why: &str| {
        pts += n;
        reasons.push(format!("+{n} {why}"));
    };
    let domain = l.email.rsplit('@').next().unwrap_or("");
    if !FREE_MAIL.contains(&domain) {
        add(20, "business email domain");
    }
    if l.phone.is_some() {
        add(15, "phone given");
    }
    if l.company.is_some() {
        add(20, "company given");
    }
    if consented {
        add(10, "marketing consent");
    }
    match l.source.as_str() {
        "referral" => add(30, "referral source"),
        "event" => add(15, "event source"),
        "web" => add(10, "web source"),
        _ => add(5, "other source"),
    }
    Score {
        points: u8::try_from(pts.min(100)).unwrap_or(100),
        reasons,
    }
}

impl Crm {
    /// Capture a lead. Replaying the same `idempotency_key` returns the first result.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.lead.create`; [`Error::Invalid`] for a blank name or
    /// source or a malformed email; [`Error::Conflict`] naming the existing lead when the
    /// email or phone is already on a lead in this tenant.
    pub fn capture_lead(
        &mut self,
        ctx: &Ctx,
        input: LeadInput,
        idempotency_key: &str,
    ) -> Result<String> {
        self.guard(ctx, "crm.lead.create", "lead")?;
        let key = (ctx.tenant.clone(), format!("lead:{idempotency_key}"));
        if !idempotency_key.is_empty() {
            if let Some(id) = self.idem.get(&key) {
                return Ok(id.clone());
            }
        }
        let email = input.email.trim().to_lowercase();
        if input.name.trim().is_empty() || input.source.trim().is_empty() || !valid_email(&email) {
            return Err(Error::Invalid(
                "name, source and a valid email are required".into(),
            ));
        }
        let phone = input.phone.as_deref().and_then(digits);
        if let Some(dup) = self.leads.values().find(|l| {
            l.tenant == ctx.tenant && (l.email == email || (phone.is_some() && l.phone == phone))
        }) {
            let msg = format!("duplicate of {}", dup.id);
            self.note(ctx, "crm.lead.create", "lead", "rejected: duplicate");
            return Err(Error::Conflict(msg));
        }
        let id = self.next_id("lead");
        let consented = input.consent.as_ref().is_some_and(|(_, g)| *g);
        let mut lead = Lead {
            id: id.clone(),
            tenant: ctx.tenant.clone(),
            name: input.name.trim().into(),
            email,
            phone,
            company: input
                .company
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()),
            source: normalise(&input.source),
            status: LeadStatus::New,
            owner: None,
            score: None,
            created: ctx.now,
        };
        lead.score = Some(score(&lead, consented));
        self.leads.insert(id.clone(), lead);
        if let Some((purpose, granted)) = input.consent {
            self.push_consent(ctx, &id, &purpose, granted);
        }
        if !idempotency_key.is_empty() {
            self.idem.insert(key, id.clone());
        }
        self.note(ctx, "crm.lead.create", &id, "ok");
        Ok(id)
    }

    fn lead_mut(&mut self, ctx: &Ctx, id: &str) -> Result<&mut Lead> {
        self.leads
            .get_mut(id)
            .filter(|l| l.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("lead {id}")))
    }

    /// A lead of the caller's tenant.
    ///
    /// # Errors
    /// [`Error::NotFound`] (also for another tenant's lead).
    pub fn lead(&self, ctx: &Ctx, id: &str) -> Result<&Lead> {
        self.leads
            .get(id)
            .filter(|l| l.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("lead {id}")))
    }

    /// Give the lead to the next owner in `owners` (round robin per tenant).
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.lead.assign`; [`Error::Invalid`] for an empty list;
    /// [`Error::State`] once converted.
    pub fn assign_lead(&mut self, ctx: &Ctx, id: &str, owners: &[&str]) -> Result<String> {
        self.guard(ctx, "crm.lead.assign", id)?;
        if owners.is_empty() {
            return Err(Error::Invalid("no owners to assign to".into()));
        }
        if matches!(self.lead(ctx, id)?.status, LeadStatus::Converted { .. }) {
            return Err(Error::State("lead already converted".into()));
        }
        let cur = self.cursor.entry(ctx.tenant.clone()).or_insert(0);
        let owner = owners[*cur % owners.len()].to_string();
        *cur += 1;
        self.lead_mut(ctx, id)?.owner = Some(owner.clone());
        self.note(ctx, "crm.lead.assign", id, &format!("to {owner}"));
        Ok(owner)
    }

    /// Mark a new lead qualified, or disqualified when `reason` is given.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.lead.qualify`; [`Error::State`] unless the lead is new.
    pub fn qualify_lead(&mut self, ctx: &Ctx, id: &str, reason: Option<&str>) -> Result<()> {
        self.guard(ctx, "crm.lead.qualify", id)?;
        let l = self.lead_mut(ctx, id)?;
        if l.status != LeadStatus::New {
            return Err(Error::State("only a new lead can be qualified".into()));
        }
        l.status = match reason.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => LeadStatus::Disqualified(r.into()),
            None => LeadStatus::Qualified,
        };
        let out = if matches!(l.status, LeadStatus::Qualified) {
            "qualified"
        } else {
            "disqualified"
        };
        self.note(ctx, "crm.lead.qualify", id, out);
        Ok(())
    }

    /// Convert a qualified lead into an account (matched by company name, else created), a
    /// contact and an opportunity. Converting again returns the same ids.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.lead.convert`; [`Error::State`] unless qualified.
    pub fn convert_lead(
        &mut self,
        ctx: &Ctx,
        id: &str,
        deal_name: &str,
        amount: Money,
    ) -> Result<(String, String, String)> {
        self.guard_amount(ctx, "crm.lead.convert", id, Some(amount.minor))?;
        let lead = self.lead(ctx, id)?.clone();
        match &lead.status {
            LeadStatus::Converted {
                account,
                contact,
                opportunity,
            } => return Ok((account.clone(), contact.clone(), opportunity.clone())),
            LeadStatus::Qualified => {}
            _ => {
                return Err(Error::State(
                    "only a qualified lead can be converted".into(),
                ))
            }
        }
        if deal_name.trim().is_empty() {
            return Err(Error::Invalid("deal name is required".into()));
        }
        let acct_name = lead.company.clone().unwrap_or_else(|| lead.name.clone());
        let key = normalise(&acct_name);
        let existing = self
            .accounts
            .values()
            .find(|a| a.tenant == ctx.tenant && a.key == key)
            .map(|a| a.id.clone());
        let account = if let Some(a) = existing {
            a
        } else {
            let aid = self.next_id("acct");
            self.accounts.insert(
                aid.clone(),
                Account {
                    id: aid.clone(),
                    tenant: ctx.tenant.clone(),
                    name: acct_name,
                    key,
                    created: ctx.now,
                },
            );
            aid
        };
        let existing = self
            .contacts
            .values()
            .find(|c| c.tenant == ctx.tenant && c.account == account && c.email == lead.email)
            .map(|c| c.id.clone());
        let contact = if let Some(c) = existing {
            c
        } else {
            let cid = self.next_id("cont");
            self.contacts.insert(
                cid.clone(),
                Contact {
                    id: cid.clone(),
                    tenant: ctx.tenant.clone(),
                    account: account.clone(),
                    name: lead.name.clone(),
                    email: lead.email.clone(),
                    phone: lead.phone.clone(),
                },
            );
            cid
        };
        self.copy_consents(ctx, id, &contact);
        let opp = self.next_id("opp");
        self.opps.insert(
            opp.clone(),
            Opportunity {
                id: opp.clone(),
                tenant: ctx.tenant.clone(),
                account: account.clone(),
                name: deal_name.trim().into(),
                amount,
                stage: Stage::Prospecting,
                lost_reason: None,
                created: ctx.now,
                closed: None,
            },
        );
        self.lead_mut(ctx, id)?.status = LeadStatus::Converted {
            account: account.clone(),
            contact: contact.clone(),
            opportunity: opp.clone(),
        };
        self.event(
            ctx,
            Some(&account),
            "lead",
            &format!("lead {id} converted to opportunity {opp}"),
            Provenance::Human,
        );
        self.note(
            ctx,
            "crm.lead.convert",
            id,
            &format!("-> {account},{contact},{opp}"),
        );
        Ok((account, contact, opp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn ctx(t: &str) -> Ctx {
        Ctx::new(t, "ann", &["crm.*"], 100)
    }

    fn input(email: &str) -> LeadInput {
        LeadInput {
            name: "Bo Li".into(),
            email: email.into(),
            phone: Some("+1 (555) 010-2000".into()),
            company: Some("Acme  Corp".into()),
            source: "Referral".into(),
            consent: Some(("marketing".into(), true)),
        }
    }

    #[test]
    fn capture_validates_dedupes_and_is_idempotent() {
        let mut crm = Crm::new(Config::default());
        let c = ctx("t1");
        let id = crm.capture_lead(&c, input("Bo@Acme.com "), "k1").unwrap();
        assert_eq!(
            crm.capture_lead(&c, input("other@x.com"), "k1").unwrap(),
            id
        );
        assert_eq!(crm.lead(&c, &id).unwrap().email, "bo@acme.com");
        assert_eq!(
            crm.lead(&c, &id).unwrap().phone.as_deref(),
            Some("15550102000")
        );
        // same email, and same phone with a different email
        let e = crm
            .capture_lead(&c, input("bo@acme.com"), "k2")
            .unwrap_err();
        assert_eq!(e, Error::Conflict(format!("duplicate of {id}")));
        assert!(matches!(
            crm.capture_lead(&c, input("new@acme.com"), "k3"),
            Err(Error::Conflict(_))
        ));
        for bad in ["", "a@b", "a b@c.com", "@c.com", "a@@c.com"] {
            assert!(
                matches!(crm.capture_lead(&c, input(bad), ""), Err(Error::Invalid(_))),
                "{bad}"
            );
        }
        // another tenant may hold the same person and cannot see ours
        let c2 = ctx("t2");
        let id2 = crm.capture_lead(&c2, input("bo@acme.com"), "").unwrap();
        assert_ne!(id, id2);
        assert!(matches!(crm.lead(&c2, &id), Err(Error::NotFound(_))));
    }

    #[test]
    fn score_is_explained() {
        let mut crm = Crm::new(Config::default());
        let c = ctx("t");
        let id = crm.capture_lead(&c, input("bo@acme.com"), "").unwrap();
        let s = crm.lead(&c, &id).unwrap().score.clone().unwrap();
        assert_eq!(s.points, 20 + 15 + 20 + 10 + 30);
        assert_eq!(s.reasons.len(), 5);
        let mut free = input("x@gmail.com");
        free.phone = None;
        free.company = None;
        free.consent = None;
        free.source = "cold".into();
        let id = crm.capture_lead(&c, free, "").unwrap();
        assert_eq!(crm.lead(&c, &id).unwrap().score.as_ref().unwrap().points, 5);
    }

    #[test]
    fn round_robin_and_permissions() {
        let mut crm = Crm::new(Config::default());
        let c = ctx("t");
        let a = crm.capture_lead(&c, input("a@acme.com"), "").unwrap();
        let mut i2 = input("b@acme.com");
        i2.phone = None;
        let b = crm.capture_lead(&c, i2, "").unwrap();
        assert_eq!(crm.assign_lead(&c, &a, &["x", "y"]).unwrap(), "x");
        assert_eq!(crm.assign_lead(&c, &b, &["x", "y"]).unwrap(), "y");
        assert!(matches!(
            crm.assign_lead(&c, &a, &[]),
            Err(Error::Invalid(_))
        ));
        let weak = Ctx::new("t", "eve", &["crm.lead.create"], 1);
        assert!(matches!(
            crm.assign_lead(&weak, &a, &["x"]),
            Err(Error::Denied(_))
        ));
        assert!(matches!(
            crm.qualify_lead(&weak, &a, None),
            Err(Error::Denied(_))
        ));
    }

    #[test]
    fn conversion_matches_account_and_is_idempotent() {
        let mut crm = Crm::new(Config::default());
        let c = ctx("t");
        let a = crm.capture_lead(&c, input("a@acme.com"), "").unwrap();
        let money = Money::new(10_000, "USD").unwrap();
        assert!(matches!(
            crm.convert_lead(&c, &a, "Deal", money.clone()),
            Err(Error::State(_))
        ));
        crm.qualify_lead(&c, &a, None).unwrap();
        assert!(matches!(
            crm.qualify_lead(&c, &a, None),
            Err(Error::State(_))
        ));
        let first = crm.convert_lead(&c, &a, "Deal", money.clone()).unwrap();
        assert_eq!(
            crm.convert_lead(&c, &a, "Other", money.clone()).unwrap(),
            first
        );
        // a second lead from the same company (spacing and case differ) joins the account
        let mut i2 = input("b@acme.com");
        i2.phone = None;
        i2.company = Some("ACME corp".into());
        let b = crm.capture_lead(&c, i2, "").unwrap();
        crm.qualify_lead(&c, &b, None).unwrap();
        let second = crm.convert_lead(&c, &b, "Deal 2", money).unwrap();
        assert_eq!(first.0, second.0);
        assert_ne!(first.1, second.1);
        // consent travelled to the contact
        assert!(crm.can_market(&c, &first.1, "marketing").unwrap());
        assert!(crm.audit().verify().is_ok());
    }
}
