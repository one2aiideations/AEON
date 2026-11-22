//! Tensor element types and metadata value types from the GGUF v3 specification.
//!
//! Covers the `GGML_TYPE_*` element type ids used in tensor infos and the
//! metadata value type ids used in key/value pairs, along with the block
//! geometry needed to compute the byte size of a tensor's data.

use std::fmt;

/// Element type of a tensor, as stored in the `type` field of a GGUF tensor
/// info. The discriminants match the `GGML_TYPE_*` values from the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
#[allow(non_camel_case_types)] // names follow the GGUF specification
pub enum GgmlType {
    /// 32-bit float.
    F32 = 0,
    /// 16-bit IEEE float.
    F16 = 1,
    /// 4-bit, 32-element blocks with one f16 scale.
    Q4_0 = 2,
    /// 4-bit, 32-element blocks with f16 scale and minimum.
    Q4_1 = 3,
    /// 5-bit, 32-element blocks with one f16 scale.
    Q5_0 = 6,
    /// 5-bit, 32-element blocks with f16 scale and minimum.
    Q5_1 = 7,
    /// 8-bit, 32-element blocks with one f16 scale.
    Q8_0 = 8,
    /// 8-bit, 32-element blocks with f16 scale and sum.
    Q8_1 = 9,
    /// 2-bit k-quant, 256-element super-blocks.
    Q2_K = 10,
    /// 3-bit k-quant, 256-element super-blocks.
    Q3_K = 11,
    /// 4-bit k-quant, 256-element super-blocks.
    Q4_K = 12,
    /// 5-bit k-quant, 256-element super-blocks.
    Q5_K = 13,
    /// 6-bit k-quant, 256-element super-blocks.
    Q6_K = 14,
    /// 8-bit k-quant, 256-element super-blocks.
    Q8_K = 15,
    /// 8-bit signed integer.
    I8 = 24,
    /// 16-bit signed integer.
    I16 = 25,
    /// 32-bit signed integer.
    I32 = 26,
    /// 64-bit signed integer.
    I64 = 27,
    /// 64-bit float.
    F64 = 28,
    /// bfloat16.
    BF16 = 30,
}

impl GgmlType {
    /// Resolves a raw `u32` from the file to a known type.
    pub fn from_u32(v: u32) -> Result<Self, TypeError> {
        match v {
            0 => Ok(Self::F32),
            1 => Ok(Self::F16),
            2 => Ok(Self::Q4_0),
            3 => Ok(Self::Q4_1),
            6 => Ok(Self::Q5_0),
            7 => Ok(Self::Q5_1),
            8 => Ok(Self::Q8_0),
            9 => Ok(Self::Q8_1),
            10 => Ok(Self::Q2_K),
            11 => Ok(Self::Q3_K),
            12 => Ok(Self::Q4_K),
            13 => Ok(Self::Q5_K),
            14 => Ok(Self::Q6_K),
            15 => Ok(Self::Q8_K),
            24 => Ok(Self::I8),
            25 => Ok(Self::I16),
            26 => Ok(Self::I32),
            27 => Ok(Self::I64),
            28 => Ok(Self::F64),
            30 => Ok(Self::BF16),
            _ => Err(TypeError::UnknownGgmlType(v)),
        }
    }

    /// Number of elements contained in one block.
    pub fn block_size(self) -> u64 {
        match self {
            Self::F32
            | Self::F16
            | Self::BF16
            | Self::F64
            | Self::I8
            | Self::I16
            | Self::I32
            | Self::I64 => 1,
            Self::Q4_0 | Self::Q4_1 | Self::Q5_0 | Self::Q5_1 | Self::Q8_0 | Self::Q8_1 => 32,
            Self::Q2_K | Self::Q3_K | Self::Q4_K | Self::Q5_K | Self::Q6_K | Self::Q8_K => 256,
        }
    }

    /// Number of bytes occupied by one block, including any scales/zero points.
    pub fn type_size(self) -> u64 {
        match self {
            Self::I8 => 1,
            Self::F16 | Self::BF16 | Self::I16 => 2,
            Self::F32 | Self::I32 => 4,
            Self::F64 | Self::I64 => 8,
            Self::Q4_0 => 18,
            Self::Q4_1 => 20,
            Self::Q5_0 => 22,
            Self::Q5_1 => 24,
            Self::Q8_0 => 34,
            Self::Q8_1 => 36,
            Self::Q2_K => 84,
            Self::Q3_K => 110,
            Self::Q4_K => 144,
            Self::Q5_K => 176,
            Self::Q6_K => 210,
            Self::Q8_K => 292,
        }
    }

