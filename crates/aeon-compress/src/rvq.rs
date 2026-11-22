//! Residual vector quantisation storage accounting.
//!
//! The codebook fitting of the original (k-means stages) is not ported; only the size
//! arithmetic, which is what compression planning needs to compare against other schemes.

/// Bytes of a residual-VQ encoding: per stage, `K` codewords of `dimension` f32 values
/// plus one label per vector. Labels take 1, 2 or 4 bytes depending on `K`.
/// Returns `None` on arithmetic overflow.
pub fn storage_bytes(vectors: u64, dimension: u64, k: u64, stages: u64) -> Option<u64> {
    let label_bytes: u64 = if k <= 256 {
        1
    } else if k <= 65_536 {
        2
    } else {
        4
    };
    let codebook = k.checked_mul(dimension)?.checked_mul(4)?;
    let labels = vectors.checked_mul(label_bytes)?;
    stages.checked_mul(codebook.checked_add(labels)?)
}

/// Compression ratio against dense f32 storage of `vectors * dimension` values
/// (`> 1` means smaller). `None` on overflow or when the encoding would be empty.
pub fn compression_ratio(vectors: u64, dimension: u64, k: u64, stages: u64) -> Option<f64> {
    let dense = vectors.checked_mul(dimension)?.checked_mul(4)?;
    let enc = storage_bytes(vectors, dimension, k, stages)?;
    if enc == 0 {
        return None;
    }
    Some(dense as f64 / enc as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_width_follows_codebook_size() {
        // 1 stage, 10 vectors, dim 4
        assert_eq!(storage_bytes(10, 4, 256, 1), Some(256 * 4 * 4 + 10));
        assert_eq!(storage_bytes(10, 4, 257, 1), Some(257 * 4 * 4 + 20));
        assert_eq!(storage_bytes(10, 4, 65_536, 1), Some(65_536 * 4 * 4 + 20));
        assert_eq!(storage_bytes(10, 4, 65_537, 1), Some(65_537 * 4 * 4 + 40));
    }

    #[test]
    fn stages_multiply() {
        let one = storage_bytes(100, 8, 16, 1).unwrap();
        assert_eq!(storage_bytes(100, 8, 16, 3), Some(3 * one));
        assert_eq!(storage_bytes(100, 8, 16, 0), Some(0));
    }

    #[test]
    fn overflow_is_none() {
        assert_eq!(storage_bytes(u64::MAX, 8, 16, 1), None);
        assert_eq!(storage_bytes(1, u64::MAX, 16, 1), None);
        assert_eq!(storage_bytes(1, 8, 16, u64::MAX), None);
    }

    #[test]
    fn ratio_against_dense() {
        // 1_000_000 vectors of dim 8, K=256, 1 stage: dense 32 MB vs ~1 MB + 8 KB... check shape of result
        let r = compression_ratio(1_000_000, 8, 256, 1).unwrap();
        assert!(r > 30.0 && r < 32.0, "{r}");
        assert_eq!(compression_ratio(10, 8, 16, 0), None);
    }
}
