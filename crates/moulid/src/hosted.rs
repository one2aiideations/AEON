//! Using a hosted model from the console: asking for the provider and key, getting consent,
//! washing messages, and chatting through `mouli-hosted`.
//!
//! Hosted mode is the one place MOULI sends what the person types off the machine, so it is
//! explicit: nothing connects until the person has typed `yes` to a plain statement of where the
//! messages go. Emails, card numbers and passwords or API keys found in the messages are replaced
//! before sending by the washer of `mouli-systems` (policy: GDPR and DPDP together): emails, phone
//! numbers and IP addresses become pseudonyms that are put back in the reply; card, bank and ID
//! numbers (Aadhaar, PAN, IBAN, GSTIN, UPI) and credentials are masked. It cannot see names or
//! addresses written as plain text.

use crate::persist::Store;
use crate::ui::{Backend, Turn};
use mouli_hosted::{chat_stream, keyfile, Error, Provider, Secret};
use mouli_http::{Client, Url};
use mouli_systems::wash::{Action, Kind, Policy, Washer};
pub use mouli_utility::prompt::{ask, ask_secret};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

/// Provider name used for the key file.
pub const PROVIDER_NAME: &str = "hosted";
/// System prompt for hosted chats (the local one says there is no internet connection).
pub const SYSTEM_PROMPT: &str = "You are MOULI, a helpful assistant. Answer clearly and concisely.";
const REPLY_TOKENS: u32 = 512;

/// Directory for key files on the data partition.
fn key_dir(store: &Store) -> std::path::PathBuf {
    store.root().join("keys")
}

/// What the person must agree to before anything is sent.
#[must_use]
pub fn consent_text(host: &str, saving_key: bool) -> String {
    format!(
        "\n  Hosted model\n  ------------\n\
         \x20 Your messages will leave this computer: they are sent over an encrypted connection to\n\
         \x20 {host}, which will read them and may keep them under its own terms.\n\
         \x20 Emails, phone numbers, IP addresses, card, bank and ID numbers (such as Aadhaar and PAN)\n\
         \x20 and passwords/API keys are replaced before sending, but names, addresses and other\n\
         \x20 personal text are not.\n\
         {}\
         \x20 Type /models at any time to go back to local models.\n",
        if saving_key {
            "  The API key is saved on the MOULI-DATA partition, encrypted with your passphrase.\n"
        } else {
            "  The API key is kept in memory only and is gone when you power off.\n"
        }
    )
}

