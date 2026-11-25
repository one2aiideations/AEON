//! Conversation: roles, messages and sessions.

use crate::error::{Error, Result};
use crate::ids::IdGen;
use std::collections::BTreeMap;

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Instructions to the assistant.
    System,
    /// The person.
    User,
    /// The model.
    Assistant,
    /// A capability result.
    Tool,
}

impl Role {
    /// Parse `system`, `user`, `assistant` or `tool`.
    ///
    /// # Errors
    /// Returns [`Error::Invalid`] for any other word.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            other => Err(Error::Invalid(format!("unsupported message role: {other}"))),
        }
    }
}

/// One turn of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Identifier.
    pub id: String,
    /// Author role.
    pub role: Role,
    /// Text content.
    pub content: String,
    /// Logical time of creation.
    pub tick: u64,
    /// Free-form metadata (for example artifact ids).
    pub metadata: BTreeMap<String, String>,
}

/// A conversation owned by one user of one tenant.
#[derive(Debug, Clone)]
pub struct Session {
    /// Identifier.
    pub id: String,
    /// Tenant that owns the session.
    pub tenant: String,
    /// User who opened it.
    pub user: String,
    /// Name of the role the user acts in.
    pub role: String,
    messages: Vec<Message>,
}

impl Session {
    pub(crate) fn new(id: String, tenant: &str, user: &str, role: &str) -> Self {
        Self {
            id,
            tenant: tenant.into(),
            user: user.into(),
            role: role.into(),
            messages: Vec::new(),
        }
    }

    /// Append a message.
    ///
    /// # Errors
    /// Returns [`Error::Invalid`] when the content is blank.
    pub fn add(
        &mut self,
        role: Role,
        content: &str,
        tick: u64,
        ids: &mut IdGen,
    ) -> Result<&Message> {
        if content.trim().is_empty() {
            return Err(Error::Invalid("message content must not be blank".into()));
        }
        self.messages.push(Message {
            id: ids.next("msg"),
            role,
            content: content.into(),
            tick,
            metadata: BTreeMap::new(),
        });
        Ok(&self.messages[self.messages.len() - 1])
    }

    /// Messages in order.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_and_unknown_roles() {
        let mut ids = IdGen::new();
        let mut s = Session::new("s".into(), "t", "u", "r");
        assert!(s.add(Role::User, "  ", 1, &mut ids).is_err());
        assert!(Role::parse("robot").is_err());
        assert_eq!(Role::parse("tool").unwrap(), Role::Tool);
        s.add(Role::User, "hi", 1, &mut ids).unwrap();
        s.add(Role::Assistant, "hello", 2, &mut ids).unwrap();
        assert_eq!(s.messages().len(), 2);
        assert_ne!(s.messages()[0].id, s.messages()[1].id);
    }
}
