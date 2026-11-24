//! Plain-text tensor manifest.
//!
//! One record per line: `name | offset | bytes | dtype | rank | d0,d1,...`.
//! Blank lines and lines starting with `#` are skipped.

use crate::index::{ModelIndex, TensorMeta};
use crate::ModelError;
use mouli_tensor::{DType, MAX_RANK};

/// Parse `text` into an index and validate it against `model_bytes`.
pub fn parse(text: &str, model_bytes: u64) -> Result<ModelIndex, ModelError> {
    let mut index = ModelIndex::new();
    for (n, raw) in text.lines().enumerate() {
        let line_no = n + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || ModelError::Manifest(line_no);
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        let [name, offset, bytes, dtype, rank, dims] = fields[..] else {
            return Err(bad());
        };
        let offset: u64 = offset.parse().map_err(|_| bad())?;
        let bytes: u64 = bytes.parse().map_err(|_| bad())?;
        let rank: usize = rank.parse().map_err(|_| bad())?;
        let dtype = DType::from_name(dtype).ok_or_else(bad)?;
        if rank == 0 || rank > MAX_RANK {
            return Err(bad());
        }
        let shape: Vec<u64> = dims
            .split(',')
            .map(|d| d.trim().parse::<u64>().ok().filter(|&v| v > 0))
            .collect::<Option<_>>()
            .ok_or_else(bad)?;
        if shape.len() != rank {
            return Err(bad());
        }
        index
            .add(TensorMeta {
                name: name.to_owned(),
                offset,
                bytes,
                dtype,
                shape,
            })
            .map_err(|_| bad())?;
    }
    if index.is_empty() {
        return Err(ModelError::EmptyManifest);
    }
    index.validate(model_bytes)?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "# demo\n\
        embed | 0 | 16 | F32 | 2 | 2,2\n\
        \n\
        head  | 16 | 4 | I8 | 1 | 4\n";

    #[test]
    fn parses_records() {
        let ix = parse(GOOD, 20).unwrap();
        assert_eq!(ix.len(), 2);
        let e = ix.find("embed").unwrap();
        assert_eq!(e.shape, vec![2, 2]);
        assert_eq!(e.dtype, DType::F32);
    }

    #[test]
    fn reports_line_numbers() {
        assert_eq!(
            parse("a | 0 | 4 | I8 | 1\n", 4),
            Err(ModelError::Manifest(1))
        );
        assert_eq!(
            parse("# x\na | 0 | 4 | Q4 | 1 | 4\n", 4),
            Err(ModelError::Manifest(2))
        );
        assert_eq!(
            parse("a | 0 | 4 | I8 | 2 | 4\n", 4),
            Err(ModelError::Manifest(1))
        );
    }

    #[test]
    fn empty_and_range() {
        assert_eq!(parse("# nothing\n", 4), Err(ModelError::EmptyManifest));
        assert_eq!(parse(GOOD, 19), Err(ModelError::BadRange));
    }
}
