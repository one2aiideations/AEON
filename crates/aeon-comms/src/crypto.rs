//! Session keys and sealed messages.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use core::fmt;
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// Crypto errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// Authentication failed (tampered, wrong key, or wrong header).
    Open,
    /// Counter already seen or too old.
    Replay,
    /// Peer key is a low-order point.
    BadPeer,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "message failed authentication",
            Self::Replay => "replayed message",
            Self::BadPeer => "invalid peer key",
        })
    }
}

impl std::error::Error for CryptoError {}

/// A device identity key pair.
pub struct Identity {
    secret: StaticSecret,
    public: PublicKey,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("public", &self.public.as_bytes())
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Identity from 32 secret bytes (from the OS random source in production).
    #[must_use]
    pub fn from_secret(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    /// Public key to share.
    #[must_use]
    pub fn public(&self) -> [u8; 32] {
        *self.public.as_bytes()
    }

    /// Session with `peer`. Both sides derive the same keys; each direction
    /// uses its own key.
    pub fn session(&self, peer: [u8; 32]) -> Result<Session, CryptoError> {
        let peer_pk = PublicKey::from(peer);
        let shared = self.secret.diffie_hellman(&peer_pk);
        if !shared.was_contributory() {
            return Err(CryptoError::BadPeer);
        }
        let me = self.public();
        let (lo, hi) = if me < peer { (me, peer) } else { (peer, me) };
        let hk = Hkdf::<Sha256>::new(Some(b"aeon-comms-v1"), shared.as_bytes());
        let mut okm = [0u8; 64];
        let mut info = Vec::with_capacity(64);
        info.extend_from_slice(&lo);
        info.extend_from_slice(&hi);
        hk.expand(&info, &mut okm).map_err(|_| CryptoError::BadPeer)?;
        let (k_lo, k_hi) = okm.split_at(32);
        let (send, recv) = if me == lo { (k_lo, k_hi) } else { (k_hi, k_lo) };
        Ok(Session {
            send: ChaCha20Poly1305::new(Key::from_slice(send)),
            recv: ChaCha20Poly1305::new(Key::from_slice(recv)),
            send_ctr: 0,
            recv_window: Window::default(),
        })
    }
}

/// Sliding replay window over the last 64 counters.
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    top: u64,
    bits: u64,
    any: bool,
}

impl Window {
    fn check_and_set(&mut self, c: u64) -> bool {
        if !self.any {
            self.any = true;
            self.top = c;
            self.bits = 1;
            return true;
        }
        if c > self.top {
            let shift = c - self.top;
            self.bits = if shift >= 64 { 0 } else { self.bits << shift };
            self.bits |= 1;
            self.top = c;
            return true;
        }
        let back = self.top - c;
        if back >= 64 || (self.bits >> back) & 1 == 1 {
            return false;
        }
        self.bits |= 1 << back;
        true
    }
}

/// A sealed message: counter plus ciphertext. The header is authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// Message counter (also the nonce).
    pub counter: u64,
    /// Ciphertext with tag.
    pub ciphertext: Vec<u8>,
}

/// Keys for one peer.
pub struct Session {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_ctr: u64,
    recv_window: Window,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("send_ctr", &self.send_ctr)
            .finish_non_exhaustive()
    }
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    n
}

impl Session {
    /// Encrypt `plaintext`, binding `header` (e.g. conversation id and sender).
    pub fn seal(&mut self, header: &[u8], plaintext: &[u8]) -> Result<Sealed, CryptoError> {
        let counter = self.send_ctr;
        self.send_ctr += 1;
        let n = nonce(counter);
        let mut aad = header.to_vec();
        aad.extend_from_slice(&counter.to_be_bytes());
        let ciphertext = self
            .send
            .encrypt(
                Nonce::from_slice(&n),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Open)?;
        Ok(Sealed {
            counter,
            ciphertext,
        })
    }

    /// Decrypt and reject replays.
    pub fn open(&mut self, header: &[u8], m: &Sealed) -> Result<Vec<u8>, CryptoError> {
        let n = nonce(m.counter);
        let mut aad = header.to_vec();
        aad.extend_from_slice(&m.counter.to_be_bytes());
        let plain = self
            .recv
            .decrypt(
                Nonce::from_slice(&n),
                Payload {
                    msg: &m.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Open)?;
        if !self.recv_window.check_and_set(m.counter) {
            return Err(CryptoError::Replay);
        }
        Ok(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Session, Session) {
        let a = Identity::from_secret([1; 32]);
        let b = Identity::from_secret([2; 32]);
        (a.session(b.public()).unwrap(), b.session(a.public()).unwrap())
    }

    #[test]
    fn round_trip_both_directions() {
        let (mut a, mut b) = pair();
        let m = a.seal(b"chat:1|alice", b"hello bob").unwrap();
        assert_eq!(b.open(b"chat:1|alice", &m).unwrap(), b"hello bob");
        let r = b.seal(b"chat:1|bob", b"hi").unwrap();
        assert_eq!(a.open(b"chat:1|bob", &r).unwrap(), b"hi");
        // Directions use different keys: a cannot open its own message.
        let own = a.seal(b"h", b"x").unwrap();
        assert_eq!(a.open(b"h", &own), Err(CryptoError::Open));
    }

    #[test]
    fn tamper_header_and_replay() {
        let (mut a, mut b) = pair();
        let m = a.seal(b"hdr", b"pay 10").unwrap();
        let mut bad = m.clone();
        bad.ciphertext[0] ^= 1;
        assert_eq!(b.open(b"hdr", &bad), Err(CryptoError::Open));
        assert_eq!(b.open(b"other", &m), Err(CryptoError::Open));
        assert!(b.open(b"hdr", &m).is_ok());
        assert_eq!(b.open(b"hdr", &m), Err(CryptoError::Replay));
        // Out-of-order delivery inside the window is fine.
        let m1 = a.seal(b"hdr", b"1").unwrap();
        let m2 = a.seal(b"hdr", b"2").unwrap();
        assert!(b.open(b"hdr", &m2).is_ok());
        assert!(b.open(b"hdr", &m1).is_ok());
    }

    #[test]
    fn low_order_peer_rejected() {
        let a = Identity::from_secret([1; 32]);
        assert_eq!(a.session([0; 32]).unwrap_err(), CryptoError::BadPeer);
    }
}
