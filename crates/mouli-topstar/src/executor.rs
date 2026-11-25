use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::{
    authorize, Action, ActionResult, ActionState, AuditOutcome, AuthorizationDecision,
    CapabilityRegistry, ExecutionAuditLog, ExecutionOutcome, ExecutionRecord, ExecutionRecordStore,
    ExecutionRecordStoreError, Identity, Policy, ResponsibilityContract, RevocationRegistry,
};

const MAX_RECENT_EXECUTION_RECORDS: usize = 4096;

pub trait ActionHandler: Send + Sync {
    fn execute(&self, action: &Action) -> Result<serde_json::Value, String>;
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecutionError {
    #[error("unknown capability")]
    UnknownCapability,
    #[error("execution authority has been revoked")]
    Revoked,
    #[error("authorization denied: {0}")]
    AuthorizationDenied(String),
    #[error("invalid action state")]
    InvalidState,
    #[error("execution failed: {0}")]
    HandlerFailed(String),
    #[error("audit logging failed: {0}")]
    Audit(String),
    #[error("execution provenance persistence failed: {0}")]
    Provenance(String),
}

#[derive(Debug, Default)]
pub struct ExecutionEngine {
    capabilities: CapabilityRegistry,
    revocations: RevocationRegistry,
    audit: ExecutionAuditLog,
    recent_records: Arc<Mutex<VecDeque<ExecutionRecord>>>,
    record_store: Option<ExecutionRecordStore>,
}

impl ExecutionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_record_store(record_store: ExecutionRecordStore) -> Self {
        Self {
            record_store: Some(record_store),
            ..Self::default()
        }
    }

    pub fn record_store(&self) -> Option<&ExecutionRecordStore> {
        self.record_store.as_ref()
    }

    pub fn capabilities(&self) -> &CapabilityRegistry {
        &self.capabilities
    }

    pub fn capabilities_mut(&mut self) -> &mut CapabilityRegistry {
        &mut self.capabilities
    }

    pub fn revocations(&self) -> &RevocationRegistry {
        &self.revocations
    }

    pub fn revocations_mut(&mut self) -> &mut RevocationRegistry {
        &mut self.revocations
    }

    pub fn audit(&self) -> &ExecutionAuditLog {
        &self.audit
    }

