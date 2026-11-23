//! Handshake message encoding and parsing.

use crate::Error;
use aeon_pki::verify::Host;

pub const CLIENT_HELLO: u8 = 1;
pub const SERVER_HELLO: u8 = 2;
pub const NEW_SESSION_TICKET: u8 = 4;
pub const ENCRYPTED_EXTENSIONS: u8 = 8;
pub const CERTIFICATE: u8 = 11;
pub const CERTIFICATE_VERIFY: u8 = 15;
pub const FINISHED: u8 = 20;
pub const KEY_UPDATE: u8 = 24;

pub const SUITE_CHACHA20_POLY1305_SHA256: u16 = 0x1303;
pub const GROUP_X25519: u16 = 0x001d;
pub const SIG_ECDSA_P256_SHA256: u16 = 0x0403;
pub const SIG_ECDSA_P384_SHA384: u16 = 0x0503;
pub const SIG_RSA_PSS_SHA256: u16 = 0x0804;
pub const SIG_RSA_PSS_SHA384: u16 = 0x0805;

const HELLO_RETRY_REQUEST: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91,
    0xC2, 0xA2, 0x11, 0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

/// A bounds-checked cursor over message bytes.
pub struct Cursor<'a> {
    data: &'a [u8],
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.data.len() {
            return Err(Error::Protocol("message too short"));
        }
        let (a, b) = self.data.split_at(n);
        self.data = b;
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u24(&mut self) -> Result<usize, Error> {
        let b = self.take(3)?;
        Ok(usize::from(b[0]) << 16 | usize::from(b[1]) << 8 | usize::from(b[2]))
    }

    /// A vector with a one-byte length.
    pub fn vec8(&mut self) -> Result<&'a [u8], Error> {
        let n = usize::from(self.u8()?);
        self.take(n)
    }

    /// A vector with a two-byte length.
    pub fn vec16(&mut self) -> Result<&'a [u8], Error> {
        let n = usize::from(self.u16()?);
        self.take(n)
    }

    /// A vector with a three-byte length.
    pub fn vec24(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u24()?;
        self.take(n)
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn finish(&self) -> Result<(), Error> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(Error::Protocol("trailing bytes in message"))
        }
    }
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend(v.to_be_bytes());
}

fn put_ext(out: &mut Vec<u8>, ty: u16, body: &[u8]) {
    put16(out, ty);
    put16(out, body.len() as u16);
    out.extend(body);
}

/// Wrap a body in a handshake header.
pub fn frame(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![ty];
    m.extend(&(body.len() as u32).to_be_bytes()[1..]);
    m.extend(body);
    m
}

/// Build a ClientHello (handshake-framed).
pub fn client_hello(
    random: &[u8; 32],
    session_id: &[u8; 32],
    host: &Host,
    key_share: &[u8; 32],
) -> Vec<u8> {
    let mut b = Vec::new();
    put16(&mut b, 0x0303);
    b.extend(random);
    b.push(32);
    b.extend(session_id);
    put16(&mut b, 2);
    put16(&mut b, SUITE_CHACHA20_POLY1305_SHA256);
    b.extend([1, 0]); // compression: null only
    let mut ext = Vec::new();
    if let Host::Dns(name) = host {
        let mut sni = Vec::new();
        put16(&mut sni, (name.len() + 3) as u16);
        sni.push(0);
        put16(&mut sni, name.len() as u16);
        sni.extend(name.as_bytes());
        put_ext(&mut ext, 0, &sni);
    }
    put_ext(&mut ext, 10, &[0, 2, 0, 0x1d]);
    let mut sigs = Vec::new();
    for s in [
        SIG_ECDSA_P256_SHA256,
        SIG_ECDSA_P384_SHA384,
        SIG_RSA_PSS_SHA256,
        SIG_RSA_PSS_SHA384,
    ] {
        put16(&mut sigs, s);
    }
    let mut sig_ext = Vec::new();
    put16(&mut sig_ext, sigs.len() as u16);
    sig_ext.extend(sigs);
    put_ext(&mut ext, 13, &sig_ext);
    put_ext(&mut ext, 43, &[2, 3, 4]);
    let mut ks = Vec::new();
    put16(&mut ks, 36);
    put16(&mut ks, GROUP_X25519);
    put16(&mut ks, 32);
    ks.extend(key_share);
    put_ext(&mut ext, 51, &ks);
    put16(&mut b, ext.len() as u16);
    b.extend(ext);
    frame(CLIENT_HELLO, &b)
}

