//! Marketing: segments, campaigns and governed sending.
//!
//! The flow is plan, approve, execute. A plan lists who would be contacted and why others are
//! skipped (no consent, withdrawn consent, not eligible, contacted too recently). Sending is
//! consequential, so a second person must approve it. Execution plans again at send time,
//! because consent can change between approval and sending, and it never sends to the same
//! person twice for one campaign, so a retry after a partial failure is safe. An autonomous
//! agent can draft segments, campaigns and plans through these same calls; it cannot send
//! without that approval.

use crate::model::{Lead, LeadStatus, Provenance};
use crate::{Crm, Ctx, Error, Result};

/// Who a segment selects from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudienceKind {
    /// Leads that are new or qualified.
    Leads,
    /// All contacts.
    Contacts,
}

/// A saved audience.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Name.
    pub name: String,
    /// Leads or contacts.
    pub kind: AudienceKind,
    /// Leads only: minimum score.
    pub min_score: u8,
    /// Leads only: required source.
    pub source: Option<String>,
}

/// Campaign lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CampaignStatus {
    /// Being prepared.
    Draft,
    /// Waiting for a second person.
    PendingApproval,
    /// Approved, not yet sent.
    Approved,
    /// Every planned message was delivered.
    Sent,
}

/// A message to a segment.
#[derive(Debug, Clone)]
pub struct Campaign {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Name.
    pub name: String,
    /// Segment id.
    pub segment: String,
    /// Consent purpose the recipients must have granted.
    pub purpose: String,
    /// Message template; `{name}` is replaced by the recipient's name.
    pub template: String,
    /// Do not contact anyone who got any campaign within this many seconds.
    pub min_gap_secs: u64,
    /// Status.
    pub status: CampaignStatus,
    pub(crate) requester: Option<String>,
}

/// Why someone was left out of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// No consent for the purpose, or it was withdrawn.
    NoConsent,
    /// Contacted by a campaign too recently.
    TooSoon,
    /// Already received this campaign.
    AlreadySent,
}

/// One message to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// Lead or contact id.
    pub subject: String,
    /// Address.
    pub to: String,
    /// Rendered message.
    pub body: String,
}

/// What a campaign would do right now.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Messages to send.
    pub deliveries: Vec<Delivery>,
    /// Who was skipped and why.
    pub skipped: Vec<(String, Skip)>,
}

pub(crate) struct SendRecord {
    pub tenant: String,
    pub campaign: String,
    pub subject: String,
    pub at: u64,
}

/// Where messages go. Real email or SMS providers implement this behind an adapter; none is
/// included here.
pub trait Channel {
    /// Deliver one message.
    ///
    /// # Errors
    /// A reason when delivery failed; the CRM will not mark that person as sent.
    fn deliver(&mut self, to: &str, body: &str) -> std::result::Result<(), String>;
}

/// Result of [`Crm::execute_campaign`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executed {
    /// Delivered now.
    pub delivered: usize,
    /// Failed now (retry later).
    pub failed: usize,
    /// People who stopped being eligible since the plan was approved.
    pub dropped: usize,
}

fn render(template: &str, name: &str) -> String {
    template.replace("{name}", name)
}

fn valid_template(t: &str) -> bool {
    let mut rest = t;
    while let Some(i) = rest.find('{') {
        if !rest[i..].starts_with("{name}") {
            return false;
        }
        rest = &rest[i + 6..];
    }
    !rest.contains('}') && !t.trim().is_empty()
}

fn lead_in(l: &Lead, seg: &Segment) -> bool {
    matches!(l.status, LeadStatus::New | LeadStatus::Qualified)
        && l.score.as_ref().is_some_and(|s| s.points >= seg.min_score)
        && seg.source.as_ref().is_none_or(|s| *s == l.source)
}

