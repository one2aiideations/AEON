//! Short-lived, audience-bound authorisation envelopes: the handoff from the authority to a
//! consumer such as AgentBox (see `docs/MOULIONE_AGENTBOX_BOUNDARY` in the C tree).
//!
//! An envelope binds subject, tenant, org node, scopes, audience, decision id, correlation id,
//! issue and expiry times and a unique token id, and is signed with HMAC-SHA256.
//!
//! Deviation from the boundary document, stated plainly: MOULIONE's real contract is JWKS-style
//! (asymmetric) and was not available to read. HMAC means the verifier holds the signing key, so
//! this suits one trust domain (the local machine, one deployment) and must not be exposed to
//! parties that should only verify. Swapping in an asymmetric scheme needs a reviewed crypto
//! dependency, which the port rules forbid adding silently.

use mouli_interface::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Longest lifetime an envelope may have, in seconds.
pub const MAX_TTL_SECS: u64 = 900;
/// Shortest accepted signing key.
pub const MIN_KEY_LEN: usize = 32;

/// What an envelope asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    /// Principal the decision was made for.
    pub subject: String,
    /// Tenant.
    pub tenant: String,
    /// Company, plant or other org node the decision covers.
    pub node: String,
    /// Permitted actions.
    pub scopes: Vec<String>,
    /// The only service that may accept it.
    pub audience: String,
    /// Authorisation decision id, for audit correlation and revocation.
    pub decision_id: String,
    /// Request correlation id.
    pub correlation_id: String,
    /// Unique token id, spent once.
    pub token_id: String,
    /// Issue time (seconds).
    pub issued_at: u64,
    /// Expiry time (seconds).
    pub expires_at: u64,
}

/// HMAC-SHA256 (RFC 2104).
#[must_use]
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn field(name: &str, v: &str) -> Result<()> {
    if v.is_empty() || v.contains('\n') {
        return Err(Error::Invalid(format!(
            "claim {name} must be non-empty and single-line"
        )));
    }
    Ok(())
}

fn canonical(c: &Claims) -> Result<String> {
    for (n, v) in [
        ("sub", &c.subject),
        ("ten", &c.tenant),
        ("node", &c.node),
        ("aud", &c.audience),
        ("dec", &c.decision_id),
        ("cor", &c.correlation_id),
        ("jti", &c.token_id),
    ] {
        field(n, v)?;
    }
    if c.scopes.is_empty()
        || c.scopes
            .iter()
            .any(|s| s.is_empty() || s.contains([',', '\n']))
    {
        return Err(Error::Invalid(
            "scopes must be non-empty names without commas".into(),
        ));
    }
    Ok(format!(
        "sub={}\nten={}\nnode={}\nscp={}\naud={}\ndec={}\ncor={}\njti={}\niat={}\nexp={}",
        c.subject,
        c.tenant,
        c.node,
        c.scopes.join(","),
        c.audience,
        c.decision_id,
        c.correlation_id,
        c.token_id,
        c.issued_at,
        c.expires_at
    ))
}

fn parse(text: &str) -> Option<Claims> {
    let m: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
    let g = |k: &str| m.get(k).map(|v| (*v).to_string());
    Some(Claims {
        subject: g("sub")?,
        tenant: g("ten")?,
        node: g("node")?,
        scopes: g("scp")?.split(',').map(str::to_string).collect(),
        audience: g("aud")?,
        decision_id: g("dec")?,
        correlation_id: g("cor")?,
        token_id: g("jti")?,
        issued_at: g("iat")?.parse().ok()?,
        expires_at: g("exp")?.parse().ok()?,
    })
}

/// Sign claims into a token `v1.<claims hex>.<mac hex>`.
///
/// # Errors
/// [`Error::Invalid`] for a short key, bad fields, or a lifetime that is zero or above
/// [`MAX_TTL_SECS`].
pub fn sign(key: &[u8], c: &Claims) -> Result<String> {
    if key.len() < MIN_KEY_LEN {
        return Err(Error::Invalid(format!(
            "signing key must be at least {MIN_KEY_LEN} bytes"
        )));
    }
    if c.expires_at <= c.issued_at || c.expires_at - c.issued_at > MAX_TTL_SECS {
        return Err(Error::Invalid(format!(
            "lifetime must be 1..={MAX_TTL_SECS} seconds"
        )));
    }
    let text = canonical(c)?;
    Ok(format!(
        "v1.{}.{}",
        hex(text.as_bytes()),
        hex(&hmac_sha256(key, text.as_bytes()))
    ))
}