    /// Canonical short name, e.g. `"Q4_K"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::F32 => "F32",
            Self::F16 => "F16",
            Self::Q4_0 => "Q4_0",
            Self::Q4_1 => "Q4_1",
            Self::Q5_0 => "Q5_0",
            Self::Q5_1 => "Q5_1",
            Self::Q8_0 => "Q8_0",
            Self::Q8_1 => "Q8_1",
            Self::Q2_K => "Q2_K",
            Self::Q3_K => "Q3_K",
            Self::Q4_K => "Q4_K",
            Self::Q5_K => "Q5_K",
            Self::Q6_K => "Q6_K",
            Self::Q8_K => "Q8_K",
            Self::I8 => "I8",
            Self::I16 => "I16",
            Self::I32 => "I32",
            Self::I64 => "I64",
            Self::F64 => "F64",
            Self::BF16 => "BF16",
        }
    }

    /// Bytes required to store `n_elements` elements of this type.
    ///
    /// Returns [`TypeError::NotBlockAligned`] if `n_elements` is not a multiple
    /// of [`GgmlType::block_size`], and [`TypeError::Overflow`] if the size
    /// computation exceeds `u64`.
    pub fn row_bytes(self, n_elements: u64) -> Result<u64, TypeError> {
        let block = self.block_size();
        if !n_elements.is_multiple_of(block) {
            return Err(TypeError::NotBlockAligned {
                ty: self,
                n: n_elements,
            });
        }
        let blocks = n_elements.checked_div(block).ok_or(TypeError::Overflow)?;
        blocks
            .checked_mul(self.type_size())
            .ok_or(TypeError::Overflow)
    }
}

/// Type of a metadata value, as stored in the `value_type` field of a GGUF
/// metadata key/value pair. The discriminants match the
/// `GGUF_METADATA_VALUE_TYPE_*` values from the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
#[allow(clippy::upper_case_acronyms)] // names follow the GGUF specification
pub enum ValueType {
    /// Unsigned 8-bit integer.
    U8 = 0,
    /// Signed 8-bit integer.
    I8 = 1,
    /// Unsigned 16-bit integer.
    U16 = 2,
    /// Signed 16-bit integer.
    I16 = 3,
    /// Unsigned 32-bit integer.
    U32 = 4,
    /// Signed 32-bit integer.
    I32 = 5,
    /// 32-bit float.
    F32 = 6,
    /// Boolean stored as one byte (0 or 1).
    BOOL = 7,
    /// Length-prefixed UTF-8 string.
    STRING = 8,
    /// Typed array: element type, count, elements.
    ARRAY = 9,
    /// Unsigned 64-bit integer.
    U64 = 10,
    /// Signed 64-bit integer.
    I64 = 11,
    /// 64-bit float.
    F64 = 12,
}

impl ValueType {
    /// Resolves a raw `u32` from the file to a known value type.
    pub fn from_u32(v: u32) -> Result<Self, TypeError> {
        match v {
            0 => Ok(Self::U8),
            1 => Ok(Self::I8),
            2 => Ok(Self::U16),
            3 => Ok(Self::I16),
            4 => Ok(Self::U32),
            5 => Ok(Self::I32),
            6 => Ok(Self::F32),
            7 => Ok(Self::BOOL),
            8 => Ok(Self::STRING),
            9 => Ok(Self::ARRAY),
            10 => Ok(Self::U64),
            11 => Ok(Self::I64),
            12 => Ok(Self::F64),
            _ => Err(TypeError::UnknownValueType(v)),
        }
    }

    /// Size in bytes for scalar types, or `None` for variable-length types
    /// (`STRING` and `ARRAY`). `BOOL` is one byte, matching the on-disk
    /// representation.
    pub fn fixed_size(self) -> Option<u64> {
        match self {
            Self::U8 | Self::I8 | Self::BOOL => Some(1),
            Self::U16 | Self::I16 => Some(2),
            Self::U32 | Self::I32 | Self::F32 => Some(4),
            Self::U64 | Self::I64 | Self::F64 => Some(8),
            Self::STRING | Self::ARRAY => None,
        }
    }
}

/// Errors produced while decoding type ids and computing tensor sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeError {
    /// The raw id does not correspond to any known `GGML_TYPE_*` value.
    UnknownGgmlType(u32),
    /// The raw id does not correspond to any known metadata value type.
    UnknownValueType(u32),
    /// `n` elements is not a multiple of the block size of `ty`.
    NotBlockAligned {
        /// Element type whose block size was violated.
        ty: GgmlType,
        /// Element count that was requested.
        n: u64,
    },
    /// A size computation overflowed `u64`.
    Overflow,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeError::UnknownGgmlType(v) => write!(f, "unknown GGML type id: {v}"),
            TypeError::UnknownValueType(v) => write!(f, "unknown metadata value type id: {v}"),
            TypeError::NotBlockAligned { ty, n } => write!(
                f,
                "element count {n} is not aligned to block size ({}) of type {}",
                ty.block_size(),
                ty.name()
            ),
            TypeError::Overflow => f.write_str("size computation overflowed"),
        }
    }
}

