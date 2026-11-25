//! Stateful, tenant-bound replication protocol session coordinator.
//!
//! The session validates the M049 message sequence, negotiates protocol compatibility,
//! correlates fetch responses, and emits typed events for a transport/application layer.
//! It does not perform I/O, authentication, encryption, or durable state management.

use std::collections::BTreeSet;

use crate::wire::Message;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionConfig {
    pub tenant_id: String,
    pub local_replica_id: String,
    pub protocol_min: u8,
    pub protocol_max: u8,
    pub max_pending_requests: usize,
    pub max_inbound_requests: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    AwaitingHello,
    Established,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionError {
    InvalidConfig,
    Closed,
    HelloRequired,
    DuplicateHello,
    TenantMismatch,
    PeerIdentityMismatch,
    IncompatibleProtocol,
    InvalidRequestId,
    DuplicateRequest,
    RequestCapacity,
    UnknownRequest,
    InvalidSequence,
    InvalidMessageDirection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    Established {
        peer_replica_id: String,
        negotiated_version: u8,
    },
    InventoryReceived {
        replica_id: String,
        high_watermark: u64,
        digest: [u8; 32],
    },
    FetchRequested {
        request_id: u64,
        after_sequence: u64,
        max_records: u32,
        max_bytes: u64,
    },
    RecordsReceived {
        request_id: u64,
        first_sequence: u64,
        records: Vec<crate::wire::MemoryRecord>,
    },
    Acknowledged {
        request_id: u64,
        through_sequence: u64,
    },
    Rejected {
        request_id: u64,
        code: u16,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicationSession {
    config: SessionConfig,
    phase: SessionPhase,
    peer_replica_id: Option<String>,
    negotiated_version: Option<u8>,
    pending_outbound: BTreeSet<u64>,
    pending_inbound: BTreeSet<u64>,
    last_inventory_watermark: Option<u64>,
}

impl ReplicationSession {
    pub fn new(config: SessionConfig) -> Result<Self, SessionError> {
        if config.tenant_id.trim().is_empty()
            || config.local_replica_id.trim().is_empty()
            || config.protocol_min > config.protocol_max
            || config.max_pending_requests == 0
            || config.max_inbound_requests == 0
        {
            return Err(SessionError::InvalidConfig);
        }

        Ok(Self {
            config,
            phase: SessionPhase::AwaitingHello,
            peer_replica_id: None,
            negotiated_version: None,
            pending_outbound: BTreeSet::new(),
            pending_inbound: BTreeSet::new(),
            last_inventory_watermark: None,
        })
    }

    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    pub fn peer_replica_id(&self) -> Option<&str> {
        self.peer_replica_id.as_deref()
    }

    pub fn negotiated_version(&self) -> Option<u8> {
        self.negotiated_version
    }

    pub fn pending_outbound_count(&self) -> usize {
        self.pending_outbound.len()
    }

    pub fn pending_inbound_count(&self) -> usize {
        self.pending_inbound.len()
    }

    /// Produces the local handshake message. The caller sends it over its transport.
    pub fn hello(&self) -> Result<Message, SessionError> {
        if self.phase == SessionPhase::Closed {
            return Err(SessionError::Closed);
        }
        Ok(Message::Hello {
            tenant_id: self.config.tenant_id.clone(),
            replica_id: self.config.local_replica_id.clone(),
            protocol_min: self.config.protocol_min,
            protocol_max: self.config.protocol_max,
        })
    }

    /// Registers a fetch before sending it, so a response can be correlated safely.
    pub fn begin_fetch(
        &mut self,
        request_id: u64,
        after_sequence: u64,
        max_records: u32,
        max_bytes: u64,
    ) -> Result<Message, SessionError> {
        self.ensure_established()?;
        if request_id == 0 || max_records == 0 || max_bytes == 0 {
            return Err(SessionError::InvalidRequestId);
        }
        if self.pending_outbound.contains(&request_id) || self.pending_inbound.contains(&request_id)
        {
            return Err(SessionError::DuplicateRequest);
        }
        if self.pending_outbound.len() >= self.config.max_pending_requests {
            return Err(SessionError::RequestCapacity);
        }
        self.pending_outbound.insert(request_id);
        Ok(Message::Fetch {
            tenant_id: self.config.tenant_id.clone(),
            request_id,
            after_sequence,
            max_records,
            max_bytes,
        })
    }

    /// Acknowledges a received records batch and clears its inbound request correlation.
    pub fn acknowledge_records(
        &mut self,
        request_id: u64,
        through_sequence: u64,
    ) -> Result<Message, SessionError> {
        self.ensure_established()?;
        if !self.pending_inbound.remove(&request_id) {
            return Err(SessionError::UnknownRequest);
        }
        Ok(Message::Acknowledge {
            tenant_id: self.config.tenant_id.clone(),
            request_id,
            through_sequence,
        })
    }

    /// Validates an inbound message and returns the application event it represents.
    pub fn receive(&mut self, message: Message) -> Result<SessionEvent, SessionError> {
        if self.phase == SessionPhase::Closed {
            return Err(SessionError::Closed);
        }

        match message {
            Message::Hello {
                tenant_id,
                replica_id,
                protocol_min,
                protocol_max,
            } => {
                if self.phase != SessionPhase::AwaitingHello {
                    return Err(SessionError::DuplicateHello);
                }
                if tenant_id != self.config.tenant_id {
                    return Err(SessionError::TenantMismatch);
                }
                if replica_id.trim().is_empty() || replica_id == self.config.local_replica_id {
                    return Err(SessionError::PeerIdentityMismatch);
                }
                if protocol_min > protocol_max {
                    return Err(SessionError::IncompatibleProtocol);
                }
                let low = self.config.protocol_min.max(protocol_min);
                let high = self.config.protocol_max.min(protocol_max);
                if low > high {
                    return Err(SessionError::IncompatibleProtocol);
                }
                let negotiated = high;
                self.peer_replica_id = Some(replica_id.clone());
                self.negotiated_version = Some(negotiated);
                self.phase = SessionPhase::Established;
                Ok(SessionEvent::Established {
                    peer_replica_id: replica_id,
                    negotiated_version: negotiated,
                })
            }
            other => {
                self.ensure_established()?;
                match other {
                    Message::Inventory {
                        tenant_id,
                        replica_id,
                        high_watermark,
                        inventory_digest,
                    } => {
                        self.validate_tenant(&tenant_id)?;
                        self.validate_peer(&replica_id)?;
                        if let Some(previous) = self.last_inventory_watermark {
                            if high_watermark < previous {
                                return Err(SessionError::InvalidSequence);
                            }
                        }
                        self.last_inventory_watermark = Some(high_watermark);
                        Ok(SessionEvent::InventoryReceived {
                            replica_id,
                            high_watermark,
                            digest: inventory_digest,
                        })
                    }
                    Message::Fetch {
                        tenant_id,
                        request_id,
                        after_sequence,
                        max_records,
                        max_bytes,
                    } => {
                        self.validate_tenant(&tenant_id)?;
                        if request_id == 0 || max_records == 0 || max_bytes == 0 {
                            return Err(SessionError::InvalidRequestId);
                        }
                        if self.pending_inbound.contains(&request_id)
                            || self.pending_outbound.contains(&request_id)
                        {
                            return Err(SessionError::DuplicateRequest);
                        }
                        if self.pending_inbound.len() >= self.config.max_inbound_requests {
                            return Err(SessionError::RequestCapacity);
                        }
                        self.pending_inbound.insert(request_id);
                        Ok(SessionEvent::FetchRequested {
                            request_id,
                            after_sequence,
                            max_records,
                            max_bytes,
                        })
                    }
                    Message::Records {
                        tenant_id,
                        request_id,
                        first_sequence,
                        records,
                    } => {
                        self.validate_tenant(&tenant_id)?;
                        if !self.pending_outbound.remove(&request_id) {
                            return Err(SessionError::UnknownRequest);
                        }
                        if !records.is_empty() && first_sequence == 0 {
                            return Err(SessionError::InvalidSequence);
                        }
                        Ok(SessionEvent::RecordsReceived {
                            request_id,
                            first_sequence,
                            records,
                        })
                    }
                    Message::Acknowledge {
                        tenant_id,
                        request_id,
                        through_sequence,
                    } => {
                        self.validate_tenant(&tenant_id)?;
                        if !self.pending_inbound.remove(&request_id) {
                            return Err(SessionError::UnknownRequest);
                        }
                        Ok(SessionEvent::Acknowledged {
                            request_id,
                            through_sequence,
                        })
                    }
                    Message::Reject {
                        tenant_id,
                        request_id,
                        code,
                        message,
                    } => {
                        self.validate_tenant(&tenant_id)?;
                        let removed_outbound = self.pending_outbound.remove(&request_id);
                        let removed_inbound = self.pending_inbound.remove(&request_id);
                        if !removed_outbound && !removed_inbound {
                            return Err(SessionError::UnknownRequest);
                        }
                        Ok(SessionEvent::Rejected {
                            request_id,
                            code,
                            message,
                        })
                    }
                    Message::Hello { .. } => Err(SessionError::DuplicateHello),
                }
            }
        }
    }

    /// Closes the session and clears all volatile request correlations.
    pub fn close(&mut self) {
        self.phase = SessionPhase::Closed;
        self.pending_outbound.clear();
        self.pending_inbound.clear();
    }

    fn ensure_established(&self) -> Result<(), SessionError> {
        match self.phase {
            SessionPhase::AwaitingHello => Err(SessionError::HelloRequired),
            SessionPhase::Established => Ok(()),
            SessionPhase::Closed => Err(SessionError::Closed),
        }
    }

    fn validate_tenant(&self, tenant_id: &str) -> Result<(), SessionError> {
        if tenant_id == self.config.tenant_id {
            Ok(())
        } else {
            Err(SessionError::TenantMismatch)
        }
    }

    fn validate_peer(&self, replica_id: &str) -> Result<(), SessionError> {
        if self.peer_replica_id.as_deref() == Some(replica_id) {
            Ok(())
        } else {
            Err(SessionError::PeerIdentityMismatch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(replica: &str) -> SessionConfig {
        SessionConfig {
            tenant_id: "tenant-a".to_owned(),
            local_replica_id: replica.to_owned(),
            protocol_min: 1,
            protocol_max: 3,
            max_pending_requests: 8,
            max_inbound_requests: 8,
        }
    }

    fn established() -> ReplicationSession {
        let mut session = match ReplicationSession::new(config("local")) {
            Ok(value) => value,
            Err(_) => panic!("test config should be valid"),
        };
        let event = session.receive(Message::Hello {
            tenant_id: "tenant-a".to_owned(),
            replica_id: "remote".to_owned(),
            protocol_min: 2,
            protocol_max: 4,
        });
        assert_eq!(
            event,
            Ok(SessionEvent::Established {
                peer_replica_id: "remote".to_owned(),
                negotiated_version: 3,
            })
        );
        session
    }

    #[test]
    fn negotiates_highest_common_protocol_version() {
        let session = established();
        assert_eq!(session.phase(), SessionPhase::Established);
        assert_eq!(session.negotiated_version(), Some(3));
        assert_eq!(session.peer_replica_id(), Some("remote"));
    }

    #[test]
    fn correlates_fetch_records_and_acknowledgement() {
        let mut session = established();
        let fetch = session.begin_fetch(7, 20, 10, 4096);
        assert!(matches!(fetch, Ok(Message::Fetch { request_id: 7, .. })));
        let records = session.receive(Message::Records {
            tenant_id: "tenant-a".to_owned(),
            request_id: 7,
            first_sequence: 21,
            records: Vec::new(),
        });
        assert!(matches!(
            records,
            Ok(SessionEvent::RecordsReceived { request_id: 7, .. })
        ));
        assert_eq!(session.pending_outbound_count(), 0);
    }

    #[test]
    fn rejects_wrong_tenant_and_unknown_response() {
        let mut session = established();
        assert_eq!(
            session.receive(Message::Inventory {
                tenant_id: "tenant-b".to_owned(),
                replica_id: "remote".to_owned(),
                high_watermark: 1,
                inventory_digest: [0; 32],
            }),
            Err(SessionError::TenantMismatch)
        );
        assert_eq!(
            session.receive(Message::Acknowledge {
                tenant_id: "tenant-a".to_owned(),
                request_id: 99,
                through_sequence: 1,
            }),
            Err(SessionError::UnknownRequest)
        );
    }

    #[test]
    fn tracks_inbound_fetch_until_acknowledged() {
        let mut session = established();
        let event = session.receive(Message::Fetch {
            tenant_id: "tenant-a".to_owned(),
            request_id: 4,
            after_sequence: 10,
            max_records: 5,
            max_bytes: 1000,
        });
        assert!(matches!(
            event,
            Ok(SessionEvent::FetchRequested { request_id: 4, .. })
        ));
        assert_eq!(session.pending_inbound_count(), 1);
        let ack = session.acknowledge_records(4, 15);
        assert!(matches!(
            ack,
            Ok(Message::Acknowledge {
                through_sequence: 15,
                ..
            })
        ));
        assert_eq!(session.pending_inbound_count(), 0);
    }

    #[test]
    fn receives_ack_for_records_served_to_peer() {
        let mut session = established();
        let request = session.receive(Message::Fetch {
            tenant_id: "tenant-a".to_owned(),
            request_id: 22,
            after_sequence: 3,
            max_records: 4,
            max_bytes: 2048,
        });
        assert!(matches!(
            request,
            Ok(SessionEvent::FetchRequested { request_id: 22, .. })
        ));
        let ack = session.receive(Message::Acknowledge {
            tenant_id: "tenant-a".to_owned(),
            request_id: 22,
            through_sequence: 8,
        });
        assert_eq!(
            ack,
            Ok(SessionEvent::Acknowledged {
                request_id: 22,
                through_sequence: 8,
            })
        );
        assert_eq!(session.pending_inbound_count(), 0);
    }

    #[test]
    fn rejects_regressing_inventory_and_closes_cleanly() {
        let mut session = established();
        let first = Message::Inventory {
            tenant_id: "tenant-a".to_owned(),
            replica_id: "remote".to_owned(),
            high_watermark: 10,
            inventory_digest: [1; 32],
        };
        assert!(session.receive(first).is_ok());
        let second = Message::Inventory {
            tenant_id: "tenant-a".to_owned(),
            replica_id: "remote".to_owned(),
            high_watermark: 9,
            inventory_digest: [2; 32],
        };
        assert_eq!(session.receive(second), Err(SessionError::InvalidSequence));
        session.close();
        assert_eq!(session.phase(), SessionPhase::Closed);
        assert_eq!(session.begin_fetch(1, 0, 1, 1), Err(SessionError::Closed));
    }
}
