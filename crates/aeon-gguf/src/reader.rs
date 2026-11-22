//! GGUF parsing and validation.
#![forbid(unsafe_code)]

use crate::cursor::{align_up, Cursor};
use crate::error::GgufError;
use crate::tensor::TensorInfo;
use crate::types::{GgmlType, ValueType};
use crate::value::{parse_value, Array, Value};
use std::collections::HashMap;

const MAGIC: u32 = 0x4655_4747;
const DEFAULT_ALIGNMENT: u64 = 32;
const MAX_NAME_LEN: usize = 65535;
/// Smallest metadata entry: key length (8) + one key byte + value type (4).
const MIN_KV_BYTES: u64 = 8 + 1 + 4;
/// Smallest tensor info: name length (8) + one byte + n_dims (4) + one dim (8) + type (4) + offset (8).
const MIN_TENSOR_BYTES: u64 = 8 + 1 + 4 + 8 + 4 + 8;

/// A parsed GGUF file: metadata and tensor table, without the tensor data itself.
#[derive(Debug, Clone)]
pub struct Gguf {
    /// File format version (2 or 3).
    pub version: u32,
    /// Alignment of tensor offsets and of the data section.
    pub alignment: u64,
    /// Absolute offset where the tensor data section starts.
    pub data_start: u64,
    /// `true` if the tensor data bounds were *not* checked against the input length
    /// (produced by [`parse_header_only`]).
    pub header_only: bool,
    metadata: Vec<(String, Value)>,
    metadata_index: HashMap<String, usize>,
    tensors: Vec<TensorInfo>,
    tensor_index: HashMap<String, usize>,
}

/// A short description of a model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSummary {
    /// `general.architecture`.
    pub architecture: String,
    /// `general.name`, if present.
    pub name: Option<String>,
    /// Total element count over all tensors.
    pub n_params: u64,
    /// Total tensor data bytes.
    pub n_tensor_bytes: u64,
    /// `<arch>.context_length`, if present.
    pub context_length: Option<u32>,
    /// `<arch>.block_count`, if present.
    pub n_layers: Option<u32>,
    /// `<arch>.expert_count`, if present.
    pub n_experts: Option<u32>,
    /// Number of tensors of each element type, ordered by type id.
    pub type_histogram: Vec<(GgmlType, u64)>,
}

/// Parse and fully validate a GGUF file held in memory.
pub fn parse(bytes: &[u8]) -> Result<Gguf, GgufError> {
    parse_inner(bytes, true)
}

/// Like [`parse`], but the tensor data need not be present: the bounds of tensor data
/// against the input length are not checked, and [`Gguf::header_only`] is set.
/// Everything that can be checked without the data (offsets, overlaps, alignment) still is.
pub fn parse_header_only(bytes: &[u8]) -> Result<Gguf, GgufError> {
    parse_inner(bytes, false)
}

fn valid_name(s: &str, ascii_only: bool) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_LEN && (!ascii_only || s.is_ascii())
}

