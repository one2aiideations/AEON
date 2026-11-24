//! Records and value types. Money is exact (integer minor units); every record carries its tenant.

use crate::Error;

/// An amount in minor units (cents) of one currency. Never a float.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Money {
    /// Amount in minor units, not negative.
    pub minor: i64,
    /// ISO 4217 style code, three upper-case letters.
    pub currency: String,
}

impl Money {
    /// Build an amount.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a negative amount or a malformed currency code.
    pub fn new(minor: i64, currency: &str) -> Result<Self, Error> {
        if minor < 0 {
            return Err(Error::Invalid("amount must not be negative".into()));
        }
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(Error::Invalid(format!("bad currency code '{currency}'")));
        }
        Ok(Self {
            minor,
            currency: currency.into(),
        })
    }
}

/// Where a timeline entry or score came from, so suggestions are never mistaken for facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Entered or decided by a person.
    Human,
    /// Computed by a deterministic rule in this crate.
    Rule,
    /// Produced by a model; advisory only, never changes state by itself.
    AiSuggestion,
}

/// Explainable lead score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Score {
    /// 0 to 100.
    pub points: u8,
    /// One line per rule that fired.
    pub reasons: Vec<String>,
}

/// Lifecycle of a lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadStatus {
    /// Captured, not yet judged.
    New,
    /// Judged worth pursuing.
    Qualified,
    /// Not pursued, with the reason.
    Disqualified(String),
    /// Became an account, contact and opportunity.
    Converted {
        /// Account id.
        account: String,
        /// Contact id.
        contact: String,
        /// Opportunity id.
        opportunity: String,
    },
}

/// A prospect before qualification.
#[derive(Debug, Clone)]
pub struct Lead {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Person's name.
    pub name: String,
    /// Normalised (lower-case) email.
    pub email: String,
    /// Phone, digits only.
    pub phone: Option<String>,
    /// Company name as given.
    pub company: Option<String>,
    /// Acquisition source (`web`, `referral`, `event`, ...).
    pub source: String,
    /// Status.
    pub status: LeadStatus,
    /// Assigned owner.
    pub owner: Option<String>,
    /// Latest score.
    pub score: Option<Score>,
    /// Creation time (unix seconds).
    pub created: u64,
}

/// A company or household.
#[derive(Debug, Clone)]
pub struct Account {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Display name.
    pub name: String,
    /// Normalised name used to match accounts.
    pub key: String,
    /// Creation time.
    pub created: u64,
}

/// A person at an account.
#[derive(Debug, Clone)]
pub struct Contact {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Account id.
    pub account: String,
    /// Name.
    pub name: String,
    /// Normalised email.
    pub email: String,
    /// Phone, digits only.
    pub phone: Option<String>,
}

/// Opportunity stages in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// First contact.
    Prospecting,
    /// Need and budget confirmed.
    Qualified,
    /// Proposal sent.
    Proposal,
    /// Terms being agreed.
    Negotiation,
    /// Won (terminal).
    ClosedWon,
    /// Lost (terminal).
    ClosedLost,
}

impl Stage {
    /// Win probability in percent used for the weighted forecast.
    #[must_use]
    pub fn probability(self) -> u8 {
        match self {
            Self::Prospecting => 10,
            Self::Qualified => 25,
            Self::Proposal => 50,
            Self::Negotiation => 75,
            Self::ClosedWon => 100,
            Self::ClosedLost => 0,
        }
    }

    /// True for won and lost.
    #[must_use]
    pub fn is_closed(self) -> bool {
        matches!(self, Self::ClosedWon | Self::ClosedLost)
    }

    /// The next open stage, if any (negotiation leads to won only via closing).
    #[must_use]
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Prospecting => Some(Self::Qualified),
            Self::Qualified => Some(Self::Proposal),
            Self::Proposal => Some(Self::Negotiation),
            Self::Negotiation => Some(Self::ClosedWon),
            Self::ClosedWon | Self::ClosedLost => None,
        }
    }
}

/// A potential sale.
#[derive(Debug, Clone)]
pub struct Opportunity {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Account id.
    pub account: String,
    /// Name.
    pub name: String,
    /// Expected value.
    pub amount: Money,
    /// Stage.
    pub stage: Stage,
    /// Why it was lost.
    pub lost_reason: Option<String>,
    /// Creation time.
    pub created: u64,
    /// Close time.
    pub closed: Option<u64>,
}

/// Case priority, which sets the SLA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Outage: respond in 1 hour, resolve in 4.
    P1,
    /// Major: 4 hours and 1 day.
    P2,
    /// Normal: 8 hours and 3 days.
    P3,
    /// Minor: 1 day and 7 days.
    P4,
}

impl Priority {
    /// (first response, resolution) targets in seconds.
    #[must_use]
    pub fn targets(self) -> (u64, u64) {
        const H: u64 = 3600;
        match self {
            Self::P1 => (H, 4 * H),
            Self::P2 => (4 * H, 24 * H),
            Self::P3 => (8 * H, 72 * H),
            Self::P4 => (24 * H, 168 * H),
        }
    }
}

/// A customer service case.
#[derive(Debug, Clone)]
pub struct Case {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Account id.
    pub account: String,
    /// Subject line.
    pub subject: String,
    /// Priority.
    pub priority: Priority,
    /// Opened at.
    pub opened: u64,
    /// First response at.
    pub first_response: Option<u64>,
    /// Resolved at.
    pub resolved: Option<u64>,
}

/// How a case stands against its SLA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlaStatus {
    /// Within targets.
    Ok,
    /// 80% or more of a target used.
    AtRisk,
    /// A target was missed.
    Breached,
}

/// Renewal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewalStatus {
    /// Not decided.
    Open,
    /// Renewed.
    Renewed,
    /// Customer left.
    Churned,
}

/// A contract renewal.
#[derive(Debug, Clone)]
pub struct Renewal {
    /// Id.
    pub id: String,
    /// Tenant.
    pub tenant: String,
    /// Account id.
    pub account: String,
    /// Due time.
    pub due: u64,
    /// Contract value.
    pub amount: Money,
    /// Status.
    pub status: RenewalStatus,
}

/// Health band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 70 and above.
    Healthy,
    /// 40 to 69.
    Watch,
    /// Below 40.
    AtRisk,
}

/// Account health with the reasons behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// 0 to 100.
    pub score: u8,
    /// Band.
    pub band: Band,
    /// One line per factor.
    pub reasons: Vec<String>,
}

/// One entry in the customer timeline.
#[derive(Debug, Clone)]
pub struct TimelineEvent {
    /// Tenant.
    pub tenant: String,
    /// Account it belongs to, if any.
    pub account: Option<String>,
    /// When.
    pub at: u64,
    /// Kind (`note`, `call`, `stage`, `case`, ...).
    pub kind: String,
    /// What happened.
    pub summary: String,
    /// Who did it.
    pub by: String,
    /// Origin.
    pub provenance: Provenance,
}
