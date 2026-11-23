//! AEON-CRM domain core.
//!
//! Built from the AEON-CRM product spec (`AEON-CRM.md`) with design ideas from the owner's
//! AEONCRM prototype (agent writes go through governed tools, every write audited). This is the
//! domain layer only: records, state machines and rules, behind one tenant-scoped [`Crm`]
//! service. What is here:
//!
//! - [`leads`]: capture with source and consent, duplicate detection, explainable scoring,
//!   round-robin assignment, qualification and idempotent conversion into account, contact and
//!   opportunity.
//! - [`pipeline`]: opportunity stages with one-step forward moves, lost reasons, an approval
//!   requirement (different approver, single use) for closing large deals, and a weighted
//!   forecast in exact money.
//! - [`service`]: cases with priority SLAs, renewals and an explainable account health score.
//! - [`timeline`]: the customer timeline, consent records and marketing eligibility, with
//!   provenance so AI suggestions are never mixed up with facts.
//!
//! - [`authorizer`]: optional hand-over of every permission decision to `aeon-auth`, the
//!   enterprise authority, instead of the permission strings in [`Ctx`].
//!
//! Every operation takes a [`Ctx`] (tenant, user, permissions, time), is default-deny, never
//! sees another tenant's records, and appends to a hash-chained audit log (denials included).
//!
//! Deviations and gaps, stated plainly: the spec asks for one executable with one source file;
//! this is a library of modules like the other AEON crates. Storage is in memory (the spec
//! requires AEON-DATA, not wired in), permissions are plain strings unless an `aeon-auth` gate is
//! installed (see [`authorizer`]; amount and attribute conditions are not passed), and campaigns and journeys, quotes and proposals, the knowledge base, deal-risk
//! models, import/export and erasure are not implemented. Nothing here has run against real
//! data.
#![forbid(unsafe_code)]

pub mod authorizer;
pub mod leads;
pub mod model;
pub mod pipeline;
pub mod service;
pub mod timeline;

use aeon_interface::audit::AuditLog;
use authorizer::Authorizer;
use model::{Account, Case, Contact, Lead, Opportunity, Provenance, Renewal, TimelineEvent};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Errors from CRM operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Input failed validation.
    Invalid(String),
    /// The caller lacks permission.
    Denied(String),
    /// No such record (also for another tenant's record).
    NotFound(String),
    /// Duplicate or already decided.
    Conflict(String),
    /// The operation is not legal in the record's current state.
    State(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (k, m) = match self {
            Self::Invalid(m) => ("invalid", m),
            Self::Denied(m) => ("denied", m),
            Self::NotFound(m) => ("not found", m),
            Self::Conflict(m) => ("conflict", m),
            Self::State(m) => ("state", m),
        };
        write!(f, "{k}: {m}")
    }
}

impl std::error::Error for Error {}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Result of an operation that may need a second person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// Done.
    Done(T),
    /// Held until someone else approves; pass the id to [`Crm::decide`].
    Pending {
        /// Approval id.
        approval: String,
    },
}

/// Who is acting, for what tenant, with which permissions, at what time.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Tenant the call runs in.
    pub tenant: String,
    /// Acting user.
    pub user: String,
    /// Granted permissions: exact (`crm.lead.create`), prefix wildcard (`crm.*`) or `*`.
    pub perms: BTreeSet<String>,
    /// Current time, unix seconds (supplied by the caller so behaviour is reproducible).
    pub now: u64,
}

impl Ctx {
    /// Build a context.
    #[must_use]
    pub fn new(tenant: &str, user: &str, perms: &[&str], now: u64) -> Self {
        Self {
            tenant: tenant.into(),
            user: user.into(),
            perms: perms.iter().map(|p| (*p).to_string()).collect(),
            now,
        }
    }

    /// The same actor at another time.
    #[must_use]
    pub fn at(&self, now: u64) -> Self {
        Self {
            now,
            ..self.clone()
        }
    }

    fn allows(&self, need: &str) -> bool {
        self.perms.iter().any(|p| {
            p == "*"
                || p == need
                || p.strip_suffix(".*")
                    .is_some_and(|pre| need.strip_prefix(pre).is_some_and(|r| r.starts_with('.')))
        })
    }
}

/// Tunable rules.
#[derive(Debug, Clone)]
pub struct Config {
    /// Closing an opportunity at or above this many minor units needs another person's approval.
    pub approval_threshold_minor: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            approval_threshold_minor: 5_000_000,
        }
    }
}

pub(crate) enum ApprovalState {
    Pending,
    Approved,
    Refused,
}

pub(crate) struct Approval {
    pub tenant: String,
    pub requester: String,
    pub opportunity: String,
    pub state: ApprovalState,
}

