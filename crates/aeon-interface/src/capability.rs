//! The one action interface. Every application (mail, sheet, studio, files, ...) implements
//! [`Application`] and registers its capabilities; users and agents call them the same way.

use crate::error::{Error, Result};
use std::collections::BTreeMap;

/// How much a capability can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Reads only.
    Read,
    /// Changes local state that can be undone.
    Write,
    /// Has effects that cannot be undone or leave the machine (send, pay, delete).
    Consequential,
}

/// A resource a capability needs, checked against policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// A hosted model.
    ExternalModel,
    /// The network.
    Network,
    /// A host process.
    HostProcess,
}

/// Declaration of one capability.
#[derive(Debug, Clone)]
pub struct CapabilitySpec {
    /// Full name, `app.action`.
    pub name: String,
    /// One line for the model picker and help.
    pub description: String,
    /// Risk class.
    pub risk: Risk,
    /// Resources used.
    pub uses: Vec<Resource>,
    /// Required argument names.
    pub required: Vec<String>,
}

impl CapabilitySpec {
    /// Build a spec with no resources and no required arguments.
    #[must_use]
    pub fn new(name: &str, description: &str, risk: Risk) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            risk,
            uses: Vec::new(),
            required: Vec::new(),
        }
    }

    /// Add required arguments.
    #[must_use]
    pub fn requires(mut self, args: &[&str]) -> Self {
        self.required = args.iter().map(|a| (*a).to_string()).collect();
        self
    }

    /// Add used resources.
    #[must_use]
    pub fn using(mut self, uses: &[Resource]) -> Self {
        self.uses = uses.to_vec();
        self
    }
}

/// A request to run a capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// Capability name.
    pub capability: String,
    /// Arguments.
    pub args: BTreeMap<String, String>,
}

/// What a capability returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// Text shown in the conversation.
    pub text: String,
    /// Ids of artifacts produced.
    pub artifacts: Vec<String>,
}

/// An application exposed through the interface.
pub trait Application {
    /// Short name; every capability must start with `name.`.
    fn name(&self) -> &str;
    /// Capabilities offered.
    fn capabilities(&self) -> Vec<CapabilitySpec>;
    /// Run one capability.
    ///
    /// # Errors
    /// Return [`Error::Handler`] when the work fails.
    fn invoke(&mut self, call: &Call) -> Result<Output>;
}

/// Registry of applications and their capabilities.
#[derive(Default)]
pub struct Registry {
    apps: Vec<Box<dyn Application>>,
    specs: BTreeMap<String, (CapabilitySpec, usize)>,
}

