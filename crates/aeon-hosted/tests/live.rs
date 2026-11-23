//! A fake provider on loopback: the client must send the key only in the header, stream tokens,
//! and report failures without leaking the key.

use aeon_hosted::{chat_stream, Error, Provider, Secret};
use aeon_http::Client;
use aeon_pki::verify::TrustStore;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

const KEY: &str = "sk-test-0123456789";

fn read_request(s: &mut TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        buf.push(b[0]);
    }
    let head = String::from_utf8(buf).unwrap();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .map_or(0, |v| v.parse().unwrap());
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).unwrap();
    (head, String::from_utf8(body).unwrap())
}

/// Serve one request with `reply`, returning the base URL and the request that was received.
fn serve(reply: Vec<u8>) -> (String, mpsc::Receiver<(String, String)>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let req = read_request(&mut s);
        tx.send(req).unwrap();
        s.write_all(&reply).unwrap();
    });
    (format!("http://127.0.0.1:{port}/v1"), rx)
}

fn provider(base: String) -> Provider {
    Provider {
        name: "fake".into(),
        base_url: base,
        model: "tiny-1".into(),
        key: Secret::new(KEY).unwrap(),
    }
}

fn client() -> Client {
    Client::new(TrustStore::new())
}

fn chunked(events: &[&str]) -> Vec<u8> {
    let mut r =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n"
            .to_vec();
    for e in events {
        r.extend(format!("{:x}\r\n{e}\r\n", e.len()).as_bytes());
    }
    r.extend(b"0\r\n\r\n");
    r
}

#[test]
fn streams_tokens_and_sends_the_key_only_in_the_header() {
    let (base, rx) = serve(chunked(&[
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\", world\"}}]}\n\ndata: [DONE]\n\n",
    ]));
    let mut pieces = Vec::new();
    let reply = chat_stream(
        &client(),
        &provider(base),
        &[("system", "be brief"), ("user", "hi")],
        32,
        &mut |t| pieces.push(t.to_string()),
    )
    .unwrap();
    assert_eq!(reply, "Hello, world");
    assert_eq!(pieces, ["Hello", ", world"]);
    let (head, body) = rx.recv().unwrap();
    assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(head.contains(&format!("Authorization: Bearer {KEY}\r\n")));
    assert!(!body.contains(KEY), "the key must not be in the body");
    assert!(body.contains("\"model\":\"tiny-1\"") && body.contains("\"stream\":true"));
    assert!(body.contains("be brief") && body.contains("\"max_tokens\":32"));
}

#[test]
fn rejected_key_is_reported_without_echoing_it() {
    let msg = format!("{{\"error\":{{\"message\":\"Incorrect API key provided: {KEY}\"}}}}");
    let reply = format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{msg}",
        msg.len()
    );
    let (base, _rx) = serve(reply.into_bytes());
    let err =
        chat_stream(&client(), &provider(base), &[("user", "x")], 8, &mut |_| {}).unwrap_err();
    match &err {
        Error::Api { status, message } => {
            assert_eq!(*status, 401);
            assert!(!message.contains(KEY), "{message}");
        }
        other => panic!("{other}"),
    }
    assert!(!format!("{err} {err:?}").contains(KEY));
    assert!(err.to_string().contains("rejected the API key"));
}

#[test]
fn non_streaming_servers_are_handled() {
    let body =
        "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"whole answer\"}}]}";
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (base, _rx) = serve(reply.into_bytes());
    let mut got = String::new();
    let r = chat_stream(&client(), &provider(base), &[("user", "x")], 8, &mut |t| {
        got.push_str(t)
    })
    .unwrap();
    assert_eq!((r.as_str(), got.as_str()), ("whole answer", "whole answer"));
}

#[test]
fn a_cut_off_stream_is_an_error() {
    // chunked body that ends cleanly but without [DONE] or a finish reason
    let (base, _rx) = serve(chunked(&[
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
    ]));
    let mut got = String::new();
    let err = chat_stream(&client(), &provider(base), &[("user", "x")], 8, &mut |t| {
        got.push_str(t)
    })
    .unwrap_err();
    assert!(matches!(err, Error::BadReply(_)), "{err}");
    assert_eq!(got, "Hel"); // what arrived was still delivered
                            // connection dropped in the middle of a chunk
    let mut broken = chunked(&["data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n"]);
    broken.truncate(broken.len() - 20);
    let (base, _rx) = serve(broken);
    assert!(chat_stream(&client(), &provider(base), &[("user", "x")], 8, &mut |_| {}).is_err());
}

#[test]
fn plain_http_to_a_remote_provider_is_refused_before_the_key_is_sent() {
    let p = Provider {
        name: "remote".into(),
        base_url: "http://api.example.com/v1".into(),
        model: "m".into(),
        key: Secret::new(KEY).unwrap(),
    };
    let err = chat_stream(&client(), &p, &[("user", "x")], 8, &mut |_| {}).unwrap_err();
    assert!(
        matches!(err, Error::Http(aeon_http::Error::Url(_))),
        "{err}"
    );
}
