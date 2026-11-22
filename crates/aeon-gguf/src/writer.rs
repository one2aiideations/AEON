//! Build valid GGUF v3 files in memory.
#![forbid(unsafe_code)]

use crate::cursor::align_up;
use crate::error::GgufError;
use crate::types::GgmlType;
use crate::value::{Array, Value};
use std::collections::HashSet;

const MAGIC: u32 = 0x4655_4747;
const VERSION: u32 = 3;
const ALIGNMENT_KEY: &str = "general.alignment";

struct PendingTensor {
    name: String,
    dims: Vec<u64>,
    ty: GgmlType,
    data: Vec<u8>,
}

/// Collects metadata and tensors, then lays out a GGUF v3 file.
///
/// Used by tests and by AEON's tiny test-model generator.
pub struct GgufWriter {
    metadata: Vec<(String, Value)>,
    tensors: Vec<PendingTensor>,
    alignment: u32,
}

impl Default for GgufWriter {
    fn default() -> Self {
        Self::new()
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn put_array(out: &mut Vec<u8>, a: &Array) {
    out.extend_from_slice(&(a.element_type() as u32).to_le_bytes());
    out.extend_from_slice(&(a.len() as u64).to_le_bytes());
    match a {
        Array::U8(v) => out.extend_from_slice(v),
        Array::I8(v) => v.iter().for_each(|x| out.push(*x as u8)),
        Array::U16(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::I16(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::U32(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::I32(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::F32(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::Bool(v) => v.iter().for_each(|x| out.push(u8::from(*x))),
        Array::String(v) => v.iter().for_each(|s| put_str(out, s)),
        Array::Array(v) => v.iter().for_each(|x| put_array(out, x)),
        Array::U64(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::I64(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
        Array::F64(v) => v
            .iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
    }
}

fn put_value(out: &mut Vec<u8>, v: &Value) {
    out.extend_from_slice(&(v.value_type() as u32).to_le_bytes());
    match v {
        Value::U8(x) => out.push(*x),
        Value::I8(x) => out.push(*x as u8),
        Value::U16(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::I16(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::U32(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::I32(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::F32(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::Bool(x) => out.push(u8::from(*x)),
        Value::String(s) => put_str(out, s),
        Value::Array(a) => put_array(out, a),
        Value::U64(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::I64(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::F64(x) => out.extend_from_slice(&x.to_le_bytes()),
    }
}

fn pad_to(out: &mut Vec<u8>, len: usize) {
    if out.len() < len {
        out.resize(len, 0);
    }
}

impl GgufWriter {
    /// A writer with the default alignment (32).
    pub fn new() -> Self {
        GgufWriter {
            metadata: Vec::new(),
            tensors: Vec::new(),
            alignment: 32,
        }
    }

    /// Append a metadata entry (validated in [`GgufWriter::finish`]).
    pub fn add_metadata(&mut self, key: &str, value: Value) {
        self.metadata.push((key.to_string(), value));
    }

    /// Set the data alignment; written as `general.alignment` when it is not 32.
    /// Must be a non-zero power of two (checked in [`GgufWriter::finish`]).
    pub fn set_alignment(&mut self, alignment: u32) {
        self.alignment = alignment;
    }

    /// Add a tensor. `data` must be exactly the size the type and dimensions require.
    pub fn add_tensor(
        &mut self,
        name: &str,
        dims: &[u64],
        ty: GgmlType,
        data: Vec<u8>,
    ) -> Result<(), GgufError> {
        if name.is_empty() || name.len() > 65535 {
            return Err(GgufError::BadString { offset: 0 });
        }
        if self.tensors.iter().any(|t| t.name == name) {
            return Err(GgufError::DuplicateTensor(name.to_string()));
        }
        if dims.is_empty() || dims.len() > 4 || dims.contains(&0) {
            return Err(GgufError::BadDims {
                tensor: name.to_string(),
            });
        }
        if !dims[0].is_multiple_of(ty.block_size()) {
            return Err(GgufError::NotBlockAligned {
                tensor: name.to_string(),
            });
        }
        let n_elements = dims
            .iter()
            .try_fold(1u64, |a, d| a.checked_mul(*d))
            .ok_or(GgufError::Overflow)?;
        let expected = ty.row_bytes(n_elements)?;
        if data.len() as u64 != expected {
            return Err(GgufError::DataLengthMismatch {
                tensor: name.to_string(),
                expected,
                found: data.len() as u64,
            });
        }
        self.tensors.push(PendingTensor {
            name: name.to_string(),
            dims: dims.to_vec(),
            ty,
            data,
        });
        Ok(())
    }

    /// Lay out and return the file bytes.
    pub fn finish(self) -> Result<Vec<u8>, GgufError> {
        let alignment = u64::from(self.alignment);
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(GgufError::BadAlignment(alignment));
        }
        let mut metadata = self.metadata;
        metadata.retain(|(k, _)| k != ALIGNMENT_KEY);
        if self.alignment != 32 {
            metadata.insert(0, (ALIGNMENT_KEY.to_string(), Value::U32(self.alignment)));
        }
        let mut keys = HashSet::new();
        for (k, _) in &metadata {
            if k.is_empty() || k.len() > 65535 || !k.is_ascii() {
                return Err(GgufError::BadString { offset: 0 });
            }
            if !keys.insert(k.as_str()) {
                return Err(GgufError::DuplicateKey(k.clone()));
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.tensors.len() as u64).to_le_bytes());
        out.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
        for (k, v) in &metadata {
            put_str(&mut out, k);
            put_value(&mut out, v);
        }

        let mut offsets = Vec::with_capacity(self.tensors.len());
        let mut cursor = 0u64;
        for t in &self.tensors {
            let offset = align_up(cursor, alignment)?;
            cursor = offset
                .checked_add(t.data.len() as u64)
                .ok_or(GgufError::Overflow)?;
            offsets.push(offset);
            put_str(&mut out, &t.name);
            out.extend_from_slice(&(t.dims.len() as u32).to_le_bytes());
            for d in &t.dims {
                out.extend_from_slice(&d.to_le_bytes());
            }
            out.extend_from_slice(&(t.ty as u32).to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
        }

        let data_start = align_up(out.len() as u64, alignment)?;
        pad_to(
            &mut out,
            usize::try_from(data_start).map_err(|_| GgufError::Overflow)?,
        );
        for (t, offset) in self.tensors.iter().zip(offsets) {
            let at = data_start.checked_add(offset).ok_or(GgufError::Overflow)?;
            pad_to(
                &mut out,
                usize::try_from(at).map_err(|_| GgufError::Overflow)?,
            );
            out.extend_from_slice(&t.data);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::parse;

    #[test]
    fn empty_file_is_valid_v3() {
        let b = GgufWriter::new().finish().unwrap();
        assert_eq!(&b[..4], b"GGUF");
        assert_eq!(u32::from_le_bytes([b[4], b[5], b[6], b[7]]), 3);
        assert!(parse(&b).is_ok());
    }

    #[test]
    fn alignment_is_recorded_and_applied() {
        let mut w = GgufWriter::new();
        w.set_alignment(64);
        w.add_tensor("a", &[4], GgmlType::F32, vec![0; 16]).unwrap();
        w.add_tensor("b", &[4], GgmlType::F32, vec![0; 16]).unwrap();
        let b = w.finish().unwrap();
        let g = parse(&b).unwrap();
        assert_eq!(g.alignment, 64);
        assert_eq!(g.data_start % 64, 0);
        assert_eq!(g.tensor("b").unwrap().offset, 64);
        assert_eq!(g.get_u32("general.alignment").unwrap(), 64);
    }

    #[test]
    fn user_alignment_key_is_replaced_by_set_alignment() {
        let mut w = GgufWriter::new();
        w.add_metadata("general.alignment", Value::U32(16));
        w.set_alignment(128);
        let g = parse(&w.finish().unwrap()).unwrap();
        assert_eq!(g.alignment, 128);
        assert_eq!(
            g.metadata()
                .filter(|(k, _)| *k == "general.alignment")
                .count(),
            1
        );
    }

    #[test]
    fn invalid_alignment_is_rejected() {
        for a in [0u32, 3, 100] {
            let mut w = GgufWriter::new();
            w.set_alignment(a);
            assert!(
                matches!(w.finish(), Err(GgufError::BadAlignment(_))),
                "alignment {a}"
            );
        }
    }

    #[test]
    fn add_tensor_validates() {
        let mut w = GgufWriter::new();
        assert!(matches!(
            w.add_tensor("", &[4], GgmlType::F32, vec![0; 16]),
            Err(GgufError::BadString { .. })
        ));
        assert!(matches!(
            w.add_tensor("a", &[], GgmlType::F32, vec![]),
            Err(GgufError::BadDims { .. })
        ));
        assert!(matches!(
            w.add_tensor("a", &[1, 1, 1, 1, 1], GgmlType::F32, vec![0; 4]),
            Err(GgufError::BadDims { .. })
        ));
        assert!(matches!(
            w.add_tensor("a", &[4, 0], GgmlType::F32, vec![]),
            Err(GgufError::BadDims { .. })
        ));
        assert!(matches!(
            w.add_tensor("a", &[33], GgmlType::Q8_0, vec![0; 34]),
            Err(GgufError::NotBlockAligned { .. })
        ));
        assert!(matches!(
            w.add_tensor("a", &[4], GgmlType::F32, vec![0; 15]),
            Err(GgufError::DataLengthMismatch {
                expected: 16,
                found: 15,
                ..
            })
        ));
        assert!(matches!(
            w.add_tensor("a", &[u64::MAX, 2], GgmlType::I8, vec![]),
            Err(GgufError::Overflow)
        ));
        w.add_tensor("a", &[4], GgmlType::F32, vec![0; 16]).unwrap();
        assert!(matches!(
            w.add_tensor("a", &[4], GgmlType::F32, vec![0; 16]),
            Err(GgufError::DuplicateTensor(_))
        ));
    }

    #[test]
    fn finish_validates_metadata_keys() {
        let mut w = GgufWriter::new();
        w.add_metadata("k", Value::U8(1));
        w.add_metadata("k", Value::U8(2));
        assert!(matches!(w.finish(), Err(GgufError::DuplicateKey(_))));
        let mut w = GgufWriter::new();
        w.add_metadata("", Value::U8(1));
        assert!(matches!(w.finish(), Err(GgufError::BadString { .. })));
        let mut w = GgufWriter::new();
        w.add_metadata("clé", Value::U8(1));
        assert!(matches!(w.finish(), Err(GgufError::BadString { .. })));
    }

    #[test]
    fn tensor_data_lands_at_aligned_offsets() {
        let mut w = GgufWriter::new();
        w.add_tensor("a", &[1], GgmlType::I8, vec![7]).unwrap();
        w.add_tensor("b", &[1], GgmlType::I8, vec![9]).unwrap();
        let bytes = w.finish().unwrap();
        let g = parse(&bytes).unwrap();
        assert_eq!(g.tensor("b").unwrap().offset, 32);
        assert_eq!(g.tensor_data(&bytes, "a").unwrap(), &[7]);
        assert_eq!(g.tensor_data(&bytes, "b").unwrap(), &[9]);
        // padding between tensors is zero
        let start = g.data_start as usize;
        assert!(bytes[start + 1..start + 32].iter().all(|b| *b == 0));
    }
}
