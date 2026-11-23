//! The ChaCha20 stream cipher (RFC 8439 section 2.1 to 2.4): 256-bit key, 96-bit nonce,
//! 32-bit block counter.

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// One 64-byte keystream block.
#[must_use]
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut init = [0u32; 16];
    init[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for i in 0..8 {
        init[4 + i] = le32(&key[i * 4..]);
    }
    init[12] = counter;
    for i in 0..3 {
        init[13 + i] = le32(&nonce[i * 4..]);
    }
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[i * 4..i * 4 + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// XOR `data` with the keystream starting at block `counter` (encrypts and decrypts).
///
/// # Panics
/// When the data is long enough to overflow the 32-bit block counter (256 GiB).
pub fn xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    let mut ctr = counter;
    for chunk in data.chunks_mut(64) {
        let ks = block(key, ctr, nonce);
        for (d, k) in chunk.iter_mut().zip(ks) {
            *d ^= k;
        }
        ctr = ctr.checked_add(1).expect("ChaCha20 block counter overflow");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::from_hex;

    fn key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        k
    }

    #[test]
    fn rfc8439_block_function() {
        let nonce: [u8; 12] = from_hex("000000090000004a00000000")
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            block(&key(), 1, &nonce).to_vec(),
            from_hex("10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4ed2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e").unwrap()
        );
    }

    #[test]
    fn rfc8439_encryption() {
        let nonce: [u8; 12] = from_hex("000000000000004a00000000")
            .unwrap()
            .try_into()
            .unwrap();
        let mut msg = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".to_vec();
        xor(&key(), 1, &nonce, &mut msg);
        assert_eq!(
            msg,
            from_hex("6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0bf91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d807ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab77937365af90bbf74a35be6b40b8eedf2785e42874d").unwrap()
        );
        xor(&key(), 1, &nonce, &mut msg);
        assert!(msg.starts_with(b"Ladies and Gentlemen"));
    }

    #[test]
    fn crosses_block_boundaries_consistently() {
        let nonce = [7u8; 12];
        let mut whole = vec![0u8; 200];
        xor(&key(), 5, &nonce, &mut whole);
        let mut a = vec![0u8; 64];
        let mut b = vec![0u8; 136];
        xor(&key(), 5, &nonce, &mut a);
        xor(&key(), 6, &nonce, &mut b);
        assert_eq!(&whole[..64], &a[..]);
        assert_eq!(&whole[64..], &b[..]);
    }
}
