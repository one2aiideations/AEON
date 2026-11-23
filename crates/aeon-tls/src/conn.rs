//! The TLS 1.3 client connection: handshake, record layer, post-handshake messages.

use crate::keys::{self, hash, Direction};
use crate::msg::{self, Cursor};
use crate::Error;
use aeon_crypto::aead;
use aeon_crypto::x25519;
use aeon_pki::verify::{verify_server, Host, TrustStore};
use aeon_pki::x509::{PublicKey, SigAlg};
use aeon_pki::Hash;
use std::io::{self, Read, Write};

const HANDSHAKE: u8 = 22;
const ALERT: u8 = 21;
const CHANGE_CIPHER_SPEC: u8 = 20;
const APPLICATION_DATA: u8 = 23;
const MAX_PLAINTEXT: usize = 16_384;
const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + 256;
/// Largest handshake message accepted (certificate chains are the big ones).
const MAX_HANDSHAKE: usize = 128 * 1024;

/// How to connect.
pub struct Config {
    /// The host the certificate must name.
    pub host: Host,
    /// Trusted roots.
    pub trust: TrustStore,
    /// The current time, seconds since the Unix epoch, for certificate validity.
    pub now: i64,
    /// Source of random bytes ([`crate::os_random`] in production).
    pub random: fn(&mut [u8]) -> io::Result<()>,
}

/// An established TLS 1.3 connection over `S`.
pub struct Connection<S> {
    stream: S,
    send: Option<Direction>,
    recv: Option<Direction>,
    handshake_buf: Vec<u8>,
    plain: Vec<u8>,
    plain_pos: usize,
    finished: bool,
    handshaking: bool,
    closed: bool,
}

fn alert_for(e: &Error) -> u8 {
    match e {
        Error::Protocol(_) => 10,
        Error::Unsupported(_) => 40,
        Error::Certificate(_) => 42,
        Error::BadSignature(_) => 51,
        Error::Alert(_) | Error::Io(_) => 80,
    }
}

impl<S: Read + Write> Connection<S> {
    /// Run the handshake over `stream`.
    ///
    /// # Errors
    /// [`Error`] on transport failure, protocol violations, or when the server's certificate or
    /// signatures do not verify. A fatal alert is sent to the server on a protocol failure.
    pub fn connect(stream: S, config: &Config) -> Result<Self, Error> {
        let mut c = Self {
            stream,
            send: None,
            recv: None,
            handshake_buf: Vec::new(),
            plain: Vec::new(),
            plain_pos: 0,
            finished: false,
            handshaking: true,
            closed: false,
        };
        match c.handshake(config) {
            Ok(()) => Ok(c),
            Err(e) => {
                if !matches!(e, Error::Io(_) | Error::Alert(_)) {
                    let _ = c.write_record(ALERT, &[2, alert_for(&e)]);
                }
                Err(e)
            }
        }
    }

