//! Connecting: DNS, TCP, and TLS with the system trust store.

use crate::message::{send, Request, Response};
use crate::url::Scheme;
use crate::Error;
use mouli_pki::verify::{Host, TrustStore};
use mouli_tls::{os_random, Config, Connection};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default location of the CA bundle in the MOULI image.
pub const SYSTEM_CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The transport under a response: plain loopback TCP or TLS.
pub enum Transport {
    /// Plain TCP (loopback only).
    Plain(TcpStream),
    /// TLS over TCP.
    Tls(Box<Connection<TcpStream>>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(c) => c.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(c) => c.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(c) => c.flush(),
        }
    }
}

/// An HTTP client with a trust store and timeouts.
#[derive(Clone)]
pub struct Client {
    trust: TrustStore,
    /// How long to wait for a TCP connection.
    pub connect_timeout: Duration,
    /// How long to wait for any single read or write.
    pub io_timeout: Duration,
}

impl Client {
    /// A client trusting exactly `trust`.
    #[must_use]
    pub fn new(trust: TrustStore) -> Self {
        Self {
            trust,
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(120),
        }
    }

    /// A client trusting the certificates in a PEM bundle file.
    ///
    /// # Errors
    /// When the file cannot be read or holds no usable certificate.
    pub fn from_bundle(path: &str) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)?;
        let (trust, _skipped) = TrustStore::from_pem(&text);
        if trust.is_empty() {
            return Err(Error::Url("the CA bundle holds no usable certificate"));
        }
        Ok(Self::new(trust))
    }

    /// A client using [`SYSTEM_CA_BUNDLE`].
    ///
    /// # Errors
    /// See [`Client::from_bundle`].
    pub fn system() -> Result<Self, Error> {
        Self::from_bundle(SYSTEM_CA_BUNDLE)
    }

    /// Send `req` and return the response with its streaming body.
    ///
    /// # Errors
    /// Resolution, connection, TLS and protocol failures. Plain `http` to a non-loopback host
    /// is refused.
    pub fn request(&self, req: &Request) -> Result<Response<Transport>, Error> {
        let url = &req.url;
        if url.scheme == Scheme::Http && !url.is_loopback() {
            return Err(Error::Url(
                "plain http is only allowed to loopback addresses",
            ));
        }
        let mut last = None;
        let mut tcp = None;
        for addr in (url.host.as_str(), url.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, self.connect_timeout) {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let tcp =
            tcp.ok_or_else(|| {
                Error::Io(last.unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "no address for host")
                }))
            })?;
        tcp.set_read_timeout(Some(self.io_timeout))?;
        tcp.set_write_timeout(Some(self.io_timeout))?;
        let transport = match url.scheme {
            Scheme::Http => Transport::Plain(tcp),
            Scheme::Https => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
                let cfg = Config {
                    host: Host::parse(&url.host),
                    trust: self.trust.clone(),
                    now,
                    random: os_random,
                };
                Transport::Tls(Box::new(Connection::connect(tcp, &cfg)?))
            }
        };
        send(transport, req)
    }
}
