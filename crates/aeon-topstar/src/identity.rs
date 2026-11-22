use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ActorId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: ActorId,
    pub name: String,
}

impl Identity {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_unique_identity() {
        let first = Identity::new("human");
        let second = Identity::new("agent");
        assert_ne!(first.id, second.id);
    }
}
