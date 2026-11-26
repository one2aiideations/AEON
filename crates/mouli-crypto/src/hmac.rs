//! HMAC-SHA-256 (RFC 2104), HKDF (RFC 5869) and TLS 1.3's `HKDF-Expand-Label` (RFC 8446 7.1).

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;
/// SHA-256 output size.
pub const HASH_LEN: usize = 32;

/// HMAC-SHA-256 of `msg` under `key`.
#[must_use]
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; HASH_LEN] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..HASH_LEN].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// PBKDF2 with HMAC-SHA-256 (RFC 8018), one 32-byte block: stretches a passphrase so that trying
/// guesses against a stolen file is slow.
///
/// `None` when `rounds` is zero. Cost is `rounds` HMAC calls; 200 000 is a sensible floor for a
/// passphrase check on a laptop. It does not make a short or common passphrase safe.
#[must_use]
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32) -> Option<[u8; HASH_LEN]> {
    if rounds == 0 {
        return None;
    }
    let mut first = salt.to_vec();
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &first);
    let mut out = u;
    for _ in 1..rounds {
        u = hmac_sha256(password, &u);
        for (o, x) in out.iter_mut().zip(&u) {
            *o ^= x;
        }
    }
    Some(out)
}

/// HKDF-Extract: a pseudorandom key from `ikm`. An empty `salt` means a string of zeros.
#[must_use]
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; HASH_LEN] {
    if salt.is_empty() {
        hmac_sha256(&[0u8; HASH_LEN], ikm)
    } else {
        hmac_sha256(salt, ikm)
    }
}

/// HKDF-Expand: `len` bytes of output keying material.
///
/// # Panics
/// When `len` exceeds 255 hash blocks (8160 bytes), the HKDF limit.
#[must_use]
pub fn hkdf_expand(prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    assert!(len <= 255 * HASH_LEN, "HKDF output too long");
    let mut out = Vec::with_capacity(len);
    let mut t: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while out.len() < len {
        let mut msg = t.clone();
        msg.extend_from_slice(info);
        msg.push(counter);
        t = hmac_sha256(prk, &msg).to_vec();
        out.extend_from_slice(&t);
        counter = counter.wrapping_add(1);
    }
    out.truncate(len);
    out
}

/// TLS 1.3 `HKDF-Expand-Label(secret, label, context, len)`; the label is prefixed `tls13 `.
///
/// # Panics
/// When the label, context or length do not fit the protocol's length fields.
#[must_use]
pub fn expand_label(secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let full = format!("tls13 {label}");
    let mut info = Vec::new();
    info.extend_from_slice(&u16::try_from(len).expect("length fits u16").to_be_bytes());
    info.push(u8::try_from(full.len()).expect("label fits u8"));
    info.extend_from_slice(full.as_bytes());
    info.push(u8::try_from(context.len()).expect("context fits u8"));
    info.extend_from_slice(context);
    hkdf_expand(secret, &info, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_sha256_matches_the_published_vectors() {
        // PBKDF2-HMAC-SHA256, password "password", salt "salt".
        for (rounds, expected) in [
            (
                1,
                "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b",
            ),
            (
                2,
                "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43",
            ),
            (
                4096,
                "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a",
            ),
        ] {
            let got = pbkdf2_sha256(b"password", b"salt", rounds).unwrap();
            assert_eq!(
                got.to_vec(),
                crate::from_hex(expected).unwrap(),
                "rounds {rounds}"
            );
        }
    }

    #[test]
    fn pbkdf2_sha256_depends_on_every_input_and_refuses_zero_rounds() {
        let base = pbkdf2_sha256(b"pw", b"salt", 3).unwrap();
        assert_ne!(base, pbkdf2_sha256(b"pw2", b"salt", 3).unwrap());
        assert_ne!(base, pbkdf2_sha256(b"pw", b"salt2", 3).unwrap());
        assert_ne!(base, pbkdf2_sha256(b"pw", b"salt", 4).unwrap());
        assert_eq!(base, pbkdf2_sha256(b"pw", b"salt", 3).unwrap());
        assert!(pbkdf2_sha256(b"pw", b"salt", 0).is_none());
    }
    use crate::from_hex;

    fn hx(s: &str) -> Vec<u8> {
        from_hex(s).unwrap()
    }

    #[test]
    fn rfc4231_hmac_vectors() {
        // test case 1
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There").to_vec(),
            hx("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        // test case 2
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?").to_vec(),
            hx("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
        // test case 6: key longer than the block size
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )
            .to_vec(),
            hx("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }

    #[test]
    fn rfc5869_hkdf_vectors() {
        // test case 1
        let prk = hkdf_extract(&hx("000102030405060708090a0b0c"), &[0x0b; 22]);
        assert_eq!(
            prk.to_vec(),
            hx("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5")
        );
        assert_eq!(
            hkdf_expand(&prk, &hx("f0f1f2f3f4f5f6f7f8f9"), 42),
            hx("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865")
        );
        // test case 3: empty salt and info
        let prk = hkdf_extract(&[], &[0x0b; 22]);
        assert_eq!(
            hkdf_expand(&prk, &[], 42),
            hx("8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8")
        );
    }

    #[test]
    fn rfc8448_tls13_early_secret_and_label() {
        // RFC 8448 section 3 (simple 1-RTT handshake): early secret with no PSK, and the
        // derived secret that feeds the handshake secret.
        let early = hkdf_extract(&[], &[0u8; 32]);
        assert_eq!(
            early.to_vec(),
            hx("33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a")
        );
        let empty_hash = Sha256::digest(b"");
        assert_eq!(
            expand_label(&early, "derived", &empty_hash, 32),
            hx("6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba")
        );
    }
}
