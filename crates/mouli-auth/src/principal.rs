//! People, service accounts and agents.

use mouli_interface::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// What kind of actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A person.
    Human,
    /// A service or integration account.
    Service,
    /// An AI agent.
    Agent,
}

/// An identity inside one tenant.
#[derive(Debug, Clone)]
pub struct Principal {
    /// Globally unique id.
    pub id: String,
    /// Owning tenant.
    pub tenant: String,
    /// Kind of actor.
    pub kind: Kind,
    /// Roles held.
    pub roles: BTreeSet<String>,
    /// Attributes for policy conditions (department, clearance, ...).
    pub attrs: BTreeMap<String, String>,
    /// Inactive principals are always denied.
    pub active: bool,
}

/// All principals.
#[derive(Debug, Default)]
pub struct Directory {
    items: BTreeMap<String, Principal>,
}

impl Directory {
    /// Empty directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an active principal.
    ///
    /// # Errors
    /// [`Error::Invalid`] for blank ids, [`Error::Conflict`] when the id exists.
    pub fn create(
        &mut self,
        id: &str,
        tenant: &str,
        kind: Kind,
        roles: &[&str],
        attrs: &[(&str, &str)],
    ) -> Result<()> {
        if id.trim().is_empty() || tenant.trim().is_empty() {
            return Err(Error::Invalid(
                "principal id and tenant are required".into(),
            ));
        }
        if self.items.contains_key(id) {
            return Err(Error::Conflict(format!("principal {id} already exists")));
        }
        self.items.insert(
            id.into(),
            Principal {
                id: id.into(),
                tenant: tenant.into(),
                kind,
                roles: roles.iter().map(|r| (*r).to_string()).collect(),
                attrs: attrs
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
                active: true,
            },
        );
        Ok(())
    }

    /// Look up a principal inside `tenant`. Another tenant's principal looks missing.
    #[must_use]
    pub fn get(&self, tenant: &str, id: &str) -> Option<&Principal> {
        self.items.get(id).filter(|p| p.tenant == tenant)
    }

    /// Deactivate a principal.
    ///
    /// # Errors
    /// [`Error::NotFound`] when absent from `tenant`.
    pub fn deactivate(&mut self, tenant: &str, id: &str) -> Result<()> {
        let p = self
            .items
            .get_mut(id)
            .filter(|p| p.tenant == tenant)
            .ok_or_else(|| Error::NotFound(format!("principal {id}")))?;
        p.active = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_scoped_lookup_and_deactivation() {
        let mut d = Directory::new();
        d.create("ann", "acme", Kind::Human, &["clerk"], &[("dept", "press")])
            .unwrap();
        assert!(d.create("ann", "beta", Kind::Human, &[], &[]).is_err());
        assert!(d.create("", "acme", Kind::Agent, &[], &[]).is_err());
        assert!(d.get("acme", "ann").unwrap().active);
        assert!(d.get("beta", "ann").is_none());
        assert!(d.deactivate("beta", "ann").is_err());
        d.deactivate("acme", "ann").unwrap();
        assert!(!d.get("acme", "ann").unwrap().active);
    }
}
