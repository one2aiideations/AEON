//! A Unix-socket server for the gateway: one request per line, events written back as lines.

use crate::gateway::{Backend, Gateway, MAX_LINE};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// Bind `path` (replacing a stale socket file) and restrict it to the owner (mode 0600), so
/// only the user running the gateway can talk to it.
///
/// # Errors
/// I/O errors from removing, binding or changing permissions.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Serve one connection until it closes. Over-long lines are refused with an `error` event
/// and the connection is closed.
///
/// # Errors
/// I/O errors on the stream.
pub fn serve_connection<B: Backend>(
    gw: &mut Gateway<B>,
    stream: UnixStream,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    loop {
        let mut raw = Vec::new();
        let n = (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut raw)?;
        if n == 0 {
            return Ok(());
        }
        if raw.len() > MAX_LINE {
            writer.write_all(
                b"{\"event\":\"error\",\"kind\":\"protocol\",\"message\":\"request line too long\"}\n",
            )?;
            return Ok(());
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut failed = None;
        gw.handle(line, &mut |ev| {
            if failed.is_none() {
                let r = writer
                    .write_all(ev.as_bytes())
                    .and_then(|()| writer.write_all(b"\n"))
                    .and_then(|()| writer.flush());
                if let Err(e) = r {
                    failed = Some(e);
                }
            }
        });
        if let Some(e) = failed {
            return Err(e);
        }
    }
}

/// Accept connections one after another until the listener fails.
///
/// A client that disconnects mid-reply only ends its own connection.
///
/// # Errors
/// The error from `accept`.
pub fn serve<B: Backend>(gw: &mut Gateway<B>, listener: &UnixListener) -> std::io::Result<()> {
    for stream in listener.incoming() {
        let _ = serve_connection(gw, stream?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeon_interface::policy::{Policy, Role as Perm};
    use aeon_interface::routing::{ModelEntry, Route};
    use aeon_interface::workflow::Limits;
    use aeon_interface::Workspace;

    struct Fixed;
    impl Backend for Fixed {
        fn complete(
            &mut self,
            _: &Route,
            _: &[(String, String)],
            _: &str,
            on: &mut dyn FnMut(&str),
        ) -> Result<String, String> {
            on("hi");
            Ok("hi".into())
        }
    }

    #[test]
    fn talks_over_a_socket() {
        let dir = std::env::temp_dir().join(format!("aeon-gw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gw.sock");
        let listener = bind(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut ws = Workspace::new(Policy::default(), Limits::default()).unwrap();
        ws.add_role(Perm::new("user", &["chat"]));
        ws.add_model(ModelEntry::new("m", "local", true, &["text"], 4096, 0, 1));
        let mut gw = Gateway::new(ws, Fixed);
        let p2 = path.clone();
        let client = std::thread::spawn(move || {
            let mut c = UnixStream::connect(p2).unwrap();
            c.write_all(b"{\"op\":\"open\",\"tenant\":\"t\",\"user\":\"u\",\"role\":\"user\"}\n")
                .unwrap();
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            let sid = crate::json::parse(&l)
                .unwrap()
                .str("session")
                .unwrap()
                .to_string();
            let say = format!(
                "{{\"op\":\"say\",\"tenant\":\"t\",\"session\":\"{sid}\",\"text\":\"yo\"}}\n"
            );
            c.write_all(say.as_bytes()).unwrap();
            let mut got = Vec::new();
            for _ in 0..2 {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                got.push(l);
            }
            // Oversized line is refused.
            let big = vec![b'a'; MAX_LINE + 10];
            let _ = c.write_all(&big);
            let mut l = String::new();
            let _ = r.read_line(&mut l);
            (got, l)
        });
        let (stream, _) = listener.accept().unwrap();
        let _ = serve_connection(&mut gw, stream);
        let (got, big) = client.join().unwrap();
        assert!(got[0].contains("\"delta\"") && got[1].contains("\"done\""));
        assert!(big.contains("too long"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