    fn handshake(&mut self, cfg: &Config) -> Result<(), Error> {
        let mut secret = [0u8; 32];
        let mut random = [0u8; 32];
        let mut session_id = [0u8; 32];
        (cfg.random)(&mut secret)?;
        (cfg.random)(&mut random)?;
        (cfg.random)(&mut session_id)?;
        let public = x25519::public_key(&secret);

        let ch = msg::client_hello(&random, &session_id, &cfg.host, &public);
        self.write_record(HANDSHAKE, &ch)?;
        let mut transcript = ch;

        // ServerHello (plaintext).
        let (ty, body, raw) = self.next_handshake()?;
        if ty != msg::SERVER_HELLO {
            return Err(Error::Protocol("expected ServerHello"));
        }
        let sh = msg::parse_server_hello(&body, &session_id)?;
        transcript.extend(&raw);
        if !self.handshake_buf.is_empty() {
            return Err(Error::Protocol("data after ServerHello"));
        }
        let shared = x25519::shared_secret(&secret, &sh.key_share)
            .ok_or(Error::Protocol("invalid server key share"))?;
        let hs_secret = keys::handshake_secret(&shared);
        let th = hash(&transcript);
        let c_hs = keys::derive_secret(&hs_secret, "c hs traffic", &th);
        let s_hs = keys::derive_secret(&hs_secret, "s hs traffic", &th);
        self.recv = Some(Direction::new(s_hs));

        // EncryptedExtensions, Certificate, CertificateVerify, Finished (encrypted).
        let (ty, body, raw) = self.next_handshake()?;
        if ty != msg::ENCRYPTED_EXTENSIONS {
            return Err(Error::Protocol("expected EncryptedExtensions"));
        }
        msg::check_encrypted_extensions(&body)?;
        transcript.extend(&raw);

        let (ty, body, raw) = self.next_handshake()?;
        if ty != msg::CERTIFICATE {
            return Err(Error::Protocol("expected Certificate"));
        }
        let chain = msg::parse_certificate(&body)?;
        transcript.extend(&raw);
        let leaf =
            verify_server(&chain, &cfg.trust, &cfg.host, cfg.now).map_err(Error::Certificate)?;

        let (ty, body, raw) = self.next_handshake()?;
        if ty != msg::CERTIFICATE_VERIFY {
            return Err(Error::Protocol("expected CertificateVerify"));
        }
        let (alg, sig) = msg::parse_certificate_verify(&body)?;
        let signed = msg::certificate_verify_input(&hash(&transcript));
        check_handshake_signature(&leaf.key, alg, &signed, &sig)?;
        transcript.extend(&raw);

        let (ty, body, raw) = self.next_handshake()?;
        if ty != msg::FINISHED {
            return Err(Error::Protocol("expected Finished"));
        }
        let expected = keys::finished(&s_hs, &hash(&transcript));
        if !aeon_crypto::ct_eq(&body, &expected) {
            return Err(Error::BadSignature("server Finished does not verify"));
        }
        transcript.extend(&raw);
        if !self.handshake_buf.is_empty() {
            return Err(Error::Protocol("data after server Finished"));
        }

        let th = hash(&transcript);
        let master = keys::master_secret(&hs_secret);
        let c_ap = keys::derive_secret(&master, "c ap traffic", &th);
        let s_ap = keys::derive_secret(&master, "s ap traffic", &th);

        // Client Finished under the handshake keys, then switch to application keys.
        self.write_record(CHANGE_CIPHER_SPEC, &[1])?; // middlebox compatibility
        self.send = Some(Direction::new(c_hs));
        let fin = msg::frame(msg::FINISHED, &keys::finished(&c_hs, &th));
        self.write_record(HANDSHAKE, &fin)?;
        self.send = Some(Direction::new(c_ap));
        self.recv = Some(Direction::new(s_ap));
        self.handshaking = false;
        self.finished = true;
        Ok(())
    }

    /// Next complete handshake message: type, body, and the full bytes for the transcript.
    fn next_handshake(&mut self) -> Result<(u8, Vec<u8>, Vec<u8>), Error> {
        loop {
            if self.handshake_buf.len() >= 4 {
                let len = usize::from(self.handshake_buf[1]) << 16
                    | usize::from(self.handshake_buf[2]) << 8
                    | usize::from(self.handshake_buf[3]);
                if len > MAX_HANDSHAKE {
                    return Err(Error::Protocol("handshake message too large"));
                }
                if self.handshake_buf.len() >= 4 + len {
                    let raw: Vec<u8> = self.handshake_buf.drain(..4 + len).collect();
                    return Ok((raw[0], raw[4..].to_vec(), raw));
                }
            }
            let (ty, data) = self.read_record()?;
            match ty {
                HANDSHAKE => self.handshake_buf.extend(data),
                ALERT => return Err(alert_error(&data)),
                _ => return Err(Error::Protocol("unexpected record during handshake")),
            }
        }
    }

