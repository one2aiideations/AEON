use thiserror::Error;

use crate::{
    authorize, Action, ActionResult, ActionState, AuthorizationDecision, CapabilityRegistry,
    Evidence, Identity, Policy, ResponsibilityContract,
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("unknown capability")]
    UnknownCapability,
    #[error("authorization denied: {0}")]
    AuthorizationDenied(String),
}

#[derive(Debug, Default)]
pub struct Runtime {
    capabilities: CapabilityRegistry,
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn capabilities(&self) -> &CapabilityRegistry {
        &self.capabilities
    }

    pub fn capabilities_mut(&mut self) -> &mut CapabilityRegistry {
        &mut self.capabilities
    }

    pub fn authorize_action(
        &self,
        action: &Action,
        identity: &Identity,
        responsibility: &ResponsibilityContract,
        policy: &Policy,
        now_unix: i64,
    ) -> Result<(), RuntimeError> {
        if !self.capabilities.contains(action.capability) {
            return Err(RuntimeError::UnknownCapability);
        }

        match authorize(
            identity,
            action.capability,
            responsibility,
            policy,
            now_unix,
        ) {
            AuthorizationDecision::Allow => Ok(()),
            AuthorizationDecision::Deny(reason) => Err(RuntimeError::AuthorizationDenied(reason)),
        }
    }

    pub fn execute(
        &self,
        mut action: Action,
        identity: &Identity,
        responsibility: &ResponsibilityContract,
        policy: &Policy,
        now_unix: i64,
    ) -> Result<ActionResult, RuntimeError> {
        self.authorize_action(&action, identity, responsibility, policy, now_unix)?;
        action.state = ActionState::Executing;
        action.state = ActionState::Succeeded;

        Ok(ActionResult::success(
            action.id,
            serde_json::json!({
                "status": "executed",
                "capability": action.capability,
            }),
        ))
    }

    pub fn verify_evidence(&self, evidence: &Evidence) -> bool {
        !evidence.source.is_empty()
    }
}
