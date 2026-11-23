//! Chains made with the OpenSSL command line (see tools/gen_pki.sh).

use aeon_pki::pem;
use aeon_pki::verify::{verify_server, Host, TrustStore, VerifyError};
use aeon_pki::x509::Certificate;

fn load(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/data/pki/{name}.pem", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    pem::blocks(&text, "CERTIFICATE").remove(0)
}

fn store(names: &[&str]) -> TrustStore {
    let mut s = TrustStore::new();
    for n in names {
        s.add_der(&load(n)).unwrap();
    }
    s
}

// Year 2030: inside the 100-year validity of every generated certificate.
const NOW: i64 = 1_893_456_000;

fn host(h: &str) -> Host {
    Host::parse(h)
}

#[test]
fn good_chains_verify() {
    let ec = store(&["ec-root"]);
    let chain = [load("ec-leaf"), load("ec-int")];
    assert!(verify_server(&chain, &ec, &host("example.test"), NOW).is_ok());
    assert!(verify_server(&chain, &ec, &host("anything.wild.test"), NOW).is_ok());
    assert!(verify_server(&chain, &ec, &host("192.0.2.1"), NOW).is_ok());
    let rsa = store(&["rsa-root"]);
    for leaf in ["rsa-leaf", "pss-leaf"] {
        let chain = [load(leaf), load("rsa-int")];
        assert!(
            verify_server(&chain, &rsa, &host("example.test"), NOW).is_ok(),
            "{leaf}"
        );
    }
}

#[test]
fn names_are_checked() {
    let ec = store(&["ec-root"]);
    let chain = [load("ec-leaf"), load("ec-int")];
    for bad in ["other.test", "wild.test", "a.b.wild.test", "192.0.2.2"] {
        assert_eq!(
            verify_server(&chain, &ec, &host(bad), NOW).unwrap_err(),
            VerifyError::NameMismatch,
            "{bad}"
        );
    }
    // the common name is ignored when there is no SAN
    let chain = [load("nosan-leaf"), load("ec-int")];
    assert_eq!(
        verify_server(&chain, &ec, &host("example.test"), NOW).unwrap_err(),
        VerifyError::NameMismatch
    );
}

#[test]
fn time_is_checked() {
    let ec = store(&["ec-root"]);
    let chain = [load("ec-leaf"), load("ec-int")];
    let far_future = 5_000_000_000i64 * 2;
    assert!(matches!(
        verify_server(&chain, &ec, &host("example.test"), far_future),
        Err(VerifyError::NotValidNow(_))
    ));
    assert!(matches!(
        verify_server(&chain, &ec, &host("example.test"), 1_000_000_000),
        Err(VerifyError::NotValidNow(_))
    ));
}

#[test]
fn untrusted_or_forged_chains_fail() {
    let chain = [load("ec-leaf"), load("ec-int")];
    // no roots at all, wrong roots
    assert_eq!(
        verify_server(&chain, &TrustStore::new(), &host("example.test"), NOW).unwrap_err(),
        VerifyError::UnknownIssuer
    );
    assert_eq!(
        verify_server(&chain, &store(&["rsa-root"]), &host("example.test"), NOW).unwrap_err(),
        VerifyError::UnknownIssuer
    );
    // missing intermediate
    assert_eq!(
        verify_server(
            &chain[..1],
            &store(&["ec-root"]),
            &host("example.test"),
            NOW
        )
        .unwrap_err(),
        VerifyError::UnknownIssuer
    );
    // impostor root with the same name but another key
    let forged = [load("fake-leaf")];
    assert_eq!(
        verify_server(&forged, &store(&["ec-root"]), &host("example.test"), NOW).unwrap_err(),
        VerifyError::BadSignature
    );
    // the same chain is fine when its own root is trusted
    assert!(verify_server(&forged, &store(&["fake-root"]), &host("example.test"), NOW).is_ok());
    // a leaf cannot vouch for itself or a chain cannot end in itself
    assert!(verify_server(
        &[load("ec-root")],
        &TrustStore::new(),
        &host("example.test"),
        NOW
    )
    .is_err());
}

#[test]
fn constraints_are_enforced() {
    let ec = store(&["ec-root"]);
    // pathlen 0 intermediate may not have another CA below it
    let deep = [load("deep-leaf"), load("deep-int2"), load("ec-int")];
    assert_eq!(
        verify_server(&deep, &ec, &host("example.test"), NOW).unwrap_err(),
        VerifyError::PathTooLong
    );
    // a leaf cannot act as an intermediate
    let abuse = [load("deep-leaf"), load("ec-leaf"), load("ec-int")];
    assert!(verify_server(&abuse, &ec, &host("example.test"), NOW).is_err());
    // clientAuth-only certificate is refused for a server
    let client = [load("client-leaf"), load("ec-int")];
    assert_eq!(
        verify_server(&client, &ec, &host("example.test"), NOW).unwrap_err(),
        VerifyError::WrongPurpose
    );
    // unknown critical extension
    let crit = [load("crit-leaf"), load("ec-int")];
    assert!(matches!(
        verify_server(&crit, &ec, &host("example.test"), NOW),
        Err(VerifyError::Parse(_))
    ));
}

#[test]
fn tampering_breaks_the_signature() {
    let ec = store(&["ec-root"]);
    let mut leaf = load("ec-leaf");
    let at = leaf.len() / 2; // inside the TBS
    leaf[at] ^= 1;
    let chain = [leaf, load("ec-int")];
    assert!(verify_server(&chain, &ec, &host("example.test"), NOW).is_err());
}

#[test]
fn parsed_fields() {
    let c = Certificate::parse(&load("ec-leaf")).unwrap();
    assert_eq!(c.dns_names, ["example.test", "*.wild.test"]);
    assert_eq!(c.ip_addrs, [vec![192, 0, 2, 1]]);
    assert!(!c.is_ca());
    assert!(c.not_before < NOW && NOW < c.not_after);
    let i = Certificate::parse(&load("ec-int")).unwrap();
    assert!(i.is_ca());
    assert_eq!(i.basic_constraints.unwrap().path_len, Some(0));
    assert_eq!(i.subject, c.issuer);
}

#[test]
fn bundle_loading() {
    let mut text = String::new();
    for n in ["ec-root", "rsa-root"] {
        text.push_str(
            &std::fs::read_to_string(format!(
                "{}/tests/data/pki/{n}.pem",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap(),
        );
    }
    text.push_str("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n");
    let (s, skipped) = TrustStore::from_pem(&text);
    assert_eq!((s.len(), skipped), (2, 1));
}

/// Only runs where a system CA bundle exists: nearly all of it must be usable.
#[test]
fn system_bundle_mostly_loads() {
    let Ok(text) = std::fs::read_to_string("/etc/ssl/certs/ca-certificates.crt") else {
        return;
    };
    let (s, skipped) = TrustStore::from_pem(&text);
    eprintln!("system bundle: {} loaded, {skipped} skipped", s.len());
    assert!(s.len() > 100 && skipped * 20 < s.len() + skipped);
}
