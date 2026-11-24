//! PEM bundles (such as `/etc/ssl/certs/ca-certificates.crt`) and base64.

/// Decode standard base64, ignoring ASCII whitespace. `None` on any other bad character,
/// bad padding or a length that cannot be valid.
#[must_use]
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if pad > 0 {
            return None; // data after padding
        }
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Leftover bits must be zero and padding must complete the quantum.
    if pad > 2 || acc != 0 || (bits != 0 && pad == 0 && bits >= 6) {
        return None;
    }
    Some(out)
}

/// Extract every `-----BEGIN <label>-----` block with the given label as DER bytes. Blocks that
/// do not decode are skipped.
#[must_use]
pub fn blocks(text: &str, label: &str) -> Vec<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(s) = rest.find(&begin) {
        let body_start = s + begin.len();
        let Some(e) = rest[body_start..].find(&end) else {
            break;
        };
        if let Some(der) = base64_decode(&rest[body_start..body_start + e]) {
            out.push(der);
        }
        rest = &rest[body_start + e + end.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_cases() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9v\nYmFy\n").unwrap(), b"foobar");
        assert!(base64_decode("Zm9v!").is_none());
        assert!(base64_decode("Zg==Zg==").is_none());
        assert!(base64_decode("Zh==").is_none()); // nonzero trailing bits
    }

    #[test]
    fn finds_blocks() {
        let text = "junk\n-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n\
                    -----BEGIN OTHER-----\nAAAA\n-----END OTHER-----\n\
                    -----BEGIN CERTIFICATE-----\nYmFy\n-----END CERTIFICATE-----\n";
        assert_eq!(
            blocks(text, "CERTIFICATE"),
            vec![b"foo".to_vec(), b"bar".to_vec()]
        );
    }
}
