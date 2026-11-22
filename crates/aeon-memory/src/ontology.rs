//! A class hierarchy with typed relations.
//!
//! Gives the knowledge graph a vocabulary: classes form an is-a tree, and relations declared
//! on a class are inherited by its subclasses. That supports questions like "is a `Sparrow` a
//! `Bird`?", "what is the nearest class two things share?" and "what can a `Bird` do?" without
//! listing every fact on every subclass.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

/// What kind of thing a class describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// An abstract idea.
    Concept,
    /// A kind of thing that exists.
    Entity,
    /// A property.
    Attribute,
    /// Something that can be done.
    Action,
    /// Something that happens.
    Event,
    /// A relationship type.
    Relation,
}

/// One class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    /// Unique name.
    pub name: String,
    /// Kind.
    pub kind: ClassKind,
    /// Free text.
    pub description: String,
    /// Direct superclass, if any.
    pub parent: Option<String>,
}

/// A relation between two classes.
#[derive(Debug, Clone, PartialEq)]
pub struct Relation {
    /// Source class.
    pub source: String,
    /// Relation name ("can", "has", "part-of", ...).
    pub name: String,
    /// Target class.
    pub target: String,
    /// Strength.
    pub weight: f32,
}

/// Ontology failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OntologyError {
    /// A class with this name exists.
    Duplicate(String),
    /// A named class does not exist.
    Unknown(String),
    /// A weight is NaN or infinite.
    BadWeight,
    /// The class name or relation name is empty.
    EmptyName,
}

impl fmt::Display for OntologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OntologyError::Duplicate(n) => write!(f, "class {n:?} already exists"),
            OntologyError::Unknown(n) => write!(f, "unknown class {n:?}"),
            OntologyError::BadWeight => f.write_str("weight must be finite"),
            OntologyError::EmptyName => f.write_str("names must not be empty"),
        }
    }
}

impl std::error::Error for OntologyError {}

/// The ontology.
#[derive(Debug, Clone, Default)]
pub struct Ontology {
    classes: BTreeMap<String, Class>,
    children: BTreeMap<String, BTreeSet<String>>,
    relations: Vec<Relation>,
}

impl Ontology {
    /// Empty ontology.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a class. A parent must already exist, so the hierarchy can never contain a cycle.
    pub fn add_class(
        &mut self,
        name: &str,
        kind: ClassKind,
        description: &str,
        parent: Option<&str>,
    ) -> Result<(), OntologyError> {
        if name.is_empty() {
            return Err(OntologyError::EmptyName);
        }
        if self.classes.contains_key(name) {
            return Err(OntologyError::Duplicate(name.to_string()));
        }
        if let Some(p) = parent {
            if !self.classes.contains_key(p) {
                return Err(OntologyError::Unknown(p.to_string()));
            }
            self.children
                .entry(p.to_string())
                .or_default()
                .insert(name.to_string());
        }
        self.classes.insert(
            name.to_string(),
            Class {
                name: name.to_string(),
                kind,
                description: description.to_string(),
                parent: parent.map(str::to_string),
            },
        );
        Ok(())
    }

    /// Look a class up.
    pub fn class(&self, name: &str) -> Option<&Class> {
        self.classes.get(name)
    }

    /// Number of classes.
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// `true` if there are no classes.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Superclasses of `name`, nearest first. Empty for roots and unknown names.
    pub fn ancestors(&self, name: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut cur = self.classes.get(name).and_then(|c| c.parent.as_deref());
        while let Some(p) = cur {
            out.push(p);
            cur = self.classes.get(p).and_then(|c| c.parent.as_deref());
        }
        out
    }

    /// All subclasses of `name` (children, grandchildren, ...), breadth first, siblings by name.
    pub fn descendants(&self, name: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(name);
        while let Some(n) = queue.pop_front() {
            if let Some(kids) = self.children.get(n) {
                for k in kids {
                    out.push(k.as_str());
                    queue.push_back(k.as_str());
                }
            }
        }
        out
    }