/// What the ServerHello selected.
pub struct ServerHello {
    pub key_share: [u8; 32],
}

/// Parse a ServerHello body, checking it against what the ClientHello offered.
pub fn parse_server_hello(body: &[u8], session_id: &[u8; 32]) -> Result<ServerHello, Error> {
    let mut c = Cursor::new(body);
    if c.u16()? != 0x0303 {
        return Err(Error::Protocol("bad legacy version"));
    }
    if c.take(32)? == HELLO_RETRY_REQUEST {
        return Err(Error::Unsupported("HelloRetryRequest"));
    }
    if c.vec8()? != session_id {
        return Err(Error::Protocol("session id not echoed"));
    }
    if c.u16()? != SUITE_CHACHA20_POLY1305_SHA256 {
        return Err(Error::Protocol("server chose an unoffered cipher suite"));
    }
    if c.u8()? != 0 {
        return Err(Error::Protocol("compression is not allowed"));
    }
    let mut exts = Cursor::new(c.vec16()?);
    c.finish()?;
    let (mut version, mut share) = (false, None);
    let mut seen = Vec::new();
    while !exts.is_empty() {
        let ty = exts.u16()?;
        let data = exts.vec16()?;
        if seen.contains(&ty) {
            return Err(Error::Protocol("duplicate extension"));
        }
        seen.push(ty);
        match ty {
            43 => {
                if data != [3, 4] {
                    return Err(Error::Protocol("server did not choose TLS 1.3"));
                }
                version = true;
            }
            51 => {
                let mut k = Cursor::new(data);
                if k.u16()? != GROUP_X25519 {
                    return Err(Error::Protocol("server chose an unoffered group"));
                }
                let key = k.vec16()?;
                k.finish()?;
                let key: [u8; 32] = key
                    .try_into()
                    .map_err(|_| Error::Protocol("bad key share length"))?;
                share = Some(key);
            }
            _ => return Err(Error::Protocol("unexpected extension in ServerHello")),
        }
    }
    match (version, share) {
        (true, Some(key_share)) => Ok(ServerHello { key_share }),
        _ => Err(Error::Protocol("ServerHello lacks version or key share")),
    }
}

/// Check EncryptedExtensions: extensions we never offered are not allowed.
pub fn check_encrypted_extensions(body: &[u8]) -> Result<(), Error> {
    let mut c = Cursor::new(body);
    let mut exts = Cursor::new(c.vec16()?);
    c.finish()?;
    while !exts.is_empty() {
        let ty = exts.u16()?;
        exts.vec16()?;
        // server_name (0) acknowledgement and supported_groups (10) hints are the only
        // extensions a server may send that we offered.
        if ty != 0 && ty != 10 {
            return Err(Error::Protocol("unsolicited extension"));
        }
    }
    Ok(())
}

/// The certificate chain from a Certificate message, leaf first.
pub fn parse_certificate(body: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut c = Cursor::new(body);
    if !c.vec8()?.is_empty() {
        return Err(Error::Protocol("certificate request context must be empty"));
    }
    let mut list = Cursor::new(c.vec24()?);
    c.finish()?;
    let mut certs = Vec::new();
    while !list.is_empty() {
        certs.push(list.vec24()?.to_vec());
        list.vec16()?; // per-certificate extensions (OCSP, SCT) are ignored
    }
    Ok(certs)
}

/// Algorithm and signature from a CertificateVerify message.
pub fn parse_certificate_verify(body: &[u8]) -> Result<(u16, Vec<u8>), Error> {
    let mut c = Cursor::new(body);
    let alg = c.u16()?;
    let sig = c.vec16()?.to_vec();
    c.finish()?;
    Ok((alg, sig))
}

