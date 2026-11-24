//! Request writing and response parsing.

use crate::url::Url;
use crate::Error;
use std::io::{self, BufRead, BufReader, Read, Write};

const MAX_LINE: usize = 16 * 1024;
const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 100;

/// An HTTP request.
#[derive(Clone, Debug)]
pub struct Request {
    /// Method, such as `GET` or `POST`.
    pub method: String,
    /// Target.
    pub url: Url,
    /// Extra headers (`Host`, `Content-Length`, `Connection` and the like are added for you).
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl Request {
    /// A GET request.
    #[must_use]
    pub fn get(url: Url) -> Self {
        Self {
            method: "GET".into(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A POST request with a body and content type.
    #[must_use]
    pub fn post(url: Url, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            method: "POST".into(),
            url,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
        }
    }

    /// Add a header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        let token = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        };
        if !token(&self.method) {
            return Err(Error::BadRequest("bad method"));
        }
        let mut out = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\n",
            self.method,
            self.url.path,
            self.url.host_header()
        );
        for (n, v) in &self.headers {
            if !token(n) {
                return Err(Error::BadRequest("bad header name"));
            }
            if v.bytes().any(|b| (b < b' ' && b != b'\t') || b == 0x7F) {
                return Err(Error::BadRequest("control character in header value"));
            }
            let lower = n.to_ascii_lowercase();
            if ["host", "content-length", "transfer-encoding", "connection"]
                .contains(&lower.as_str())
            {
                return Err(Error::BadRequest("that header is set by the client"));
            }
            out.push_str(&format!("{n}: {v}\r\n"));
        }
        out.push_str("Connection: close\r\nAccept-Encoding: identity\r\n");
        if !self.body.is_empty() || self.method == "POST" || self.method == "PUT" {
            out.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        out.push_str("\r\n");
        let mut bytes = out.into_bytes();
        bytes.extend(&self.body);
        Ok(bytes)
    }
}

/// How the body is delimited.
#[derive(Debug)]
enum Framing {
    Empty,
    Length(u64),
    Chunked {
        left: u64,
        need_crlf: bool,
        done: bool,
    },
    UntilClose,
}

/// The response body as a stream. Truncated bodies are errors, not silent short reads.
pub struct Body<S> {
    inner: BufReader<S>,
    framing: Framing,
}

/// A parsed response head plus a streaming body.
pub struct Response<S> {
    /// Status code.
    pub status: u16,
    /// Reason phrase (may be empty).
    pub reason: String,
    /// Headers in order, names as sent.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body<S>,
}

impl<S> Response<S> {
    /// First header named `name`, ignoring case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl<S: Read> Response<S> {
    /// Read the whole body, refusing more than `limit` bytes.
    ///
    /// # Errors
    /// Transport errors, truncation, or a body over the limit.
    pub fn read_all(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        (&mut self.body)
            .take(limit as u64 + 1)
            .read_to_end(&mut out)?;
        if out.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response body too large",
            ));
        }
        Ok(out)
    }
}

fn read_line<R: BufRead>(r: &mut R, limit: usize) -> Result<Vec<u8>, Error> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Err(Error::Protocol(
                "connection closed inside the response head",
            ));
        }
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                line.extend(&buf[..=i]);
                r.consume(i + 1);
                break;
            }
            None => {
                line.extend(buf);
                let n = buf.len();
                r.consume(n);
            }
        }
        if line.len() > limit {
            return Err(Error::TooLarge);
        }
    }
    if line.len() > limit {
        return Err(Error::TooLarge);
    }
    if !line.ends_with(b"\r\n") {
        return Err(Error::Protocol("line ends without CRLF"));
    }
    line.truncate(line.len() - 2);
    if line.contains(&b'\r') || line.contains(&0) {
        return Err(Error::Protocol("stray control character"));
    }
    Ok(line)
}

