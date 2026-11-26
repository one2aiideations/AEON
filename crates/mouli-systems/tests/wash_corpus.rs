//! Secrets that must not get through the hosted-model washer.
use mouli_systems::wash::{Policy, Washer};

const MUST_NOT_LEAK: &[(&str, &str)] = &[
        (r#"{"password": "hunter2xyz"}"#, "hunter2xyz"),
        (r#"{"api_key":"abc123def456"}"#, "abc123def456"),
        (r#"password = "correct horse battery""#, "battery"),
        ("password: 'two words here'", "words"),
        ("secret_key=AbCdEf123456", "AbCdEf123456"),
        ("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "wJalrXUtnFEMI"),
        ("client_secret = zzTopSecret99", "zzTopSecret99"),
        ("pwd=letmein123", "letmein123"),
        ("passphrase: opensesame", "opensesame"),
        ("Authorization: Bearer abcdef1234567890abcdef", "abcdef1234567890abcdef"),
        ("curl -H 'Authorization: Basic dXNlcjpwYXNzd29yZA=='", "dXNlcjpwYXNzd29yZA"),
        ("authorization=Token abc123def456ghi789", "abc123def456ghi789"),
        ("got a Bearer abcdef1234567890abcdef today", "abcdef1234567890abcdef"),
        (concat!("xo", "xb-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx"), "AbCdEfGhIjKlMnOpQrStUvWx"),
        ("ghs_abcdefghijklmnopqrstuvwxyz0123456789", "abcdefghijklmnopqrstuvwxyz0123456789"),
        ("gho_abcdefghijklmnopqrstuvwxyz0123456789", "abcdefghijklmnopqrstuvwxyz0123456789"),
        ("github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789ABCDEFGHIJKL", "mnopqrstuvwxyz0123456789"),
        ("glpat-abcdefghij0123456789", "abcdefghij0123456789"),
        ("sk_live_abcdefghij0123456789", "abcdefghij0123456789"),
        ("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U", "dozjgNryP4J3jVmNHl0w5N"),
        ("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----", "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"),
        ("-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0Z3VS5JJcds3xfn", "MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn"),
        ("postgres://admin:s3cretPw@db.internal:5432/app", "s3cretPw"),
        ("redis://:onlypass@cache:6379", "onlypass"),
        ("sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "abcdefghijklmnopqrstuvwxyz"),
        ("PASSWORD=Upper123", "Upper123"),
        ("token=a1b2c3d4", "a1b2c3d4"),
        ("my card is 4111 1111 1111 1111.", "4111 1111 1111 1111"),
        ("card 4111-1111-1111-1111", "4111-1111-1111-1111"),
        ("mail me: Jane.Doe+x@example.co.uk please", "Jane.Doe+x@example.co.uk"),
    ];

#[test]
fn known_secret_shapes_do_not_survive_washing() {
    let mut leaks = Vec::new();
    for (text, secret) in MUST_NOT_LEAK {
        let mut w = Washer::new(Policy::gdpr_and_dpdp());
        let out = w.wash(text).unwrap();
        if out.text.contains(secret) {
            leaks.push(format!("{text:?} -> {:?}", out.text));
        }
    }
    assert!(leaks.is_empty(), "leaked:\n{}", leaks.join("\n"));
}

#[test]
fn ordinary_prose_and_code_are_left_alone() {
    for text in [
        "The token economy is basic and secret santa is fun.",
        "Please reset your password at the help desk.",
        "Contact the authorization team about the bearer of bad news.",
        "let tokens = vec![1, 2, 3]; // tokenise",
        "See http://example.com/path and https://example.org/a@b",
        "eyJ short.no.dots",
    ] {
        let mut w = Washer::new(Policy::secrets_only());
        let out = w.wash(text).unwrap();
        assert_eq!(out.text, text, "changed: {text:?}");
    }
}
