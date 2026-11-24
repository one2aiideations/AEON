use serde::{Deserialize, Serialize};

use crate::ActionState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub state: ActionState,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            state: ActionState::Proposed,
        }
    }

    pub fn authorize(&mut self) -> bool {
        if self.state == ActionState::Proposed {
            self.state = ActionState::Authorized;
            true
        } else {
            false
        }
    }

    pub fn start(&mut self) -> bool {
        if self.state == ActionState::Authorized {
            self.state = ActionState::Executing;
            true
        } else {
            false
        }
    }

    pub fn succeed(&mut self) -> bool {
        if self.state == ActionState::Executing {
            self.state = ActionState::Succeeded;
            true
        } else {
            false
        }
    }

    pub fn fail(&mut self) -> bool {
        if matches!(self.state, ActionState::Authorized | ActionState::Executing) {
            self.state = ActionState::Failed;
            true
        } else {
            false
        }
    }

    pub fn cancel(&mut self) -> bool {
        if matches!(
            self.state,
            ActionState::Proposed | ActionState::Authorized | ActionState::Executing
        ) {
            self.state = ActionState::Cancelled;
            true
        } else {
            false
        }
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}
