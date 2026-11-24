//! Real sockets: a loopback server written here, and `openssl s_server` for HTTPS.

use mouli_http::{Client, Error, Request, Url};
use mouli_pki::pem;
use mouli_pki::verify::TrustStore;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn read_request(s: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        buf.push(b[0]);
    }
    let head = String::from_utf8(buf).unwrap();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .map_or(0, |v| v.parse().unwrap());
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).unwrap();
    (head, body)
}

#[test]
fn plain_loopback_chunked_and_post() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut s, _) = listener.accept().unwrap();
            let (head, body) = read_request(&mut s);
            if head.starts_with("POST /echo") {
                assert!(head.contains("Authorization: Bearer secret\r\n"));
                assert!(head.contains("Connection: close\r\n"));
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                s.write_all(reply.as_bytes()).unwrap();
                s.write_all(&body).unwrap();
            } else {
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .unwrap();
                for part in ["data: one\n\n", "data: two\n\n", "data: [DONE]\n\n"] {
                    s.write_all(format!("{:x}\r\n{part}\r\n", part.len()).as_bytes())
                        .unwrap();
                    s.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(30));
                }
                s.write_all(b"0\r\n\r\n").unwrap();
            }
        }
    });
    let client = Client::new(TrustStore::new());
    let url = |p: &str| Url::parse(&format!("http://127.0.0.1:{port}{p}")).unwrap();
    let mut r = client
        .request(
            &Request::post(url("/echo"), "text/plain", b"ping".to_vec())
                .header("Authorization", "Bearer secret"),
        )
        .unwrap();
    assert_eq!(r.read_all(100).unwrap(), b"ping");
    let mut r = client.request(&Request::get(url("/stream"))).unwrap();
    // events arrive incrementally
    let mut first = [0u8; 11];
    r.body.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"data: one\n\n");
    assert_eq!(r.read_all(1000).unwrap(), b"data: two\n\ndata: [DONE]\n\n");
    server.join().unwrap();
}

#[test]
fn plain_http_to_the_internet_is_refused_before_connecting() {
    let client = Client::new(TrustStore::new());
    let r = Request::get(Url::parse("http://example.com/").unwrap());
    assert!(matches!(client.request(&r), Err(Error::Url(_))));
}

struct Tls {
    dir: PathBuf,
    child: Child,
    port: u16,
}

fn openssl(dir: &PathBuf, args: &[&str]) -> bool {
    Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

impl Tls {
    fn start() -> Option<Self> {
        if !Command::new("openssl")
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return None;
        }
        let dir = std::env::temp_dir().join(format!("mouli-http-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(
            dir.join("leaf.ext"),
            "basicConstraints=CA:FALSE\nsubjectAltName=DNS:localhost\n",
        )
        .ok()?;
        let steps: [&[&str]; 5] = [
            &[
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
                "root.key",
            ],
            &[
                "req",
                "-x509",
                "-new",
                "-key",
                "root.key",
                "-subj",
                "/CN=MOULI http root",
                "-days",
                "30",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-out",
                "root.pem",
            ],
            &[
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
                "leaf.key",
            ],
            &[
                "req",
                "-new",
                "-key",
                "leaf.key",
                "-subj",
                "/CN=localhost",
                "-out",
                "leaf.csr",
            ],
            &[
                "x509",
                "-req",
                "-in",
                "leaf.csr",
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
                "leaf.pem",
            ],
        ];
        for s in steps {
            assert!(openssl(&dir, s), "{s:?}");
        }
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new("openssl")
            .current_dir(&dir)
            .args([
                "s_server",
                "-accept",
                &port.to_string(),
                "-cert",
                "leaf.pem",
                "-key",
                "leaf.key",
                "-tls1_3",
                "-ciphersuites",
                "TLS_CHACHA20_POLY1305_SHA256",
                "-quiet",
                "-www",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let t = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(100));
        Some(Self { dir, child, port })
    }

    fn client(&self) -> Client {
        let text = std::fs::read_to_string(self.dir.join("root.pem")).unwrap();
        let mut t = TrustStore::new();
        for d in pem::blocks(&text, "CERTIFICATE") {
            t.add_der(&d).unwrap();
        }
        Client::new(t)
    }
}

impl Drop for Tls {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn https_get_through_our_own_tls() {
    let Some(s) = Tls::start() else {
        eprintln!("openssl not installed; skipping");
        return;
    };
    let req = Request::get(Url::parse(&format!("https://localhost:{}/", s.port)).unwrap());
    let mut r = s.client().request(&req).unwrap();
    assert_eq!(r.status, 200);
    let body = String::from_utf8_lossy(&r.read_all(1 << 20).unwrap()).into_owned();
    assert!(body.contains("TLS_CHACHA20_POLY1305_SHA256"), "{body}");
}

#[test]
fn https_with_the_wrong_trust_store_fails() {
    let Some(s) = Tls::start() else {
        return;
    };
    let req = Request::get(Url::parse(&format!("https://localhost:{}/", s.port)).unwrap());
    let err = Client::new(TrustStore::new())
        .request(&req)
        .err()
        .expect("must fail");
    assert!(matches!(err, Error::Tls(_)), "{err}");
}