/// Consumer-side verifier: one audience, replay protection and revocation.
pub struct Verifier {
    key: Vec<u8>,
    audience: String,
    used: BTreeMap<String, u64>,
    revoked: BTreeSet<String>,
}

impl Verifier {
    /// Create a verifier that accepts only tokens for `audience`.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a short key or blank audience.
    pub fn new(key: &[u8], audience: &str) -> Result<Self> {
        if key.len() < MIN_KEY_LEN || audience.is_empty() {
            return Err(Error::Invalid(
                "a 32-byte key and an audience are required".into(),
            ));
        }
        Ok(Self {
            key: key.to_vec(),
            audience: audience.into(),
            used: BTreeMap::new(),
            revoked: BTreeSet::new(),
        })
    }

    /// Refuse every envelope issued for this decision from now on.
    pub fn revoke(&mut self, decision_id: &str) {
        self.revoked.insert(decision_id.into());
    }

    /// Verify a token for `tenant` and `scope` at time `now`, and spend its id.
    ///
    /// # Errors
    /// [`Error::Denied`] for a bad signature, wrong audience, tenant or scope, expiry, or
    /// revocation; [`Error::Invalid`] for malformed tokens; [`Error::Conflict`] for a replay.
    pub fn verify(&mut self, token: &str, now: u64, tenant: &str, scope: &str) -> Result<Claims> {
        let mut parts = token.split('.');
        let (Some("v1"), Some(body), Some(mac), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Invalid("malformed envelope".into()));
        };
        let (Some(body), Some(mac)) = (unhex(body), unhex(mac)) else {
            return Err(Error::Invalid("malformed envelope encoding".into()));
        };
        if !ct_eq(&hmac_sha256(&self.key, &body), &mac) {
            return Err(Error::Denied("bad envelope signature".into()));
        }
        let claims = String::from_utf8(body)
            .ok()
            .and_then(|t| parse(&t))
            .ok_or_else(|| Error::Invalid("malformed envelope claims".into()))?;
        if claims.audience != self.audience {
            return Err(Error::Denied("envelope is for another audience".into()));
        }
        if now < claims.issued_at || now >= claims.expires_at {
            return Err(Error::Denied("envelope is expired or not yet valid".into()));
        }
        if claims.expires_at - claims.issued_at > MAX_TTL_SECS {
            return Err(Error::Denied(
                "envelope lifetime exceeds the maximum".into(),
            ));
        }
        if claims.tenant != tenant {
            return Err(Error::Denied("envelope is for another tenant".into()));
        }
        if !claims.scopes.iter().any(|s| s == scope) {
            return Err(Error::Denied(format!("envelope does not grant {scope}")));
        }
        if self.revoked.contains(&claims.decision_id) {
            return Err(Error::Denied(
                "the authorisation decision was revoked".into(),
            ));
        }
        self.used.retain(|_, exp| *exp > now);
        if self.used.contains_key(&claims.token_id) {
            return Err(Error::Conflict("envelope was already used".into()));
        }
        self.used.insert(claims.token_id.clone(), claims.expires_at);
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn claims() -> Claims {
        Claims {
            subject: "ann".into(),
            tenant: "acme".into(),
            node: "pune".into(),
            scopes: vec!["agent.run".into()],
            audience: "agentbox".into(),
            decision_id: "dec-1".into(),
            correlation_id: "req-9".into(),
            token_id: "jti-1".into(),
            issued_at: 1000,
            expires_at: 1300,
        }
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let long = [0xaa; 131];
        assert_eq!(
            hex(&hmac_sha256(
                &long,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn round_trip_then_replay_is_refused() {
        let t = sign(KEY, &claims()).unwrap();
        let mut v = Verifier::new(KEY, "agentbox").unwrap();
        assert_eq!(v.verify(&t, 1100, "acme", "agent.run").unwrap(), claims());
        assert!(matches!(
            v.verify(&t, 1101, "acme", "agent.run"),
            Err(Error::Conflict(_))
        ));
    }

    #[test]
    fn every_binding_is_enforced() {
        let t = sign(KEY, &claims()).unwrap();
        let fresh = || Verifier::new(KEY, "agentbox").unwrap();
        assert!(
            matches!(
                fresh().verify(&t, 1100, "beta", "agent.run"),
                Err(Error::Denied(_))
            ),
            "tenant"
        );
        assert!(
            matches!(
                fresh().verify(&t, 1100, "acme", "agent.delete"),
                Err(Error::Denied(_))
            ),
            "scope"
        );
        assert!(
            matches!(
                Verifier::new(KEY, "other")
                    .unwrap()
                    .verify(&t, 1100, "acme", "agent.run"),
                Err(Error::Denied(_))
            ),
            "audience"
        );
        assert!(
            matches!(
                fresh().verify(&t, 1300, "acme", "agent.run"),
                Err(Error::Denied(_))
            ),
            "expired"
        );
        assert!(
            matches!(
                fresh().verify(&t, 999, "acme", "agent.run"),
                Err(Error::Denied(_))
            ),
            "not yet valid"
        );
        let mut rev = fresh();
        rev.revoke("dec-1");
        assert!(
            matches!(
                rev.verify(&t, 1100, "acme", "agent.run"),
                Err(Error::Denied(_))
            ),
            "revoked"
        );
        let other = Verifier::new(b"a-different-key-of-32-bytes-long!", "agentbox").unwrap();
        assert!(
            matches!(
                {
                    let mut o = other;
                    o.verify(&t, 1100, "acme", "agent.run")
                },
                Err(Error::Denied(_))
            ),
            "wrong key"
        );
    }

    #[test]
    fn tampering_and_malformed_tokens() {
        let t = sign(KEY, &claims()).unwrap();
        let mut v = Verifier::new(KEY, "agentbox").unwrap();
        let parts: Vec<&str> = t.split('.').collect();
        let forged = claims_with(|c| c.tenant = "beta".into());
        let fparts: Vec<String> = forged.split('.').map(str::to_string).collect();
        let spliced = format!("v1.{}.{}", fparts[1], parts[2]);
        assert!(matches!(
            v.verify(&spliced, 1100, "beta", "agent.run"),
            Err(Error::Denied(_))
        ));
        for bad in [
            "",
            "v1.zz.zz",
            "v2.aa.bb",
            "v1.aa",
            "v1.aa.bb.cc",
            "v1.abc.def",
        ] {
            assert!(
                matches!(
                    v.verify(bad, 1100, "acme", "agent.run"),
                    Err(Error::Invalid(_))
                ),
                "{bad}"
            );
        }
    }

    fn claims_with(f: impl FnOnce(&mut Claims)) -> String {
        let mut c = claims();
        f(&mut c);
        sign(KEY, &c).unwrap()
    }

    #[test]
    fn signing_rules() {
        assert!(sign(b"short", &claims()).is_err());
        let mut c = claims();
        c.expires_at = c.issued_at + MAX_TTL_SECS + 1;
        assert!(sign(KEY, &c).is_err());
        c.expires_at = c.issued_at;
        assert!(sign(KEY, &c).is_err());
        let mut c = claims();
        c.subject = "a\nb".into();
        assert!(sign(KEY, &c).is_err());
        let mut c = claims();
        c.scopes = vec![];
        assert!(sign(KEY, &c).is_err());
        assert!(Verifier::new(b"short", "x").is_err() && Verifier::new(KEY, "").is_err());
    }

    #[test]
    fn spent_ids_are_pruned_after_expiry() {
        let mut v = Verifier::new(KEY, "agentbox").unwrap();
        v.verify(&sign(KEY, &claims()).unwrap(), 1100, "acme", "agent.run")
            .unwrap();
        let mut later = claims();
        later.token_id = "jti-2".into();
        later.issued_at = 5000;
        later.expires_at = 5100;
        v.verify(&sign(KEY, &later).unwrap(), 5001, "acme", "agent.run")
            .unwrap();
        assert_eq!(v.used.len(), 1);
    }
}