    pub fn recent_execution_records(&self) -> Vec<ExecutionRecord> {
        self.recent_records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn execution_record(&self, id: crate::ExecutionRecordId) -> Option<ExecutionRecord> {
        self.recent_records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .find(|record| record.id == id)
            .cloned()
    }

    pub fn execution_records_for_action(&self, action: crate::ActionId) -> Vec<ExecutionRecord> {
        self.recent_records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .filter(|record| record.action == action)
            .cloned()
            .collect()
    }

    pub fn execution_records_for_actor(&self, actor: crate::ActorId) -> Vec<ExecutionRecord> {
        self.recent_records
            .lock()
            .expect("execution record lock poisoned")
            .iter()
            .filter(|record| record.actor == actor)
            .cloned()
            .collect()
    }

    pub const MAX_RECENT_EXECUTION_RECORDS: usize = MAX_RECENT_EXECUTION_RECORDS;

    fn retain_execution_record(&self, record: ExecutionRecord) {
        let mut records = self
            .recent_records
            .lock()
            .expect("execution record lock poisoned");
        if MAX_RECENT_EXECUTION_RECORDS == 0 {
            return;
        }
        if records.len() == MAX_RECENT_EXECUTION_RECORDS {
            records.pop_front();
        }
        records.push_back(record);
    }

    pub fn execute<H: ActionHandler>(
        &self,
        mut action: Action,
        identity: &Identity,
        responsibility: &ResponsibilityContract,
        policy: &Policy,
        now_unix: i64,
        handler: &H,
    ) -> Result<ActionResult, ExecutionError> {
        if action.state != ActionState::Proposed {
            self.audit
                .record(
                    identity.id,
                    Some(action.id),
                    now_unix,
                    AuditOutcome::Denied,
                    "invalid action state",
                )
                .map_err(|e| ExecutionError::Audit(e.to_string()))?;
            return Err(ExecutionError::InvalidState);
        }
        if !self.capabilities.contains(action.capability) {
            self.audit
                .record(
                    identity.id,
                    Some(action.id),
                    now_unix,
                    AuditOutcome::Denied,
                    "unknown capability",
                )
                .map_err(|e| ExecutionError::Audit(e.to_string()))?;
            return Err(ExecutionError::UnknownCapability);
        }

        if responsibility.revoked
            || self.revocations.is_capability_revoked(action.capability)
            || self
                .revocations
                .is_responsibility_revoked(responsibility.id)
            || self.revocations.is_action_revoked(action.id)
        {
            self.audit
                .record(
                    identity.id,
                    Some(action.id),
                    now_unix,
                    AuditOutcome::Revoked,
                    "execution authority revoked",
                )
                .map_err(|e| ExecutionError::Audit(e.to_string()))?;
            return Err(ExecutionError::Revoked);
        }

        match authorize(
            identity,
            action.capability,
            responsibility,
            policy,
            now_unix,
        ) {
            AuthorizationDecision::Allow => {}
            AuthorizationDecision::Deny(reason) => {
                self.audit
                    .record(
                        identity.id,
                        Some(action.id),
                        now_unix,
                        AuditOutcome::Denied,
                        format!("authorization denied: {reason}"),
                    )
                    .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                return Err(ExecutionError::AuthorizationDenied(reason));
            }
        }

        action.state = ActionState::Authorized;
        self.audit
            .record(
                identity.id,
                Some(action.id),
                now_unix,
                AuditOutcome::Allowed,
                "execution authorized",
            )
            .map_err(|e| ExecutionError::Audit(e.to_string()))?;
        action.state = ActionState::Executing;
        self.audit
            .record(
                identity.id,
                Some(action.id),
                now_unix,
                AuditOutcome::Started,
                "execution started",
            )
            .map_err(|e| ExecutionError::Audit(e.to_string()))?;

        let mut record = ExecutionRecord::started(
            identity.id,
            action.id,
            action.capability,
            responsibility.id,
            now_unix,
        );

        let output = match handler.execute(&action) {
            Ok(output) => output,
            Err(error) => {
                action.state = ActionState::Failed;
                record
                    .complete(now_unix, ExecutionOutcome::Failed)
                    .expect("execution record completion must be valid");
                self.audit
                    .record(
                        identity.id,
                        Some(action.id),
                        now_unix,
                        AuditOutcome::Failed,
                        format!("handler failed: {error}"),
                    )
                    .map_err(|e| ExecutionError::Audit(e.to_string()))?;
                if let Some(store) = &self.record_store {
                    store
                        .append(record.clone())
                        .map_err(|e: ExecutionRecordStoreError| {
                            ExecutionError::Provenance(e.to_string())
                        })?;
                }
                self.retain_execution_record(record);
                return Err(ExecutionError::HandlerFailed(error));
            }
        };

        action.state = ActionState::Succeeded;
        record
            .complete(now_unix, ExecutionOutcome::Succeeded)
            .expect("execution record completion must be valid");
        self.audit
            .record(
                identity.id,
                Some(action.id),
                now_unix,
                AuditOutcome::Succeeded,
                "execution succeeded",
            )
            .map_err(|e| ExecutionError::Audit(e.to_string()))?;
        if let Some(store) = &self.record_store {
            store
                .append(record.clone())
                .map_err(|e: ExecutionRecordStoreError| {
                    ExecutionError::Provenance(e.to_string())
                })?;
        }
        self.retain_execution_record(record);
        Ok(ActionResult::success(action.id, output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, CapabilityRisk, ResponsibilityContract};

    struct Handler;

    impl ActionHandler for Handler {
        fn execute(&self, _action: &Action) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({"executed": true}))
        }
    }

    struct FailingHandler;

    impl ActionHandler for FailingHandler {
        fn execute(&self, _action: &Action) -> Result<serde_json::Value, String> {
            Err("side effect failed".into())
        }
    }

    fn policy_for(r: &ResponsibilityContract) -> Policy {
        let mut policy = Policy::new("test");
        policy.allowed_capabilities = r.allowed_capabilities.clone();
        policy
    }

    fn fixture() -> (Identity, ResponsibilityContract, ExecutionEngine, Action) {
        fixture_with(ExecutionEngine::new())
    }

    fn fixture_with(
        mut engine: ExecutionEngine,
    ) -> (Identity, ResponsibilityContract, ExecutionEngine, Action) {
        let identity = Identity::new("executor");
        let capability = Capability::new("test.execute", "test execution", CapabilityRisk::Low);
        let mut responsibility = ResponsibilityContract::new("test goal", identity.id);
        responsibility.allowed_capabilities = vec![capability.id];
        responsibility.expires_at_unix = Some(i64::MAX);
        engine
            .capabilities_mut()
            .register(capability)
            .expect("register");
        let action = Action::new(
            identity.id,
            responsibility.allowed_capabilities[0],
            responsibility.id,
            "execute test",
        );
        (identity, responsibility, engine, action)
    }

    #[test]
    fn revoked_responsibility_blocks_execution() {
        let (identity, mut responsibility, engine, action) = fixture();
        responsibility.revoked = true;

        let error = engine
            .execute(
                action,
                &identity,
                &responsibility,
                &policy_for(&responsibility),
                0,
                &Handler,
            )
            .expect_err("revocation must block execution");
        assert_eq!(error, ExecutionError::Revoked);
        assert!(engine.recent_execution_records().is_empty());
    }

    #[test]
    fn centrally_revoked_action_blocks_execution() {
        let (identity, responsibility, mut engine, action) = fixture();
        engine.revocations_mut().revoke_action(action.id);

        let error = engine
            .execute(
                action,
                &identity,
                &responsibility,
                &policy_for(&responsibility),
                0,
                &Handler,
            )
            .expect_err("revoked action must be blocked");
        assert_eq!(error, ExecutionError::Revoked);
        assert!(engine.recent_execution_records().is_empty());
    }

    #[test]
    fn successful_execution_creates_completed_execution_record() {
        let (identity, responsibility, engine, action) = fixture();

        let result = engine
            .execute(
                action,
                &identity,
                &responsibility,
                &policy_for(&responsibility),
                10,
                &Handler,
            )
            .expect("execution");

        assert_eq!(result.status, crate::ResultStatus::Success);
        assert_eq!(engine.audit().len(), 3);

        let records = engine.recent_execution_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].actor, identity.id);
        assert_eq!(records[0].action, result.action);
        assert_eq!(
            records[0].capability,
            responsibility.allowed_capabilities[0]
        );
        assert_eq!(records[0].responsibility, responsibility.id);
        assert_eq!(records[0].started_at_unix, 10);
        assert_eq!(records[0].completed_at_unix, Some(10));
        assert_eq!(records[0].outcome, ExecutionOutcome::Succeeded);
        assert_eq!(
            engine.execution_record(records[0].id),
            Some(records[0].clone())
        );
        assert_eq!(
            engine.execution_records_for_action(records[0].action).len(),
            1
        );
        assert_eq!(
            engine.execution_records_for_actor(records[0].actor).len(),
            1
        );
        assert!(engine
            .execution_record(crate::ExecutionRecordId::new_v4())
            .is_none());
    }

    #[test]
    fn recent_execution_records_are_bounded_and_evict_oldest() {
        let (identity, responsibility, engine, _) = fixture();
        let mut ids = Vec::with_capacity(MAX_RECENT_EXECUTION_RECORDS + 1);

        for index in 0..=MAX_RECENT_EXECUTION_RECORDS {
            let action = Action::new(
                identity.id,
                responsibility.allowed_capabilities[0],
                responsibility.id,
                format!("execute {index}"),
            );
            engine
                .execute(
                    action,
                    &identity,
                    &responsibility,
                    &policy_for(&responsibility),
                    index as i64,
                    &Handler,
                )
                .expect("execution");
            let records = engine.recent_execution_records();
            ids.push(records.last().expect("record").id);
        }

        let records = engine.recent_execution_records();
        assert_eq!(records.len(), MAX_RECENT_EXECUTION_RECORDS);
        assert!(!records.iter().any(|record| record.id == ids[0]));
        assert_eq!(records.last().map(|record| record.id), ids.last().copied());
    }

    #[test]
    fn audit_terminal_event_precedes_provenance_retention() {
        let (identity, responsibility, engine, action) = fixture();
        let result = engine
            .execute(
                action,
                &identity,
                &responsibility,
                &policy_for(&responsibility),
                30,
                &Handler,
            )
            .expect("execution");

        let audit = engine.audit().entries();
        let record = engine
            .execution_record(engine.execution_records_for_action(result.action)[0].id)
            .expect("record");
        assert_eq!(
            audit.last().expect("audit entry").outcome,
            AuditOutcome::Succeeded
        );
        assert_eq!(record.outcome, ExecutionOutcome::Succeeded);
    }

    #[test]
    fn durable_record_store_receives_successful_execution() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let path = std::env::temp_dir().join(format!(
            "topstar-executor-store-{}.log",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let store = ExecutionRecordStore::open(&path).expect("store");
        let (identity, responsibility, engine, action) =
            fixture_with(ExecutionEngine::with_record_store(store.clone()));

        engine
            .execute(
                action,
                &identity,
                &responsibility,
                &policy_for(&responsibility),
                40,
                &Handler,
            )
            .expect("execution");

        let records = store.all();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, ExecutionOutcome::Succeeded);
        std::fs::remove_file(path).expect("temporary store should be removed");
    }

    #[test]
    fn failed_handler_creates_failed_execution_record() {
        let (identity, responsibility, engine, action) = fixture();

        let error = engine
            .execute(
                action,
                &identity,
                &responsibility,
                &policy_for(&responsibility),
                20,
                &FailingHandler,
            )
            .expect_err("handler failure must propagate");
        assert_eq!(
            error,
            ExecutionError::HandlerFailed("side effect failed".into())
        );

        let records = engine.recent_execution_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, ExecutionOutcome::Failed);
        assert_eq!(records[0].started_at_unix, 20);
        assert_eq!(records[0].completed_at_unix, Some(20));
    }
}
