//! Minimal URL parsing for `http` and `https`.

use crate::Error;

/// URL scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Plain HTTP (loopback only).
    Http,
    /// HTTP over TLS.
    Https,
}

/// A parsed absolute URL without credentials or fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    /// Scheme.
    pub scheme: Scheme,
    /// Host name or IPv4 address, lower-case.
    pub host: String,
    /// Port, defaulted from the scheme.
    pub port: u16,
    /// Path and query, always starting with `/`.
    pub path: String,
}

impl Url {
    /// Parse `http://host[:port][/path][?query]` or the `https` form.
    ///
    /// # Errors
    /// [`Error::Url`] for other schemes, credentials, IPv6 literals, bad ports, or characters
    /// that could split a request.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let (scheme, rest, default_port) = if let Some(r) = text.strip_prefix("https://") {
            (Scheme::Https, r, 443)
        } else if let Some(r) = text.strip_prefix("http://") {
            (Scheme::Http, r, 80)
        } else {
            return Err(Error::Url("only http and https are supported"));
        };
        if text.bytes().any(|b| b <= b' ' || b == 0x7F || b == b'#') {
            return Err(Error::Url(
                "spaces, control characters and fragments are not allowed",
            ));
        }
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        if authority.contains('@') {
            return Err(Error::Url("credentials in URLs are not supported"));
        }
        if authority.starts_with('[') {
            return Err(Error::Url("IPv6 literals are not supported"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|_| Error::Url("bad port"))?;
                if port == 0 {
                    return Err(Error::Url("bad port"));
                }
                (h, port)
            }
            None => (authority, default_port),
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(Error::Url("bad host"));
        }
        Ok(Self {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
            path,
        })
    }

    /// Whether the host is a loopback name or address.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.host == "localhost" || self.host.starts_with("127.")
    }

    /// Value for the `Host` header.
    #[must_use]
    pub fn host_header(&self) -> String {
        let default = match self.scheme {
            Scheme::Http => 80,
            Scheme::Https => 443,
        };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let u = Url::parse("https://API.Example.com/v1/chat?x=1").unwrap();
        assert_eq!(u.scheme, Scheme::Https);
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("api.example.com", 443, "/v1/chat?x=1")
        );
        let u = Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!((u.port, u.path.as_str()), (8080, "/"));
        assert!(u.is_loopback());
        assert_eq!(u.host_header(), "127.0.0.1:8080");
        let u = Url::parse("https://h.test?q=1").unwrap();
        assert_eq!(u.path, "/?q=1");
        assert_eq!(u.host_header(), "h.test");
    }

    #[test]
    fn rejects_unsafe_urls() {
        for bad in [
            "ftp://x.test/",
            "https://u:p@x.test/",
            "https://x.test/a b",
            "https://x.test/a\r\nHost: evil",
            "https://x.test:0/",
            "https://x.test:99999/",
            "https://[::1]/",
            "https:///path",
            "https://x_y.test/",
            "https://x.test/#frag",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }
}