/// Send `req` on `stream` and parse the response head. The body streams from the same stream.
///
/// # Errors
/// [`Error`] for bad requests, transport failures and malformed responses.
pub fn send<S: Read + Write>(mut stream: S, req: &Request) -> Result<Response<S>, Error> {
    stream.write_all(&req.encode()?)?;
    stream.flush()?;
    let mut r = BufReader::new(stream);
    let mut head_bytes = 0;
    // Skip interim 1xx responses (except 101, which is not something we ask for).
    let (status, reason, headers) = loop {
        let line = read_line(&mut r, MAX_LINE)?;
        head_bytes += line.len();
        let (status, reason) = parse_status(&line)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let l = read_line(&mut r, MAX_LINE)?;
            head_bytes += l.len();
            if head_bytes > MAX_HEAD || headers.len() > MAX_HEADERS {
                return Err(Error::TooLarge);
            }
            if l.is_empty() {
                break;
            }
            headers.push(parse_header(&l)?);
        }
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        break (status, reason, headers);
    };
    let framing = framing(&req.method, status, &headers)?;
    Ok(Response {
        status,
        reason,
        headers,
        body: Body { inner: r, framing },
    })
}

fn parse_status(line: &[u8]) -> Result<(u16, String), Error> {
    let s = std::str::from_utf8(line).map_err(|_| Error::Protocol("status line is not text"))?;
    let rest = s
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| s.strip_prefix("HTTP/1.0 "))
        .ok_or(Error::Protocol("not an HTTP/1.x response"))?;
    let code = rest.get(..3).ok_or(Error::Protocol("bad status code"))?;
    if !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Protocol("bad status code"));
    }
    let reason = match rest.get(3..) {
        None | Some("") => "",
        Some(r) => r
            .strip_prefix(' ')
            .ok_or(Error::Protocol("bad status line"))?,
    };
    let status: u16 = code
        .parse()
        .map_err(|_| Error::Protocol("bad status code"))?;
    if !(100..=599).contains(&status) {
        return Err(Error::Protocol("status out of range"));
    }
    Ok((status, reason.to_string()))
}

fn parse_header(line: &[u8]) -> Result<(String, String), Error> {
    if line[0] == b' ' || line[0] == b'\t' {
        return Err(Error::Protocol("folded header"));
    }
    let colon = line
        .iter()
        .position(|&b| b == b':')
        .ok_or(Error::Protocol("header without colon"))?;
    let (name, value) = (&line[..colon], &line[colon + 1..]);
    if name.is_empty()
        || !name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Err(Error::Protocol("bad header name"));
    }
    let value =
        std::str::from_utf8(value).map_err(|_| Error::Protocol("header value is not text"))?;
    Ok((
        String::from_utf8_lossy(name).into_owned(),
        value.trim_matches([' ', '\t']).to_string(),
    ))
}

fn framing(method: &str, status: u16, headers: &[(String, String)]) -> Result<Framing, Error> {
    let get = |n: &str| -> Vec<&str> {
        headers
            .iter()
            .filter(|(h, _)| h.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.as_str())
            .collect()
    };
    let te = get("transfer-encoding");
    let cl = get("content-length");
    if method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status) {
        return Ok(Framing::Empty);
    }
    if !te.is_empty() {
        // A length alongside chunking is the classic smuggling setup: refuse it.
        if !cl.is_empty() {
            return Err(Error::Protocol("both Transfer-Encoding and Content-Length"));
        }
        let all = te.join(",").to_ascii_lowercase();
        if all.trim() != "chunked" {
            return Err(Error::Protocol("unsupported Transfer-Encoding"));
        }
        return Ok(Framing::Chunked {
            left: 0,
            need_crlf: false,
            done: false,
        });
    }
    if cl.is_empty() {
        return Ok(Framing::UntilClose);
    }
    let mut value: Option<u64> = None;
    for part in cl.iter().flat_map(|v| v.split(',')) {
        let p = part.trim();
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Protocol("bad Content-Length"));
        }
        let n: u64 = p
            .parse()
            .map_err(|_| Error::Protocol("bad Content-Length"))?;
        if value.is_some_and(|v| v != n) {
            return Err(Error::Protocol("conflicting Content-Length"));
        }
        value = Some(n);
    }
    Ok(Framing::Length(value.unwrap_or(0)))
}

