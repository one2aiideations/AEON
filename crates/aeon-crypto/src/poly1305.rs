//! The Poly1305 one-time authenticator (RFC 8439 section 2.5), 26-bit limbs.
//!
//! A key must never authenticate two messages; [`crate::aead`] derives a fresh one per record.

const MASK: u64 = 0x3ff_ffff;

fn le32(b: &[u8]) -> u64 {
    u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

struct State {
    r: [u64; 5],
    h: [u64; 5],
}

impl State {
    fn new(r16: &[u8]) -> Self {
        Self {
            r: [
                le32(&r16[0..]) & 0x3ff_ffff,
                (le32(&r16[3..]) >> 2) & 0x3ff_ff03,
                (le32(&r16[6..]) >> 4) & 0x3ff_c0ff,
                (le32(&r16[9..]) >> 6) & 0x3f0_3fff,
                (le32(&r16[12..]) >> 8) & 0x00f_ffff,
            ],
            h: [0; 5],
        }
    }

    /// Absorb one 16-byte block; `hibit` is `1 << 24` for full blocks, 0 for the padded last one.
    fn block(&mut self, m: &[u8], hibit: u64) {
        let [r0, r1, r2, r3, r4] = self.r;
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let h = &mut self.h;
        h[0] += le32(&m[0..]) & MASK;
        h[1] += (le32(&m[3..]) >> 2) & MASK;
        h[2] += (le32(&m[6..]) >> 4) & MASK;
        h[3] += (le32(&m[9..]) >> 6) & MASK;
        h[4] += (le32(&m[12..]) >> 8) | hibit;
        let d0 = h[0] * r0 + h[1] * s4 + h[2] * s3 + h[3] * s2 + h[4] * s1;
        let mut d1 = h[0] * r1 + h[1] * r0 + h[2] * s4 + h[3] * s3 + h[4] * s2;
        let mut d2 = h[0] * r2 + h[1] * r1 + h[2] * r0 + h[3] * s4 + h[4] * s3;
        let mut d3 = h[0] * r3 + h[1] * r2 + h[2] * r1 + h[3] * r0 + h[4] * s4;
        let mut d4 = h[0] * r4 + h[1] * r3 + h[2] * r2 + h[3] * r1 + h[4] * r0;
        let mut c = d0 >> 26;
        h[0] = d0 & MASK;
        d1 += c;
        c = d1 >> 26;
        h[1] = d1 & MASK;
        d2 += c;
        c = d2 >> 26;
        h[2] = d2 & MASK;
        d3 += c;
        c = d3 >> 26;
        h[3] = d3 & MASK;
        d4 += c;
        c = d4 >> 26;
        h[4] = d4 & MASK;
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= MASK;
        h[1] += c;
    }

    fn finish(mut self) -> u128 {
        let h = &mut self.h;
        let mut c = h[1] >> 26;
        h[1] &= MASK;
        for limb in &mut h[2..5] {
            *limb += c;
            c = *limb >> 26;
            *limb &= MASK;
        }
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= MASK;
        h[1] += c;
        c = h[1] >> 26;
        h[1] &= MASK;
        h[2] += c;
        // g = h + 5 - 2^130; keep g when it did not go negative (h >= p).
        let mut g = [0u64; 5];
        let mut c = 5u64;
        for i in 0..4 {
            g[i] = h[i] + c;
            c = g[i] >> 26;
            g[i] &= MASK;
        }
        g[4] = h[4].wrapping_add(c).wrapping_sub(1 << 26);
        let keep_g = (g[4] >> 63).wrapping_sub(1); // all ones when g[4] did not underflow
        for i in 0..5 {
            h[i] = (h[i] & !keep_g) | (g[i] & keep_g);
        }
        u128::from(h[0])
            | (u128::from(h[1]) << 26)
            | (u128::from(h[2]) << 52)
            | (u128::from(h[3]) << 78)
            | (u128::from(h[4]) << 104)
    }
}

/// Length of a Poly1305 tag.
pub const TAG_LEN: usize = 16;

/// The 16-byte tag of `msg` under the one-time `key` (32 bytes: `r` then `s`).
#[must_use]
pub fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; TAG_LEN] {
    let mut st = State::new(&key[..16]);
    let (full, rest) = msg.as_chunks::<16>();
    for b in full {
        st.block(b, 1 << 24);
    }
    if !rest.is_empty() {
        let mut last = [0u8; 16];
        last[..rest.len()].copy_from_slice(rest);
        last[rest.len()] = 1;
        st.block(&last, 0);
    }
    let s = u128::from_le_bytes(key[16..].try_into().expect("16 bytes"));
    st.finish().wrapping_add(s).to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::from_hex;

    #[test]
    fn rfc8439_vector() {
        let key: [u8; 32] =
            from_hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
                .unwrap()
                .try_into()
                .unwrap();
        assert_eq!(
            poly1305(&key, b"Cryptographic Forum Research Group").to_vec(),
            from_hex("a8061dc1305136c6c22b8baf0c0127a9").unwrap()
        );
    }

    #[test]
    fn edge_lengths_and_the_all_ones_case() {
        let key = [0x42u8; 32];
        // empty message: the tag is just `s`
        assert_eq!(poly1305(&key, b"").to_vec(), key[16..].to_vec());
        // different lengths give different tags and never panic
        let mut seen = std::collections::BTreeSet::new();
        for n in 0..70usize {
            assert!(seen.insert(poly1305(&key, &vec![0xffu8; n])));
        }
    }
}
