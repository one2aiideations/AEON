//! Talk to a real `openssl s_server` over loopback. Skipped when openssl is not installed.

use aeon_pki::pem;
use aeon_pki::verify::{Host, TrustStore};
use aeon_tls::{os_random, Config, Connection, Error};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn openssl(dir: &Path, args: &[&str]) -> bool {
    let out = Command::new("openssl").current_dir(dir).args(args).output();
    match out {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!(
                "openssl {args:?} failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            false
        }
        Err(e) => {
            eprintln!("openssl {args:?}: {e}");
            false
        }
    }
}

struct Pki {
    dir: PathBuf,
}

impl Pki {
    /// A root and two leaves (EC P-256 and RSA 2048) for `localhost`, plus an unrelated root.
    fn new(tag: &str) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!("aeon-tls-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let d = dir.as_path();
        std::fs::write(d.join("ca.ext"), "basicConstraints=critical,CA:TRUE\n").ok()?;
        std::fs::write(
            d.join("leaf.ext"),
            "basicConstraints=CA:FALSE\nsubjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n",
        )
        .ok()?;
        let mut ok = true;
        for root in ["root", "other"] {
            ok &= openssl(
                d,
                &[
                    "ecparam",
                    "-name",
                    "prime256v1",
                    "-genkey",
                    "-noout",
                    "-out",
                    &format!("{root}.key"),
                ],
            );
            ok &= openssl(
                d,
                &[
                    "req",
                    "-x509",
                    "-new",
                    "-key",
                    &format!("{root}.key"),
                    "-subj",
                    &format!("/CN=AEON {root}"),
                    "-days",
                    "30",
                    "-extensions",
                    "v3_ca",
                    "-addext",
                    "basicConstraints=critical,CA:TRUE",
                    "-out",
                    &format!("{root}.pem"),
                ],
            );
        }
        for (name, gen) in [
            (
                "ec",
                vec![
                    "ecparam",
                    "-name",
                    "prime256v1",
                    "-genkey",
                    "-noout",
                    "-out",
                    "ec.key",
                ],
            ),
            ("rsa", vec!["genrsa", "-out", "rsa.key", "2048"]),
        ] {
            ok &= openssl(d, &gen);
            ok &= openssl(
                d,
                &[
                    "req",
                    "-new",
                    "-key",
                    &format!("{name}.key"),
                    "-subj",
                    "/CN=localhost",
                    "-out",
                    &format!("{name}.csr"),
                ],
            );
            ok &= openssl(
                d,
                &[
                    "x509",
                    "-req",
                    "-in",
                    &format!("{name}.csr"),
                    "-CA",
                    "root.pem",
                    "-CAkey",
                    "root.key",
                    "-CAcreateserial",
                    "-days",
                    "30",
                    "-extfile",
                    "leaf.ext",
                    "-out",
                    &format!("{name}.pem"),
                ],
            );
        }
        ok.then_some(Self { dir })
    }