    fn write_record(&mut self, ty: u8, data: &[u8]) -> Result<(), Error> {
        let mut out = Vec::with_capacity(data.len() + 32);
        match self.send.as_mut() {
            None => {
                // Only the initial ClientHello may carry the legacy 0x0301 record version.
                let minor = if ty == HANDSHAKE && data.first() == Some(&msg::CLIENT_HELLO) {
                    1
                } else {
                    3
                };
                out.extend([ty, 3, minor]);
                out.extend((data.len() as u16).to_be_bytes());
                out.extend(data);
            }
            Some(dir) => {
                let mut inner = data.to_vec();
                inner.push(ty);
                let len = (inner.len() + aeon_crypto::poly1305::TAG_LEN) as u16;
                let header = [APPLICATION_DATA, 3, 3, (len >> 8) as u8, len as u8];
                let nonce = dir
                    .next_nonce()
                    .ok_or(Error::Protocol("record sequence exhausted"))?;
                let sealed = aead::seal(dir.key(), &nonce, &header, &inner);
                out.extend(header);
                out.extend(sealed);
            }
        }
        self.stream.write_all(&out)?;
        Ok(())
    }

    /// Read one record, decrypting when keys are installed. Returns (inner type, content).
    fn read_record(&mut self) -> Result<(u8, Vec<u8>), Error> {
        loop {
            let mut header = [0u8; 5];
            self.stream.read_exact(&mut header)?;
            let len = usize::from(header[3]) << 8 | usize::from(header[4]);
            if len > MAX_CIPHERTEXT {
                return Err(Error::Protocol("record too large"));
            }
            let mut body = vec![0u8; len];
            self.stream.read_exact(&mut body)?;
            if header[0] == CHANGE_CIPHER_SPEC {
                // A compatibility-mode CCS is only legal during the handshake.
                if self.handshaking && body == [1] {
                    continue;
                }
                return Err(Error::Protocol("unexpected change_cipher_spec"));
            }
            let Some(dir) = self.recv.as_mut() else {
                if len > MAX_PLAINTEXT {
                    return Err(Error::Protocol("record too large"));
                }
                return Ok((header[0], body));
            };
            if header[0] != APPLICATION_DATA {
                return Err(Error::Protocol("plaintext record after keys were set"));
            }
            let nonce = dir
                .next_nonce()
                .ok_or(Error::Protocol("record sequence exhausted"))?;
            let mut plain = aead::open(dir.key(), &nonce, &header, &body)
                .ok_or(Error::BadSignature("record failed authentication"))?;
            // Strip padding zeros; the last non-zero byte is the real content type.
            while plain.last() == Some(&0) {
                plain.pop();
            }
            let ty = plain.pop().ok_or(Error::Protocol("empty record"))?;
            if plain.len() > MAX_PLAINTEXT {
                return Err(Error::Protocol("record too large"));
            }
            return Ok((ty, plain));
        }
    }

    /// Handle a post-handshake handshake record (tickets, key updates).
    fn post_handshake(&mut self, data: &[u8]) -> Result<(), Error> {
        self.handshake_buf.extend(data);
        while let Some((ty, body)) = self.take_buffered_message()? {
            match ty {
                msg::NEW_SESSION_TICKET => {}
                msg::KEY_UPDATE => {
                    let mut c = Cursor::new(&body);
                    let request = c.u8()?;
                    c.finish()?;
                    if request > 1 {
                        return Err(Error::Protocol("bad key update request"));
                    }
                    self.recv
                        .as_mut()
                        .ok_or(Error::Protocol("no keys"))?
                        .update();
                    if request == 1 {
                        self.send_key_update(false)?;
                    }
                }
                _ => return Err(Error::Protocol("unexpected post-handshake message")),
            }
        }
        Ok(())
    }

