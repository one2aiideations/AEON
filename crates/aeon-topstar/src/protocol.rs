use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Action, ActionResult, CapabilityId, Context, Evidence, Identity, Intent, ResponsibilityContract,
};

pub const PROTOCOL_NAME: &str = "topstar";
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;

pub type MessageId = Uuid;
pub type CorrelationId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };

    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    pub const fn compatible_with(self, other: Self) -> bool {
        self.major == other.major && self.minor >= other.minor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Request,
    Response,
    Event,
    Progress,
    Cancel,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDescriptor {
    pub name: String,
    pub version: ProtocolVersion,
    pub content_type: String,
}

impl SchemaDescriptor {
    pub fn new(
        name: impl Into<String>,
        version: ProtocolVersion,
        content_type: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version,
            content_type: content_type.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
    pub retryable_errors: Vec<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_ms: 0,
            retryable_errors: Vec::new(),
        }
    }
}

impl RetryPolicy {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.max_attempts == 0 {
            return Err(ProtocolError::InvalidRetryPolicy(
                "max_attempts must be at least one".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestOptions {
    pub timeout_ms: u64,
    pub retry: RetryPolicy,
    pub streaming: bool,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            retry: RetryPolicy::default(),
            streaming: false,
        }
    }
}

impl RequestOptions {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.timeout_ms == 0 {
            return Err(ProtocolError::InvalidTimeout);
        }
        self.retry.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SecurityMetadata {
    pub authenticated: bool,
    pub encrypted: bool,
    pub signed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolEnvelope {
    pub id: MessageId,
    pub correlation_id: Option<CorrelationId>,
    pub protocol: String,
    pub version: ProtocolVersion,
    pub kind: MessageKind,
    pub schema: SchemaDescriptor,
    pub sender: Identity,
    pub recipient: Option<Identity>,
    pub payload: serde_json::Value,
    pub metadata: BTreeMap<String, String>,
    pub security: SecurityMetadata,
}

impl ProtocolEnvelope {
    pub fn new(
        kind: MessageKind,
        schema: SchemaDescriptor,
        sender: Identity,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            correlation_id: None,
            protocol: PROTOCOL_NAME.into(),
            version: ProtocolVersion::CURRENT,
            kind,
            schema,
            sender,
            recipient: None,
            payload,
            metadata: BTreeMap::new(),
            security: SecurityMetadata::default(),
        }
    }

    pub fn with_correlation(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub fn with_recipient(mut self, recipient: Identity) -> Self {
        self.recipient = Some(recipient);
        self
    }

    pub fn validate(&self, supported: ProtocolVersion) -> Result<(), ProtocolError> {
        if self.protocol != PROTOCOL_NAME {
            return Err(ProtocolError::UnsupportedProtocol(self.protocol.clone()));
        }
        if !self.version.compatible_with(supported) {
            return Err(ProtocolError::IncompatibleVersion {
                message: self.version,
                supported,
            });
        }
        if self.schema.name.trim().is_empty() {
            return Err(ProtocolError::InvalidSchema("schema name is empty".into()));
        }
        if self.schema.content_type.trim().is_empty() {
            return Err(ProtocolError::InvalidSchema("content type is empty".into()));
        }
        if self.sender.name.trim().is_empty() {
            return Err(ProtocolError::InvalidIdentity);
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        serde_json::to_vec(self).map_err(|e| ProtocolError::Serialization(e.to_string()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        serde_json::from_slice(bytes).map_err(|e| ProtocolError::Serialization(e.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestMessage {
    pub intent: Option<Intent>,
    pub context: Option<Context>,
    pub capability: Option<CapabilityId>,
    pub responsibility: Option<ResponsibilityContract>,
    pub action: Option<Action>,
    pub options: RequestOptions,
    pub parameters: serde_json::Value,
}

impl RequestMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.options.validate()?;
        if self.intent.is_none() && self.action.is_none() {
            return Err(ProtocolError::InvalidRequest(
                "request must contain an intent or action".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseMessage {
    pub result: Option<ActionResult>,
    pub output: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventMessage {
    pub topic: String,
    pub payload: serde_json::Value,
}

impl EventMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.topic.trim().is_empty() {
            return Err(ProtocolError::InvalidEvent("topic is empty".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressMessage {
    pub completed: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}

impl ProgressMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(total) = self.total {
            if total == 0 || self.completed > total {
                return Err(ProtocolError::InvalidProgress);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelMessage {
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ErrorMessage {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.code.trim().is_empty() || self.message.trim().is_empty() {
            return Err(ProtocolError::InvalidError);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAdvertisement {
    pub capability: CapabilityId,
    pub schema: SchemaDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaNegotiation {
    pub supported: Vec<SchemaDescriptor>,
}

impl SchemaNegotiation {
    pub fn negotiate(
        &self,
        requested: &SchemaDescriptor,
    ) -> Result<SchemaDescriptor, ProtocolError> {
        self.supported
            .iter()
            .find(|candidate| {
                candidate.name == requested.name
                    && candidate.content_type == requested.content_type
                    && candidate.version.compatible_with(requested.version)
            })
            .cloned()
            .ok_or_else(|| ProtocolError::SchemaNotSupported(requested.name.clone()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceMessage {
    pub evidence: Evidence,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unsupported protocol: {0}")]
    UnsupportedProtocol(String),
    #[error("incompatible protocol version: message {message:?}, supported {supported:?}")]
    IncompatibleVersion {
        message: ProtocolVersion,
        supported: ProtocolVersion,
    },
    #[error("invalid schema: {0}")]
    InvalidSchema(String),
    #[error("invalid identity")]
    InvalidIdentity,
    #[error("invalid retry policy: {0}")]
    InvalidRetryPolicy(String),
    #[error("invalid timeout")]
    InvalidTimeout,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("invalid progress")]
    InvalidProgress,
    #[error("invalid error message")]
    InvalidError,
    #[error("schema not supported: {0}")]
    SchemaNotSupported(String),
    #[error("serialization failed: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    #[test]
    fn envelope_round_trips_through_json() {
        let sender = Identity::new("runtime");
        let schema = SchemaDescriptor::new(
            "topstar.request",
            ProtocolVersion::CURRENT,
            "application/json",
        );
        let envelope = ProtocolEnvelope::new(
            MessageKind::Request,
            schema,
            sender,
            serde_json::json!({"operation": "discover"}),
        );

        let encoded = envelope.encode().expect("encoding should succeed");
        let decoded = ProtocolEnvelope::decode(&encoded).expect("decoding should succeed");

        assert_eq!(decoded.id, envelope.id);
        assert_eq!(decoded.payload, envelope.payload);
        assert_eq!(decoded.version, ProtocolVersion::CURRENT);
    }

    #[test]
    fn request_requires_intent_or_action() {
        let request = RequestMessage {
            intent: None,
            context: None,
            capability: None,
            responsibility: None,
            action: None,
            options: RequestOptions::default(),
            parameters: serde_json::Value::Null,
        };

        assert!(matches!(
            request.validate(),
            Err(ProtocolError::InvalidRequest(_))
        ));
    }

    #[test]
    fn schema_negotiation_requires_compatible_schema() {
        let supported = SchemaNegotiation {
            supported: vec![SchemaDescriptor::new(
                "topstar.request",
                ProtocolVersion::new(1, 2),
                "application/json",
            )],
        };
        let requested = SchemaDescriptor::new(
            "topstar.request",
            ProtocolVersion::new(1, 1),
            "application/json",
        );

        assert!(supported.negotiate(&requested).is_ok());
    }

    #[test]
    fn incompatible_major_version_is_rejected() {
        let mut envelope = ProtocolEnvelope::new(
            MessageKind::Event,
            SchemaDescriptor::new(
                "topstar.event",
                ProtocolVersion::new(2, 0),
                "application/json",
            ),
            Identity::new("runtime"),
            serde_json::json!({}),
        );

        envelope.version = ProtocolVersion::new(2, 0);

        assert!(matches!(
            envelope.validate(ProtocolVersion::CURRENT),
            Err(ProtocolError::IncompatibleVersion { .. })
        ));
    }

    #[test]
    fn progress_cannot_exceed_total() {
        let progress = ProgressMessage {
            completed: 11,
            total: Some(10),
            message: None,
        };
        assert_eq!(progress.validate(), Err(ProtocolError::InvalidProgress));
    }
}