impl Crm {
    /// Define a segment.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.segment.write`; [`Error::Invalid`] for a blank name.
    pub fn create_segment(
        &mut self,
        ctx: &Ctx,
        name: &str,
        kind: AudienceKind,
        min_score: u8,
        source: Option<&str>,
    ) -> Result<String> {
        self.guard(ctx, "crm.segment.write", name)?;
        if name.trim().is_empty() {
            return Err(Error::Invalid("segment name is required".into()));
        }
        let id = self.next_id("seg");
        self.segments.insert(
            id.clone(),
            Segment {
                id: id.clone(),
                tenant: ctx.tenant.clone(),
                name: name.trim().into(),
                kind,
                min_score,
                source: source.map(crate::normalise),
            },
        );
        self.note(ctx, "crm.segment.write", &id, "ok");
        Ok(id)
    }

    /// Create a draft campaign for a segment.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.campaign.write`; [`Error::NotFound`] for the segment;
    /// [`Error::Invalid`] for a blank name or purpose, or a template that uses anything but
    /// `{name}`.
    pub fn create_campaign(
        &mut self,
        ctx: &Ctx,
        name: &str,
        segment: &str,
        purpose: &str,
        template: &str,
        min_gap_secs: u64,
    ) -> Result<String> {
        self.guard(ctx, "crm.campaign.write", name)?;
        if !self
            .segments
            .get(segment)
            .is_some_and(|s| s.tenant == ctx.tenant)
        {
            return Err(Error::NotFound(format!("segment {segment}")));
        }
        if name.trim().is_empty() || purpose.trim().is_empty() || !valid_template(template) {
            return Err(Error::Invalid(
                "name, purpose and a template using only {name} are required".into(),
            ));
        }
        let id = self.next_id("camp");
        self.campaigns.insert(
            id.clone(),
            Campaign {
                id: id.clone(),
                tenant: ctx.tenant.clone(),
                name: name.trim().into(),
                segment: segment.into(),
                purpose: purpose.trim().to_lowercase(),
                template: template.into(),
                min_gap_secs,
                status: CampaignStatus::Draft,
                requester: None,
            },
        );
        self.note(ctx, "crm.campaign.write", &id, "draft");
        Ok(id)
    }