    fn take_buffered_message(&mut self) -> Result<Option<(u8, Vec<u8>)>, Error> {
        let b = &self.handshake_buf;
        if b.len() < 4 {
            return Ok(None);
        }
        let len = usize::from(b[1]) << 16 | usize::from(b[2]) << 8 | usize::from(b[3]);
        if len > MAX_HANDSHAKE {
            return Err(Error::Protocol("handshake message too large"));
        }
        if b.len() < 4 + len {
            return Ok(None);
        }
        let raw: Vec<u8> = self.handshake_buf.drain(..4 + len).collect();
        Ok(Some((raw[0], raw[4..].to_vec())))
    }

    fn send_key_update(&mut self, request_reply: bool) -> Result<(), Error> {
        let m = msg::frame(msg::KEY_UPDATE, &[u8::from(request_reply)]);
        self.write_record(HANDSHAKE, &m)?;
        self.send
            .as_mut()
            .ok_or(Error::Protocol("no keys"))?
            .update();
        Ok(())
    }

    /// Rotate our sending keys and ask the server to rotate its own.
    ///
    /// # Errors
    /// Transport failure.
    pub fn update_keys(&mut self) -> Result<(), Error> {
        self.send_key_update(true)
    }

    /// Send `close_notify` and stop. Further reads return end of file once the server answers.
    ///
    /// # Errors
    /// Transport failure.
    pub fn close(&mut self) -> Result<(), Error> {
        if !self.closed {
            self.closed = true;
            self.write_record(ALERT, &[1, 0])?;
            self.stream.flush()?;
        }
        Ok(())
    }

    /// The underlying transport.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

fn alert_error(data: &[u8]) -> Error {
    match data {
        [_, desc] => Error::Alert(*desc),
        _ => Error::Protocol("malformed alert"),
    }
}

/// Verify CertificateVerify against the leaf key, enforcing that the algorithm matches the key.
fn check_handshake_signature(
    key: &PublicKey,
    alg: u16,
    signed: &[u8],
    sig: &[u8],
) -> Result<(), Error> {
    use aeon_pki::ecdsa::Curve;
    let sig_alg = match (alg, key) {
        (msg::SIG_ECDSA_P256_SHA256, PublicKey::Ec(k)) if k.curve() == Curve::P256 => {
            SigAlg::Ecdsa(Hash::Sha256)
        }
        (msg::SIG_ECDSA_P384_SHA384, PublicKey::Ec(k)) if k.curve() == Curve::P384 => {
            SigAlg::Ecdsa(Hash::Sha384)
        }
        (msg::SIG_RSA_PSS_SHA256, PublicKey::Rsa(_)) => SigAlg::RsaPss(Hash::Sha256, 32),
        (msg::SIG_RSA_PSS_SHA384, PublicKey::Rsa(_)) => SigAlg::RsaPss(Hash::Sha384, 48),
        _ => return Err(Error::Protocol("signature algorithm does not fit the key")),
    };
    if key.verify(sig_alg, signed, sig) {
        Ok(())
    } else {
        Err(Error::BadSignature("server handshake signature is wrong"))
    }
}

impl<S: Read + Write> Read for Connection<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || !self.finished {
            return Ok(0);
        }
        while self.plain_pos >= self.plain.len() {
            self.plain.clear();
            self.plain_pos = 0;
            let (ty, data) = match self.read_record() {
                Ok(r) => r,
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed without a TLS close_notify",
                    ));
                }
                Err(e) => return Err(e.into()),
            };
            match ty {
                APPLICATION_DATA => self.plain = data,
                HANDSHAKE => self.post_handshake(&data)?,
                ALERT => {
                    return match data.as_slice() {
                        [_, 0] => Ok(0), // close_notify
                        _ => Err(alert_error(&data).into()),
                    };
                }
                _ => return Err(Error::Protocol("unexpected record type").into()),
            }
        }
        let n = buf.len().min(self.plain.len() - self.plain_pos);
        buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
        self.plain_pos += n;
        Ok(n)
    }
}

impl<S: Read + Write> Write for Connection<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "connection closed",
            ));
        }
        let n = buf.len().min(MAX_PLAINTEXT);
        if n > 0 {
            self.write_record(APPLICATION_DATA, &buf[..n])?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