fn parse_inner(bytes: &[u8], check_data: bool) -> Result<Gguf, GgufError> {
    let mut c = Cursor::new(bytes);
    let magic = c.read_u32()?;
    if magic != MAGIC {
        return Err(GgufError::BadMagic(magic));
    }
    let version = c.read_u32()?;
    if version != 2 && version != 3 {
        return Err(GgufError::UnsupportedVersion(version));
    }
    let tensor_count = c.read_u64()?;
    let kv_count = c.read_u64()?;

    let remaining = c.remaining() as u64;
    let kv_bytes = kv_count
        .checked_mul(MIN_KV_BYTES)
        .ok_or(GgufError::CountTooLarge {
            what: "metadata",
            count: kv_count,
        })?;
    if kv_bytes > remaining {
        return Err(GgufError::CountTooLarge {
            what: "metadata",
            count: kv_count,
        });
    }
    let tensor_bytes =
        tensor_count
            .checked_mul(MIN_TENSOR_BYTES)
            .ok_or(GgufError::CountTooLarge {
                what: "tensor",
                count: tensor_count,
            })?;
    if tensor_bytes > remaining {
        return Err(GgufError::CountTooLarge {
            what: "tensor",
            count: tensor_count,
        });
    }

    let mut metadata: Vec<(String, Value)> = Vec::with_capacity(kv_count as usize);
    let mut metadata_index: HashMap<String, usize> = HashMap::with_capacity(kv_count as usize);
    for _ in 0..kv_count {
        let key_offset = c.pos();
        let key = c.read_string()?;
        if !valid_name(&key, true) {
            return Err(GgufError::BadString { offset: key_offset });
        }
        let ty = ValueType::from_u32(c.read_u32()?)?;
        let value = parse_value(&mut c, ty, 0)?;
        if metadata_index.insert(key.clone(), metadata.len()).is_some() {
            return Err(GgufError::DuplicateKey(key));
        }
        metadata.push((key, value));
    }

    let alignment = match metadata_index.get("general.alignment") {
        None => DEFAULT_ALIGNMENT,
        Some(&i) => match &metadata[i].1 {
            Value::U32(a) => u64::from(*a),
            other => {
                return Err(GgufError::WrongType {
                    key: "general.alignment".into(),
                    expected: "U32",
                    found: other.type_name(),
                })
            }
        },
    };
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(GgufError::BadAlignment(alignment));
    }

    let mut tensors: Vec<TensorInfo> = Vec::with_capacity(tensor_count as usize);
    let mut tensor_index: HashMap<String, usize> = HashMap::with_capacity(tensor_count as usize);
    for _ in 0..tensor_count {
        let name_offset = c.pos();
        let name = c.read_string()?;
        if !valid_name(&name, false) {
            return Err(GgufError::BadString {
                offset: name_offset,
            });
        }
        let n_dims = c.read_u32()?;
        if !(1..=4).contains(&n_dims) {
            return Err(GgufError::BadDims { tensor: name });
        }
        let mut dims = Vec::with_capacity(n_dims as usize);
        let mut n_elements: u64 = 1;
        for _ in 0..n_dims {
            let d = c.read_u64()?;
            if d == 0 {
                return Err(GgufError::BadDims { tensor: name });
            }
            n_elements = n_elements.checked_mul(d).ok_or(GgufError::Overflow)?;
            dims.push(d);
        }
        let ty = GgmlType::from_u32(c.read_u32()?)?;
        let offset = c.read_u64()?;
        if !offset.is_multiple_of(alignment) {
            return Err(GgufError::MisalignedOffset { tensor: name });
        }
        if !dims[0].is_multiple_of(ty.block_size()) {
            return Err(GgufError::NotBlockAligned { tensor: name });
        }
        let n_bytes = (n_elements / ty.block_size())
            .checked_mul(ty.type_size())
            .ok_or(GgufError::Overflow)?;
        if tensor_index.insert(name.clone(), tensors.len()).is_some() {
            return Err(GgufError::DuplicateTensor(name));
        }
        tensors.push(TensorInfo {
            name,
            dims,
            ty,
            offset,
            n_elements,
            n_bytes,
        });
    }

    let data_start = align_up(c.pos() as u64, alignment)?;
    if check_data && data_start > bytes.len() as u64 {
        return Err(GgufError::UnexpectedEof {
            offset: bytes.len(),
            needed: data_start - bytes.len() as u64,
        });
    }

    // Ranges must be computable, in bounds (full parse), and disjoint.
    let mut ranges = Vec::with_capacity(tensors.len());
    for t in &tensors {
        let r = t.data_range(data_start)?;
        if check_data && r.end > bytes.len() as u64 {
            return Err(GgufError::TensorOutOfBounds {
                tensor: t.name.clone(),
            });
        }
        ranges.push((r.start, r.end, t.name.as_str()));
    }
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(GgufError::TensorOverlap {
                a: pair[0].2.to_string(),
                b: pair[1].2.to_string(),
            });
        }
    }

    Ok(Gguf {
        version,
        alignment,
        data_start,
        header_only: !check_data,
        metadata,
        metadata_index,
        tensors,
        tensor_index,
    })
}

