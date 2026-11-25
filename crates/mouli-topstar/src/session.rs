use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use uuid::Uuid;

use crate::{Identity, NetworkAddress, ProtocolVersion};

pub type SessionId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Handshaking,
    Established,
    Closing,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub identity: Identity,
    pub node_id: Uuid,
    pub addresses: Vec<NetworkAddress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    pub idle_timeout: Duration,
    pub max_frame_size: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(300),
            max_frame_size: 16 * 1024 * 1024,
        }
    }
}

impl SessionLimits {
    pub fn validate(&self) -> Result<(), SessionError> {
        if self.idle_timeout.is_zero() {
            return Err(SessionError::InvalidLimits("idle timeout must be non-zero"));
        }
        if self.max_frame_size == 0 {
            return Err(SessionError::InvalidLimits(
                "max frame size must be non-zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub local: Identity,
    pub peer: PeerIdentity,
    pub protocol: ProtocolVersion,
    pub state: SessionState,
    pub authenticated: bool,
    pub encrypted: bool,
    pub established_at_unix: Option<u64>,
    pub last_activity_unix: u64,
    pub limits: SessionLimits,
}

impl Session {
    pub fn begin(
        local: Identity,
        peer: PeerIdentity,
        protocol: ProtocolVersion,
        limits: SessionLimits,
    ) -> Result<Self, SessionError> {
        limits.validate()?;
        if peer.identity.name.trim().is_empty() {
            return Err(SessionError::InvalidPeer("peer identity name is empty"));
        }

        let now = unix_now();
        Ok(Self {
            id: Uuid::new_v4(),
            local,
            peer,
            protocol,
            state: SessionState::Handshaking,
            authenticated: false,
            encrypted: false,
            established_at_unix: None,
            last_activity_unix: now,
            limits,
        })
    }

    pub fn authenticate(&mut self, encrypted: bool) -> Result<(), SessionError> {
        if self.state != SessionState::Handshaking {
            return Err(SessionError::InvalidTransition {
                from: self.state,
                operation: "authenticate",
            });
        }
        if !encrypted {
            return Err(SessionError::SecurityRequirement(
                "authenticated sessions must be encrypted",
            ));
        }
        self.authenticated = true;
        self.encrypted = true;
        self.state = SessionState::Established;
        let now = unix_now();
        self.established_at_unix = Some(now);
        self.last_activity_unix = now;
        Ok(())
    }

    pub fn touch(&mut self) -> Result<(), SessionError> {
        if self.state != SessionState::Established {
            return Err(SessionError::InvalidTransition {
                from: self.state,
                operation: "touch",
            });
        }
        self.last_activity_unix = unix_now();
        Ok(())
    }

    pub fn begin_close(&mut self) -> Result<(), SessionError> {
        match self.state {
            SessionState::Established => {
                self.state = SessionState::Closing;
                Ok(())
            }
            SessionState::Closing | SessionState::Closed => Ok(()),
            SessionState::Handshaking => Err(SessionError::InvalidTransition {
                from: self.state,
                operation: "begin_close",
            }),
        }
    }

    pub fn close(&mut self) {
        self.state = SessionState::Closed;
        self.authenticated = false;
    }

    pub fn is_idle(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.last_activity_unix) >= self.limits.idle_timeout.as_secs()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("invalid session limits: {0}")]
    InvalidLimits(&'static str),
    #[error("invalid peer: {0}")]
    InvalidPeer(&'static str),
    #[error("invalid session transition from {from:?} during {operation}")]
    InvalidTransition {
        from: SessionState,
        operation: &'static str,
    },
    #[error("security requirement failed: {0}")]
    SecurityRequirement(&'static str),
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Endpoint, NetworkAddress, Transport};

    fn peer() -> PeerIdentity {
        PeerIdentity {
            identity: Identity::new("peer"),
            node_id: Uuid::new_v4(),
            addresses: vec![NetworkAddress::new(
                Transport::Tcp,
                Endpoint::new("127.0.0.1", 8443).expect("endpoint"),
            )],
        }
    }

    #[test]
    fn authentication_establishes_encrypted_session() {
        let mut session = Session::begin(
            Identity::new("local"),
            peer(),
            ProtocolVersion::CURRENT,
            SessionLimits::default(),
        )
        .expect("session");
        session.authenticate(true).expect("authentication");
        assert_eq!(session.state, SessionState::Established);
        assert!(session.authenticated);
        assert!(session.encrypted);
        assert!(session.established_at_unix.is_some());
    }

    #[test]
    fn plaintext_authentication_is_rejected() {
        let mut session = Session::begin(
            Identity::new("local"),
            peer(),
            ProtocolVersion::CURRENT,
            SessionLimits::default(),
        )
        .expect("session");
        assert_eq!(
            session.authenticate(false),
            Err(SessionError::SecurityRequirement(
                "authenticated sessions must be encrypted"
            ))
        );
    }

    #[test]
    fn session_lifecycle_is_bounded() {
        let mut session = Session::begin(
            Identity::new("local"),
            peer(),
            ProtocolVersion::CURRENT,
            SessionLimits::default(),
        )
        .expect("session");
        session.authenticate(true).expect("authentication");
        session.begin_close().expect("close start");
        session.close();
        assert_eq!(session.state, SessionState::Closed);
        assert!(!session.authenticated);
    }
}