fn truncated() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "response body ended early")
}

fn to_io(e: Error) -> io::Error {
    match e {
        Error::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

impl<S: Read> Read for Body<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.framing {
            Framing::Empty => Ok(0),
            Framing::UntilClose => self.inner.read(buf),
            Framing::Length(left) => {
                if *left == 0 {
                    return Ok(0);
                }
                let max = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                let n = self.inner.read(&mut buf[..max])?;
                if n == 0 {
                    return Err(truncated());
                }
                *left -= n as u64;
                Ok(n)
            }
            Framing::Chunked {
                left,
                need_crlf,
                done,
            } => loop {
                if *done {
                    return Ok(0);
                }
                if *left > 0 {
                    let max = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                    let n = self.inner.read(&mut buf[..max])?;
                    if n == 0 {
                        return Err(truncated());
                    }
                    *left -= n as u64;
                    *need_crlf = *left == 0;
                    return Ok(n);
                }
                if *need_crlf {
                    if !read_line(&mut self.inner, 2).map_err(to_io)?.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "chunk not followed by CRLF",
                        ));
                    }
                    *need_crlf = false;
                }
                let line = read_line(&mut self.inner, 1024).map_err(to_io)?;
                let text = std::str::from_utf8(&line)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
                let size_text = text.split(';').next().unwrap_or("").trim();
                if size_text.is_empty()
                    || size_text.len() > 15
                    || !size_text.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"));
                }
                let size = u64::from_str_radix(size_text, 16)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
                if size == 0 {
                    // Trailers, then the final blank line.
                    let mut total = 0;
                    loop {
                        let t = read_line(&mut self.inner, MAX_LINE).map_err(to_io)?;
                        if t.is_empty() {
                            break;
                        }
                        total += t.len();
                        if total > MAX_HEAD {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "trailers too large",
                            ));
                        }
                    }
                    *done = true;
                    return Ok(0);
                }
                *left = size;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A stream that replays a canned response and records what was written.
    struct Fake {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Fake {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.input.read(b)
        }
    }

    impl Write for Fake {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.extend(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn run(reply: &[u8], req: &Request) -> Result<Response<Fake>, Error> {
        send(
            Fake {
                input: Cursor::new(reply.to_vec()),
                output: Vec::new(),
            },
            req,
        )
    }

    fn get() -> Request {
        Request::get(Url::parse("http://127.0.0.1:9/x").unwrap())
    }

    fn body(r: &mut Response<Fake>) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        r.body.read_to_end(&mut v)?;
        Ok(v)
    }

    #[test]
    fn writes_a_proper_request() {
        let req = Request::post(
            Url::parse("https://a.test/v1").unwrap(),
            "application/json",
            b"{}".to_vec(),
        )
        .header("Authorization", "Bearer k");
        let wire = String::from_utf8(req.encode().unwrap()).unwrap();
        assert!(wire.starts_with("POST /v1 HTTP/1.1\r\nHost: a.test\r\n"));
        assert!(wire.contains("Authorization: Bearer k\r\n"));
        assert!(wire.contains("Content-Length: 2\r\n"));
        assert!(wire.ends_with("\r\n\r\n{}"));
    }

    #[test]
    fn refuses_header_injection() {
        let u = Url::parse("https://a.test/").unwrap();
        for (n, v) in [
            ("X", "a\r\nEvil: 1"),
            ("X Y", "v"),
            ("", "v"),
            ("Host", "evil"),
            ("Content-Length", "1"),
        ] {
            assert!(
                Request::get(u.clone()).header(n, v).encode().is_err(),
                "{n}"
            );
        }
        let mut r = Request::get(u);
        r.method = "GE T".into();
        assert!(r.encode().is_err());
    }

    #[test]
    fn content_length_body() {
        let mut r = run(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A:  b \r\n\r\nhelloEXTRA",
            &get(),
        )
        .unwrap();
        assert_eq!(
            (r.status, r.reason.as_str(), r.header("x-a")),
            (200, "OK", Some("b"))
        );
        assert_eq!(body(&mut r).unwrap(), b"hello");
    }

    #[test]
    fn truncated_length_is_an_error() {
        let mut r = run(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello",
            &get(),
        )
        .unwrap();
        assert_eq!(
            body(&mut r).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn chunked_body_with_extensions_and_trailers() {
        let reply = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n6\r\n world\r\n0\r\nTrailer: x\r\n\r\n";
        let mut r = run(reply, &get()).unwrap();
        assert_eq!(body(&mut r).unwrap(), b"hello world");
    }

    #[test]
    fn bad_or_truncated_chunks() {
        for reply in [
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhi\r\n0\r\n\r\n"[..],
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n99999999999999999999\r\n",
        ] {
            let mut r = run(reply, &get()).unwrap();
            assert!(body(&mut r).is_err(), "{}", String::from_utf8_lossy(reply));
        }
    }

    #[test]
    fn until_close_and_no_body_cases() {
        let mut r = run(b"HTTP/1.0 200 OK\r\n\r\nall of it", &get()).unwrap();
        assert_eq!(body(&mut r).unwrap(), b"all of it");
        let mut r = run(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 4\r\n\r\nxxxx",
            &get(),
        )
        .unwrap();
        assert!(body(&mut r).unwrap().is_empty());
        let mut head = get();
        head.method = "HEAD".into();
        let mut r = run(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n", &head).unwrap();
        assert!(body(&mut r).unwrap().is_empty());
    }

    #[test]
    fn interim_responses_are_skipped() {
        let mut r = run(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            &get(),
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(body(&mut r).unwrap(), b"ok");
    }

    #[test]
    fn malformed_heads_are_refused() {
        for reply in [
            &b"HTTP/2 200 OK\r\n\r\n"[..],
            b"HTTP/1.1 20 OK\r\n\r\n",
            b"HTTP/1.1 200OK\r\n\r\n",
            b"HTTP/1.1 200 OK\nContent-Length: 0\n\n",
            b"HTTP/1.1 200 OK\r\nBad Header: x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nNoColon\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nA: b\r\n folded\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nA: b\r\n",
            b"",
        ] {
            assert!(
                run(reply, &get()).is_err(),
                "{}",
                String::from_utf8_lossy(reply)
            );
        }
        // identical duplicate lengths are fine
        assert!(run(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",
            &get()
        )
        .is_ok());
    }

    #[test]
    fn oversized_heads_are_refused() {
        let mut reply = b"HTTP/1.1 200 OK\r\n".to_vec();
        for i in 0..200 {
            reply.extend(format!("H{i}: v\r\n").as_bytes());
        }
        reply.extend(b"\r\n");
        assert!(matches!(run(&reply, &get()), Err(Error::TooLarge)));
        let mut long = b"HTTP/1.1 200 OK\r\nA: ".to_vec();
        long.extend(vec![b'x'; 40_000]);
        long.extend(b"\r\n\r\n");
        assert!(matches!(run(&long, &get()), Err(Error::TooLarge)));
    }

    #[test]
    fn read_all_enforces_a_limit() {
        let mut r = run(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789",
            &get(),
        )
        .unwrap();
        assert!(r.read_all(5).is_err());
        let mut r = run(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789",
            &get(),
        )
        .unwrap();
        assert_eq!(r.read_all(10).unwrap().len(), 10);
    }
}
