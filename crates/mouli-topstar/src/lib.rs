//! Foundational domain types for Topstar.

pub mod action;
pub mod audit;
pub mod authorization;
pub mod boundary;
pub mod capability;
pub mod chat;
pub mod context;
pub mod device;
pub mod device_discovery;
pub mod device_registry;
pub mod event;
pub mod eventing;
pub mod evidence;
pub mod execution_audit;
pub mod execution_record;
pub mod execution_record_store;
pub mod executor;
pub mod identity;
pub mod intent;
pub mod lifecycle;
pub mod network;
pub mod persistence;
pub mod policy;
pub mod protocol;
pub mod provenance;
pub mod registry;
pub mod resource;
pub mod responsibility;
pub mod result;
pub mod revocation;
pub mod runtime;
pub mod session;
pub mod verification;

pub use action::{Action, ActionId, ActionState};
pub use audit::{AuditEntry, AuditEntryId, AuditOutcome};
pub use authorization::{authorize, AuthorizationDecision};
pub use boundary::ExecutionBoundary;
pub use capability::{Capability, CapabilityId, CapabilityRisk};
pub use chat::{
    ChatError, ChatMessage, ChatRequest, ChatResponse, ChatRole, ConversationId,
    MessageId as ChatMessageId,
};
pub use context::{Context, ContextId};
pub use device::{
    Device, DeviceError, DeviceId, HardwareInfo, HealthState, MachineProfile, PowerState,
    SecurityState,
};
pub use device_discovery::{DeviceSelector, DeviceSnapshot};
pub use device_registry::{DeviceLifecycle, DeviceRecord, DeviceRegistry, DeviceRegistryError};
pub use event::{Event, EventId};
pub use eventing::{EventBroker, EventFilter, EventRecord, EventingError, SubscriptionId};
pub use evidence::{Evidence, EvidenceId};
pub use execution_audit::{AuditQuery, ExecutionAuditError, ExecutionAuditLog};
pub use execution_record::{
    ExecutionOutcome, ExecutionRecord, ExecutionRecordError, ExecutionRecordId,
};
pub use execution_record_store::{
    ExecutionRecordCursor, ExecutionRecordPage, ExecutionRecordQuery, ExecutionRecordQueryError,
    ExecutionRecordStore, ExecutionRecordStoreError,
};
pub use executor::{ActionHandler, ExecutionEngine, ExecutionError};
pub use identity::{ActorId, Identity};
pub use intent::{Intent, IntentId};
pub use lifecycle::Lifecycle;
pub use network::{Endpoint, NetworkAddress, NetworkError, NetworkIdentity, Transport};
pub use persistence::{Journal, JournalPosition, JournalRecord, PersistenceError};
pub use policy::{Policy, PolicyId};
pub use protocol::{
    CancelMessage, CapabilityAdvertisement, CorrelationId, ErrorMessage, EventMessage, MessageId,
    MessageKind, ProgressMessage, ProtocolEnvelope, ProtocolError, ProtocolVersion, RequestMessage,
    RequestOptions, ResponseMessage, RetryPolicy, SchemaDescriptor, SchemaNegotiation,
    SecurityMetadata, PROTOCOL_MAJOR, PROTOCOL_MINOR, PROTOCOL_NAME,
};
pub use provenance::{Provenance, ProvenanceId};
pub use registry::{CapabilityRegistry, RegistryError};
pub use resource::{Resource, ResourceId, ResourceKind};
pub use responsibility::{ResponsibilityContract, ResponsibilityContractId};
pub use result::{ActionResult, ResultId, ResultStatus};
pub use revocation::RevocationRegistry;
pub use runtime::{Runtime, RuntimeError};
pub use session::{PeerIdentity, Session, SessionError, SessionId, SessionLimits, SessionState};
pub use verification::{Verification, VerificationId, VerificationStatus};