/// The bytes the server signs in CertificateVerify.
pub fn certificate_verify_input(transcript_hash: &[u8; 32]) -> Vec<u8> {
    let mut v = vec![0x20; 64];
    v.extend(b"TLS 1.3, server CertificateVerify");
    v.push(0);
    v.extend(transcript_hash);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hello_is_well_formed() {
        let m = client_hello(
            &[1; 32],
            &[2; 32],
            &Host::Dns("example.com".into()),
            &[3; 32],
        );
        assert_eq!(m[0], CLIENT_HELLO);
        let len = usize::from(m[1]) << 16 | usize::from(m[2]) << 8 | usize::from(m[3]);
        assert_eq!(len, m.len() - 4);
        // walk the body
        let mut c = Cursor::new(&m[4..]);
        assert_eq!(c.u16().unwrap(), 0x0303);
        c.take(32).unwrap();
        assert_eq!(c.vec8().unwrap(), [2; 32]);
        assert_eq!(c.vec16().unwrap(), [0x13, 0x03]);
        assert_eq!(c.vec8().unwrap(), [0]);
        let mut exts = Cursor::new(c.vec16().unwrap());
        c.finish().unwrap();
        let mut types = Vec::new();
        while !exts.is_empty() {
            types.push(exts.u16().unwrap());
            exts.vec16().unwrap();
        }
        assert_eq!(types, [0, 10, 13, 43, 51]);
    }

    #[test]
    fn ip_hosts_send_no_server_name() {
        let m = client_hello(&[1; 32], &[2; 32], &Host::Ip(vec![1, 2, 3, 4]), &[3; 32]);
        let mut c = Cursor::new(&m[4..]);
        c.take(2 + 32).unwrap();
        c.vec8().unwrap();
        c.vec16().unwrap();
        c.vec8().unwrap();
        let mut exts = Cursor::new(c.vec16().unwrap());
        assert_ne!(exts.u16().unwrap(), 0);
    }

    fn server_hello(extra: &[u8], sid: &[u8; 32]) -> Vec<u8> {
        let mut b = vec![3, 3];
        b.extend([9u8; 32]);
        b.push(32);
        b.extend(sid);
        b.extend([0x13, 0x03, 0]);
        let mut ext = vec![0, 43, 0, 2, 3, 4, 0, 51, 0, 36, 0, 0x1d, 0, 32];
        ext.extend([5u8; 32]);
        ext.extend(extra);
        b.extend((ext.len() as u16).to_be_bytes());
        b.extend(ext);
        b
    }

    #[test]
    fn parses_a_good_server_hello_and_rejects_bad_ones() {
        let sid = [2u8; 32];
        assert_eq!(
            parse_server_hello(&server_hello(&[], &sid), &sid)
                .unwrap()
                .key_share,
            [5; 32]
        );
        assert!(parse_server_hello(&server_hello(&[], &sid), &[3; 32]).is_err());
        assert!(parse_server_hello(&server_hello(&[0, 99, 0, 0], &sid), &sid).is_err());
        let mut hrr = server_hello(&[], &sid);
        hrr[2..34].copy_from_slice(&HELLO_RETRY_REQUEST);
        assert!(matches!(
            parse_server_hello(&hrr, &sid),
            Err(Error::Unsupported(_))
        ));
        let mut bad_suite = server_hello(&[], &sid);
        bad_suite[34 + 33] = 0x01;
        assert!(parse_server_hello(&bad_suite, &sid).is_err());
        assert!(parse_server_hello(&server_hello(&[], &sid)[..40], &sid).is_err());
    }

    #[test]
    fn certificate_message() {
        let mut body = vec![0];
        let entry = [0, 0, 3, 1, 2, 3, 0, 0];
        body.extend([0, 0, entry.len() as u8]);
        body.extend(entry);
        assert_eq!(parse_certificate(&body).unwrap(), vec![vec![1, 2, 3]]);
        assert!(parse_certificate(&[1, 9]).is_err());
        assert!(parse_certificate(&body[..body.len() - 1]).is_err());
    }

    #[test]
    fn signed_content_layout() {
        let v = certificate_verify_input(&[7; 32]);
        assert_eq!(v.len(), 64 + 33 + 1 + 32);
        assert_eq!(&v[64..97], b"TLS 1.3, server CertificateVerify");
    }
}
