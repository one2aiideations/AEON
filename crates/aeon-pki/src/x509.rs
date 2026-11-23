//! X.509 v3 certificate parsing: the fields TLS server authentication needs.

use crate::der::{self, tag, Reader};
use crate::ecdsa::{Curve, PublicKey as EcKey};
use crate::rsa::PublicKey as RsaKey;
use crate::{Error, Hash};

/// How a certificate (or handshake) signature is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigAlg {
    /// RSASSA-PKCS1-v1_5 with the hash.
    RsaPkcs1(Hash),
    /// RSASSA-PSS with the hash, MGF1 of the same hash and the salt length.
    RsaPss(Hash, usize),
    /// ECDSA with the hash.
    Ecdsa(Hash),
}

/// A subject public key.
#[derive(Clone, Debug)]
pub enum PublicKey {
    /// RSA.
    Rsa(RsaKey),
    /// ECDSA on P-256 or P-384.
    Ec(EcKey),
}

impl PublicKey {
    /// Verify `signature` over `message` made with `alg`; false when the algorithm does not
    /// match the key type.
    #[must_use]
    pub fn verify(&self, alg: SigAlg, message: &[u8], signature: &[u8]) -> bool {
        match (self, alg) {
            (Self::Rsa(k), SigAlg::RsaPkcs1(h)) => k.verify_pkcs1(h, message, signature),
            (Self::Rsa(k), SigAlg::RsaPss(h, salt)) => k.verify_pss(h, salt, message, signature),
            (Self::Ec(k), SigAlg::Ecdsa(h)) => k.verify(h, message, signature),
            _ => false,
        }
    }
}

/// A parsed `BasicConstraints`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BasicConstraints {
    /// Whether the subject is a CA.
    pub ca: bool,
    /// Maximum number of intermediate CAs below this one.
    pub path_len: Option<u32>,
}

/// A parsed certificate. Borrows nothing: all needed slices are copied.
#[derive(Clone, Debug)]
pub struct Certificate {
    /// The whole DER encoding.
    pub der: Vec<u8>,
    tbs: Vec<u8>,
    /// Issuer name, DER.
    pub issuer: Vec<u8>,
    /// Subject name, DER.
    pub subject: Vec<u8>,
    /// Start of validity, seconds since the Unix epoch.
    pub not_before: i64,
    /// End of validity, seconds since the Unix epoch.
    pub not_after: i64,
    /// Subject public key.
    pub key: PublicKey,
    sig_alg: SigAlg,
    signature: Vec<u8>,
    /// `dNSName` entries of the subject alternative name.
    pub dns_names: Vec<String>,
    /// `iPAddress` entries of the subject alternative name.
    pub ip_addrs: Vec<Vec<u8>>,
    /// Basic constraints, when present.
    pub basic_constraints: Option<BasicConstraints>,
    /// `keyUsage` bits (bit 0 = digitalSignature … bit 5 = keyCertSign), when present.
    pub key_usage: Option<u16>,
    /// Extended key usage OIDs, when present.
    pub ext_key_usage: Option<Vec<String>>,
}

const KEY_CERT_SIGN: u16 = 1 << 5;
const SERVER_AUTH: &str = "1.3.6.1.5.5.7.3.1";
const ANY_EKU: &str = "2.5.29.37.0";

fn sig_alg(alg: &[u8]) -> Result<SigAlg, Error> {
    let mut r = der::single(alg, tag::SEQUENCE)?;
    let oid = der::oid_string(r.expect(tag::OID)?.content)?;
    Ok(match oid.as_str() {
        "1.2.840.113549.1.1.11" => SigAlg::RsaPkcs1(Hash::Sha256),
        "1.2.840.113549.1.1.12" => SigAlg::RsaPkcs1(Hash::Sha384),
        "1.2.840.113549.1.1.13" => SigAlg::RsaPkcs1(Hash::Sha512),
        "1.2.840.10045.4.3.2" => SigAlg::Ecdsa(Hash::Sha256),
        "1.2.840.10045.4.3.3" => SigAlg::Ecdsa(Hash::Sha384),
        "1.2.840.10045.4.3.4" => SigAlg::Ecdsa(Hash::Sha512),
        "1.2.840.113549.1.1.10" => pss_params(&mut r)?,
        _ => return Err(Error::Unsupported("signature algorithm")),
    })
}

