use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, NetworkError> {
        let host = host.into();
        if host.trim().is_empty() {
            return Err(NetworkError::InvalidEndpoint("host is empty".into()));
        }
        if host.len() > 253 {
            return Err(NetworkError::InvalidEndpoint("host is too long".into()));
        }
        Ok(Self { host, port })
    }

    pub fn socket_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.to_socket_addr()
    }
}

impl FromStr for Endpoint {
    type Err = NetworkError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let addr: SocketAddr = value
            .parse()
            .map_err(|_| NetworkError::InvalidEndpoint(value.into()))?;
        Self::new(addr.ip().to_string(), addr.port())
    }
}

impl Endpoint {
    fn to_socket_addr(&self) -> Result<SocketAddr, NetworkError> {
        let ip: IpAddr = self
            .host
            .parse()
            .map_err(|_| NetworkError::NonIpHost(self.host.clone()))?;
        Ok(SocketAddr::new(ip, self.port))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    Tcp,
    Quic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAddress {
    pub transport: Transport,
    pub endpoint: Endpoint,
    pub metadata: BTreeMap<String, String>,
}

impl NetworkAddress {
    pub fn new(transport: Transport, endpoint: Endpoint) -> Self {
        Self {
            transport,
            endpoint,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NetworkError {
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("hostname is not a literal IP address: {0}")]
    NonIpHost(String),
    #[error("invalid network identity: {0}")]
    InvalidIdentity(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkIdentity {
    pub node_id: uuid::Uuid,
    pub addresses: Vec<NetworkAddress>,
}

impl NetworkIdentity {
    pub fn new(node_id: uuid::Uuid, addresses: Vec<NetworkAddress>) -> Result<Self, NetworkError> {
        if addresses.is_empty() {
            return Err(NetworkError::InvalidIdentity(
                "at least one address is required".into(),
            ));
        }
        Ok(Self { node_id, addresses })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn parses_literal_socket_endpoint() {
        let endpoint: Endpoint = "127.0.0.1:8443".parse().expect("valid endpoint");
        assert_eq!(endpoint.socket_addr().expect("socket address").port(), 8443);
    }

    #[test]
    fn rejects_empty_host() {
        assert!(matches!(
            Endpoint::new("", 1),
            Err(NetworkError::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn network_identity_requires_address() {
        assert!(matches!(
            NetworkIdentity::new(Uuid::new_v4(), vec![]),
            Err(NetworkError::InvalidIdentity(_))
        ));
    }
}