pub(crate) struct ConsentRecord {
    pub tenant: String,
    pub subject: String,
    pub purpose: String,
    pub granted: bool,
}

/// The CRM service. Holds all records; every access is scoped by [`Ctx::tenant`].
pub struct Crm {
    pub(crate) cfg: Config,
    seq: u64,
    pub(crate) leads: BTreeMap<String, Lead>,
    pub(crate) accounts: BTreeMap<String, Account>,
    pub(crate) contacts: BTreeMap<String, Contact>,
    pub(crate) opps: BTreeMap<String, Opportunity>,
    pub(crate) cases: BTreeMap<String, Case>,
    pub(crate) renewals: BTreeMap<String, Renewal>,
    pub(crate) events: Vec<TimelineEvent>,
    pub(crate) approvals: BTreeMap<String, Approval>,
    pub(crate) consents: Vec<ConsentRecord>,
    pub(crate) idem: BTreeMap<(String, String), String>,
    pub(crate) cursor: BTreeMap<String, usize>,
    audit: AuditLog,
    gate: Option<Box<dyn Authorizer>>,
}

impl Crm {
    /// An empty CRM.
    #[must_use]
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            seq: 0,
            leads: BTreeMap::new(),
            accounts: BTreeMap::new(),
            contacts: BTreeMap::new(),
            opps: BTreeMap::new(),
            cases: BTreeMap::new(),
            renewals: BTreeMap::new(),
            events: Vec::new(),
            approvals: BTreeMap::new(),
            consents: Vec::new(),
            idem: BTreeMap::new(),
            cursor: BTreeMap::new(),
            audit: AuditLog::new(),
            gate: None,
        }
    }

    /// Hand every permission decision to `gate` from now on. The permissions in [`Ctx`] are
    /// then ignored, so there is exactly one authority.
    pub fn set_authorizer(&mut self, gate: Box<dyn Authorizer>) {
        self.gate = Some(gate);
    }

    /// The audit log (hash-chained; call `verify`).
    #[must_use]
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    pub(crate) fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }

    pub(crate) fn note(&mut self, ctx: &Ctx, action: &str, subject: &str, outcome: &str) {
        self.audit
            .append(ctx.now, &ctx.user, action, subject, outcome);
    }

    /// Check a permission; a denial is audited.
    pub(crate) fn guard(&mut self, ctx: &Ctx, perm: &str, subject: &str) -> Result<()> {
        if ctx.tenant.trim().is_empty() || ctx.user.trim().is_empty() {
            return Err(Error::Invalid("tenant and user are required".into()));
        }
        let verdict = match self.gate.as_mut() {
            Some(g) => g.check(&ctx.tenant, &ctx.user, perm, ctx.now),
            None if ctx.allows(perm) => Ok(()),
            None => Err(format!("missing permission {perm}")),
        };
        match verdict {
            Ok(()) => Ok(()),
            Err(why) => {
                self.note(ctx, perm, subject, "denied");
                Err(Error::Denied(why))
            }
        }
    }

    pub(crate) fn event(
        &mut self,
        ctx: &Ctx,
        account: Option<&str>,
        kind: &str,
        summary: &str,
        provenance: Provenance,
    ) {
        self.events.push(TimelineEvent {
            tenant: ctx.tenant.clone(),
            account: account.map(str::to_string),
            at: ctx.now,
            kind: kind.into(),
            summary: summary.into(),
            by: ctx.user.clone(),
            provenance,
        });
    }

    pub(crate) fn account_of(&self, ctx: &Ctx, id: &str) -> Result<&Account> {
        self.accounts
            .get(id)
            .filter(|a| a.tenant == ctx.tenant)
            .ok_or_else(|| Error::NotFound(format!("account {id}")))
    }
}

/// Lower-case, trimmed, single-spaced text for matching.
pub(crate) fn normalise(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_matching() {
        let c = Ctx::new("t", "u", &["crm.lead.*", "crm.case.open"], 0);
        assert!(c.allows("crm.lead.create") && c.allows("crm.case.open"));
        assert!(
            !c.allows("crm.lead") && !c.allows("crm.leadx.create") && !c.allows("crm.case.close")
        );
        assert!(Ctx::new("t", "u", &["*"], 0).allows("anything"));
    }

    #[test]
    fn denial_is_audited_and_blank_actor_rejected() {
        let mut crm = Crm::new(Config::default());
        let c = Ctx::new("t", "u", &[], 5);
        assert!(matches!(crm.guard(&c, "crm.x", "s"), Err(Error::Denied(_))));
        assert_eq!(crm.audit().records().len(), 1);
        let blank = Ctx::new("", "u", &["*"], 5);
        assert!(matches!(
            crm.guard(&blank, "crm.x", "s"),
            Err(Error::Invalid(_))
        ));
    }
}