/// Ask for the provider, model and key and get consent. `Ok(None)` when the person backs out.
///
/// Saved settings (and a saved key) are offered as defaults. Nothing is contacted here.
///
/// # Errors
/// I/O errors on the console.
pub fn setup<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    store: Option<&Store>,
) -> io::Result<Option<(Provider, bool)>> {
    let saved = store.map(Store::settings).unwrap_or_default();
    writeln!(
        out,
        "\n  Hosted model setup (press Enter on an empty answer to go back)"
    )?;
    writeln!(
        out,
        "  Enter the provider's API address, for example https://api.example.com/v1"
    )?;
    let default_url = saved.get("hosted.url").cloned().unwrap_or_default();
    let url = loop {
        let shown = if default_url.is_empty() {
            "  address > ".to_string()
        } else {
            format!("  address [{default_url}] > ")
        };
        let Some(a) = ask(input, out, &shown)? else {
            return Ok(None);
        };
        let a = if a.is_empty() { default_url.clone() } else { a };
        if a.is_empty() {
            return Ok(None);
        }
        match Url::parse(&a) {
            Ok(u) if u.scheme == mouli_http::Scheme::Https || u.is_loopback() => break a,
            Ok(_) => writeln!(
                out,
                "  Only https:// addresses are allowed (http only for this computer)."
            )?,
            Err(e) => writeln!(out, "  {e}")?,
        }
    };
    let host = Url::parse(&url).map(|u| u.host).unwrap_or_default();
    let default_model = saved.get("hosted.model").cloned().unwrap_or_default();
    let shown = if default_model.is_empty() {
        "  model name > ".to_string()
    } else {
        format!("  model name [{default_model}] > ")
    };
    let Some(m) = ask(input, out, &shown)? else {
        return Ok(None);
    };
    let model = if m.is_empty() { default_model } else { m };
    if model.is_empty() || model.len() > 100 || model.chars().any(char::is_control) {
        writeln!(out, "  A model name is needed.")?;
        return Ok(None);
    }

    // The key: reuse a saved one, or ask.
    let mut key: Option<Secret> = None;
    let mut key_saved = false;
    if let Some(s) = store {
        if keyfile::sealed_exists(&key_dir(s), PROVIDER_NAME) {
            let Some(a) = ask(input, out, "  A saved API key was found. Use it? [Y/n] > ")? else {
                return Ok(None);
            };
            if !a.eq_ignore_ascii_case("n") {
                for attempt in 1..=3 {
                    let Some(p) = ask_secret(
                        input,
                        out,
                        "  Passphrase for the saved key (typing is hidden) > ",
                    )?
                    else {
                        return Ok(None);
                    };
                    match keyfile::load_sealed(&key_dir(s), PROVIDER_NAME, &p) {
                        Ok(Some(k)) => {
                            key = Some(k);
                            key_saved = true;
                            break;
                        }
                        Ok(None) => break,
                        Err(e) => {
                            writeln!(
                                out,
                                "  {e}{}",
                                if attempt < 3 { "; try again." } else { "." }
                            )?;
                        }
                    }
                }
            }
        } else if let Ok(Some(l)) = keyfile::load(&key_dir(s), PROVIDER_NAME) {
            let Some(a) = ask(input, out, "  A saved API key was found. Use it? [Y/n] > ")? else {
                return Ok(None);
            };
            if !a.eq_ignore_ascii_case("n") {
                key = Some(l.key);
                key_saved = true;
            }
        }
    }
    let key = match key {
        Some(k) => k,
        None => loop {
            let Some(k) = ask_secret(input, out, "  API key (typing is hidden) > ")? else {
                return Ok(None);
            };
            if k.is_empty() {
                return Ok(None);
            }
            match Secret::new(&k) {
                Ok(k) => break k,
                Err(e) => writeln!(out, "  {e}")?,
            }
        },
    };
    if !key_saved {
        if let Some(s) = store {
            let a = ask(
                input,
                out,
                "  Save the key on the data partition for next time, encrypted? [y/N] > ",
            )?
            .unwrap_or_default();
            if a.eq_ignore_ascii_case("y") {
                let min = keyfile::MIN_PASSPHRASE;
                while let Some(p) = ask_secret(
                    input,
                    out,
                    &format!(
                        "  Choose a passphrase of at least {min} characters (typing is hidden) > "
                    ),
                )? {
                    if p.is_empty() {
                        writeln!(out, "  The key was not saved.")?;
                        break;
                    }
                    if p.chars().count() < min {
                        writeln!(out, "  That is too short.")?;
                        continue;
                    }
                    let Some(again) = ask_secret(input, out, "  Type it again > ")? else {
                        break;
                    };
                    if again != p {
                        writeln!(out, "  The two did not match.")?;
                        continue;
                    }
                    match keyfile::save_sealed(&key_dir(s), PROVIDER_NAME, &key, &p) {
                        Ok(()) => key_saved = true,
                        Err(e) => writeln!(out, "  The key could not be saved: {e}")?,
                    }
                    break;
                }
            }
        }
    }
    write!(out, "{}", consent_text(&host, key_saved))?;
    let a = ask(input, out, "  Type yes to continue > ")?.unwrap_or_default();
    if !a.eq_ignore_ascii_case("yes") {
        writeln!(out, "  Not sent anything. Back to the model list.")?;
        return Ok(None);
    }
    if let Some(s) = store {
        let _ = s.set("hosted.url", &url);
        let _ = s.set("hosted.model", &model);
    }
    Ok(Some((
        Provider {
            name: PROVIDER_NAME.into(),
            base_url: url,
            model,
            key,
        },
        key_saved,
    )))
}

/// Chat through a hosted provider, washing messages and logging turns when there is a data
/// partition.
pub struct HostedBackend {
    client: Client,
    provider: Provider,
    washer: Washer,
    log: Option<(Store, String)>,
}

