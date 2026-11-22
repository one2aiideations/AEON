//! Typed, versioned kernel objects and their relationships.

use core::fmt;
use std::collections::BTreeMap;

/// Object id.
pub type ObjectId = u64;

/// Kind of object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A person using the machine.
    User,
    /// An autonomous agent.
    Agent,
    /// A model available to engines.
    Model,
    /// A callable tool.
    Capability,
    /// A multi-step plan.
    Workflow,
    /// A stored memory.
    Memory,
    /// A live model of a device, file, or service.
    DigitalTwin,
    /// A file or document.
    File,
}

/// Lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Created, not yet active.
    Pending,
    /// In use.
    Active,
    /// Temporarily disabled.
    Suspended,
    /// Retired; kept for history.
    Archived,
}

/// One object.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelObject {
    /// Id.
    pub id: ObjectId,
    /// Kind.
    pub kind: Kind,
    /// Name, unique per (namespace, kind).
    pub name: String,
    /// Namespace (e.g. a user's home or "system").
    pub namespace: String,
    /// Owning user id (0 = system).
    pub owner: ObjectId,
    /// Free-form labels.
    pub labels: BTreeMap<String, String>,
    /// Lifecycle state.
    pub state: State,
    /// Incremented on every change.
    pub version: u64,
}

/// Store errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjError {
    /// Name empty or already used in this namespace and kind.
    Name,
    /// No such object.
    Unknown(ObjectId),
    /// Caller passed a stale version.
    Conflict {
        /// Version the caller expected.
        expected: u64,
        /// Version stored.
        actual: u64,
    },
}

impl fmt::Display for ObjError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name => f.write_str("name empty or taken"),
            Self::Unknown(id) => write!(f, "unknown object {id}"),
            Self::Conflict { expected, actual } => {
                write!(f, "version conflict: expected {expected}, found {actual}")
            }
        }
    }
}

impl std::error::Error for ObjError {}

/// Directed, labelled relationship between objects ("owns", "uses", "twin-of").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Relation {
    /// Source.
    pub from: ObjectId,
    /// Label.
    pub label: String,
    /// Target.
    pub to: ObjectId,
}

/// In-memory object store with optimistic concurrency.
#[derive(Debug, Clone, Default)]
pub struct ObjectStore {
    objects: BTreeMap<ObjectId, KernelObject>,
    relations: std::collections::BTreeSet<Relation>,
    next: ObjectId,
}

impl ObjectStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: 1,
            ..Self::default()
        }
    }

    /// Create an object in `Pending` state.
    pub fn create(
        &mut self,
        kind: Kind,
        namespace: &str,
        name: &str,
        owner: ObjectId,
    ) -> Result<ObjectId, ObjError> {
        if name.trim().is_empty() || self.find(kind, namespace, name).is_some() {
            return Err(ObjError::Name);
        }
        let id = self.next.max(1);
        self.next = id + 1;
        self.objects.insert(
            id,
            KernelObject {
                id,
                kind,
                name: name.to_owned(),
                namespace: namespace.to_owned(),
                owner,
                labels: BTreeMap::new(),
                state: State::Pending,
                version: 1,
            },
        );
        Ok(id)
    }

    /// Get by id.
    #[must_use]
    pub fn get(&self, id: ObjectId) -> Option<&KernelObject> {
        self.objects.get(&id)
    }

    /// Find by kind, namespace and name.
    #[must_use]
    pub fn find(&self, kind: Kind, namespace: &str, name: &str) -> Option<&KernelObject> {
        self.objects
            .values()
            .find(|o| o.kind == kind && o.namespace == namespace && o.name == name)
    }

    /// Apply `change` if `expected_version` matches; bumps the version.
    pub fn update(
        &mut self,
        id: ObjectId,
        expected_version: u64,
        change: impl FnOnce(&mut KernelObject),
    ) -> Result<u64, ObjError> {
        let o = self.objects.get_mut(&id).ok_or(ObjError::Unknown(id))?;
        if o.version != expected_version {
            return Err(ObjError::Conflict {
                expected: expected_version,
                actual: o.version,
            });
        }
        let (kind, ns, name) = (o.kind, o.namespace.clone(), o.name.clone());
        let mut next = o.clone();
        change(&mut next);
        // Identity fields are immutable.
        next.id = id;
        next.kind = kind;
        next.namespace = ns;
        next.name = name;
        next.version = expected_version + 1;
        *o = next;
        Ok(o.version)
    }

    /// Link two existing objects.
    pub fn relate(&mut self, from: ObjectId, label: &str, to: ObjectId) -> Result<(), ObjError> {
        for id in [from, to] {
            if !self.objects.contains_key(&id) {
                return Err(ObjError::Unknown(id));
            }
        }
        self.relations.insert(Relation {
            from,
            label: label.to_owned(),
            to,
        });
        Ok(())
    }

    /// Targets of `from` via `label`.
    pub fn related<'a>(
        &'a self,
        from: ObjectId,
        label: &'a str,
    ) -> impl Iterator<Item = ObjectId> + 'a {
        self.relations
            .iter()
            .filter(move |r| r.from == from && r.label == label)
            .map(|r| r.to)
    }

    /// Objects matching a label selector (all pairs must match).
    pub fn select<'a>(
        &'a self,
        selector: &'a [(&'a str, &'a str)],
    ) -> impl Iterator<Item = &'a KernelObject> + 'a {
        self.objects.values().filter(move |o| {
            selector
                .iter()
                .all(|(k, v)| o.labels.get(*k).is_some_and(|x| x == v))
        })
    }

    /// Delete an object and its relationships.
    pub fn delete(&mut self, id: ObjectId) -> Result<KernelObject, ObjError> {
        let o = self.objects.remove(&id).ok_or(ObjError::Unknown(id))?;
        self.relations.retain(|r| r.from != id && r.to != id);
        Ok(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_update_conflict() {
        let mut s = ObjectStore::new();
        let a = s
            .create(Kind::Agent, "home/alice", "mail-digest", 7)
            .unwrap();
        assert_eq!(
            s.create(Kind::Agent, "home/alice", "mail-digest", 7),
            Err(ObjError::Name)
        );
        assert!(s
            .create(Kind::Model, "home/alice", "mail-digest", 7)
            .is_ok());
        let v = s
            .update(a, 1, |o| {
                o.state = State::Active;
                o.labels.insert("tier".into(), "fast".into());
                o.name = "renamed".into();
            })
            .unwrap();
        assert_eq!(v, 2);
        let o = s.get(a).unwrap();
        assert_eq!(o.state, State::Active);
        assert_eq!(o.name, "mail-digest");
        assert_eq!(
            s.update(a, 1, |_| {}),
            Err(ObjError::Conflict {
                expected: 1,
                actual: 2
            })
        );
        assert_eq!(s.select(&[("tier", "fast")]).count(), 1);
    }

    #[test]
    fn relations_follow_deletes() {
        let mut s = ObjectStore::new();
        let u = s.create(Kind::User, "system", "alice", 0).unwrap();
        let f = s.create(Kind::File, "home/alice", "report.md", u).unwrap();
        s.relate(u, "owns", f).unwrap();
        assert_eq!(s.related(u, "owns").collect::<Vec<_>>(), vec![f]);
        assert_eq!(s.relate(u, "owns", 99), Err(ObjError::Unknown(99)));
        s.delete(f).unwrap();
        assert_eq!(s.related(u, "owns").count(), 0);
    }
}