impl std::error::Error for TypeError {}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_GGML_TYPES: [GgmlType; 20] = [
        GgmlType::F32,
        GgmlType::F16,
        GgmlType::Q4_0,
        GgmlType::Q4_1,
        GgmlType::Q5_0,
        GgmlType::Q5_1,
        GgmlType::Q8_0,
        GgmlType::Q8_1,
        GgmlType::Q2_K,
        GgmlType::Q3_K,
        GgmlType::Q4_K,
        GgmlType::Q5_K,
        GgmlType::Q6_K,
        GgmlType::Q8_K,
        GgmlType::I8,
        GgmlType::I16,
        GgmlType::I32,
        GgmlType::I64,
        GgmlType::F64,
        GgmlType::BF16,
    ];

    const ALL_VALUE_TYPES: [ValueType; 13] = [
        ValueType::U8,
        ValueType::I8,
        ValueType::U16,
        ValueType::I16,
        ValueType::U32,
        ValueType::I32,
        ValueType::F32,
        ValueType::BOOL,
        ValueType::STRING,
        ValueType::ARRAY,
        ValueType::U64,
        ValueType::I64,
        ValueType::F64,
    ];

    #[test]
    fn ggml_type_ids_round_trip() {
        for ty in ALL_GGML_TYPES {
            assert_eq!(GgmlType::from_u32(ty as u32), Ok(ty));
        }
    }

    #[test]
    fn ggml_type_unknown_ids_error() {
        for v in [4, 5, 16, 23, 29, 31, 100, u32::MAX] {
            assert_eq!(GgmlType::from_u32(v), Err(TypeError::UnknownGgmlType(v)));
        }
    }

    #[test]
    fn ggml_type_block_and_type_sizes() {
        let expected: &[(GgmlType, u64, u64)] = &[
            (GgmlType::F32, 1, 4),
            (GgmlType::F16, 1, 2),
            (GgmlType::BF16, 1, 2),
            (GgmlType::F64, 1, 8),
            (GgmlType::I8, 1, 1),
            (GgmlType::I16, 1, 2),
            (GgmlType::I32, 1, 4),
            (GgmlType::I64, 1, 8),
            (GgmlType::Q4_0, 32, 18),
            (GgmlType::Q4_1, 32, 20),
            (GgmlType::Q5_0, 32, 22),
            (GgmlType::Q5_1, 32, 24),
            (GgmlType::Q8_0, 32, 34),
            (GgmlType::Q8_1, 32, 36),
            (GgmlType::Q2_K, 256, 84),
            (GgmlType::Q3_K, 256, 110),
            (GgmlType::Q4_K, 256, 144),
            (GgmlType::Q5_K, 256, 176),
            (GgmlType::Q6_K, 256, 210),
            (GgmlType::Q8_K, 256, 292),
        ];
        for &(ty, block, bytes) in expected {
            assert_eq!(ty.block_size(), block, "{} block_size", ty.name());
            assert_eq!(ty.type_size(), bytes, "{} type_size", ty.name());
        }
    }

    #[test]
    fn value_type_ids_round_trip() {
        for vt in ALL_VALUE_TYPES {
            assert_eq!(ValueType::from_u32(vt as u32), Ok(vt));
        }
    }

    #[test]
    fn value_type_unknown_ids_error() {
        for v in [13, 14, 255, u32::MAX] {
            assert_eq!(ValueType::from_u32(v), Err(TypeError::UnknownValueType(v)));
        }
    }

    #[test]
    fn value_type_fixed_sizes() {
        assert_eq!(ValueType::U8.fixed_size(), Some(1));
        assert_eq!(ValueType::I8.fixed_size(), Some(1));
        assert_eq!(ValueType::BOOL.fixed_size(), Some(1));
        assert_eq!(ValueType::U16.fixed_size(), Some(2));
        assert_eq!(ValueType::I16.fixed_size(), Some(2));
        assert_eq!(ValueType::U32.fixed_size(), Some(4));
        assert_eq!(ValueType::I32.fixed_size(), Some(4));
        assert_eq!(ValueType::F32.fixed_size(), Some(4));
        assert_eq!(ValueType::U64.fixed_size(), Some(8));
        assert_eq!(ValueType::I64.fixed_size(), Some(8));
        assert_eq!(ValueType::F64.fixed_size(), Some(8));
        assert_eq!(ValueType::STRING.fixed_size(), None);
        assert_eq!(ValueType::ARRAY.fixed_size(), None);
    }

    #[test]
    fn row_bytes_q4_k_512_elements() {
        assert_eq!(GgmlType::Q4_K.row_bytes(512), Ok(288));
    }

    #[test]
    fn row_bytes_q8_0_unaligned_errors() {
        assert_eq!(
            GgmlType::Q8_0.row_bytes(33),
            Err(TypeError::NotBlockAligned {
                ty: GgmlType::Q8_0,
                n: 33,
            })
        );
    }

    #[test]
    fn row_bytes_f32_overflow_errors() {
        assert_eq!(GgmlType::F32.row_bytes(u64::MAX), Err(TypeError::Overflow));
    }

    #[test]
    fn row_bytes_zero_elements_is_zero() {
        assert_eq!(GgmlType::Q4_K.row_bytes(0), Ok(0));
    }

    #[test]
    fn row_bytes_single_block() {
        assert_eq!(GgmlType::Q5_K.row_bytes(256), Ok(176));
    }
}