impl HostedBackend {
    /// A backend using the system CA bundle.
    ///
    /// # Errors
    /// A message when the CA bundle is missing or empty.
    pub fn new(provider: Provider, store: Option<&Store>) -> Result<Self, String> {
        let client =
            Client::system().map_err(|e| format!("cannot load the trusted certificates: {e}"))?;
        Ok(Self::with_client(client, provider, store))
    }

    /// A backend using `client` (tests use their own trust store).
    #[must_use]
    pub fn with_client(client: Client, provider: Provider, store: Option<&Store>) -> Self {
        Self {
            client,
            provider,
            washer: Washer::new(Policy::gdpr_and_dpdp()),
            log: store.map(|s| (s.clone(), s.new_session())),
        }
    }

    fn record(&self, role: &str, text: &str) {
        if let Some((store, session)) = &self.log {
            let _ = store.log_turn(session, role, text);
        }
    }
}

fn explain(e: &Error) -> String {
    match e {
        Error::Http(mouli_http::Error::Io(_)) => format!(
            "{e} (is the network cable connected? the boot messages show the network state)"
        ),
        other => other.to_string(),
    }
}

impl Backend for HostedBackend {
    fn reply(&mut self, history: &[Turn]) -> Result<String, String> {
        let mut found: BTreeMap<Kind, usize> = BTreeMap::new();
        let mut washed: Vec<(&str, String)> = Vec::with_capacity(history.len());
        for t in history {
            if t.role == "user" {
                let w = self
                    .washer
                    .wash(&t.content)
                    .map_err(|_| "A message is too large to send.".to_string())?;
                for h in w.hits.iter().filter(|h| h.action != Action::Keep) {
                    *found.entry(h.kind).or_insert(0) += 1;
                }
                washed.push((t.role, w.text));
            } else {
                washed.push((t.role, t.content.clone()));
            }
        }
        if let Some(last) = history.last() {
            self.record(last.role, &last.content);
        }
        let turns: Vec<(&str, &str)> = washed.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let answer = chat_stream(
            &self.client,
            &self.provider,
            &turns,
            REPLY_TOKENS,
            &mut |_| {},
        )
        .map_err(|e| explain(&e))?;
        self.record("assistant", &answer);
        // The model may refer to a pseudonym such as [EMAIL_1]; the person wrote the original, so
        // it is put back for them to read (it never left this computer).
        let answer = self.washer.restore(&answer, true).unwrap_or(answer);
        // Counts every washed item in the history, so the note repeats while it stays in view.
        if found.is_empty() {
            Ok(answer)
        } else {
            let list: Vec<String> = found
                .iter()
                .map(|(k, n)| format!("{n} {}", k.label()))
                .collect();
            Ok(format!(
                "(replaced before sending: {})\n{answer}",
                list.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouli_pki::verify::TrustStore;
    use std::io::{Cursor, Read};
    use std::net::TcpListener;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-hosted-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn run_setup(script: &str, store: Option<&Store>) -> (Option<(Provider, bool)>, String) {
        let mut out = Vec::new();
        let r = setup(
            &mut Cursor::new(script.as_bytes().to_vec()),
            &mut out,
            store,
        )
        .unwrap();
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn full_setup_saves_key_and_settings_and_never_prints_the_key() {
        let d = tmp("a");
        let store = Store::open(&d).unwrap();
        let (r, out) = run_setup(
            "https://api.example.com/v1\nsmall-1\nsk-live-123456\ny\nhunter2hunter2\nhunter2hunter2\nyes\n",
            Some(&store),
        );
        let (p, saved) = r.expect("set up");
        assert!(saved);
        assert_eq!(
            (p.base_url.as_str(), p.model.as_str()),
            ("https://api.example.com/v1", "small-1")
        );
        assert!(!out.contains("sk-live-123456") && !out.contains("hunter2hunter2"));
        assert!(
            !std::fs::read_to_string(d.join("keys").join("hosted.sealed"))
                .unwrap_or_default()
                .contains("sk-live")
        );
        assert!(out.contains("api.example.com") && out.contains("leave this computer"));
        assert_eq!(
            store.settings().get("hosted.model").map(String::as_str),
            Some("small-1")
        );
        // second time: saved key and defaults are offered
        let (r, out) = run_setup("\n\n\nhunter2hunter2\nyes\n", Some(&store));
        let (p, saved) = r.expect("second setup");
        assert!(saved && p.key.expose() == "sk-live-123456", "{out}");
        assert_eq!(p.model, "small-1");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_passphrase_flow_rejects_short_and_mismatched_ones_and_wrong_ones_at_unlock() {
        let d = tmp("pp");
        let store = Store::open(&d).unwrap();
        // Short, then mismatched, then good; nothing is saved until both entries agree.
        let (r, out) = run_setup(
            "https://api.example.com/v1\nm1\nsk-zzz-999\ny\nshort\nabcdefgh1\nabcdefgh2\ngoodpass99\ngoodpass99\nyes\n",
            Some(&store),
        );
        assert!(r.is_some_and(|(_, saved)| saved), "{out}");
        assert!(
            out.contains("too short") && out.contains("did not match"),
            "{out}"
        );
        // Unlocking: a wrong passphrase may be retried, three failures fall back to asking for the key.
        let (r, out) = run_setup("\n\n\nnope\ngoodpass99\nyes\n", Some(&store));
        let (p, saved) = r.expect("unlocked");
        assert!(saved && p.key.expose() == "sk-zzz-999", "{out}");
        assert!(out.contains("try again"), "{out}");
        let (r, out) = run_setup("\n\n\na\nb\nc\nsk-new-1\nn\nyes\n", Some(&store));
        let (p, saved) = r.expect("a new key after three failures");
        assert!(!saved && p.key.expose() == "sk-new-1", "{out}");
        // Declining the saved key asks for a new one; an empty passphrase means do not save.
        let (r, out) = run_setup("\n\nn\nsk-other\ny\n\nyes\n", Some(&store));
        let (p, saved) = r.expect("not saved");
        assert!(!saved && p.key.expose() == "sk-other", "{out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn consent_is_required_and_bad_addresses_are_refused() {
        let (r, out) = run_setup("https://api.example.com/v1\nm\nk\nno\n", None);
        assert!(r.is_none());
        assert!(out.contains("Not sent anything"));
        assert!(out.contains("memory only"));
        let (r, out) = run_setup("http://api.example.com/v1\n\n", None);
        assert!(r.is_none());
        assert!(out.contains("Only https://"));
        let (r, _) = run_setup("", None);
        assert!(r.is_none());
    }

    #[test]
    fn backend_washes_messages_and_reports_errors() {
        // A fake provider that records the request body.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut b = [0u8; 1];
            while !buf.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut b).unwrap();
                buf.push(b[0]);
            }
            let head = String::from_utf8(buf).unwrap();
            let len: usize = head
                .lines()
                .find_map(|x| x.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0u8; len];
            s.read_exact(&mut body).unwrap();
            let events = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{events}",
                events.len()
            );
            use std::io::Write as _;
            s.write_all(reply.as_bytes()).unwrap();
            String::from_utf8(body).unwrap()
        });
        let provider = Provider {
            name: "hosted".into(),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "m".into(),
            key: Secret::new("sk-abc").unwrap(),
        };
        let mut b = HostedBackend::with_client(Client::new(TrustStore::new()), provider, None);
        let history = vec![
            Turn {
                role: "system",
                content: SYSTEM_PROMPT.into(),
            },
            Turn {
                role: "user",
                content: "mail me at jo@example.com, password=hunter2hunter2".into(),
            },
        ];
        let reply = b.reply(&history).unwrap();
        assert!(
            reply.contains("replaced before sending: 1 SECRET, 1 EMAIL") && reply.ends_with("ok"),
            "{reply}"
        );
        let body = t.join().unwrap();
        assert!(
            !body.contains("jo@example.com") && !body.contains("hunter2"),
            "{body}"
        );
        assert!(
            body.contains("[SECRET]") && body.contains("[EMAIL_1]"),
            "{body}"
        );
        // nothing listening any more: a readable error, not a panic
        let err = b.reply(&history).unwrap_err();
        assert!(
            err.contains("network") || err.contains("connect") || err.contains("refused"),
            "{err}"
        );
    }
}