impl Gguf {
    /// Metadata value for `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata_index.get(key).map(|&i| &self.metadata[i].1)
    }

    fn require(&self, key: &str) -> Result<&Value, GgufError> {
        self.get(key)
            .ok_or_else(|| GgufError::MissingKey(key.to_string()))
    }

    fn wrong(key: &str, expected: &'static str, v: &Value) -> GgufError {
        GgufError::WrongType {
            key: key.to_string(),
            expected,
            found: v.type_name(),
        }
    }

    /// Integer metadata that fits a `u32`.
    pub fn get_u32(&self, key: &str) -> Result<u32, GgufError> {
        let v = self.require(key)?;
        v.as_u32().ok_or_else(|| Self::wrong(key, "U32", v))
    }

    /// Float metadata (`F32`, or `F64` narrowed).
    pub fn get_f32(&self, key: &str) -> Result<f32, GgufError> {
        let v = self.require(key)?;
        v.as_f32().ok_or_else(|| Self::wrong(key, "F32", v))
    }

    /// String metadata.
    pub fn get_str(&self, key: &str) -> Result<&str, GgufError> {
        let v = self.require(key)?;
        v.as_str().ok_or_else(|| Self::wrong(key, "STRING", v))
    }

    /// Array metadata.
    pub fn get_array(&self, key: &str) -> Result<&Array, GgufError> {
        let v = self.require(key)?;
        v.as_array().ok_or_else(|| Self::wrong(key, "ARRAY", v))
    }

    /// `general.architecture`.
    pub fn architecture(&self) -> Result<&str, GgufError> {
        self.get_str("general.architecture")
    }

    /// Integer metadata `<architecture>.<suffix>`.
    pub fn arch_u32(&self, suffix: &str) -> Result<u32, GgufError> {
        let arch = self.architecture()?;
        self.get_u32(&format!("{arch}.{suffix}"))
    }

    /// All tensors, in file order.
    pub fn tensors(&self) -> &[TensorInfo] {
        &self.tensors
    }

    /// One tensor by name.
    pub fn tensor(&self, name: &str) -> Option<&TensorInfo> {
        self.tensor_index.get(name).map(|&i| &self.tensors[i])
    }

    /// The bytes of tensor `name` inside `file`, bounds-checked against `file`.
    pub fn tensor_data<'a>(&self, file: &'a [u8], name: &str) -> Result<&'a [u8], GgufError> {
        let t = self
            .tensor(name)
            .ok_or_else(|| GgufError::MissingTensor(name.to_string()))?;
        let r = t.data_range(self.data_start)?;
        let out_of_bounds = || GgufError::TensorOutOfBounds {
            tensor: name.to_string(),
        };
        let start = usize::try_from(r.start).map_err(|_| out_of_bounds())?;
        let end = usize::try_from(r.end).map_err(|_| out_of_bounds())?;
        file.get(start..end).ok_or_else(out_of_bounds)
    }

    /// Metadata entries in file order.
    pub fn metadata(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.metadata.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Sum of all tensors' data sizes (saturating at `u64::MAX`).
    pub fn total_tensor_bytes(&self) -> u64 {
        self.tensors
            .iter()
            .fold(0u64, |a, t| a.saturating_add(t.n_bytes))
    }

    /// Summarise the model. Needs `general.architecture`.
    pub fn summary(&self) -> Result<ModelSummary, GgufError> {
        let architecture = self.architecture()?.to_string();
        let mut n_params = 0u64;
        let mut hist: std::collections::BTreeMap<u32, (GgmlType, u64)> =
            std::collections::BTreeMap::new();
        for t in &self.tensors {
            n_params = n_params
                .checked_add(t.n_elements)
                .ok_or(GgufError::Overflow)?;
            hist.entry(t.ty as u32).or_insert((t.ty, 0)).1 += 1;
        }
        Ok(ModelSummary {
            name: self.get_str("general.name").ok().map(str::to_owned),
            n_params,
            n_tensor_bytes: self.total_tensor_bytes(),
            context_length: self.arch_u32("context_length").ok(),
            n_layers: self.arch_u32("block_count").ok(),
            n_experts: self.arch_u32("expert_count").ok(),
            type_histogram: hist.into_values().collect(),
            architecture,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::GgufWriter;

    fn sample() -> Vec<u8> {
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("llama".into()));
        w.add_metadata("general.name", Value::String("tiny".into()));
        w.add_metadata("llama.context_length", Value::U32(2048));
        w.add_metadata("llama.block_count", Value::U32(2));
        w.add_metadata("llama.rope.freq_base", Value::F32(10000.0));
        w.add_tensor("a", &[4, 2], GgmlType::F32, vec![1; 32])
            .unwrap();
        w.add_tensor("b", &[32], GgmlType::Q8_0, vec![2; 34])
            .unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn parses_header_metadata_and_tensors() {
        let bytes = sample();
        let g = parse(&bytes).unwrap();
        assert_eq!(g.version, 3);
        assert_eq!(g.alignment, 32);
        assert!(!g.header_only);
        assert_eq!(g.architecture().unwrap(), "llama");
        assert_eq!(g.get_u32("llama.context_length").unwrap(), 2048);
        assert_eq!(g.arch_u32("block_count").unwrap(), 2);
        assert_eq!(g.get_f32("llama.rope.freq_base").unwrap(), 10000.0);
        assert_eq!(g.tensors().len(), 2);
        let a = g.tensor("a").unwrap();
        assert_eq!((a.n_elements, a.n_bytes, a.offset), (8, 32, 0));
        assert_eq!(g.tensor("b").unwrap().offset, 32);
        assert_eq!(g.tensor_data(&bytes, "a").unwrap(), &[1u8; 32][..]);
        assert_eq!(g.tensor_data(&bytes, "b").unwrap(), &[2u8; 34][..]);
        assert!(matches!(
            g.tensor_data(&bytes, "zz"),
            Err(GgufError::MissingTensor(_))
        ));
        assert_eq!(g.total_tensor_bytes(), 66);
        assert_eq!(g.data_start % 32, 0);
    }

    #[test]
    fn metadata_keeps_file_order() {
        let g = parse(&sample()).unwrap();
        let keys: Vec<&str> = g.metadata().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                "general.architecture",
                "general.name",
                "llama.context_length",
                "llama.block_count",
                "llama.rope.freq_base"
            ]
        );
    }

    #[test]
    fn typed_accessor_errors() {
        let g = parse(&sample()).unwrap();
        assert!(matches!(g.get_u32("nope"), Err(GgufError::MissingKey(_))));
        assert!(matches!(
            g.get_u32("general.name"),
            Err(GgufError::WrongType {
                expected: "U32",
                found: "STRING",
                ..
            })
        ));
        assert!(matches!(
            g.get_str("llama.block_count"),
            Err(GgufError::WrongType { .. })
        ));
        assert!(matches!(
            g.get_array("general.name"),
            Err(GgufError::WrongType { .. })
        ));
        assert!(matches!(
            g.get_f32("general.name"),
            Err(GgufError::WrongType { .. })
        ));
    }

    #[test]
    fn summary_reports_model_facts() {
        let s = parse(&sample()).unwrap().summary().unwrap();
        assert_eq!(s.architecture, "llama");
        assert_eq!(s.name.as_deref(), Some("tiny"));
        assert_eq!(s.n_params, 8 + 32);
        assert_eq!(s.n_tensor_bytes, 66);
        assert_eq!(s.context_length, Some(2048));
        assert_eq!(s.n_layers, Some(2));
        assert_eq!(s.n_experts, None);
        assert_eq!(
            s.type_histogram,
            vec![(GgmlType::F32, 1), (GgmlType::Q8_0, 1)]
        );
    }

    #[test]
    fn header_only_accepts_truncated_data() {
        let bytes = sample();
        let g = parse(&bytes).unwrap();
        let cut = &bytes[..g.data_start as usize];
        assert!(matches!(
            parse(cut),
            Err(GgufError::TensorOutOfBounds { .. })
        ));
        let h = parse_header_only(cut).unwrap();
        assert!(h.header_only);
        assert_eq!(h.tensors().len(), 2);
        assert!(matches!(
            h.tensor_data(cut, "a"),
            Err(GgufError::TensorOutOfBounds { .. })
        ));
        // A header cut inside the tensor table is still an error.
        assert!(parse_header_only(&bytes[..40]).is_err());
    }

    #[test]
    fn rejects_bad_magic_and_versions() {
        let mut b = sample();
        b[0] = b'X';
        assert!(matches!(parse(&b), Err(GgufError::BadMagic(_))));
        for v in [0u32, 1, 4, 100] {
            let mut b = sample();
            b[4..8].copy_from_slice(&v.to_le_bytes());
            assert!(
                matches!(parse(&b), Err(GgufError::UnsupportedVersion(x)) if x == v),
                "version {v}"
            );
        }
        let mut b = sample();
        b[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(parse(&b).unwrap().version, 2);
    }

    #[test]
    fn huge_counts_do_not_allocate() {
        let mut b = sample();
        b[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            parse(&b),
            Err(GgufError::CountTooLarge { what: "tensor", .. })
        ));
        let mut b = sample();
        b[16..24].copy_from_slice(&(1u64 << 40).to_le_bytes());
        assert!(matches!(
            parse(&b),
            Err(GgufError::CountTooLarge {
                what: "metadata",
                ..
            })
        ));
    }

    #[test]
    fn empty_and_tiny_inputs_error() {
        assert!(matches!(parse(&[]), Err(GgufError::UnexpectedEof { .. })));
        assert!(parse(b"GGUF").is_err());
    }

    #[test]
    fn file_with_no_tensors_is_valid() {
        let mut w = GgufWriter::new();
        w.add_metadata("general.architecture", Value::String("x".into()));
        let b = w.finish().unwrap();
        let g = parse(&b).unwrap();
        assert!(g.tensors().is_empty());
        assert_eq!(g.data_start, b.len() as u64);
        assert_eq!(g.total_tensor_bytes(), 0);
        assert_eq!(g.summary().unwrap().type_histogram, vec![]);
    }

    #[test]
    fn summary_needs_architecture() {
        let b = GgufWriter::new().finish().unwrap();
        assert!(matches!(
            parse(&b).unwrap().summary(),
            Err(GgufError::MissingKey(_))
        ));
    }
}
