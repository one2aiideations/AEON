//! Server certificate chain verification and host name matching.

use crate::pem;
use crate::x509::Certificate;

/// What the connection is supposed to be talking to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    /// A DNS name, such as `api.example.com`.
    Dns(String),
    /// An IPv4 (4 bytes) or IPv6 (16 bytes) address.
    Ip(Vec<u8>),
}

impl Host {
    /// Interpret text as an IPv4 address or a DNS name.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let parts: Vec<&str> = text.split('.').collect();
        if parts.len() == 4 {
            let octets: Option<Vec<u8>> = parts.iter().map(|p| p.parse().ok()).collect();
            if let Some(o) = octets {
                return Self::Ip(o);
            }
        }
        Self::Dns(text.trim_end_matches('.').to_ascii_lowercase())
    }
}

/// Why a chain was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// Nothing was presented.
    EmptyChain,
    /// A certificate could not be parsed.
    Parse(String),
    /// A certificate is not valid yet or has expired.
    NotValidNow(String),
    /// No trusted root (or no usable intermediate) issued this certificate.
    UnknownIssuer,
    /// An issuer was found but its signature does not check out.
    BadSignature,
    /// An issuing certificate is not allowed to be a CA.
    NotACa,
    /// A CA's path length limit was exceeded.
    PathTooLong,
    /// The leaf is not for TLS server authentication.
    WrongPurpose,
    /// The certificate does not name the host.
    NameMismatch,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyChain => write!(f, "the server sent no certificate"),
            Self::Parse(e) => write!(f, "unreadable certificate: {e}"),
            Self::NotValidNow(w) => write!(f, "certificate {w}"),
            Self::UnknownIssuer => write!(f, "certificate issuer is not trusted"),
            Self::BadSignature => write!(f, "certificate signature is wrong"),
            Self::NotACa => write!(f, "an issuing certificate is not a CA"),
            Self::PathTooLong => write!(f, "certificate chain is longer than allowed"),
            Self::WrongPurpose => write!(f, "certificate is not for a TLS server"),
            Self::NameMismatch => write!(f, "certificate does not match the host name"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// The set of trusted root certificates.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    roots: Vec<Certificate>,
}

impl TrustStore {
    /// An empty store: nothing will verify.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a root from DER.
    ///
    /// # Errors
    /// The parse error.
    pub fn add_der(&mut self, der: &[u8]) -> Result<(), crate::Error> {
        self.roots.push(Certificate::parse(der)?);
        Ok(())
    }

    /// Load every certificate in a PEM bundle. Returns the store and how many entries were
    /// skipped because this client cannot use them (unsupported algorithms, malformed).
    #[must_use]
    pub fn from_pem(text: &str) -> (Self, usize) {
        let mut store = Self::new();
        let mut skipped = 0;
        for der in pem::blocks(text, "CERTIFICATE") {
            if store.add_der(&der).is_err() {
                skipped += 1;
            }
        }
        (store, skipped)
    }