/// RSASSA-PSS-params: hash, MGF1 with the same hash, salt length (default 20 is refused).
fn pss_params(r: &mut Reader) -> Result<SigAlg, Error> {
    let mut p = der::single(r.expect(tag::SEQUENCE)?.raw, tag::SEQUENCE)?;
    let hash_of = |alg: &[u8]| -> Result<Hash, Error> {
        let mut a = der::single(alg, tag::SEQUENCE)?;
        match der::oid_string(a.expect(tag::OID)?.content)?.as_str() {
            "2.16.840.1.101.3.4.2.1" => Ok(Hash::Sha256),
            "2.16.840.1.101.3.4.2.2" => Ok(Hash::Sha384),
            "2.16.840.1.101.3.4.2.3" => Ok(Hash::Sha512),
            _ => Err(Error::Unsupported("PSS hash")),
        }
    };
    let h = p
        .optional(tag::context(0))?
        .ok_or(Error::Unsupported("PSS default hash (SHA-1)"))?;
    let hash = hash_of(Reader::new(h.content).expect(tag::SEQUENCE)?.raw)?;
    let m = p
        .optional(tag::context(1))?
        .ok_or(Error::Unsupported("PSS default MGF (SHA-1)"))?;
    let mut mr = Reader::new(m.content);
    let mgf = mr.expect(tag::SEQUENCE)?;
    let mut mg = Reader::new(mgf.content);
    if der::oid_string(mg.expect(tag::OID)?.content)? != "1.2.840.113549.1.1.8" {
        return Err(Error::Unsupported("PSS mask function"));
    }
    if hash_of(mg.expect(tag::SEQUENCE)?.raw)? != hash {
        return Err(Error::Unsupported("PSS mask hash differs"));
    }
    let s = p
        .optional(tag::context(2))?
        .ok_or(Error::Unsupported("PSS default salt length"))?;
    let salt = der::unsigned_integer(Reader::new(s.content).expect(tag::INTEGER)?.content)?;
    if salt.len() > 2 {
        return Err(Error::Unsupported("PSS salt length"));
    }
    let salt = salt.iter().fold(0usize, |a, &b| a << 8 | usize::from(b));
    Ok(SigAlg::RsaPss(hash, salt))
}

fn spki(content: &[u8]) -> Result<PublicKey, Error> {
    let mut r = Reader::new(content);
    let alg = r.expect(tag::SEQUENCE)?;
    let bits = der::whole_bit_string(r.expect(tag::BIT_STRING)?.content)?;
    r.finish()?;
    let mut a = Reader::new(alg.content);
    let oid = der::oid_string(a.expect(tag::OID)?.content)?;
    match oid.as_str() {
        "1.2.840.113549.1.1.1" => RsaKey::from_der(bits)
            .map(PublicKey::Rsa)
            .ok_or(Error::BadKey("RSA key rejected")),
        "1.2.840.10045.2.1" => {
            let curve_oid = der::oid_string(a.expect(tag::OID)?.content)?;
            let curve = Curve::from_oid(&curve_oid).ok_or(Error::Unsupported("EC curve"))?;
            EcKey::from_sec1(curve, bits)
                .map(PublicKey::Ec)
                .ok_or(Error::BadKey("EC point rejected"))
        }
        _ => Err(Error::Unsupported("public key algorithm")),
    }
}