    /// A campaign of the caller's tenant.
    ///
    /// # Errors
    /// [`Error::NotFound`].
    pub fn campaign(&self, ctx: &Ctx, id: &str) -> Result<&Campaign> {
        self.campaigns
            .get(id)
            .filter(|c| c.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("campaign {id}")))
    }

    fn compute_plan(&self, ctx: &Ctx, c: &Campaign) -> Result<Plan> {
        let seg = self
            .segments
            .get(&c.segment)
            .ok_or_else(|| Error::NotFound(format!("segment {}", c.segment)))?;
        let mut people: Vec<(String, String, String)> = match seg.kind {
            AudienceKind::Leads => self
                .leads
                .values()
                .filter(|l| l.tenant == ctx.tenant && lead_in(l, seg))
                .map(|l| (l.id.clone(), l.email.clone(), l.name.clone()))
                .collect(),
            AudienceKind::Contacts => self
                .contacts
                .values()
                .filter(|p| p.tenant == ctx.tenant)
                .map(|p| (p.id.clone(), p.email.clone(), p.name.clone()))
                .collect(),
        };
        people.sort();
        let mut plan = Plan::default();
        for (id, email, name) in people {
            let mine = |r: &&SendRecord| r.tenant == ctx.tenant && r.subject == id;
            if self.sends.iter().filter(mine).any(|r| r.campaign == c.id) {
                plan.skipped.push((id, Skip::AlreadySent));
            } else if !self.can_market(ctx, &id, &c.purpose)? {
                plan.skipped.push((id, Skip::NoConsent));
            } else if self
                .sends
                .iter()
                .filter(mine)
                .any(|r| ctx.now.saturating_sub(r.at) < c.min_gap_secs)
            {
                plan.skipped.push((id, Skip::TooSoon));
            } else {
                plan.deliveries.push(Delivery {
                    body: render(&c.template, &name),
                    subject: id,
                    to: email,
                });
            }
        }
        Ok(plan)
    }

    /// What the campaign would do right now. Changes nothing.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.campaign.plan`; [`Error::NotFound`].
    pub fn plan_campaign(&mut self, ctx: &Ctx, id: &str) -> Result<Plan> {
        self.guard(ctx, "crm.campaign.plan", id)?;
        let c = self.campaign(ctx, id)?.clone();
        self.compute_plan(ctx, &c)
    }

    /// Ask for approval to send. The plan must contain at least one delivery.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.campaign.send`; [`Error::NotFound`]; [`Error::State`]
    /// unless the campaign is a draft; [`Error::Invalid`] when nobody would receive it.
    pub fn request_send(&mut self, ctx: &Ctx, id: &str) -> Result<()> {
        self.guard(ctx, "crm.campaign.send", id)?;
        let c = self.campaign(ctx, id)?.clone();
        if c.status != CampaignStatus::Draft {
            return Err(Error::State("only a draft can be submitted".into()));
        }
        if self.compute_plan(ctx, &c)?.deliveries.is_empty() {
            return Err(Error::Invalid("nobody is eligible to receive this".into()));
        }
        if let Some(c) = self.campaigns.get_mut(id) {
            c.status = CampaignStatus::PendingApproval;
            c.requester = Some(ctx.user.clone());
        }
        self.note(ctx, "crm.campaign.send", id, "held for approval");
        Ok(())
    }

    /// Approve or send back a submitted campaign. The approver needs
    /// `crm.campaign.approve` and must not be the requester.
    ///
    /// # Errors
    /// [`Error::Denied`]; [`Error::NotFound`]; [`Error::State`] unless pending.
    pub fn approve_campaign(&mut self, ctx: &Ctx, id: &str, approve: bool) -> Result<()> {
        self.guard(ctx, "crm.campaign.approve", id)?;
        let c = self.campaign(ctx, id)?;
        if c.status != CampaignStatus::PendingApproval {
            return Err(Error::State("campaign is not waiting for approval".into()));
        }
        if c.requester.as_deref() == Some(ctx.user.as_str()) {
            self.note(ctx, "crm.campaign.approve", id, "rejected: self approval");
            return Err(Error::Denied(
                "the requester cannot approve their own campaign".into(),
            ));
        }
        if let Some(c) = self.campaigns.get_mut(id) {
            c.status = if approve {
                CampaignStatus::Approved
            } else {
                CampaignStatus::Draft
            };
        }
        self.note(
            ctx,
            "crm.campaign.approve",
            id,
            if approve { "approved" } else { "sent back" },
        );
        Ok(())
    }

    /// Send an approved campaign through `channel`. Plans again first, so people whose consent
    /// was withdrawn since approval are dropped. Safe to call again after failures: people
    /// already delivered are skipped. The campaign becomes [`CampaignStatus::Sent`] only when
    /// nobody is left to send to.
    ///
    /// # Errors
    /// [`Error::Denied`] without `crm.campaign.send`; [`Error::NotFound`]; [`Error::State`]
    /// unless approved.
    pub fn execute_campaign(
        &mut self,
        ctx: &Ctx,
        id: &str,
        channel: &mut dyn Channel,
    ) -> Result<Executed> {
        self.guard(ctx, "crm.campaign.send", id)?;
        let c = self.campaign(ctx, id)?.clone();
        if c.status != CampaignStatus::Approved {
            return Err(Error::State("campaign is not approved".into()));
        }
        let plan = self.compute_plan(ctx, &c)?;
        let dropped = plan
            .skipped
            .iter()
            .filter(|(_, why)| matches!(why, Skip::NoConsent))
            .count();
        let (mut delivered, mut failed) = (0, 0);
        for d in &plan.deliveries {
            if channel.deliver(&d.to, &d.body).is_ok() {
                delivered += 1;
                self.sends.push(SendRecord {
                    tenant: ctx.tenant.clone(),
                    campaign: id.into(),
                    subject: d.subject.clone(),
                    at: ctx.now,
                });
                let acct = self.contacts.get(&d.subject).map(|p| p.account.clone());
                if let Some(a) = acct {
                    self.event(
                        ctx,
                        Some(&a),
                        "campaign",
                        &format!("received {}", c.name),
                        Provenance::Rule,
                    );
                }
            } else {
                failed += 1;
            }
        }
        if failed == 0 {
            if let Some(c) = self.campaigns.get_mut(id) {
                c.status = CampaignStatus::Sent;
            }
        }
        self.note(
            ctx,
            "crm.campaign.send",
            id,
            &format!("delivered {delivered}, failed {failed}, dropped {dropped}"),
        );
        Ok(Executed {
            delivered,
            failed,
            dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::LeadInput;
    use crate::Config;

    struct Log {
        sent: Vec<(String, String)>,
        fail_for: Option<String>,
    }
    impl Channel for Log {
        fn deliver(&mut self, to: &str, body: &str) -> std::result::Result<(), String> {
            if self.fail_for.as_deref() == Some(to) {
                return Err("bounce".into());
            }
            self.sent.push((to.into(), body.into()));
            Ok(())
        }
    }

    fn lead(crm: &mut Crm, c: &Ctx, email: &str, consent: Option<bool>, source: &str) -> String {
        crm.capture_lead(
            c,
            LeadInput {
                name: format!("N-{email}"),
                email: email.into(),
                company: Some("Co".into()),
                source: source.into(),
                consent: consent.map(|g| ("promo".into(), g)),
                ..LeadInput::default()
            },
            "",
        )
        .unwrap()
    }

    fn world() -> (Crm, Ctx, Ctx, String) {
        let mut crm = Crm::new(Config::default());
        let ann = Ctx::new("t", "ann", &["crm.*"], 1000);
        let bob = Ctx::new("t", "bob", &["crm.campaign.approve"], 1000);
        lead(&mut crm, &ann, "yes@x.com", Some(true), "web");
        lead(&mut crm, &ann, "no@x.com", Some(false), "web");
        lead(&mut crm, &ann, "none@x.com", None, "web");
        lead(&mut crm, &ann, "event@x.com", Some(true), "event");
        let seg = crm
            .create_segment(&ann, "web leads", AudienceKind::Leads, 0, Some("web"))
            .unwrap();
        let camp = crm
            .create_campaign(&ann, "Spring", &seg, "Promo", "Hi {name}!", 3600)
            .unwrap();
        (crm, ann, bob, camp)
    }

    #[test]
    fn plan_respects_consent_and_segment() {
        let (mut crm, ann, _, camp) = world();
        let p = crm.plan_campaign(&ann, &camp).unwrap();
        assert_eq!(p.deliveries.len(), 1);
        assert_eq!(p.deliveries[0].to, "yes@x.com");
        assert_eq!(p.deliveries[0].body, "Hi N-yes@x.com!");
        assert_eq!(
            p.skipped
                .iter()
                .filter(|(_, s)| *s == Skip::NoConsent)
                .count(),
            2
        );
        // the event lead is not in the web segment at all
        assert_eq!(p.deliveries.len() + p.skipped.len(), 3);
    }

    #[test]
    fn template_and_inputs_are_validated() {
        let (mut crm, ann, _, _) = world();
        let seg = crm
            .create_segment(&ann, "s", AudienceKind::Contacts, 0, None)
            .unwrap();
        for bad in ["Hi {nam}", "Hi {name", "Hi }", "  "] {
            assert!(
                matches!(
                    crm.create_campaign(&ann, "n", &seg, "p", bad, 0),
                    Err(Error::Invalid(_))
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            crm.create_campaign(&ann, "n", "seg-x", "p", "ok", 0),
            Err(Error::NotFound(_))
        ));
        let other = Ctx::new("t2", "z", &["*"], 1);
        assert!(matches!(
            crm.create_campaign(&other, "n", &seg, "p", "ok", 0),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn send_needs_a_second_person_and_replans_at_send_time() {
        let (mut crm, ann, bob, camp) = world();
        let mut ch = Log {
            sent: vec![],
            fail_for: None,
        };
        // cannot send a draft
        assert!(matches!(
            crm.execute_campaign(&ann, &camp, &mut ch),
            Err(Error::State(_))
        ));
        crm.request_send(&ann, &camp).unwrap();
        assert!(matches!(
            crm.execute_campaign(&ann, &camp, &mut ch),
            Err(Error::State(_))
        ));
        assert!(matches!(
            crm.approve_campaign(&ann, &camp, true),
            Err(Error::Denied(_))
        ));
        // consent withdrawn after submission but before approval/sending
        let yes = crm.plan_campaign(&ann, &camp).unwrap().deliveries[0]
            .subject
            .clone();
        crm.approve_campaign(&bob, &camp, true).unwrap();
        crm.record_consent(&ann, &yes, "promo", false).unwrap();
        let r = crm.execute_campaign(&ann, &camp, &mut ch).unwrap();
        assert_eq!((r.delivered, r.failed, r.dropped), (0, 0, 3));
        assert!(ch.sent.is_empty());
        assert_eq!(
            crm.campaign(&ann, &camp).unwrap().status,
            CampaignStatus::Sent
        );
        assert!(crm.audit().verify().is_ok());
    }

    #[test]
    fn retry_after_failure_never_double_sends_and_gap_applies() {
        let (mut crm, ann, bob, camp) = world();
        let extra = lead(&mut crm, &ann, "two@x.com", Some(true), "web");
        let _ = extra;
        let mut ch = Log {
            sent: vec![],
            fail_for: Some("two@x.com".into()),
        };
        crm.request_send(&ann, &camp).unwrap();
        crm.approve_campaign(&bob, &camp, true).unwrap();
        let r = crm.execute_campaign(&ann, &camp, &mut ch).unwrap();
        assert_eq!((r.delivered, r.failed), (1, 1));
        assert_eq!(
            crm.campaign(&ann, &camp).unwrap().status,
            CampaignStatus::Approved
        );
        ch.fail_for = None;
        let r = crm.execute_campaign(&ann, &camp.clone(), &mut ch).unwrap();
        assert_eq!((r.delivered, r.failed), (1, 0));
        assert_eq!(ch.sent.len(), 2);
        assert_eq!(
            crm.campaign(&ann, &camp).unwrap().status,
            CampaignStatus::Sent
        );
        // a second campaign inside the gap skips people who just received one
        let seg = crm
            .create_segment(&ann, "all web", AudienceKind::Leads, 0, Some("web"))
            .unwrap();
        let c2 = crm
            .create_campaign(&ann, "Next", &seg, "promo", "Again {name}", 3600)
            .unwrap();
        let p = crm.plan_campaign(&ann.at(1000 + 60), &c2).unwrap();
        assert!(p.deliveries.is_empty());
        assert_eq!(
            p.skipped
                .iter()
                .filter(|(_, s)| *s == Skip::TooSoon)
                .count(),
            2
        );
        let p = crm.plan_campaign(&ann.at(1000 + 4000), &c2).unwrap();
        assert_eq!(p.deliveries.len(), 2);
    }

    #[test]
    fn nobody_eligible_cannot_be_submitted_and_tenants_are_isolated() {
        let mut crm = Crm::new(Config::default());
        let ann = Ctx::new("t", "ann", &["crm.*"], 1);
        lead(&mut crm, &ann, "none@x.com", None, "web");
        let seg = crm
            .create_segment(&ann, "s", AudienceKind::Leads, 0, None)
            .unwrap();
        let camp = crm
            .create_campaign(&ann, "C", &seg, "promo", "x", 0)
            .unwrap();
        assert!(matches!(
            crm.request_send(&ann, &camp),
            Err(Error::Invalid(_))
        ));
        let other = Ctx::new("t2", "z", &["*"], 1);
        assert!(matches!(
            crm.plan_campaign(&other, &camp),
            Err(Error::NotFound(_))
        ));
        let weak = Ctx::new("t", "w", &[], 1);
        assert!(matches!(
            crm.plan_campaign(&weak, &camp),
            Err(Error::Denied(_))
        ));
    }
}
