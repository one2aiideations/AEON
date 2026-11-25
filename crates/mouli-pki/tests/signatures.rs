//! Signatures made by OpenSSL must verify, and altered ones must not.

use mouli_pki::ecdsa::{Curve, PublicKey as EcKey};
use mouli_pki::rsa::PublicKey as RsaKey;
use mouli_pki::Hash;

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn hash(name: &str) -> Hash {
    match name {
        "sha256" => Hash::Sha256,
        "sha384" => Hash::Sha384,
        "sha512" => Hash::Sha512,
        _ => panic!("hash {name}"),
    }
}

#[test]
fn openssl_vectors() {
    let data = include_str!("data/sig_vectors.txt");
    let (mut valid, mut invalid) = (0, 0);
    for line in data.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let (kind, param, h, salt) = (f[0], f[1], hash(f[2]), f[3].parse::<usize>().unwrap());
        let (pubkey, msg, sig) = (unhex(f[4]), unhex(f[5]), unhex(f[6]));
        let expected = f[7] == "1";
        let got = match kind {
            "ecdsa" => {
                let curve = if param == "p256" {
                    Curve::P256
                } else {
                    Curve::P384
                };
                EcKey::from_sec1(curve, &pubkey)
                    .expect("valid point")
                    .verify(h, &msg, &sig)
            }
            "pkcs1" => RsaKey::from_der(&pubkey)
                .expect("valid key")
                .verify_pkcs1(h, &msg, &sig),
            "pss" => RsaKey::from_der(&pubkey)
                .expect("valid key")
                .verify_pss(h, salt, &msg, &sig),
            _ => panic!("kind"),
        };
        assert_eq!(got, expected, "{line}");
        if expected {
            valid += 1;
        } else {
            invalid += 1;
        }
    }
    assert!(valid == 96 && invalid == 192, "{valid} {invalid}");
}