    /// `true` if `child` is `ancestor` or a (transitive) subclass of it. Unknown names give `false`.
    pub fn is_a(&self, child: &str, ancestor: &str) -> bool {
        self.classes.contains_key(child)
            && self.classes.contains_key(ancestor)
            && (child == ancestor || self.ancestors(child).contains(&ancestor))
    }

    /// Distance from the root: roots are 0.
    pub fn depth(&self, name: &str) -> Option<usize> {
        self.classes
            .contains_key(name)
            .then(|| self.ancestors(name).len())
    }

    /// The nearest class that both `a` and `b` are (or descend from). `None` if either is
    /// unknown or they live in different trees.
    pub fn lowest_common_ancestor(&self, a: &str, b: &str) -> Option<&str> {
        if !self.classes.contains_key(a) || !self.classes.contains_key(b) {
            return None;
        }
        let mut chain_a: Vec<&str> = vec![self.classes.get_key_value(a)?.0.as_str()];
        chain_a.extend(self.ancestors(a));
        let set_b: BTreeSet<&str> = std::iter::once(self.classes.get_key_value(b)?.0.as_str())
            .chain(self.ancestors(b))
            .collect();
        chain_a.into_iter().find(|n| set_b.contains(n))
    }

    /// Declare `source --name--> target`. Both classes must exist.
    pub fn add_relation(
        &mut self,
        source: &str,
        name: &str,
        target: &str,
        weight: f32,
    ) -> Result<(), OntologyError> {
        if name.is_empty() {
            return Err(OntologyError::EmptyName);
        }
        if !weight.is_finite() {
            return Err(OntologyError::BadWeight);
        }
        for c in [source, target] {
            if !self.classes.contains_key(c) {
                return Err(OntologyError::Unknown(c.to_string()));
            }
        }
        self.relations.push(Relation {
            source: source.to_string(),
            name: name.to_string(),
            target: target.to_string(),
            weight,
        });
        Ok(())
    }