    /// Number of trusted roots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

const MAX_CHAIN: usize = 8;

fn check_time(c: &Certificate, now: i64, what: &str) -> Result<(), VerifyError> {
    if now < c.not_before {
        return Err(VerifyError::NotValidNow(format!("{what} is not valid yet")));
    }
    if now > c.not_after {
        return Err(VerifyError::NotValidNow(format!("{what} has expired")));
    }
    Ok(())
}

/// Verify a server's certificate chain (leaf first, as sent in TLS) for `host` at time `now`
/// (seconds since the Unix epoch). On success returns the leaf certificate, whose key the
/// handshake signature must then be checked against.
///
/// # Errors
/// The first reason the chain or name is unacceptable.
pub fn verify_server(
    chain: &[Vec<u8>],
    roots: &TrustStore,
    host: &Host,
    now: i64,
) -> Result<Certificate, VerifyError> {
    if chain.is_empty() {
        return Err(VerifyError::EmptyChain);
    }
    if chain.len() > MAX_CHAIN {
        return Err(VerifyError::PathTooLong);
    }
    let certs: Vec<Certificate> = chain
        .iter()
        .map(|d| Certificate::parse(d).map_err(|e| VerifyError::Parse(e.to_string())))
        .collect::<Result<_, _>>()?;
    let leaf = &certs[0];
    check_time(leaf, now, "the server certificate")?;
    if !leaf.allows_server_auth() {
        return Err(VerifyError::WrongPurpose);
    }
    if !matches_host(leaf, host) {
        return Err(VerifyError::NameMismatch);
    }
    let mut used = vec![false; certs.len()];
    used[0] = true;
    let mut current = leaf;
    // CAs passed so far between the leaf and `current` (not counting `current`).
    let mut below = 0u32;
    let mut saw_bad_signature = false;
    loop {
        // A trusted root that issued `current` ends the path.
        for root in roots.roots.iter().filter(|r| r.subject == current.issuer) {
            if !current.signed_by(&root.key) {
                saw_bad_signature = true;
                continue;
            }
            check_time(root, now, "a trusted root")?;
            if !root.is_ca() {
                return Err(VerifyError::NotACa);
            }
            if root
                .basic_constraints
                .and_then(|b| b.path_len)
                .is_some_and(|p| below > p)
            {
                return Err(VerifyError::PathTooLong);
            }
            return Ok(leaf.clone());
        }
        // Otherwise climb through an intermediate the server sent.
        let next = certs
            .iter()
            .enumerate()
            .find(|(i, c)| !used[*i] && c.subject == current.issuer && current.signed_by(&c.key));
        let Some((i, issuer)) = next else {
            return Err(
                if saw_bad_signature
                    || certs
                        .iter()
                        .any(|c| c.subject == current.issuer && !current.signed_by(&c.key))
                {
                    VerifyError::BadSignature
                } else {
                    VerifyError::UnknownIssuer
                },
            );
        };
        used[i] = true;
        check_time(issuer, now, "an intermediate certificate")?;
        if !issuer.is_ca() {
            return Err(VerifyError::NotACa);
        }
        if issuer
            .basic_constraints
            .and_then(|b| b.path_len)
            .is_some_and(|p| below > p)
        {
            return Err(VerifyError::PathTooLong);
        }
        below += 1;
        current = issuer;
    }
}

/// Whether `cert` names `host`. Only the subject alternative names count; the common name is
/// ignored, as modern clients do.
#[must_use]
pub fn matches_host(cert: &Certificate, host: &Host) -> bool {
    match host {
        Host::Ip(ip) => cert.ip_addrs.iter().any(|a| a == ip),
        Host::Dns(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            !name.is_empty() && cert.dns_names.iter().any(|p| dns_matches(p, &name))
        }
    }
}

/// Match a presented DNS name (maybe `*.example.com`) against a lower-case host name.
fn dns_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim_end_matches('.');
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // The wildcard is the whole left-most label; at least two labels must follow it.
        if suffix.contains('*') || !suffix.contains('.') {
            return false;
        }
        return match host.split_once('.') {
            Some((label, rest)) => !label.is_empty() && rest == suffix,
            None => false,
        };
    }
    !pattern.contains('*') && pattern == host
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_rules() {
        assert!(dns_matches("example.com", "example.com"));
        assert!(!dns_matches("example.com", "www.example.com"));
        assert!(dns_matches("*.example.com", "www.example.com"));
        assert!(!dns_matches("*.example.com", "example.com"));
        assert!(!dns_matches("*.example.com", "a.b.example.com"));
        assert!(!dns_matches("*.com", "example.com"));
        assert!(!dns_matches("w*.example.com", "www.example.com"));
        assert!(!dns_matches("*.example.com", ".example.com"));
        assert!(!dns_matches("a.*.example.com", "a.b.example.com"));
    }

    #[test]
    fn host_parsing() {
        assert_eq!(Host::parse("192.0.2.1"), Host::Ip(vec![192, 0, 2, 1]));
        assert_eq!(
            Host::parse("API.Example.com."),
            Host::Dns("api.example.com".into())
        );
        assert_eq!(Host::parse("1.2.3"), Host::Dns("1.2.3".into()));
    }
}
