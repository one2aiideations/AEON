//! Error type for GGUF parsing, validation and writing.
#![forbid(unsafe_code)]

use crate::types::TypeError;
use std::fmt;

/// Everything that can go wrong reading or writing a GGUF file.
#[derive(Debug)]
pub enum GgufError {
    /// The input ended before `needed` more bytes could be read at `offset`.
    UnexpectedEof {
        /// Byte offset where the read started.
        offset: usize,
        /// Bytes the read required.
        needed: u64,
    },
    /// The magic number is not `GGUF`.
    BadMagic(u32),
    /// The file version is not 2 or 3.
    UnsupportedVersion(u32),
    /// A string had invalid UTF-8, an impossible length, or an invalid key/name.
    BadString {
        /// Byte offset of the string.
        offset: usize,
    },
    /// A metadata value type id is unknown.
    UnknownValueType(u32),
    /// A tensor element type id is unknown.
    UnknownGgmlType(u32),
    /// A boolean byte was neither 0 nor 1.
    BadBool(u8),
    /// A metadata key appears twice.
    DuplicateKey(String),
    /// A tensor name appears twice.
    DuplicateTensor(String),
    /// Arrays are nested more than four levels deep.
    ArrayTooDeep,
    /// A count in the file is larger than the remaining bytes could hold.
    CountTooLarge {
        /// What was being counted.
        what: &'static str,
        /// The count read from the file.
        count: u64,
    },
    /// A tensor has 0 or more than 4 dimensions, or a zero dimension.
    BadDims {
        /// Tensor name.
        tensor: String,
    },
    /// The first dimension is not a multiple of the element type's block size.
    NotBlockAligned {
        /// Tensor name.
        tensor: String,
    },
    /// A size or offset computation overflowed.
    Overflow,
    /// The alignment is zero or not a power of two.
    BadAlignment(u64),
    /// A tensor offset is not a multiple of the alignment.
    MisalignedOffset {
        /// Tensor name.
        tensor: String,
    },
    /// A tensor's data range extends past the end of the file.
    TensorOutOfBounds {
        /// Tensor name.
        tensor: String,
    },
    /// Two tensors' data ranges overlap.
    TensorOverlap {
        /// First tensor.
        a: String,
        /// Second tensor.
        b: String,
    },
    /// A metadata value has a different type than requested.
    WrongType {
        /// Metadata key.
        key: String,
        /// Type the caller asked for.
        expected: &'static str,
        /// Type found.
        found: &'static str,
    },
    /// A required metadata key is absent.
    MissingKey(String),
    /// A named tensor does not exist.
    MissingTensor(String),
    /// Tensor data supplied to the writer has the wrong length.
    DataLengthMismatch {
        /// Tensor name.
        tensor: String,
        /// Bytes the type and dimensions require.
        expected: u64,
        /// Bytes supplied.
        found: u64,
    },
    /// An I/O error.
    Io(std::io::Error),
}

impl fmt::Display for GgufError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GgufError::UnexpectedEof { offset, needed } => {
                write!(
                    f,
                    "unexpected end of file at offset {offset}: needed {needed} more bytes"
                )
            }
            GgufError::BadMagic(m) => write!(f, "bad magic 0x{m:08x}, expected 0x46554747 (GGUF)"),
            GgufError::UnsupportedVersion(v) => {
                write!(f, "unsupported GGUF version {v} (supported: 2, 3)")
            }
            GgufError::BadString { offset } => write!(f, "invalid string at offset {offset}"),
            GgufError::UnknownValueType(t) => write!(f, "unknown metadata value type {t}"),
            GgufError::UnknownGgmlType(t) => write!(f, "unknown tensor element type {t}"),
            GgufError::BadBool(b) => write!(f, "invalid boolean byte {b} (must be 0 or 1)"),
            GgufError::DuplicateKey(k) => write!(f, "duplicate metadata key {k:?}"),
            GgufError::DuplicateTensor(n) => write!(f, "duplicate tensor name {n:?}"),
            GgufError::ArrayTooDeep => f.write_str("arrays nested more than 4 levels deep"),
            GgufError::CountTooLarge { what, count } => {
                write!(
                    f,
                    "{what} count {count} exceeds what the remaining bytes can hold"
                )
            }
            GgufError::BadDims { tensor } => write!(f, "tensor {tensor:?} has invalid dimensions"),
            GgufError::NotBlockAligned { tensor } => {
                write!(
                    f,
                    "tensor {tensor:?}: first dimension is not a multiple of the block size"
                )
            }
            GgufError::Overflow => f.write_str("size or offset computation overflowed"),
            GgufError::BadAlignment(a) => {
                write!(f, "invalid alignment {a} (must be a non-zero power of two)")
            }
            GgufError::MisalignedOffset { tensor } => {
                write!(
                    f,
                    "tensor {tensor:?}: data offset is not a multiple of the alignment"
                )
            }
            GgufError::TensorOutOfBounds { tensor } => {
                write!(f, "tensor {tensor:?} extends past the end of the file")
            }
            GgufError::TensorOverlap { a, b } => write!(f, "tensors {a:?} and {b:?} overlap"),
            GgufError::WrongType {
                key,
                expected,
                found,
            } => {
                write!(
                    f,
                    "metadata key {key:?}: expected {expected}, found {found}"
                )
            }
            GgufError::MissingKey(k) => write!(f, "missing metadata key {k:?}"),
            GgufError::MissingTensor(n) => write!(f, "missing tensor {n:?}"),
            GgufError::DataLengthMismatch {
                tensor,
                expected,
                found,
            } => {
                write!(
                    f,
                    "tensor {tensor:?}: expected {expected} data bytes, got {found}"
                )
            }
            GgufError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for GgufError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GgufError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for GgufError {
    fn from(e: std::io::Error) -> Self {
        GgufError::Io(e)
    }
}

impl From<TypeError> for GgufError {
    /// `NotBlockAligned` loses its tensor name here; the reader reports that case itself.
    fn from(e: TypeError) -> Self {
        match e {
            TypeError::UnknownGgmlType(v) => GgufError::UnknownGgmlType(v),
            TypeError::UnknownValueType(v) => GgufError::UnknownValueType(v),
            TypeError::NotBlockAligned { .. } => GgufError::NotBlockAligned {
                tensor: String::new(),
            },
            TypeError::Overflow => GgufError::Overflow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn display_mentions_offsets_and_names() {
        let s = GgufError::UnexpectedEof {
            offset: 12,
            needed: 8,
        }
        .to_string();
        assert!(s.contains("12") && s.contains('8'));
        assert!(GgufError::DuplicateKey("k".into())
            .to_string()
            .contains("\"k\""));
        assert!(GgufError::TensorOverlap {
            a: "x".into(),
            b: "y".into()
        }
        .to_string()
        .contains("\"y\""));
        assert!(GgufError::BadMagic(0xdead_beef)
            .to_string()
            .contains("deadbeef"));
    }

    #[test]
    fn io_error_has_source() {
        let e = GgufError::from(std::io::Error::other("boom"));
        assert!(e.source().is_some());
        assert!(GgufError::Overflow.source().is_none());
    }

    #[test]
    fn type_errors_convert() {
        assert!(matches!(
            GgufError::from(TypeError::UnknownGgmlType(99)),
            GgufError::UnknownGgmlType(99)
        ));
        assert!(matches!(
            GgufError::from(TypeError::UnknownValueType(77)),
            GgufError::UnknownValueType(77)
        ));
        assert!(matches!(
            GgufError::from(TypeError::Overflow),
            GgufError::Overflow
        ));
    }
}