    /// Relations of `name` declared directly on `source`.
    pub fn direct(&self, source: &str, name: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.source == source && r.name == name)
            .collect()
    }

    /// Relations of `name` that apply to `source`: its own plus those inherited from every
    /// ancestor. Own relations come first, then nearer ancestors before farther ones.
    pub fn inherited(&self, source: &str, name: &str) -> Vec<&Relation> {
        if !self.classes.contains_key(source) {
            return Vec::new();
        }
        let mut out = self.direct(source, name);
        for a in self.ancestors(source) {
            out.extend(self.direct(a, name));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animals() -> Ontology {
        let mut o = Ontology::new();
        let e = ClassKind::Entity;
        o.add_class("Animal", e, "living thing", None).unwrap();
        o.add_class("Bird", e, "", Some("Animal")).unwrap();
        o.add_class("Mammal", e, "", Some("Animal")).unwrap();
        o.add_class("Sparrow", e, "", Some("Bird")).unwrap();
        o.add_class("Penguin", e, "", Some("Bird")).unwrap();
        o.add_class("Dog", e, "", Some("Mammal")).unwrap();
        o.add_class("Rock", e, "", None).unwrap();
        o
    }

    #[test]
    fn building_validates() {
        let mut o = animals();
        assert_eq!(o.len(), 7);
        assert!(!o.is_empty());
        assert_eq!(
            o.add_class("Bird", ClassKind::Entity, "", None),
            Err(OntologyError::Duplicate("Bird".into()))
        );
        assert_eq!(
            o.add_class("Cat", ClassKind::Entity, "", Some("Feline")),
            Err(OntologyError::Unknown("Feline".into()))
        );
        assert_eq!(
            o.add_class("", ClassKind::Entity, "", None),
            Err(OntologyError::EmptyName)
        );
        assert_eq!(o.len(), 7, "failed adds change nothing");
        assert_eq!(o.class("Sparrow").unwrap().parent.as_deref(), Some("Bird"));
        assert!(o.class("Nope").is_none());
    }

    #[test]
    fn is_a_is_reflexive_transitive_and_directional() {
        let o = animals();
        assert!(o.is_a("Sparrow", "Sparrow"));
        assert!(o.is_a("Sparrow", "Bird"));
        assert!(o.is_a("Sparrow", "Animal"));
        assert!(!o.is_a("Bird", "Sparrow"));
        assert!(!o.is_a("Sparrow", "Mammal"));
        assert!(!o.is_a("Sparrow", "Rock"));
        assert!(!o.is_a("Ghost", "Ghost"));
        assert!(!o.is_a("Sparrow", "Ghost"));
    }

    #[test]
    fn ancestors_descendants_and_depth() {
        let o = animals();
        assert_eq!(o.ancestors("Sparrow"), ["Bird", "Animal"]);
        assert!(o.ancestors("Animal").is_empty());
        assert!(o.ancestors("Ghost").is_empty());
        assert_eq!(
            o.descendants("Animal"),
            ["Bird", "Mammal", "Penguin", "Sparrow", "Dog"]
        );
        assert_eq!(o.descendants("Bird"), ["Penguin", "Sparrow"]);
        assert!(o.descendants("Dog").is_empty());
        assert_eq!(o.depth("Animal"), Some(0));
        assert_eq!(o.depth("Sparrow"), Some(2));
        assert_eq!(o.depth("Ghost"), None);
    }

    #[test]
    fn lowest_common_ancestor() {
        let o = animals();
        assert_eq!(o.lowest_common_ancestor("Sparrow", "Penguin"), Some("Bird"));
        assert_eq!(o.lowest_common_ancestor("Sparrow", "Dog"), Some("Animal"));
        assert_eq!(o.lowest_common_ancestor("Sparrow", "Bird"), Some("Bird"));
        assert_eq!(o.lowest_common_ancestor("Dog", "Dog"), Some("Dog"));
        assert_eq!(o.lowest_common_ancestor("Sparrow", "Rock"), None);
        assert_eq!(o.lowest_common_ancestor("Sparrow", "Ghost"), None);
    }

    #[test]
    fn relations_validate() {
        let mut o = animals();
        assert_eq!(
            o.add_relation("Bird", "can", "Fly", 1.0),
            Err(OntologyError::Unknown("Fly".into()))
        );
        assert_eq!(
            o.add_relation("Ghost", "can", "Bird", 1.0),
            Err(OntologyError::Unknown("Ghost".into()))
        );
        assert_eq!(
            o.add_relation("Bird", "", "Animal", 1.0),
            Err(OntologyError::EmptyName)
        );
        assert_eq!(
            o.add_relation("Bird", "eats", "Animal", f32::NAN),
            Err(OntologyError::BadWeight)
        );
        assert!(o.add_relation("Bird", "eats", "Animal", 0.2).is_ok());
    }

    #[test]
    fn relations_are_inherited_down_the_hierarchy() {
        let mut o = animals();
        o.add_class("Flying", ClassKind::Action, "", None).unwrap();
        o.add_class("Swimming", ClassKind::Action, "", None)
            .unwrap();
        o.add_relation("Bird", "can", "Flying", 0.9).unwrap();
        o.add_relation("Penguin", "can", "Swimming", 1.0).unwrap();
        o.add_relation("Animal", "can", "Swimming", 0.1).unwrap();
        let targets = |c: &str| {
            o.inherited(c, "can")
                .iter()
                .map(|r| (r.target.as_str(), r.weight))
                .collect::<Vec<_>>()
        };
        assert_eq!(targets("Sparrow"), [("Flying", 0.9), ("Swimming", 0.1)]);
        assert_eq!(
            targets("Penguin"),
            [("Swimming", 1.0), ("Flying", 0.9), ("Swimming", 0.1)]
        );
        assert_eq!(targets("Rock"), Vec::<(&str, f32)>::new());
        assert!(o.inherited("Ghost", "can").is_empty());
        assert_eq!(o.direct("Sparrow", "can").len(), 0);
        assert_eq!(o.direct("Penguin", "can").len(), 1);
        assert!(o.inherited("Sparrow", "eats").is_empty());
    }
}