impl Registry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an application.
    ///
    /// # Errors
    /// [`Error::Invalid`] when a capability is not named `app.action`;
    /// [`Error::Conflict`] when a name is already registered. Nothing is registered on error.
    pub fn register(&mut self, app: Box<dyn Application>) -> Result<()> {
        let prefix = format!("{}.", app.name());
        let specs = app.capabilities();
        for s in &specs {
            if app.name().trim().is_empty()
                || !s.name.starts_with(&prefix)
                || s.name.len() == prefix.len()
            {
                return Err(Error::Invalid(format!(
                    "capability {} must be named {prefix}<action>",
                    s.name
                )));
            }
            if self.specs.contains_key(&s.name) {
                return Err(Error::Conflict(format!(
                    "capability {} already registered",
                    s.name
                )));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        if !specs.iter().all(|s| seen.insert(&s.name)) {
            return Err(Error::Conflict(
                "duplicate capability within one application".into(),
            ));
        }
        let idx = self.apps.len();
        for s in specs {
            self.specs.insert(s.name.clone(), (s, idx));
        }
        self.apps.push(app);
        Ok(())
    }

    /// Spec of a capability.
    ///
    /// # Errors
    /// [`Error::NotFound`] when unknown.
    pub fn spec(&self, name: &str) -> Result<&CapabilitySpec> {
        self.specs
            .get(name)
            .map(|(s, _)| s)
            .ok_or_else(|| Error::NotFound(format!("capability {name}")))
    }

    /// All specs sorted by name.
    #[must_use]
    pub fn specs(&self) -> Vec<&CapabilitySpec> {
        self.specs.values().map(|(s, _)| s).collect()
    }

    /// Check arguments and run the capability. Policy is the caller's job.
    ///
    /// # Errors
    /// [`Error::NotFound`], [`Error::Invalid`] for a missing required argument, or whatever the
    /// application returns.
    pub fn dispatch(&mut self, call: &Call) -> Result<Output> {
        let (spec, idx) = self
            .specs
            .get(&call.capability)
            .ok_or_else(|| Error::NotFound(format!("capability {}", call.capability)))?;
        if let Some(missing) = spec.required.iter().find(|r| !call.args.contains_key(*r)) {
            return Err(Error::Invalid(format!(
                "{} requires argument {missing}",
                call.capability
            )));
        }
        let idx = *idx;
        self.apps[idx].invoke(call)
    }
}

#[cfg(test)]
pub(crate) mod testapp {
    use super::*;

    /// Test application: `note.read`, `note.write`, `note.send` (consequential), `note.fail`.
    pub struct Notes {
        pub sent: Vec<String>,
    }

    impl Application for Notes {
        fn name(&self) -> &str {
            "note"
        }
        fn capabilities(&self) -> Vec<CapabilitySpec> {
            vec![
                CapabilitySpec::new("note.read", "read a note", Risk::Read),
                CapabilitySpec::new("note.write", "write a note", Risk::Write).requires(&["text"]),
                CapabilitySpec::new("note.send", "send a note out", Risk::Consequential)
                    .requires(&["to"]),
                CapabilitySpec::new("note.cloud", "summarise with a hosted model", Risk::Read)
                    .using(&[Resource::ExternalModel]),
                CapabilitySpec::new("note.fail", "always fails", Risk::Read),
            ]
        }
        fn invoke(&mut self, call: &Call) -> Result<Output> {
            match call.capability.as_str() {
                "note.send" => {
                    self.sent.push(call.args["to"].clone());
                    Ok(Output {
                        text: format!("sent to {}", call.args["to"]),
                        artifacts: vec![],
                    })
                }
                "note.fail" => Err(Error::Handler("boom".into())),
                other => Ok(Output {
                    text: format!("ran {other}"),
                    artifacts: vec![],
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testapp::Notes;
    use super::*;

    fn call(c: &str, args: &[(&str, &str)]) -> Call {
        Call {
            capability: c.into(),
            args: args
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        }
    }

    #[test]
    fn register_and_dispatch() {
        let mut r = Registry::new();
        r.register(Box::new(Notes { sent: vec![] })).unwrap();
        assert_eq!(r.specs().len(), 5);
        assert_eq!(
            r.dispatch(&call("note.read", &[])).unwrap().text,
            "ran note.read"
        );
        assert!(matches!(
            r.dispatch(&call("note.write", &[])),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            r.dispatch(&call("nope.x", &[])),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            r.dispatch(&call("note.fail", &[])),
            Err(Error::Handler(_))
        ));
    }

    #[test]
    fn duplicate_registration_is_refused() {
        let mut r = Registry::new();
        r.register(Box::new(Notes { sent: vec![] })).unwrap();
        assert!(matches!(
            r.register(Box::new(Notes { sent: vec![] })),
            Err(Error::Conflict(_))
        ));
        assert_eq!(r.specs().len(), 5);
    }

    #[test]
    fn misnamed_capability_is_refused() {
        struct Bad;
        impl Application for Bad {
            fn name(&self) -> &str {
                "bad"
            }
            fn capabilities(&self) -> Vec<CapabilitySpec> {
                vec![CapabilitySpec::new("other.x", "", Risk::Read)]
            }
            fn invoke(&mut self, _: &Call) -> Result<Output> {
                Ok(Output::default())
            }
        }
        assert!(matches!(
            Registry::new().register(Box::new(Bad)),
            Err(Error::Invalid(_))
        ));
    }
}
