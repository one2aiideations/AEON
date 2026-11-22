//! Page intents: actions a page offers through its forms.

use crate::html::{tokenize, Token};

/// What an action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Search the site.
    Search,
    /// Sign in.
    Login,
    /// Create an account.
    Signup,
    /// Buy or pay.
    Purchase,
    /// Subscribe to emails.
    Subscribe,
    /// Book or reserve.
    Book,
    /// Send a message.
    Contact,
    /// Anything else.
    Other,
}

impl Kind {
    /// Actions that commit the user or touch credentials or money.
    #[must_use]
    pub fn sensitive(self) -> bool {
        matches!(
            self,
            Self::Login | Self::Signup | Self::Purchase | Self::Subscribe | Self::Book | Self::Contact
        )
    }
}

/// A form field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// `name` (or `id`).
    pub name: String,
    /// Input type, lowercased (`text`, `password`, `hidden`, ...).
    pub kind: String,
    /// Default value.
    pub value: String,
}

impl Field {
    /// Field holds a secret the agent must never fill.
    #[must_use]
    pub fn secret(&self) -> bool {
        self.kind == "password"
            || ["card", "cvv", "cvc", "iban", "otp", "pin", "ssn"]
                .iter()
                .any(|k| self.name.to_ascii_lowercase().contains(k))
    }
}

/// One action on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Stable id within the page (`form:N`).
    pub id: String,
    /// Classified kind.
    pub kind: Kind,
    /// HTTP method (`GET` or `POST`).
    pub method: String,
    /// Absolute target URL.
    pub action: String,
    /// Fields.
    pub fields: Vec<Field>,
    /// Name of the anti-forgery field, if any.
    pub csrf_field: Option<String>,
    /// Needs the user's explicit consent before executing.
    pub needs_consent: bool,
}

/// Classify from visible text, field names and action URL.
#[must_use]
pub fn classify(text: &str, fields: &[Field], action: &str) -> Kind {
    let t = format!(
        "{} {} {}",
        text.to_lowercase(),
        action.to_lowercase(),
        fields.iter().map(|f| f.name.to_lowercase()).collect::<Vec<_>>().join(" ")
    );
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    if fields.iter().any(|f| f.kind == "password") {
        if has(&["register", "sign up", "signup", "create account", "confirm"]) {
            return Kind::Signup;
        }
        return Kind::Login;
    }
    if has(&["checkout", "buy", "purchase", "pay", "order", "cart", "card"]) {
        Kind::Purchase
    } else if has(&["subscribe", "newsletter"]) {
        Kind::Subscribe
    } else if has(&["book", "reserve", "appointment"]) {
        Kind::Book
    } else if has(&["contact", "message", "feedback"]) {
        Kind::Contact
    } else if has(&["search", "query"]) || fields.iter().any(|f| f.kind == "search" || f.name == "q") {
        Kind::Search
    } else {
        Kind::Other
    }
}

/// Resolve `href` against `base` (absolute, root-relative, or relative).
#[must_use]
pub fn resolve(base: &str, href: &str) -> String {
    if href.is_empty() {
        return base.to_owned();
    }
    if href.contains("://") {
        return href.to_owned();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let host_end = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if href.starts_with('/') {
        return format!("{}{href}", &base[..host_end]);
    }
    let dir_end = base[host_end..].rfind('/').map_or(base.len(), |i| host_end + i + 1);
    if dir_end > base.len() || host_end == base.len() {
        return format!("{}/{href}", &base[..host_end]);
    }
    format!("{}{href}", &base[..dir_end])
}

const CSRF_NAMES: [&str; 6] = [
    "csrf",
    "xsrf",
    "authenticity_token",
    "requestverificationtoken",
    "csrfmiddlewaretoken",
    "_token",
];

/// Find the forms on a page and turn them into intents.
#[must_use]
pub fn detect(html: &str, base_url: &str) -> Vec<Intent> {
    let tokens = tokenize(html);
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if !matches!(&tokens[i], Token::Start { tag, .. } if tag == "form") {
            i += 1;
            continue;
        }
        let form = &tokens[i];
        let method = form.attr("method").unwrap_or("get").to_ascii_uppercase();
        let action = resolve(base_url, form.attr("action").unwrap_or(""));
        let mut fields = Vec::new();
        let mut text = String::new();
        i += 1;
        while i < tokens.len() && !matches!(&tokens[i], Token::End(tag) if tag == "form") {
            match &tokens[i] {
                t @ Token::Start { tag, .. } if matches!(tag.as_str(), "input" | "select" | "textarea" | "button") => {
                    let name = t.attr("name").or_else(|| t.attr("id")).unwrap_or("").to_owned();
                    let kind = t
                        .attr("type")
                        .unwrap_or(if tag == "input" { "text" } else { tag })
                        .to_ascii_lowercase();
                    if let Some(v) = t.attr("value") {
                        text.push(' ');
                        text.push_str(v);
                    }
                    if !name.is_empty() {
                        fields.push(Field {
                            name,
                            kind,
                            value: t.attr("value").unwrap_or("").to_owned(),
                        });
                    }
                }
                Token::Text(s) => {
                    text.push(' ');
                    text.push_str(s);
                }
                _ => {}
            }
            i += 1;
        }
        let csrf_field = fields
            .iter()
            .find(|f| {
                let n = f.name.to_ascii_lowercase();
                CSRF_NAMES.iter().any(|c| n.contains(c))
            })
            .map(|f| f.name.clone());
        let kind = classify(&text, &fields, &action);
        let needs_consent = kind.sensitive() || method == "POST" || fields.iter().any(Field::secret);
        out.push(Intent {
            id: format!("form:{}", out.len()),
            kind,
            method,
            action,
            fields,
            csrf_field,
            needs_consent,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
      <form action="/search" method="get"><input name="q" type="search"><button>Search</button></form>
      <form action="login" method="post">
        <input type="hidden" name="csrf_token" value="abc">
        <input name="user"><input name="pass" type="password"><button>Sign in</button>
      </form>
      <form action="https://shop.example/checkout" method="post"><input name="card_number"><input type="submit" value="Pay now"></form>
    "#;

    #[test]
    fn detects_and_classifies() {
        let v = detect(PAGE, "https://site.example/account/index.html");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].kind, Kind::Search);
        assert_eq!(v[0].action, "https://site.example/search");
        assert!(!v[0].needs_consent);
        assert_eq!(v[1].kind, Kind::Login);
        assert_eq!(v[1].action, "https://site.example/account/login");
        assert_eq!(v[1].csrf_field.as_deref(), Some("csrf_token"));
        assert!(v[1].needs_consent);
        assert_eq!(v[2].kind, Kind::Purchase);
        assert!(v[2].fields[0].secret());
    }

    #[test]
    fn resolves_urls() {
        assert_eq!(resolve("https://a.b/x/y", "z"), "https://a.b/x/z");
        assert_eq!(resolve("https://a.b/x/y", "/z"), "https://a.b/z");
        assert_eq!(resolve("https://a.b", "z"), "https://a.b/z");
        assert_eq!(resolve("https://a.b/x", "https://c.d/"), "https://c.d/");
        assert_eq!(resolve("https://a.b/x", ""), "https://a.b/x");
    }
}
