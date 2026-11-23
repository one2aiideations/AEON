//! Compare every primitive with vectors from an independent implementation (OpenSSL through the
//! Python `cryptography` package, see `tools/gen_vectors.py`).

use aeon_crypto::aead::{open, seal};
use aeon_crypto::from_hex;
use aeon_crypto::hmac::{hkdf_expand, hkdf_extract, hmac_sha256};
use aeon_crypto::x25519::x25519;

fn hx(s: &str) -> Vec<u8> {
    if s == "-" {
        Vec::new()
    } else {
        from_hex(s).expect("hex")
    }
}

fn arr<const N: usize>(s: &str) -> [u8; N] {
    hx(s).try_into().expect("length")
}

#[test]
fn matches_openssl() {
    let text = include_str!("vectors.txt");
    let (mut x, mut a, mut m, mut k) = (0, 0, 0, 0);
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "x25519" => {
                assert_eq!(x25519(&arr(f[1]), &arr(f[2])).to_vec(), hx(f[3]), "{line}");
                x += 1;
            }
            "aead" => {
                let (key, nonce, aad, pt, sealed) = (
                    arr::<32>(f[1]),
                    arr::<12>(f[2]),
                    hx(f[3]),
                    hx(f[4]),
                    hx(f[5]),
                );
                assert_eq!(seal(&key, &nonce, &aad, &pt), sealed, "{line}");
                assert_eq!(open(&key, &nonce, &aad, &sealed).as_deref(), Some(&pt[..]));
                a += 1;
            }
            "hmac" => {
                assert_eq!(
                    hmac_sha256(&hx(f[1]), &hx(f[2])).to_vec(),
                    hx(f[3]),
                    "{line}"
                );
                m += 1;
            }
            "hkdf" => {
                let prk = hkdf_extract(&hx(f[1]), &hx(f[2]));
                let len: usize = f[4].parse().unwrap();
                assert_eq!(hkdf_expand(&prk, &hx(f[3]), len), hx(f[5]), "{line}");
                k += 1;
            }
            other => panic!("unknown vector kind {other}"),
        }
    }
    assert!(x >= 30 && a >= 77 && m == 7 && k == 5, "{x} {a} {m} {k}");
}
