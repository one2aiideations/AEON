//! Timeline, activities, AI suggestions, consent and marketing eligibility.

use crate::model::{Provenance, TimelineEvent};
use crate::{ConsentRecord, Crm, Ctx, Error, Result};

impl Crm {
    /// Record a human activity (note, call, meeting) on an account.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.activity.log`; [`Error::NotFound`]; [`Error::Invalid`] for
    /// a blank kind or summary.
    pub fn log_activity(
        &mut self,
        ctx: &Ctx,
        account: &str,
        kind: &str,
        summary: &str,
    ) -> Result<()> {
        self.guard(ctx, "crm.activity.log", account)?;
        self.account_of(ctx, account)?;
        if kind.trim().is_empty() || summary.trim().is_empty() {
            return Err(Error::Invalid("kind and summary are required".into()));
        }
        self.event(
            ctx,
            Some(account),
            kind.trim(),
            summary.trim(),
            Provenance::Human,
        );
        self.note(ctx, "crm.activity.log", account, kind.trim());
        Ok(())
    }

    /// Store a model's suggestion on an account's timeline. It is labelled
    /// [`Provenance::AiSuggestion`] and changes nothing else.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.ai.suggest`; [`Error::NotFound`]; [`Error::Invalid`].
    pub fn record_ai_suggestion(&mut self, ctx: &Ctx, account: &str, text: &str) -> Result<()> {
        self.guard(ctx, "crm.ai.suggest", account)?;
        self.account_of(ctx, account)?;
        if text.trim().is_empty() {
            return Err(Error::Invalid("suggestion text is required".into()));
        }
        self.event(
            ctx,
            Some(account),
            "suggestion",
            text.trim(),
            Provenance::AiSuggestion,
        );
        self.note(ctx, "crm.ai.suggest", account, "stored");
        Ok(())
    }

    /// An account's timeline, oldest first.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.timeline.read`; [`Error::NotFound`].
    pub fn timeline(&mut self, ctx: &Ctx, account: &str) -> Result<Vec<TimelineEvent>> {
        self.guard(ctx, "crm.timeline.read", account)?;
        self.account_of(ctx, account)?;
        let mut v: Vec<TimelineEvent> = self
            .events
            .iter()
            .filter(|e| e.tenant == ctx.tenant && e.account.as_deref() == Some(account))
            .cloned()
            .collect();
        v.sort_by_key(|e| e.at); // stable: insertion order breaks ties
        Ok(v)
    }

    pub(crate) fn push_consent(&mut self, ctx: &Ctx, subject: &str, purpose: &str, granted: bool) {
        self.consents.push(ConsentRecord {
            tenant: ctx.tenant.clone(),
            subject: subject.into(),
            purpose: purpose.trim().to_lowercase(),
            granted,
        });
    }

    /// Carry a lead's latest consent per purpose over to its contact.
    pub(crate) fn copy_consents(&mut self, ctx: &Ctx, from: &str, to: &str) {
        let mut latest: Vec<(String, bool)> = Vec::new();
        for r in self
            .consents
            .iter()
            .filter(|r| r.tenant == ctx.tenant && r.subject == from)
        {
            match latest.iter_mut().find(|(p, _)| *p == r.purpose) {
                Some(e) => e.1 = r.granted,
                None => latest.push((r.purpose.clone(), r.granted)),
            }
        }
        for (p, g) in latest {
            self.push_consent(ctx, to, &p, g);
        }
    }

    fn subject_exists(&self, ctx: &Ctx, id: &str) -> bool {
        self.leads.get(id).is_some_and(|l| l.tenant == ctx.tenant)
            || self
                .contacts
                .get(id)
                .is_some_and(|c| c.tenant == ctx.tenant)
    }

    /// Record that a lead or contact granted or withdrew consent for a purpose.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.consent.write`; [`Error::NotFound`]; [`Error::Invalid`].
    pub fn record_consent(
        &mut self,
        ctx: &Ctx,
        subject: &str,
        purpose: &str,
        granted: bool,
    ) -> Result<()> {
        self.guard(ctx, "crm.consent.write", subject)?;
        if !self.subject_exists(ctx, subject) {
            return Err(Error::NotFound(format!("subject {subject}")));
        }
        if purpose.trim().is_empty() {
            return Err(Error::Invalid("purpose is required".into()));
        }
        self.push_consent(ctx, subject, purpose, granted);
        self.note(
            ctx,
            "crm.consent.write",
            subject,
            &format!(
                "{} {}",
                purpose.trim().to_lowercase(),
                if granted { "granted" } else { "withdrawn" }
            ),
        );
        Ok(())
    }

    /// May this person be contacted for `purpose`? The latest record wins; no record means no.
    ///
    /// # Errors
    /// [`Error::NotFound`] when the subject is not in the caller's tenant.
    pub fn can_market(&self, ctx: &Ctx, subject: &str, purpose: &str) -> Result<bool> {
        if !self.subject_exists(ctx, subject) {
            return Err(Error::NotFound(format!("subject {subject}")));
        }
        let p = purpose.trim().to_lowercase();
        Ok(self
            .consents
            .iter()
            .rev()
            .find(|r| r.tenant == ctx.tenant && r.subject == subject && r.purpose == p)
            .is_some_and(|r| r.granted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::LeadInput;
    use crate::model::Money;
    use crate::Config;

    fn setup() -> (Crm, Ctx, String, String) {
        let mut crm = Crm::new(Config::default());
        let c = Ctx::new("t", "ann", &["*"], 10);
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
        (crm, c, l, a)
    }

    #[test]
    fn timeline_orders_and_labels_provenance() {
        let (mut crm, c, _, a) = setup();
        crm.log_activity(&c.at(30), &a, "call", "intro call")
            .unwrap();
        crm.record_ai_suggestion(&c.at(20), &a, "send case study")
            .unwrap();
        let t = crm.timeline(&c, &a).unwrap();
        let kinds: Vec<_> = t.iter().map(|e| (e.kind.as_str(), e.provenance)).collect();
        assert_eq!(
            kinds,
            [
                ("lead", Provenance::Human),
                ("suggestion", Provenance::AiSuggestion),
                ("call", Provenance::Human)
            ]
        );
        let other = Ctx::new("t2", "x", &["*"], 1);
        assert!(matches!(crm.timeline(&other, &a), Err(Error::NotFound(_))));
    }

    #[test]
    fn consent_latest_wins_default_no_and_tenant_scoped() {
        let (mut crm, c, l, _) = setup();
        assert!(!crm.can_market(&c, &l, "marketing").unwrap());
        crm.record_consent(&c, &l, "Marketing", true).unwrap();
        assert!(crm.can_market(&c, &l, "marketing").unwrap());
        crm.record_consent(&c, &l, "marketing", false).unwrap();
        assert!(!crm.can_market(&c, &l, "marketing").unwrap());
        let other = Ctx::new("t2", "x", &["*"], 1);
        assert!(matches!(
            crm.can_market(&other, &l, "marketing"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            crm.record_consent(&other, &l, "m", true),
            Err(Error::NotFound(_))
        ));
        let weak = Ctx::new("t", "x", &[], 1);
        assert!(matches!(
            crm.log_activity(&weak, "acct-1", "n", "s"),
            Err(Error::Denied(_))
        ));
    }
}
