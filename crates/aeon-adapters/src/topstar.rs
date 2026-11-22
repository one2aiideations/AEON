//! TOPSTAR message adapter.
//!
//! Validates a decoded TOPSTAR message before AEON acts on it. Protocol
//! metadata is never trusted for authentication: only the host-supplied
//! [`Verifier`] (signature over canonical bytes, sender bound to key) can
//! accept a message.

use core::fmt;

/// Protocol major version accepted.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Highest protocol minor version accepted.
pub const PROTOCOL_MINOR: u16 = 0;
/// Largest payload accepted.
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// Longest schema name, content type or key id.
pub const MAX_LABEL: usize = 63;

/// Message kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Request expecting a response.
    Request,
    /// Response to a request.
    Response,
    /// Unsolicited event.
    Event,
    /// Progress update.
    Progress,
    /// Cancel a request.
    Cancel,
    /// Error report.
    Error,
}

/// AEON-side view of a TOPSTAR message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Protocol major version.
    pub major: u16,
    /// Protocol minor version.
    pub minor: u16,
    /// Kind.
    pub kind: Kind,
    /// Correlation UUID (may be empty).
    pub correlation_id: String,
    /// Sender UUID.
    pub sender_id: String,
    /// Payload schema name.
    pub schema: String,
    /// Payload content type.
    pub content_type: String,
    /// Signing key id.
    pub key_id: String,
    /// Payload.
    pub payload: Vec<u8>,
    /// Signature over canonical bytes.
    pub signature: Vec<u8>,
    /// Request timeout.
    pub timeout_ms: u32,
    /// Request retries allowed.
    pub retry_attempts: u32,
    /// Request carries an intent.
    pub has_intent: bool,
    /// Request carries an action.
    pub has_action: bool,
}

/// Why a message was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Unsupported protocol version.
    Version,
    /// Schema name or content type missing or too long.
    Schema,
    /// Sender, correlation or key id invalid.
    Identity,
    /// Payload too large.
    Payload,
    /// Request lacks intent/action, timeout or retries.
    Request,
    /// No signature.
    Signature,
    /// Verifier refused.
    Authentication,
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Version => "unsupported protocol version",
            Self::Schema => "bad schema or content type",
            Self::Identity => "bad sender, correlation or key id",
            Self::Payload => "payload too large",
            Self::Request => "incomplete request",
            Self::Signature => "missing signature",
            Self::Authentication => "authentication failed",
        })
    }
}

impl std::error::Error for Reject {}

/// Host-side cryptographic check bound to a trusted key registry.
pub trait Verifier {
    /// True only after verifying the signature over canonical bytes and
    /// binding `sender_id` to the verified key.
    fn verify(&self, msg: &Message) -> bool;
}

impl<F: Fn(&Message) -> bool> Verifier for F {
    fn verify(&self, msg: &Message) -> bool {
        self(msg)
    }
}

/// Canonical UUID text: 8-4-4-4-12 hex digits.
#[must_use]
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_LABEL && !s.contains('\0')
}

/// Validate `msg`; checks run cheapest first and the verifier runs last.
pub fn validate(msg: &Message, verifier: &dyn Verifier) -> Result<(), Reject> {
    if msg.major != PROTOCOL_MAJOR || msg.minor > PROTOCOL_MINOR {
        return Err(Reject::Version);
    }
    if !label_ok(&msg.schema) || !label_ok(&msg.content_type) {
        return Err(Reject::Schema);
    }
    if !is_uuid(&msg.sender_id)
        || !(msg.correlation_id.is_empty() || is_uuid(&msg.correlation_id))
        || !label_ok(&msg.key_id)
    {
        return Err(Reject::Identity);
    }
    if msg.payload.len() > MAX_PAYLOAD {
        return Err(Reject::Payload);
    }
    if msg.kind == Kind::Request
        && (!(msg.has_intent || msg.has_action) || msg.timeout_ms == 0 || msg.retry_attempts == 0)
    {
        return Err(Reject::Request);
    }
    if msg.signature.is_empty() {
        return Err(Reject::Signature);
    }
    if !verifier.verify(msg) {
        return Err(Reject::Authentication);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> Message {
        Message {
            major: 1,
            minor: 0,
            kind: Kind::Request,
            correlation_id: String::new(),
            sender_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            schema: "aeon.task.v1".into(),
            content_type: "application/json".into(),
            key_id: "k1".into(),
            payload: b"{}".to_vec(),
            signature: vec![1; 64],
            timeout_ms: 1000,
            retry_attempts: 1,
            has_intent: true,
            has_action: false,
        }
    }

    fn yes(_: &Message) -> bool {
        true
    }

    #[test]
    fn accepts_good_message() {
        assert_eq!(validate(&msg(), &yes), Ok(()));
    }

    #[test]
    fn rejections() {
        let no = |_: &Message| false;
        assert_eq!(validate(&msg(), &no), Err(Reject::Authentication));

        let mut m = msg();
        m.major = 2;
        assert_eq!(validate(&m, &yes), Err(Reject::Version));

        let mut m = msg();
        m.sender_id = "not-a-uuid".into();
        assert_eq!(validate(&m, &yes), Err(Reject::Identity));

        let mut m = msg();
        m.has_intent = false;
        assert_eq!(validate(&m, &yes), Err(Reject::Request));
        m.kind = Kind::Event;
        assert_eq!(validate(&m, &yes), Ok(()));

        let mut m = msg();
        m.payload = vec![0; MAX_PAYLOAD + 1];
        assert_eq!(validate(&m, &yes), Err(Reject::Payload));

        let mut m = msg();
        m.signature.clear();
        assert_eq!(validate(&m, &yes), Err(Reject::Signature));
    }

    #[test]
    fn uuid_shape() {
        assert!(is_uuid("00000000-0000-0000-0000-00000000000A"));
        assert!(!is_uuid("00000000-0000-0000-0000-00000000000"));
        assert!(!is_uuid("00000000x0000-0000-0000-000000000000"));
    }
}