    fn trust(&self, root: &str) -> TrustStore {
        let text = std::fs::read_to_string(self.dir.join(format!("{root}.pem"))).unwrap();
        let mut t = TrustStore::new();
        for der in pem::blocks(&text, "CERTIFICATE") {
            t.add_der(&der).unwrap();
        }
        t
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(pki: &Pki, leaf: &str, extra: &[&str]) -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new("openssl")
            .current_dir(&pki.dir)
            .args([
                "s_server",
                "-accept",
                &port.to_string(),
                "-cert",
                &format!("{leaf}.pem"),
                "-key",
                &format!("{leaf}.key"),
                "-tls1_3",
                "-ciphersuites",
                "TLS_CHACHA20_POLY1305_SHA256",
                "-quiet",
            ])
            .args(if std::env::var("AEON_TLS_DEBUG").is_ok() {
                vec!["-msg"]
            } else {
                vec![]
            })
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(if std::env::var("AEON_TLS_DEBUG").is_ok() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if std::env::var("AEON_TLS_DEBUG").is_ok() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .unwrap();
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // That probe connection made s_server handle and drop one client; give it a moment.
        std::thread::sleep(Duration::from_millis(100));
        Self { child, port }
    }

    fn connect(&self, trust: TrustStore, host: &str) -> Result<Connection<TcpStream>, Error> {
        let tcp = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        Connection::connect(
            tcp,
            &Config {
                host: Host::parse(host),
                trust,
                now,
                random: os_random,
            },
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn have_openssl() -> bool {
    Command::new("openssl")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn fetch(conn: &mut Connection<TcpStream>, path: &str) -> Vec<u8> {
    conn.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .unwrap();
    let mut out = Vec::new();
    conn.read_to_end(&mut out).unwrap();
    out
}

#[test]
fn ecdsa_and_rsa_servers() {
    if !have_openssl() {
        eprintln!("openssl not installed; skipping");
        return;
    }
    let pki = Pki::new("ok").expect("test PKI");
    for leaf in ["ec", "rsa"] {
        let server = Server::start(&pki, leaf, &["-www"]);
        let mut conn = server
            .connect(pki.trust("root"), "localhost")
            .unwrap_or_else(|e| panic!("{leaf}: {e}"));
        let reply = fetch(&mut conn, "/");
        let text = String::from_utf8_lossy(&reply);
        assert!(text.starts_with("HTTP/1.0 200"), "{leaf}: {text}");
        assert!(
            text.contains("TLS_CHACHA20_POLY1305_SHA256"),
            "{leaf}: {text}"
        );
    }
}

#[test]
fn large_transfer_and_key_update() {
    if !have_openssl() {
        return;
    }
    let pki = Pki::new("big").expect("test PKI");
    let data: Vec<u8> = (0..1_500_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
    std::fs::write(pki.dir.join("big.bin"), &data).unwrap();
    let server = Server::start(&pki, "ec", &["-WWW"]);
    let mut conn = server.connect(pki.trust("root"), "localhost").unwrap();
    // Rotate keys in both directions before and after the exchange.
    conn.update_keys().unwrap();
    conn.write_all(b"GET /big.bin HTTP/1.0\r\n\r\n").unwrap();
    let mut got = Vec::new();
    conn.read_to_end(&mut got).unwrap();
    let start = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(got.len() - start, data.len());
    assert!(got[start..] == data[..]);
}

#[test]
fn wrong_host_and_untrusted_root_are_refused() {
    if !have_openssl() {
        return;
    }
    let pki = Pki::new("bad").expect("test PKI");
    let server = Server::start(&pki, "ec", &["-www"]);
    let err = server
        .connect(pki.trust("root"), "other.example")
        .err()
        .expect("must fail");
    assert!(
        matches!(
            err,
            Error::Certificate(aeon_pki::verify::VerifyError::NameMismatch)
        ),
        "{err}"
    );
    let server = Server::start(&pki, "ec", &["-www"]);
    let err = server
        .connect(pki.trust("other"), "localhost")
        .err()
        .expect("must fail");
    assert!(
        matches!(
            err,
            Error::Certificate(aeon_pki::verify::VerifyError::UnknownIssuer)
        ),
        "{err}"
    );
    let server = Server::start(&pki, "ec", &["-www"]);
    let err = server
        .connect(TrustStore::new(), "localhost")
        .err()
        .expect("must fail");
    assert!(matches!(err, Error::Certificate(_)), "{err}");
}

#[test]
fn a_server_that_cannot_do_chacha_is_refused() {
    if !have_openssl() {
        return;
    }
    let pki = Pki::new("aes").expect("test PKI");
    let server = Server::start(&pki, "ec", &["-ciphersuites", "TLS_AES_128_GCM_SHA256"]);
    assert!(server.connect(pki.trust("root"), "localhost").is_err());
}
