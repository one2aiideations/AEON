//! Tenant and organisation hierarchy: tenant > institution > company > legal entity > branch >
//! plant > department. Every node belongs to exactly one tenant.

use aeon_interface::{Error, Result};
use std::collections::BTreeMap;

/// Depth of a node. A child must be strictly deeper than its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Tenant root.
    Tenant,
    /// Institution or group.
    Institution,
    /// Company.
    Company,
    /// Legal entity.
    LegalEntity,
    /// Branch.
    Branch,
    /// Plant.
    Plant,
    /// Department.
    Department,
}

#[derive(Debug, Clone)]
struct Node {
    tenant: String,
    level: Level,
    parent: Option<String>,
}

/// The hierarchy of all tenants.
#[derive(Debug, Default)]
pub struct Org {
    nodes: BTreeMap<String, Node>,
}

impl Org {
    /// Empty hierarchy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a tenant. The root node has the tenant's own id.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a blank id, [`Error::Conflict`] when it exists.
    pub fn create_tenant(&mut self, id: &str) -> Result<()> {
        self.insert(id, id, Level::Tenant, None)
    }

    /// Add a node under `parent`.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown parent, [`Error::Invalid`] when the level is not
    /// deeper than the parent's, [`Error::Conflict`] when the id exists anywhere.
    pub fn add(&mut self, id: &str, level: Level, parent: &str) -> Result<()> {
        let p = self
            .nodes
            .get(parent)
            .ok_or_else(|| Error::NotFound(format!("node {parent}")))?;
        if level <= p.level {
            return Err(Error::Invalid(format!(
                "{level:?} cannot sit under {:?}",
                p.level
            )));
        }
        let tenant = p.tenant.clone();
        self.insert(id, &tenant, level, Some(parent.to_string()))
    }

    fn insert(
        &mut self,
        id: &str,
        tenant: &str,
        level: Level,
        parent: Option<String>,
    ) -> Result<()> {
        if id.trim().is_empty() {
            return Err(Error::Invalid("node id must not be blank".into()));
        }
        if self.nodes.contains_key(id) {
            return Err(Error::Conflict(format!("node {id} already exists")));
        }
        self.nodes.insert(
            id.into(),
            Node {
                tenant: tenant.into(),
                level,
                parent,
            },
        );
        Ok(())
    }

    /// Tenant that owns a node.
    #[must_use]
    pub fn tenant_of(&self, node: &str) -> Option<&str> {
        self.nodes.get(node).map(|n| n.tenant.as_str())
    }

    /// Level of a node.
    #[must_use]
    pub fn level_of(&self, node: &str) -> Option<Level> {
        self.nodes.get(node).map(|n| n.level)
    }

    /// Whether `node` is `ancestor` or below it.
    #[must_use]
    pub fn within(&self, node: &str, ancestor: &str) -> bool {
        let mut cur = Some(node);
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            cur = self.nodes.get(id).and_then(|n| n.parent.as_deref());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> Org {
        let mut o = Org::new();
        o.create_tenant("acme").unwrap();
        o.add("acme-co", Level::Company, "acme").unwrap();
        o.add("pune", Level::Plant, "acme-co").unwrap();
        o.add("press", Level::Department, "pune").unwrap();
        o.create_tenant("beta").unwrap();
        o.add("beta-co", Level::Company, "beta").unwrap();
        o
    }

    #[test]
    fn hierarchy_rules() {
        let mut o = org();
        assert_eq!(o.tenant_of("press"), Some("acme"));
        assert!(
            o.within("press", "acme-co") && o.within("press", "press") && o.within("pune", "acme")
        );
        assert!(!o.within("acme-co", "pune") && !o.within("press", "beta-co"));
        assert!(
            matches!(o.add("x", Level::Company, "pune"), Err(Error::Invalid(_))),
            "company under plant"
        );
        assert!(matches!(
            o.add("x", Level::Plant, "nope"),
            Err(Error::NotFound(_))
        ));
        assert!(
            matches!(
                o.add("pune", Level::Branch, "acme-co"),
                Err(Error::Conflict(_))
            ),
            "ids are global"
        );
        assert!(o.create_tenant(" ").is_err());
        assert_eq!(o.level_of("pune"), Some(Level::Plant));
        assert_eq!(o.tenant_of("ghost"), None);
    }
}