/// Days from 1970-01-01 for a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn parse_time(e: &der::Element) -> Result<i64, Error> {
    let s = std::str::from_utf8(e.content).map_err(|_| Error::Der("time is not ASCII"))?;
    let digits = |a: usize, b: usize| -> Result<i64, Error> {
        s.get(a..b)
            .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|t| t.parse().ok())
            .ok_or(Error::Der("bad time"))
    };
    let (year, rest) = match e.tag {
        tag::UTC_TIME => {
            let yy = digits(0, 2)?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, 2)
        }
        tag::GENERALIZED_TIME => (digits(0, 4)?, 4),
        _ => return Err(Error::Der("not a time")),
    };
    let expected_len = rest + 10 + 1;
    if s.len() != expected_len || !s.ends_with('Z') {
        return Err(Error::Der("time must be YYMMDDHHMMSSZ"));
    }
    let (mo, d, h, mi, sec) = (
        digits(rest, rest + 2)?,
        digits(rest + 2, rest + 4)?,
        digits(rest + 4, rest + 6)?,
        digits(rest + 6, rest + 8)?,
        digits(rest + 8, rest + 10)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 59 {
        return Err(Error::Der("time out of range"));
    }
    Ok(days_from_civil(year, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

impl Certificate {
    /// Parse one DER certificate.
    ///
    /// # Errors
    /// [`Error`] for malformed or unsupported certificates. Unknown *critical* extensions are
    /// refused; unknown non-critical ones are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut outer = der::single(bytes, tag::SEQUENCE)?;
        let tbs_el = outer.expect(tag::SEQUENCE)?;
        let alg_el = outer.expect(tag::SEQUENCE)?;
        let sig_el = outer.expect(tag::BIT_STRING)?;
        outer.finish()?;
        let outer_alg = sig_alg(alg_el.raw)?;
        let signature = der::whole_bit_string(sig_el.content)?.to_vec();

        let mut t = Reader::new(tbs_el.content);
        // Only v3 (2) is accepted: extensions such as basicConstraints need it.
        let version = t
            .optional(tag::context(0))?
            .ok_or(Error::Unsupported("certificate version 1"))?;
        let v = Reader::new(version.content).expect(tag::INTEGER)?.content;
        if v != [2] {
            return Err(Error::Unsupported("certificate version"));
        }
        der::unsigned_integer(t.expect(tag::INTEGER)?.content)?; // serial
        let inner_alg = t.expect(tag::SEQUENCE)?;
        if sig_alg(inner_alg.raw)? != outer_alg {
            return Err(Error::Der("signature algorithms differ"));
        }
        let issuer = t.expect(tag::SEQUENCE)?.raw.to_vec();
        let mut validity = Reader::new(t.expect(tag::SEQUENCE)?.content);
        let not_before = parse_time(&validity.read()?)?;
        let not_after = parse_time(&validity.read()?)?;
        validity.finish()?;
        let subject = t.expect(tag::SEQUENCE)?.raw.to_vec();
        let key = spki(t.expect(tag::SEQUENCE)?.content)?;
        // issuerUniqueID [1], subjectUniqueID [2] are ignored.
        t.optional(0xA1)?;
        t.optional(0xA2)?;
        let mut cert = Self {
            der: bytes.to_vec(),
            tbs: tbs_el.raw.to_vec(),
            issuer,
            subject,
            not_before,
            not_after,
            key,
            sig_alg: outer_alg,
            signature,
            dns_names: Vec::new(),
            ip_addrs: Vec::new(),
            basic_constraints: None,
            key_usage: None,
            ext_key_usage: None,
        };
        if let Some(ext) = t.optional(tag::context(3))? {
            cert.extensions(ext.content)?;
        }
        t.finish()?;
        Ok(cert)
    }

    fn extensions(&mut self, content: &[u8]) -> Result<(), Error> {
        let mut list = der::single(content, tag::SEQUENCE)?;
        let mut seen = Vec::new();
        while !list.is_empty() {
            let mut e = Reader::new(list.expect(tag::SEQUENCE)?.content);
            let oid = der::oid_string(e.expect(tag::OID)?.content)?;
            let critical = match e.optional(tag::BOOLEAN)? {
                Some(b) => match b.content {
                    [0xFF] => true,
                    [0x00] => return Err(Error::Der("DEFAULT boolean encoded")),
                    _ => return Err(Error::Der("bad boolean")),
                },
                None => false,
            };
            let value = e.expect(tag::OCTET_STRING)?.content;
            e.finish()?;
            if seen.contains(&oid) {
                return Err(Error::Der("duplicate extension"));
            }
            seen.push(oid.clone());
            match oid.as_str() {
                "2.5.29.19" => self.basic_constraints = Some(parse_basic_constraints(value)?),
                "2.5.29.15" => self.key_usage = Some(parse_key_usage(value)?),
                "2.5.29.37" => self.ext_key_usage = Some(parse_eku(value)?),
                "2.5.29.17" => self.parse_san(value)?,
                // Handled elsewhere or harmless for authentication.
                "2.5.29.14"
                | "2.5.29.35"
                | "1.3.6.1.5.5.7.1.1"
                | "2.5.29.31"
                | "2.5.29.32"
                | "1.3.6.1.4.1.11129.2.4.2" => {}
                _ if critical => return Err(Error::Unsupported("critical extension")),
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_san(&mut self, value: &[u8]) -> Result<(), Error> {
        let mut names = der::single(value, tag::SEQUENCE)?;
        while !names.is_empty() {
            let n = names.read()?;
            match n.tag {
                0x82 => {
                    let s = std::str::from_utf8(n.content)
                        .map_err(|_| Error::Der("dNSName is not ASCII"))?;
                    if !s.is_ascii() {
                        return Err(Error::Der("dNSName is not ASCII"));
                    }
                    self.dns_names.push(s.to_ascii_lowercase());
                }
                0x87 if n.content.len() == 4 || n.content.len() == 16 => {
                    self.ip_addrs.push(n.content.to_vec());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Verify that `issuer_key` made this certificate's signature.
    #[must_use]
    pub fn signed_by(&self, issuer_key: &PublicKey) -> bool {
        issuer_key.verify(self.sig_alg, &self.tbs, &self.signature)
    }

    /// Whether the certificate may sign other certificates.
    #[must_use]
    pub fn is_ca(&self) -> bool {
        self.basic_constraints.is_some_and(|b| b.ca)
            && self.key_usage.is_none_or(|u| u & KEY_CERT_SIGN != 0)
    }

    /// Whether the certificate may be used to authenticate a TLS server.
    #[must_use]
    pub fn allows_server_auth(&self) -> bool {
        self.ext_key_usage
            .as_ref()
            .is_none_or(|e| e.iter().any(|o| o == SERVER_AUTH || o == ANY_EKU))
    }
}

fn parse_basic_constraints(value: &[u8]) -> Result<BasicConstraints, Error> {
    let mut r = der::single(value, tag::SEQUENCE)?;
    let mut bc = BasicConstraints::default();
    if let Some(b) = r.optional(tag::BOOLEAN)? {
        bc.ca = match b.content {
            [0xFF] => true,
            _ => return Err(Error::Der("bad boolean")),
        };
    }
    if let Some(p) = r.optional(tag::INTEGER)? {
        let v = der::unsigned_integer(p.content)?;
        if v.len() > 2 {
            return Err(Error::Der("path length too large"));
        }
        bc.path_len = Some(v.iter().fold(0u32, |a, &b| a << 8 | u32::from(b)));
    }
    r.finish()?;
    Ok(bc)
}

fn parse_key_usage(value: &[u8]) -> Result<u16, Error> {
    let mut r = Reader::new(value);
    let bs = r.expect(tag::BIT_STRING)?;
    r.finish()?;
    let (unused, bits) = bs
        .content
        .split_first()
        .ok_or(Error::Der("empty key usage"))?;
    if *unused > 7 || bits.is_empty() || bits.len() > 2 {
        return Err(Error::Der("bad key usage"));
    }
    let mut v = 0u16;
    for (i, b) in bits.iter().enumerate() {
        for bit in 0..8 {
            if b & (0x80 >> bit) != 0 {
                v |= 1 << (i * 8 + bit);
            }
        }
    }
    Ok(v)
}

fn parse_eku(value: &[u8]) -> Result<Vec<String>, Error> {
    let mut r = der::single(value, tag::SEQUENCE)?;
    let mut out = Vec::new();
    while !r.is_empty() {
        out.push(der::oid_string(r.expect(tag::OID)?.content)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
    }

    #[test]
    fn times() {
        let utc = der::Element {
            tag: tag::UTC_TIME,
            content: b"240229120000Z",
            raw: &[],
        };
        assert_eq!(parse_time(&utc).unwrap(), 19_782 * 86_400 + 12 * 3600);
        let gen = der::Element {
            tag: tag::GENERALIZED_TIME,
            content: b"20500101000000Z",
            raw: &[],
        };
        assert_eq!(
            parse_time(&gen).unwrap(),
            days_from_civil(2050, 1, 1) * 86_400
        );
        let bad = der::Element {
            tag: tag::UTC_TIME,
            content: b"2402291200Z",
            raw: &[],
        };
        assert!(parse_time(&bad).is_err());
        let bad_month = der::Element {
            tag: tag::UTC_TIME,
            content: b"241329120000Z",
            raw: &[],
        };
        assert!(parse_time(&bad_month).is_err());
    }

    #[test]
    fn key_usage_bits() {
        // digitalSignature + keyCertSign: 0b1000_0100, 2 unused
        assert_eq!(
            parse_key_usage(&[0x03, 0x02, 0x02, 0x84]).unwrap(),
            0b0010_0001
        );
    }
}
