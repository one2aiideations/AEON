//! Human approval for consequential calls. A request is decided once, by someone other than
//! the requester, and an approval can be spent only once and only on the action it names.

use crate::error::{Error, Result};
use crate::ids::IdGen;
use std::collections::BTreeMap;

/// State of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a decision.
    Pending,
    /// Approved by this person.
    Approved(String),
    /// Denied by this person.
    Denied(String),
    /// Approved and already used.
    Spent,
}

/// A held action.
#[derive(Debug, Clone)]
pub struct Request {
    /// Identifier.
    pub id: String,
    /// Capability to run.
    pub action: String,
    /// Who asked.
    pub requester: String,
    /// What will happen, for the approver.
    pub detail: String,
    /// Logical creation time.
    pub tick: u64,
    status: Status,
}

impl Request {
    /// Current status.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }
}

/// Queue of requests.
#[derive(Debug, Default)]
pub struct ApprovalQueue {
    items: BTreeMap<String, Request>,
}

impl ApprovalQueue {
    /// Empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a request and return its id.
    pub fn request(
        &mut self,
        action: &str,
        requester: &str,
        detail: &str,
        tick: u64,
        ids: &mut IdGen,
    ) -> String {
        let id = ids.next("apr");
        self.items.insert(
            id.clone(),
            Request {
                id: id.clone(),
                action: action.into(),
                requester: requester.into(),
                detail: detail.into(),
                tick,
                status: Status::Pending,
            },
        );
        id
    }

    /// Record a decision.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown id, [`Error::Denied`] when the approver is the
    /// requester, [`Error::Conflict`] when it was already decided.
    pub fn decide(&mut self, id: &str, approver: &str, approve: bool) -> Result<()> {
        let r = self
            .items
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("approval {id}")))?;
        if r.requester == approver {
            return Err(Error::Denied(
                "a request cannot be decided by its requester".into(),
            ));
        }
        if r.status != Status::Pending {
            return Err(Error::Conflict(format!("approval {id} already decided")));
        }
        r.status = if approve {
            Status::Approved(approver.into())
        } else {
            Status::Denied(approver.into())
        };
        Ok(())
    }

    /// Spend an approval for `action`.
    ///
    /// # Errors
    /// [`Error::NotFound`], [`Error::Invalid`] when the action differs, [`Error::State`] when
    /// not approved, [`Error::Conflict`] when already spent.
    pub fn spend(&mut self, id: &str, action: &str) -> Result<()> {
        let r = self
            .items
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("approval {id}")))?;
        if r.action != action {
            return Err(Error::Invalid(format!(
                "approval {id} is for {}, not {action}",
                r.action
            )));
        }
        match r.status {
            Status::Approved(_) => {
                r.status = Status::Spent;
                Ok(())
            }
            Status::Spent => Err(Error::Conflict(format!("approval {id} already used"))),
            _ => Err(Error::State(format!("approval {id} is not approved"))),
        }
    }

    /// Look a request up.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Request> {
        self.items.get(id)
    }

    /// Requests still waiting, oldest first.
    #[must_use]
    pub fn pending(&self) -> Vec<&Request> {
        self.items
            .values()
            .filter(|r| r.status == Status::Pending)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_eyes_and_single_use() {
        let (mut q, mut ids) = (ApprovalQueue::new(), IdGen::new());
        let a = q.request("mail.send", "ann", "send to bob", 1, &mut ids);
        let b = q.request("mail.send", "ann", "send to cy", 2, &mut ids);
        assert_ne!(a, b);
        assert_eq!(q.pending().len(), 2);
        assert!(matches!(q.decide(&a, "ann", true), Err(Error::Denied(_))));
        assert!(matches!(q.spend(&a, "mail.send"), Err(Error::State(_))));
        q.decide(&a, "dee", true).unwrap();
        assert!(matches!(
            q.decide(&a, "eve", false),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(q.spend(&a, "mail.delete"), Err(Error::Invalid(_))));
        q.spend(&a, "mail.send").unwrap();
        assert!(matches!(q.spend(&a, "mail.send"), Err(Error::Conflict(_))));
        assert_eq!(q.pending().len(), 1);
        q.decide(&b, "dee", false).unwrap();
        assert!(matches!(q.spend(&b, "mail.send"), Err(Error::State(_))));
        assert!(q.get("missing").is_none());
    }
}
