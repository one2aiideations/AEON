//! CPU forward pass for llama-family models stored as GGUF (engine milestone E2).
//!
//! Supports architectures `llama` (interleaved RoPE) and `qwen2` (split-half RoPE, QKV biases),
//! dense feed-forward only. Weights stay quantised in the file bytes and are dequantised row by
//! row inside each matrix-vector product, so memory use is the model file plus the KV cache.
//!
//! # Verification status
//!
//! Tests compare the engine with an independent naive implementation on tiny synthetic models
//! and check quantised-vs-f32 closeness. They do **not** compare logits with a reference engine
//! on a real model; milestone E2 requires that before outputs are trusted. Speed is scalar and
//! single-threaded: this is the correctness baseline for later SIMD work (E3).

pub mod gguf {
    //! GGUF model file reader and writer for the MOULI engine.
    //!
    //! Parses GGUF versions 2 and 3: header, typed metadata and tensor infos, with
    //! full validation (alignment, offsets, bounds, overlaps, block alignment,
    //! duplicates). Also writes valid v3 files, and maps files from disk.
    //!
    //! # Safety guarantees
    //!
    //! * No `unsafe` code except one `Mmap::map` call in [`mmap`].
    //! * Parsing never panics on input data: every read is bounds-checked and every
    //!   size computation is checked.
    //! * Counts read from a file are capped by the bytes remaining before anything
    //!   is allocated, so a malicious file cannot request more memory than its own
    //!   length allows.
    #![deny(unsafe_code)]

    mod cursor {
        //! Bounds-checked little-endian reader over a byte slice.
        #![forbid(unsafe_code)]

        use crate::gguf::error::GgufError;

        /// Reads from a slice; every method reports the offset on failure.
        pub(crate) struct Cursor<'a> {
            data: &'a [u8],
            pos: usize,
        }

        macro_rules! read_num {
            ($name:ident, $ty:ty, $n:expr) => {
                pub(crate) fn $name(&mut self) -> Result<$ty, GgufError> {
                    let b = self.read_bytes($n)?;
                    let mut a = [0u8; $n];
                    a.copy_from_slice(b);
                    Ok(<$ty>::from_le_bytes(a))
                }
            };
        }

        impl<'a> Cursor<'a> {
            pub(crate) fn new(data: &'a [u8]) -> Self {
                Cursor { data, pos: 0 }
            }

            pub(crate) fn pos(&self) -> usize {
                self.pos
            }

            pub(crate) fn remaining(&self) -> usize {
                self.data.len().saturating_sub(self.pos)
            }

            pub(crate) fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], GgufError> {
                let end = self
                    .pos
                    .checked_add(n)
                    .filter(|e| *e <= self.data.len())
                    .ok_or(GgufError::UnexpectedEof {
                        offset: self.pos,
                        needed: n as u64,
                    })?;
                let s = &self.data[self.pos..end];
                self.pos = end;
                Ok(s)
            }

            pub(crate) fn read_u8(&mut self) -> Result<u8, GgufError> {
                Ok(self.read_bytes(1)?[0])
            }

            read_num!(read_u16, u16, 2);
            read_num!(read_u32, u32, 4);
            read_num!(read_u64, u64, 8);
            read_num!(read_i16, i16, 2);
            read_num!(read_i32, i32, 4);
            read_num!(read_i64, i64, 8);
            read_num!(read_f32, f32, 4);
            read_num!(read_f64, f64, 8);

            pub(crate) fn read_i8(&mut self) -> Result<i8, GgufError> {
                Ok(self.read_u8()? as i8)
            }

            /// A gguf_string: u64 length then that many UTF-8 bytes.
            pub(crate) fn read_string(&mut self) -> Result<String, GgufError> {
                let start = self.pos;
                let len = self.read_u64()?;
                let len = usize::try_from(len)
                    .ok()
                    .filter(|l| *l <= self.remaining())
                    .ok_or(GgufError::BadString { offset: start })?;
                let bytes = self.read_bytes(len)?;
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|_| GgufError::BadString { offset: start })
            }

            /// Advance to the next multiple of `alignment` (a power of two).
            #[cfg(test)]
            pub(crate) fn align_to(&mut self, alignment: u64) -> Result<(), GgufError> {
                let pos = self.pos as u64;
                let aligned = align_up(pos, alignment)?;
                let n = usize::try_from(aligned - pos).map_err(|_| GgufError::Overflow)?;
                self.read_bytes(n).map(|_| ())
            }
        }

        /// Round `x` up to a multiple of `alignment` (non-zero power of two), checked.
        pub(crate) fn align_up(x: u64, alignment: u64) -> Result<u64, GgufError> {
            if alignment == 0 || !alignment.is_power_of_two() {
                return Err(GgufError::BadAlignment(alignment));
            }
            let mask = alignment - 1;
            x.checked_add(mask)
                .map(|v| v & !mask)
                .ok_or(GgufError::Overflow)
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn reads_little_endian_numbers() {
                let data = [1u8, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0xff];
                let mut c = Cursor::new(&data);
                assert_eq!(c.read_u32().unwrap(), 1);
                assert_eq!(c.read_u64().unwrap(), 2);
                assert_eq!(c.read_i8().unwrap(), -1);
                assert_eq!(c.remaining(), 0);
                assert_eq!(c.pos(), 13);
            }

            #[test]
            fn eof_reports_offset_and_need() {
                let mut c = Cursor::new(&[1, 2, 3]);
                c.read_u8().unwrap();
                match c.read_u32() {
                    Err(GgufError::UnexpectedEof { offset, needed }) => {
                        assert_eq!((offset, needed), (1, 4))
                    }
                    other => panic!("{other:?}"),
                }
                // A failed read does not move the cursor.
                assert_eq!(c.pos(), 1);
            }

            #[test]
            fn floats_and_signed() {
                let mut v = Vec::new();
                v.extend_from_slice(&1.5f32.to_le_bytes());
                v.extend_from_slice(&(-2.25f64).to_le_bytes());
                v.extend_from_slice(&(-7i16).to_le_bytes());
                v.extend_from_slice(&(-9i32).to_le_bytes());
                v.extend_from_slice(&(-11i64).to_le_bytes());
                let mut c = Cursor::new(&v);
                assert_eq!(c.read_f32().unwrap(), 1.5);
                assert_eq!(c.read_f64().unwrap(), -2.25);
                assert_eq!(c.read_i16().unwrap(), -7);
                assert_eq!(c.read_i32().unwrap(), -9);
                assert_eq!(c.read_i64().unwrap(), -11);
            }

            #[test]
            fn strings() {
                let mut v = 3u64.to_le_bytes().to_vec();
                v.extend_from_slice(b"abc");
                assert_eq!(Cursor::new(&v).read_string().unwrap(), "abc");

                let mut bad_len = u64::MAX.to_le_bytes().to_vec();
                bad_len.extend_from_slice(b"abc");
                assert!(matches!(
                    Cursor::new(&bad_len).read_string(),
                    Err(GgufError::BadString { offset: 0 })
                ));

                let mut too_long = 10u64.to_le_bytes().to_vec();
                too_long.extend_from_slice(b"abc");
                assert!(matches!(
                    Cursor::new(&too_long).read_string(),
                    Err(GgufError::BadString { .. })
                ));

                let mut bad_utf8 = 2u64.to_le_bytes().to_vec();
                bad_utf8.extend_from_slice(&[0xff, 0xfe]);
                assert!(matches!(
                    Cursor::new(&bad_utf8).read_string(),
                    Err(GgufError::BadString { .. })
                ));

                let empty = 0u64.to_le_bytes();
                assert_eq!(Cursor::new(&empty).read_string().unwrap(), "");
            }

            #[test]
            fn align_helpers() {
                assert_eq!(align_up(0, 32).unwrap(), 0);
                assert_eq!(align_up(1, 32).unwrap(), 32);
                assert_eq!(align_up(32, 32).unwrap(), 32);
                assert_eq!(align_up(33, 8).unwrap(), 40);
                assert!(matches!(align_up(1, 0), Err(GgufError::BadAlignment(0))));
                assert!(matches!(align_up(1, 3), Err(GgufError::BadAlignment(3))));
                assert!(matches!(align_up(u64::MAX, 32), Err(GgufError::Overflow)));
                let data = [0u8; 40];
                let mut c = Cursor::new(&data);
                c.read_bytes(5).unwrap();
                c.align_to(16).unwrap();
                assert_eq!(c.pos(), 16);
                let mut c = Cursor::new(&data[..10]);
                c.read_bytes(5).unwrap();
                assert!(c.align_to(16).is_err());
            }
        }
    }
    pub mod error {
        //! Error type for GGUF parsing, validation and writing.
        #![forbid(unsafe_code)]

        use crate::gguf::types::TypeError;
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
                    GgufError::BadMagic(m) => {
                        write!(f, "bad magic 0x{m:08x}, expected 0x46554747 (GGUF)")
                    }
                    GgufError::UnsupportedVersion(v) => {
                        write!(f, "unsupported GGUF version {v} (supported: 2, 3)")
                    }
                    GgufError::BadString { offset } => {
                        write!(f, "invalid string at offset {offset}")
                    }
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
                    GgufError::BadDims { tensor } => {
                        write!(f, "tensor {tensor:?} has invalid dimensions")
                    }
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
                    GgufError::TensorOverlap { a, b } => {
                        write!(f, "tensors {a:?} and {b:?} overlap")
                    }
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
    }
    pub mod mmap {
        //! Memory-mapped GGUF files.
        //!
        //! This module holds the crate's only `unsafe` code: one `Mmap::map` call.
        #![allow(unsafe_code)]

        use crate::gguf::error::GgufError;
        use crate::gguf::reader::{parse, Gguf};
        use memmap2::Mmap;
        use std::fs::File;
        use std::path::Path;

        /// A GGUF file mapped read-only into memory, with its parsed table.
        ///
        /// The file must not be truncated or modified while it is mapped. Truncating a mapped
        /// file makes reads of the removed pages fault (`SIGBUS` on Linux), which aborts the
        /// process; that cannot be prevented from inside this crate. MOULI model files are
        /// written once and then treated as immutable.
        pub struct MappedGguf {
            map: Mmap,
            gguf: Gguf,
        }

        impl MappedGguf {
            /// Map and fully validate the file at `path`.
            pub fn open(path: impl AsRef<Path>) -> Result<Self, GgufError> {
                let file = File::open(path)?;
                // SAFETY: the mapping is read-only and private to this process. The invariant the
                // caller must uphold is that no one truncates or rewrites the file while this value
                // lives (see the type docs); otherwise only page faults on removed pages are possible,
                // not memory corruption through this API, which exposes the map as `&[u8]` only.
                let map = unsafe { Mmap::map(&file)? };
                let gguf = parse(&map)?;
                Ok(MappedGguf { map, gguf })
            }

            /// The parsed file table.
            pub fn gguf(&self) -> &Gguf {
                &self.gguf
            }

            /// The whole mapped file.
            pub fn bytes(&self) -> &[u8] {
                &self.map
            }

            /// The bytes of one tensor.
            pub fn tensor_data(&self, name: &str) -> Result<&[u8], GgufError> {
                self.gguf.tensor_data(&self.map, name)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::gguf::types::GgmlType;
            use crate::gguf::value::Value;
            use crate::gguf::writer::GgufWriter;

            fn temp_path(tag: &str) -> std::path::PathBuf {
                let n = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                std::env::temp_dir()
                    .join(format!("mouli-gguf-{tag}-{}-{n}.gguf", std::process::id()))
            }

            #[test]
            fn maps_and_reads_tensors() {
                let mut w = GgufWriter::new();
                w.add_metadata("general.architecture", Value::String("llama".into()));
                w.add_tensor("w", &[4], GgmlType::F32, vec![3; 16]).unwrap();
                let path = temp_path("ok");
                std::fs::write(&path, w.finish().unwrap()).unwrap();
                let m = MappedGguf::open(&path).unwrap();
                assert_eq!(m.gguf().architecture().unwrap(), "llama");
                assert_eq!(m.tensor_data("w").unwrap(), &[3u8; 16][..]);
                assert!(m.bytes().starts_with(b"GGUF"));
                assert!(m.tensor_data("missing").is_err());
                drop(m);
                std::fs::remove_file(path).unwrap();
            }

            #[test]
            fn missing_file_and_bad_file_are_errors() {
                assert!(matches!(
                    MappedGguf::open("/nonexistent/mouli/model.gguf"),
                    Err(GgufError::Io(_))
                ));
                let path = temp_path("bad");
                std::fs::write(&path, b"not a gguf file at all").unwrap();
                assert!(matches!(
                    MappedGguf::open(&path),
                    Err(GgufError::BadMagic(_))
                ));
                std::fs::remove_file(path).unwrap();
            }

            #[test]
            fn empty_file_cannot_be_mapped_but_is_reported() {
                let path = temp_path("empty");
                std::fs::write(&path, b"").unwrap();
                assert!(MappedGguf::open(&path).is_err());
                std::fs::remove_file(path).unwrap();
            }
        }
    }
    pub mod reader {
        //! GGUF parsing and validation.
        #![forbid(unsafe_code)]

        use crate::gguf::cursor::{align_up, Cursor};
        use crate::gguf::error::GgufError;
        use crate::gguf::tensor::TensorInfo;
        use crate::gguf::types::{GgmlType, ValueType};
        use crate::gguf::value::{parse_value, Array, Value};
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
            let mut metadata_index: HashMap<String, usize> =
                HashMap::with_capacity(kv_count as usize);
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
            let mut tensor_index: HashMap<String, usize> =
                HashMap::with_capacity(tensor_count as usize);
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
            pub fn tensor_data<'a>(
                &self,
                file: &'a [u8],
                name: &str,
            ) -> Result<&'a [u8], GgufError> {
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
            use crate::gguf::writer::GgufWriter;

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
    }
    pub mod tensor {
        //! Tensor descriptors.
        #![forbid(unsafe_code)]

        use crate::gguf::error::GgufError;
        use crate::gguf::types::GgmlType;
        use std::ops::Range;

        /// One tensor's description from the GGUF tensor info table.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct TensorInfo {
            /// Unique, non-empty name.
            pub name: String,
            /// Dimensions, 1 to 4 entries, each at least 1. `dims[0]` is the contiguous one.
            pub dims: Vec<u64>,
            /// Element type.
            pub ty: GgmlType,
            /// Offset of the data from the start of the tensor data section.
            pub offset: u64,
            /// Total element count (product of `dims`).
            pub n_elements: u64,
            /// Data size in bytes.
            pub n_bytes: u64,
        }

        impl TensorInfo {
            /// Absolute byte range of this tensor's data, given where the data section starts.
            pub fn data_range(&self, data_start: u64) -> Result<Range<u64>, GgufError> {
                let start = data_start
                    .checked_add(self.offset)
                    .ok_or(GgufError::Overflow)?;
                let end = start.checked_add(self.n_bytes).ok_or(GgufError::Overflow)?;
                Ok(start..end)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn info(offset: u64, n_bytes: u64) -> TensorInfo {
                TensorInfo {
                    name: "t".into(),
                    dims: vec![4],
                    ty: GgmlType::F32,
                    offset,
                    n_elements: 4,
                    n_bytes,
                }
            }

            #[test]
            fn range_adds_start_offset_and_size() {
                assert_eq!(info(64, 16).data_range(128).unwrap(), 192..208);
                assert_eq!(info(0, 16).data_range(0).unwrap(), 0..16);
            }

            #[test]
            fn range_overflow_is_an_error() {
                assert!(matches!(
                    info(u64::MAX, 1).data_range(1),
                    Err(GgufError::Overflow)
                ));
                assert!(matches!(
                    info(u64::MAX - 1, 16).data_range(0),
                    Err(GgufError::Overflow)
                ));
            }
        }
    }
    pub mod types {
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
                    Self::Q4_0 | Self::Q4_1 | Self::Q5_0 | Self::Q5_1 | Self::Q8_0 | Self::Q8_1 => {
                        32
                    }
                    Self::Q2_K | Self::Q3_K | Self::Q4_K | Self::Q5_K | Self::Q6_K | Self::Q8_K => {
                        256
                    }
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
                    TypeError::UnknownValueType(v) => {
                        write!(f, "unknown metadata value type id: {v}")
                    }
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
    }
    pub mod value {
        //! Metadata values and typed arrays.
        #![forbid(unsafe_code)]

        use crate::gguf::cursor::Cursor;
        use crate::gguf::error::GgufError;
        use crate::gguf::types::ValueType;

        /// Deepest allowed array nesting (an array of arrays of arrays of arrays is level 4).
        pub const MAX_ARRAY_DEPTH: u32 = 4;

        /// A metadata value.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Value {
            /// Unsigned 8-bit integer.
            U8(u8),
            /// Signed 8-bit integer.
            I8(i8),
            /// Unsigned 16-bit integer.
            U16(u16),
            /// Signed 16-bit integer.
            I16(i16),
            /// Unsigned 32-bit integer.
            U32(u32),
            /// Signed 32-bit integer.
            I32(i32),
            /// 32-bit float.
            F32(f32),
            /// Boolean.
            Bool(bool),
            /// UTF-8 string.
            String(String),
            /// Typed array.
            Array(Array),
            /// Unsigned 64-bit integer.
            U64(u64),
            /// Signed 64-bit integer.
            I64(i64),
            /// 64-bit float.
            F64(f64),
        }

        /// A homogeneous array, stored as a typed vector so large vocabularies stay compact.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Array {
            /// `u8` elements.
            U8(Vec<u8>),
            /// `i8` elements.
            I8(Vec<i8>),
            /// `u16` elements.
            U16(Vec<u16>),
            /// `i16` elements.
            I16(Vec<i16>),
            /// `u32` elements.
            U32(Vec<u32>),
            /// `i32` elements.
            I32(Vec<i32>),
            /// `f32` elements.
            F32(Vec<f32>),
            /// Boolean elements.
            Bool(Vec<bool>),
            /// String elements.
            String(Vec<String>),
            /// Nested arrays.
            Array(Vec<Array>),
            /// `u64` elements.
            U64(Vec<u64>),
            /// `i64` elements.
            I64(Vec<i64>),
            /// `f64` elements.
            F64(Vec<f64>),
        }

        impl Value {
            /// Name of this value's type, for error messages.
            pub fn type_name(&self) -> &'static str {
                match self {
                    Value::U8(_) => "U8",
                    Value::I8(_) => "I8",
                    Value::U16(_) => "U16",
                    Value::I16(_) => "I16",
                    Value::U32(_) => "U32",
                    Value::I32(_) => "I32",
                    Value::F32(_) => "F32",
                    Value::Bool(_) => "BOOL",
                    Value::String(_) => "STRING",
                    Value::Array(_) => "ARRAY",
                    Value::U64(_) => "U64",
                    Value::I64(_) => "I64",
                    Value::F64(_) => "F64",
                }
            }

            /// The wire type of this value.
            pub fn value_type(&self) -> ValueType {
                match self {
                    Value::U8(_) => ValueType::U8,
                    Value::I8(_) => ValueType::I8,
                    Value::U16(_) => ValueType::U16,
                    Value::I16(_) => ValueType::I16,
                    Value::U32(_) => ValueType::U32,
                    Value::I32(_) => ValueType::I32,
                    Value::F32(_) => ValueType::F32,
                    Value::Bool(_) => ValueType::BOOL,
                    Value::String(_) => ValueType::STRING,
                    Value::Array(_) => ValueType::ARRAY,
                    Value::U64(_) => ValueType::U64,
                    Value::I64(_) => ValueType::I64,
                    Value::F64(_) => ValueType::F64,
                }
            }

            /// Any integer value that fits a `u32` losslessly (negative values do not fit).
            pub fn as_u32(&self) -> Option<u32> {
                self.as_u64().and_then(|v| u32::try_from(v).ok())
            }

            /// Any integer value that fits a `u64` losslessly (negative values do not fit).
            pub fn as_u64(&self) -> Option<u64> {
                match *self {
                    Value::U8(v) => Some(u64::from(v)),
                    Value::U16(v) => Some(u64::from(v)),
                    Value::U32(v) => Some(u64::from(v)),
                    Value::U64(v) => Some(v),
                    Value::I8(v) => u64::try_from(v).ok(),
                    Value::I16(v) => u64::try_from(v).ok(),
                    Value::I32(v) => u64::try_from(v).ok(),
                    Value::I64(v) => u64::try_from(v).ok(),
                    _ => None,
                }
            }

            /// Any integer value that fits an `i64` losslessly.
            pub fn as_i64(&self) -> Option<i64> {
                match *self {
                    Value::I8(v) => Some(i64::from(v)),
                    Value::I16(v) => Some(i64::from(v)),
                    Value::I32(v) => Some(i64::from(v)),
                    Value::I64(v) => Some(v),
                    Value::U8(v) => Some(i64::from(v)),
                    Value::U16(v) => Some(i64::from(v)),
                    Value::U32(v) => Some(i64::from(v)),
                    Value::U64(v) => i64::try_from(v).ok(),
                    _ => None,
                }
            }

            /// `F32` as is; `F64` narrowed to `f32` (may lose precision). Other types give `None`.
            pub fn as_f32(&self) -> Option<f32> {
                match *self {
                    Value::F32(v) => Some(v),
                    Value::F64(v) => Some(v as f32),
                    _ => None,
                }
            }

            /// The boolean, if this is a `Bool`.
            pub fn as_bool(&self) -> Option<bool> {
                match *self {
                    Value::Bool(v) => Some(v),
                    _ => None,
                }
            }

            /// The string, if this is a `String`.
            pub fn as_str(&self) -> Option<&str> {
                match self {
                    Value::String(s) => Some(s),
                    _ => None,
                }
            }

            /// The array, if this is an `Array`.
            pub fn as_array(&self) -> Option<&Array> {
                match self {
                    Value::Array(a) => Some(a),
                    _ => None,
                }
            }
        }

        impl Array {
            /// Number of elements.
            pub fn len(&self) -> usize {
                match self {
                    Array::U8(v) => v.len(),
                    Array::I8(v) => v.len(),
                    Array::U16(v) => v.len(),
                    Array::I16(v) => v.len(),
                    Array::U32(v) => v.len(),
                    Array::I32(v) => v.len(),
                    Array::F32(v) => v.len(),
                    Array::Bool(v) => v.len(),
                    Array::String(v) => v.len(),
                    Array::Array(v) => v.len(),
                    Array::U64(v) => v.len(),
                    Array::I64(v) => v.len(),
                    Array::F64(v) => v.len(),
                }
            }

            /// `true` if there are no elements.
            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }

            /// The element wire type.
            pub fn element_type(&self) -> ValueType {
                match self {
                    Array::U8(_) => ValueType::U8,
                    Array::I8(_) => ValueType::I8,
                    Array::U16(_) => ValueType::U16,
                    Array::I16(_) => ValueType::I16,
                    Array::U32(_) => ValueType::U32,
                    Array::I32(_) => ValueType::I32,
                    Array::F32(_) => ValueType::F32,
                    Array::Bool(_) => ValueType::BOOL,
                    Array::String(_) => ValueType::STRING,
                    Array::Array(_) => ValueType::ARRAY,
                    Array::U64(_) => ValueType::U64,
                    Array::I64(_) => ValueType::I64,
                    Array::F64(_) => ValueType::F64,
                }
            }

            /// The elements, if this is a string array.
            pub fn as_strings(&self) -> Option<&[String]> {
                match self {
                    Array::String(v) => Some(v),
                    _ => None,
                }
            }

            /// The elements, if this is an `f32` array.
            pub fn as_f32s(&self) -> Option<&[f32]> {
                match self {
                    Array::F32(v) => Some(v),
                    _ => None,
                }
            }

            /// The elements, if this is an `i32` array.
            pub fn as_i32s(&self) -> Option<&[i32]> {
                match self {
                    Array::I32(v) => Some(v),
                    _ => None,
                }
            }

            /// The elements, if this is a `u32` array.
            pub fn as_u32s(&self) -> Option<&[u32]> {
                match self {
                    Array::U32(v) => Some(v),
                    _ => None,
                }
            }
        }

        /// Smallest number of bytes one element of `ty` can occupy on disk.
        fn min_element_bytes(ty: ValueType) -> u64 {
            match ty {
                ValueType::STRING => 8,    // u64 length
                ValueType::ARRAY => 4 + 8, // element type + count
                other => other.fixed_size().unwrap_or(1),
            }
        }

        /// Parse one value of type `ty`. `depth` is the array nesting level already entered.
        pub(crate) fn parse_value(
            c: &mut Cursor<'_>,
            ty: ValueType,
            depth: u32,
        ) -> Result<Value, GgufError> {
            Ok(match ty {
                ValueType::U8 => Value::U8(c.read_u8()?),
                ValueType::I8 => Value::I8(c.read_i8()?),
                ValueType::U16 => Value::U16(c.read_u16()?),
                ValueType::I16 => Value::I16(c.read_i16()?),
                ValueType::U32 => Value::U32(c.read_u32()?),
                ValueType::I32 => Value::I32(c.read_i32()?),
                ValueType::F32 => Value::F32(c.read_f32()?),
                ValueType::BOOL => Value::Bool(parse_bool(c)?),
                ValueType::STRING => Value::String(c.read_string()?),
                ValueType::ARRAY => Value::Array(parse_array(c, depth + 1)?),
                ValueType::U64 => Value::U64(c.read_u64()?),
                ValueType::I64 => Value::I64(c.read_i64()?),
                ValueType::F64 => Value::F64(c.read_f64()?),
            })
        }

        fn parse_bool(c: &mut Cursor<'_>) -> Result<bool, GgufError> {
            match c.read_u8()? {
                0 => Ok(false),
                1 => Ok(true),
                b => Err(GgufError::BadBool(b)),
            }
        }

        /// Parse an array body (element type, count, elements) at nesting `level` (1 = outermost).
        fn parse_array(c: &mut Cursor<'_>, level: u32) -> Result<Array, GgufError> {
            if level > MAX_ARRAY_DEPTH {
                return Err(GgufError::ArrayTooDeep);
            }
            let elem = ValueType::from_u32(c.read_u32()?)?;
            let count = c.read_u64()?;
            // Cap the count by the bytes left before allocating anything.
            let min = min_element_bytes(elem);
            let needed = count.checked_mul(min).ok_or(GgufError::CountTooLarge {
                what: "array",
                count,
            })?;
            if needed > c.remaining() as u64 {
                return Err(GgufError::CountTooLarge {
                    what: "array",
                    count,
                });
            }
            let n = usize::try_from(count).map_err(|_| GgufError::CountTooLarge {
                what: "array",
                count,
            })?;

            macro_rules! fixed {
                ($variant:ident, $read:ident) => {{
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(c.$read()?);
                    }
                    Array::$variant(v)
                }};
            }
            Ok(match elem {
                ValueType::U8 => fixed!(U8, read_u8),
                ValueType::I8 => fixed!(I8, read_i8),
                ValueType::U16 => fixed!(U16, read_u16),
                ValueType::I16 => fixed!(I16, read_i16),
                ValueType::U32 => fixed!(U32, read_u32),
                ValueType::I32 => fixed!(I32, read_i32),
                ValueType::F32 => fixed!(F32, read_f32),
                ValueType::U64 => fixed!(U64, read_u64),
                ValueType::I64 => fixed!(I64, read_i64),
                ValueType::F64 => fixed!(F64, read_f64),
                ValueType::BOOL => {
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(parse_bool(c)?);
                    }
                    Array::Bool(v)
                }
                ValueType::STRING => {
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(c.read_string()?);
                    }
                    Array::String(v)
                }
                ValueType::ARRAY => {
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(parse_array(c, level + 1)?);
                    }
                    Array::Array(v)
                }
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn arr_bytes(elem: u32, count: u64, body: &[u8]) -> Vec<u8> {
                let mut v = elem.to_le_bytes().to_vec();
                v.extend_from_slice(&count.to_le_bytes());
                v.extend_from_slice(body);
                v
            }

            #[test]
            fn integer_accessors_are_lossless_only() {
                assert_eq!(Value::U8(7).as_u32(), Some(7));
                assert_eq!(Value::I32(5).as_u32(), Some(5));
                assert_eq!(Value::I32(-5).as_u32(), None);
                assert_eq!(Value::U64(u64::from(u32::MAX) + 1).as_u32(), None);
                assert_eq!(Value::U64(u64::MAX).as_i64(), None);
                assert_eq!(Value::I8(-3).as_i64(), Some(-3));
                assert_eq!(Value::F32(1.0).as_u32(), None);
                assert_eq!(Value::String("x".into()).as_u64(), None);
            }

            #[test]
            fn float_bool_str_array_accessors() {
                assert_eq!(Value::F32(1.5).as_f32(), Some(1.5));
                assert_eq!(Value::F64(2.5).as_f32(), Some(2.5));
                assert_eq!(Value::U32(1).as_f32(), None);
                assert_eq!(Value::Bool(true).as_bool(), Some(true));
                assert_eq!(Value::U8(1).as_bool(), None);
                assert_eq!(Value::String("a".into()).as_str(), Some("a"));
                assert!(Value::Array(Array::U32(vec![1])).as_array().is_some());
                assert!(Value::U32(1).as_array().is_none());
            }

            #[test]
            fn array_helpers() {
                let a = Array::String(vec!["a".into(), "b".into()]);
                assert_eq!(a.len(), 2);
                assert!(!a.is_empty());
                assert_eq!(a.as_strings().unwrap().len(), 2);
                assert!(a.as_f32s().is_none());
                assert_eq!(Array::F32(vec![1.0]).as_f32s().unwrap(), &[1.0]);
                assert_eq!(Array::I32(vec![-1]).as_i32s().unwrap(), &[-1]);
                assert_eq!(Array::U32(vec![3]).as_u32s().unwrap(), &[3]);
                assert!(Array::U8(vec![]).is_empty());
                assert_eq!(Array::Array(vec![]).element_type(), ValueType::ARRAY);
            }

            #[test]
            fn parses_scalar_and_typed_arrays() {
                let mut c = Cursor::new(&[0xfe, 0xff]);
                assert_eq!(
                    parse_value(&mut c, ValueType::I16, 0).unwrap(),
                    Value::I16(-2)
                );

                let bytes = arr_bytes(4, 3, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
                let mut c = Cursor::new(&bytes);
                let v = parse_value(&mut c, ValueType::ARRAY, 0).unwrap();
                assert_eq!(v, Value::Array(Array::U32(vec![1, 2, 3])));
            }

            #[test]
            fn bad_bool_scalar_and_array() {
                assert!(matches!(
                    parse_value(&mut Cursor::new(&[2]), ValueType::BOOL, 0),
                    Err(GgufError::BadBool(2))
                ));
                let bytes = arr_bytes(7, 2, &[1, 5]);
                assert!(matches!(
                    parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
                    Err(GgufError::BadBool(5))
                ));
            }

            #[test]
            fn count_larger_than_remaining_is_rejected_before_allocating() {
                let bytes = arr_bytes(4, u64::MAX, &[0; 8]);
                assert!(matches!(
                    parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
                    Err(GgufError::CountTooLarge { what: "array", .. })
                ));
                let bytes = arr_bytes(8, 1_000_000, &[0; 16]);
                assert!(matches!(
                    parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
                    Err(GgufError::CountTooLarge { .. })
                ));
            }

            #[test]
            fn unknown_element_type_is_an_error() {
                let bytes = arr_bytes(99, 0, &[]);
                assert!(matches!(
                    parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
                    Err(GgufError::UnknownValueType(99))
                ));
            }

            fn nested(levels: u32) -> Vec<u8> {
                // `levels` arrays deep; the innermost holds one u8.
                let mut inner = arr_bytes(0, 1, &[9]);
                for _ in 1..levels {
                    let mut outer = 9u32.to_le_bytes().to_vec();
                    outer.extend_from_slice(&1u64.to_le_bytes());
                    outer.extend_from_slice(&inner);
                    inner = outer;
                }
                inner
            }

            #[test]
            fn nesting_limit_is_four_levels() {
                for ok in 1..=4 {
                    let b = nested(ok);
                    assert!(
                        parse_value(&mut Cursor::new(&b), ValueType::ARRAY, 0).is_ok(),
                        "depth {ok}"
                    );
                }
                let b = nested(5);
                assert!(matches!(
                    parse_value(&mut Cursor::new(&b), ValueType::ARRAY, 0),
                    Err(GgufError::ArrayTooDeep)
                ));
            }

            #[test]
            fn strings_in_arrays() {
                let mut body = Vec::new();
                for s in ["ab", "", "cde"] {
                    body.extend_from_slice(&(s.len() as u64).to_le_bytes());
                    body.extend_from_slice(s.as_bytes());
                }
                let bytes = arr_bytes(8, 3, &body);
                let v = parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0).unwrap();
                assert_eq!(
                    v,
                    Value::Array(Array::String(vec!["ab".into(), "".into(), "cde".into()]))
                );
            }
        }
    }
    pub mod writer {
        //! Build valid GGUF v3 files in memory.
        #![forbid(unsafe_code)]

        use crate::gguf::cursor::align_up;
        use crate::gguf::error::GgufError;
        use crate::gguf::types::GgmlType;
        use crate::gguf::value::{Array, Value};
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
        /// Used by tests and by MOULI's tiny test-model generator.
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
            use crate::gguf::reader::parse;

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
    }

    pub use error::GgufError;
    pub use mmap::MappedGguf;
    pub use reader::{parse, parse_header_only, Gguf, ModelSummary};
    pub use tensor::TensorInfo;
    pub use types::{GgmlType, ValueType};
    pub use value::{Array, Value};
    pub use writer::GgufWriter;
}
pub mod kernels {
    //! MOULI inference engine.
    //!
    //! This crate replaces llama.cpp inside MOULI. It starts with scalar CPU
    //! reference kernels ([`ops`]); SIMD and Vulkan backends must match these
    //! outputs within tolerance.
    #![forbid(unsafe_code)]

    pub mod ops {
        //! Scalar reference kernels (f32).
        //!
        //! Ported from the retired C `aeon_transformer_compute`. These favour
        //! clarity over speed and are the oracle for optimised backends.

        use crate::kernels::OpError;

        /// In-place, numerically stable softmax.
        pub fn softmax(x: &mut [f32]) -> Result<(), OpError> {
            if x.is_empty() {
                return Err(OpError::Param);
            }
            let max = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mut sum = 0.0f32;
            for v in x.iter_mut() {
                *v = (*v - max).exp();
                sum += *v;
            }
            if !positive(sum) || !sum.is_finite() {
                return Err(OpError::NonFinite);
            }
            for v in x.iter_mut() {
                *v /= sum;
            }
            Ok(())
        }

        /// RMS normalisation: `out = x / sqrt(mean(x^2) + eps) * weight`.
        ///
        /// `weight` may be empty to skip the learned scale.
        pub fn rmsnorm(
            x: &[f32],
            weight: &[f32],
            eps: f32,
            out: &mut [f32],
        ) -> Result<(), OpError> {
            if x.is_empty() || eps.is_nan() || eps < 0.0 {
                return Err(OpError::Param);
            }
            if out.len() != x.len() || !(weight.is_empty() || weight.len() == x.len()) {
                return Err(OpError::Shape);
            }
            let ss: f32 = x.iter().map(|v| v * v).sum();
            let inv = 1.0 / (ss / x.len() as f32 + eps).sqrt();
            if !inv.is_finite() {
                return Err(OpError::NonFinite);
            }
            for (i, (o, v)) in out.iter_mut().zip(x).enumerate() {
                let w = weight.get(i).copied().unwrap_or(1.0);
                *o = v * inv * w;
            }
            Ok(())
        }

        /// Rotary position embedding on interleaved pairs, applied to `q` and `k`
        /// (each `heads * head_dim`), with frequency base `theta` (10000 for Llama).
        pub fn rope(
            q: &mut [f32],
            k: &mut [f32],
            heads: usize,
            head_dim: usize,
            position: usize,
            theta: f32,
        ) -> Result<(), OpError> {
            if heads == 0 || head_dim == 0 || !head_dim.is_multiple_of(2) || !positive(theta) {
                return Err(OpError::Param);
            }
            let n = heads.checked_mul(head_dim).ok_or(OpError::Param)?;
            if q.len() != n || k.len() != n {
                return Err(OpError::Shape);
            }
            for j in (0..head_dim).step_by(2) {
                let angle = position as f32 * theta.powf(-(j as f32) / head_dim as f32);
                let (s, c) = angle.sin_cos();
                for h in 0..heads {
                    let b = h * head_dim + j;
                    for t in [&mut *q, &mut *k] {
                        let (a0, a1) = (t[b], t[b + 1]);
                        t[b] = a0 * c - a1 * s;
                        t[b + 1] = a0 * s + a1 * c;
                    }
                }
            }
            Ok(())
        }

        /// Single-query attention over `tokens` cached keys/values of width `dim`.
        ///
        /// `scratch` must hold at least `tokens` floats; no allocation happens.
        pub fn attention(
            q: &[f32],
            k: &[f32],
            v: &[f32],
            tokens: usize,
            scale: f32,
            scratch: &mut [f32],
            out: &mut [f32],
        ) -> Result<(), OpError> {
            let dim = q.len();
            if dim == 0 || tokens == 0 || !positive(scale) {
                return Err(OpError::Param);
            }
            let kv = tokens.checked_mul(dim).ok_or(OpError::Param)?;
            if k.len() != kv || v.len() != kv || out.len() != dim || scratch.len() < tokens {
                return Err(OpError::Shape);
            }
            let scores = &mut scratch[..tokens];
            for (s, krow) in scores.iter_mut().zip(k.chunks_exact(dim)) {
                *s = dot(q, krow) * scale;
            }
            softmax(scores)?;
            out.fill(0.0);
            for (&p, vrow) in scores.iter().zip(v.chunks_exact(dim)) {
                for (o, &x) in out.iter_mut().zip(vrow) {
                    *o += p * x;
                }
            }
            Ok(())
        }

        /// `out = silu(gate) * up`.
        pub fn swiglu(gate: &[f32], up: &[f32], out: &mut [f32]) -> Result<(), OpError> {
            if gate.is_empty() {
                return Err(OpError::Param);
            }
            if up.len() != gate.len() || out.len() != gate.len() {
                return Err(OpError::Shape);
            }
            for ((o, &g), &u) in out.iter_mut().zip(gate).zip(up) {
                *o = silu(g) * u;
            }
            Ok(())
        }

        /// Row-major matrix-vector product: `out[r] = sum_c w[r][c] * x[c]`.
        pub fn matvec(w: &[f32], x: &[f32], out: &mut [f32]) -> Result<(), OpError> {
            let cols = x.len();
            if cols == 0 || out.is_empty() {
                return Err(OpError::Param);
            }
            if w.len() != cols.checked_mul(out.len()).ok_or(OpError::Param)? {
                return Err(OpError::Shape);
            }
            for (o, row) in out.iter_mut().zip(w.chunks_exact(cols)) {
                *o = dot(row, x);
            }
            Ok(())
        }

        /// True for finite or infinite values strictly above zero (false for NaN).
        fn positive(x: f32) -> bool {
            x > 0.0
        }

        /// SiLU activation.
        #[must_use]
        pub fn silu(x: f32) -> f32 {
            x / (1.0 + (-x).exp())
        }

        /// Dot product of equal-length slices (extra elements are ignored).
        #[must_use]
        pub fn dot(a: &[f32], b: &[f32]) -> f32 {
            a.iter().zip(b).map(|(x, y)| x * y).sum()
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn close(a: f32, b: f32) -> bool {
                (a - b).abs() < 1e-5
            }

            #[test]
            fn softmax_sums_to_one() {
                let mut x = [1.0, 2.0, 3.0];
                softmax(&mut x).unwrap();
                assert!(close(x.iter().sum(), 1.0));
                assert!(x[2] > x[1] && x[1] > x[0]);
                assert_eq!(softmax(&mut []), Err(OpError::Param));
            }

            #[test]
            fn rmsnorm_unit() {
                let mut out = [0.0; 2];
                rmsnorm(&[3.0, 4.0], &[], 0.0, &mut out).unwrap();
                let r = (12.5f32).sqrt();
                assert!(close(out[0], 3.0 / r) && close(out[1], 4.0 / r));
                rmsnorm(&[3.0, 4.0], &[2.0, 0.0], 0.0, &mut out).unwrap();
                assert!(close(out[0], 6.0 / r) && close(out[1], 0.0));
                assert_eq!(rmsnorm(&[1.0], &[], -1.0, &mut [0.0]), Err(OpError::Param));
            }

            #[test]
            fn rope_position_zero_is_identity_and_preserves_norm() {
                let mut q = [1.0, 2.0, 3.0, 4.0];
                let mut k = q;
                rope(&mut q, &mut k, 1, 4, 0, 10000.0).unwrap();
                assert_eq!(q, [1.0, 2.0, 3.0, 4.0]);
                rope(&mut q, &mut k, 1, 4, 7, 10000.0).unwrap();
                assert!(close(q[0] * q[0] + q[1] * q[1], 5.0));
                assert_eq!(q, k);
                assert_eq!(rope(&mut q, &mut k, 1, 3, 0, 10000.0), Err(OpError::Param));
            }

            #[test]
            fn attention_picks_matching_key() {
                let q = [10.0, 0.0];
                let k = [1.0, 0.0, 0.0, 1.0];
                let v = [5.0, 5.0, -5.0, -5.0];
                let mut s = [0.0; 2];
                let mut out = [0.0; 2];
                attention(&q, &k, &v, 2, 1.0, &mut s, &mut out).unwrap();
                assert!(out[0] > 4.99);
                assert_eq!(
                    attention(&q, &k, &v, 2, 1.0, &mut [0.0; 1], &mut out),
                    Err(OpError::Shape)
                );
            }

            #[test]
            fn swiglu_and_matvec() {
                let mut out = [0.0; 1];
                swiglu(&[0.0], &[3.0], &mut out).unwrap();
                assert!(close(out[0], 0.0));
                let mut y = [0.0; 2];
                matvec(&[1.0, 2.0, 3.0, 4.0], &[1.0, 1.0], &mut y).unwrap();
                assert_eq!(y, [3.0, 7.0]);
                assert_eq!(matvec(&[1.0], &[1.0, 1.0], &mut y), Err(OpError::Shape));
            }
        }
    }
    pub mod quant {
        //! Simple 4-bit packed weights with one scale per matrix.
        //!
        //! Two weights per byte, low nibble first, stored offset by 8 (so nibble
        //! `n` means `n - 8`). This is the minimal format the retired C core used;
        //! GGUF block formats (Q4_0 with per-block f16 scales, K-quants) live in
        //! `gguf` and get their own kernels.

        use crate::kernels::OpError;

        /// Unpack `count` signed 4-bit values.
        pub fn unpack_nibbles(packed: &[u8], count: usize) -> Result<Vec<i8>, OpError> {
            if packed.len() < count.div_ceil(2) {
                return Err(OpError::Shape);
            }
            Ok((0..count).map(|i| nibble(packed, i)).collect())
        }

        fn nibble(packed: &[u8], i: usize) -> i8 {
            let b = packed[i / 2];
            let n = if i % 2 == 1 { b >> 4 } else { b & 0x0f };
            // n is 0..=15, so the result is in -8..=7.
            n as i8 - 8
        }

        /// `out[r] = scale * sum_c q[r][c] * x[c]` over packed row-major weights.
        pub fn gemv_nibbles(
            packed: &[u8],
            x: &[f32],
            scale: f32,
            out: &mut [f32],
        ) -> Result<(), OpError> {
            let cols = x.len();
            if cols == 0 || out.is_empty() {
                return Err(OpError::Param);
            }
            let n = cols.checked_mul(out.len()).ok_or(OpError::Param)?;
            if packed.len() < n.div_ceil(2) {
                return Err(OpError::Shape);
            }
            for (r, o) in out.iter_mut().enumerate() {
                let base = r * cols;
                let acc: f32 = x
                    .iter()
                    .enumerate()
                    .map(|(c, &xv)| f32::from(nibble(packed, base + c)) * xv)
                    .sum();
                *o = acc * scale;
            }
            Ok(())
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn unpack_and_gemv() {
                // values: [-8, 7, 0, 1] -> nibbles [0, 15, 8, 9]
                let packed = [0xf0u8, 0x98];
                assert_eq!(unpack_nibbles(&packed, 4).unwrap(), vec![-8, 7, 0, 1]);
                let mut out = [0.0f32; 2];
                gemv_nibbles(&packed, &[1.0, 1.0], 0.5, &mut out).unwrap();
                assert_eq!(out, [-0.5, 0.5]);
                assert!(unpack_nibbles(&packed, 5).is_err());
            }
        }
    }

    use core::fmt;

    /// Kernel errors.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum OpError {
        /// Slice lengths do not match the given dimensions.
        Shape,
        /// A parameter is out of range (zero size, odd head dim, bad epsilon).
        Param,
        /// The result is not finite.
        NonFinite,
    }

    impl fmt::Display for OpError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Self::Shape => "slice length does not match dimensions",
                Self::Param => "parameter out of range",
                Self::NonFinite => "non-finite result",
            })
        }
    }

    impl std::error::Error for OpError {}
}
pub mod quant {
    //! Dequantisation of GGUF tensor data to `f32`.
    //!
    //! Written from the published GGUF block layouts, not from any other implementation.
    //! Supported element types: F32, F16, BF16, Q4_0, Q4_1, Q5_0, Q5_1, Q8_0, Q4_K, Q5_K, Q6_K.
    //! Other types return [`QuantError::Unsupported`].
    //!
    //! **Verification status:** the tests build blocks by hand from the layout description and
    //! compare with values computed directly from the formulas. They have *not* been compared
    //! with output from a reference engine on real model files; engine milestone E1 requires that
    //! comparison against the oracle before these routines are trusted for inference.
    //!
    //! All functions are safe, bounds-checked and panic-free on arbitrary input.

    mod float {
        //! Float element types.

        use crate::quant::sz;

        /// IEEE 754 binary16 to `f32` (exact for every input, including subnormals, infinities, NaN).
        pub fn f16_to_f32(h: u16) -> f32 {
            let sign = u32::from(h >> 15) << 31;
            let exp = u32::from((h >> 10) & 0x1f);
            let frac = u32::from(h & 0x3ff);
            let bits = match (exp, frac) {
                (0, 0) => sign,
                (0, f) => {
                    // subnormal: normalise
                    let shift = f.leading_zeros() - 21; // bring the top set bit to bit 10
                    let frac = (f << shift) & 0x3ff;
                    let exp = 127 - 15 + 1 - shift;
                    sign | (exp << 23) | (frac << 13)
                }
                (0x1f, 0) => sign | 0x7f80_0000,
                (0x1f, f) => sign | 0x7f80_0000 | (f << 13),
                (e, f) => sign | ((e + 127 - 15) << 23) | (f << 13),
            };
            f32::from_bits(bits)
        }

        /// bfloat16 to `f32` (the top 16 bits of an `f32`).
        pub fn bf16_to_f32(b: u16) -> f32 {
            f32::from_bits(u32::from(b) << 16)
        }

        pub(crate) fn f32s(bytes: &[u8], out: &mut [f32]) {
            for (o, c) in out.iter_mut().zip(bytes.chunks_exact(sz(4))) {
                *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
        }

        pub(crate) fn f16s(bytes: &[u8], out: &mut [f32]) {
            for (o, c) in out.iter_mut().zip(bytes.chunks_exact(sz(2))) {
                *o = f16_to_f32(u16::from_le_bytes([c[0], c[1]]));
            }
        }

        pub(crate) fn bf16s(bytes: &[u8], out: &mut [f32]) {
            for (o, c) in out.iter_mut().zip(bytes.chunks_exact(sz(2))) {
                *o = bf16_to_f32(u16::from_le_bytes([c[0], c[1]]));
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn f16_known_values() {
                assert_eq!(f16_to_f32(0x0000), 0.0);
                assert_eq!(f16_to_f32(0x8000).to_bits(), (-0.0f32).to_bits());
                assert_eq!(f16_to_f32(0x3c00), 1.0);
                assert_eq!(f16_to_f32(0xbc00), -1.0);
                assert_eq!(f16_to_f32(0x3800), 0.5);
                assert_eq!(f16_to_f32(0x4000), 2.0);
                assert_eq!(f16_to_f32(0x7bff), 65504.0);
                assert_eq!(f16_to_f32(0x0400), 2.0f32.powi(-14)); // smallest normal
                assert_eq!(f16_to_f32(0x0001), 2.0f32.powi(-24)); // smallest subnormal
                assert_eq!(f16_to_f32(0x03ff), 1023.0 * 2.0f32.powi(-24));
                assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
                assert_eq!(f16_to_f32(0xfc00), f32::NEG_INFINITY);
                assert!(f16_to_f32(0x7e00).is_nan());
                assert_eq!(f16_to_f32(0x3555), 0.333_251_95); // nearest to 1/3
            }

            #[test]
            fn f16_every_subnormal_is_exact() {
                for f in 1u16..0x400 {
                    assert_eq!(f16_to_f32(f), f32::from(f) * 2.0f32.powi(-24), "frac {f}");
                }
            }

            #[test]
            fn f16_is_monotonic_over_positive_finite_values() {
                let mut last = -1.0f32;
                for h in 0u16..0x7c00 {
                    let v = f16_to_f32(h);
                    assert!(v > last || (h == 0 && v == 0.0), "{h:#x}");
                    last = v;
                }
            }

            #[test]
            fn bf16_known_values() {
                assert_eq!(bf16_to_f32(0x3f80), 1.0);
                assert_eq!(bf16_to_f32(0xc000), -2.0);
                assert_eq!(bf16_to_f32(0x0000), 0.0);
                assert_eq!(bf16_to_f32(0x7f80), f32::INFINITY);
                assert!(bf16_to_f32(0x7fc0).is_nan());
            }

            #[test]
            fn slices_decode_in_order() {
                let mut out = [0.0f32; 2];
                f32s(&[0, 0, 0x80, 0x3f, 0, 0, 0, 0x40], &mut out);
                assert_eq!(out, [1.0, 2.0]);
                f16s(&[0x00, 0x3c, 0x00, 0x40], &mut out);
                assert_eq!(out, [1.0, 2.0]);
                bf16s(&[0x80, 0x3f, 0x00, 0x40], &mut out);
                assert_eq!(out, [1.0, 2.0]);
            }
        }
    }
    mod kquant {
        //! 256-element super-block formats: Q4_K, Q5_K, Q6_K.
        //!
        //! Layouts (little-endian):
        //!
        //! * `Q4_K` (144 bytes): `d: f16`, `dmin: f16`, `scales: [u8; 12]`, `qs: [u8; 128]`.
        //!   Eight sub-blocks of 32 elements, each with a 6-bit scale `sc` and 6-bit minimum `m`
        //!   (packed into `scales`, see [`scale_min`]); value = `d * sc * q - dmin * m`, `q` a 4-bit
        //!   value. Sub-blocks `2c` and `2c + 1` share the 32 bytes `qs[32c..32c + 32]`: the first takes
        //!   the low nibbles, the second the high nibbles.
        //! * `Q5_K` (176 bytes): as Q4_K plus `qh: [u8; 32]` between `scales` and `qs`; bit `2c` of
        //!   `qh[l]` is the fifth bit of the low-nibble element, bit `2c + 1` of the high-nibble one.
        //! * `Q6_K` (210 bytes): `ql: [u8; 128]`, `qh: [u8; 64]`, `scales: [i8; 16]`, `d: f16`.
        //!   Six-bit values with 16 sub-blocks of 16 elements; value = `d * scale * (q6 - 32)`.

        use crate::quant::float::f16_to_f32;
        use crate::quant::sz;

        /// Unpack the 6-bit scale and minimum of sub-block `j` (0..8) from the 12 packed bytes.
        ///
        /// Sub-blocks 0..4 keep their scale in `scales[j]` and minimum in `scales[j + 4]` (low 6 bits).
        /// Sub-blocks 4..8 split each value: the low 4 bits live in `scales[j + 4]` (scale in the low
        /// nibble, minimum in the high nibble) and the top 2 bits in the high bits of `scales[j - 4]`
        /// (scale) and `scales[j]` (minimum).
        pub(crate) fn scale_min(j: usize, s: &[u8]) -> (u8, u8) {
            if j < 4 {
                (s[j] & 63, s[j + 4] & 63)
            } else {
                (
                    (s[j + 4] & 0xf) | ((s[j - 4] >> 6) << 4),
                    (s[j + 4] >> 4) | ((s[j] >> 6) << 4),
                )
            }
        }

        fn h(bytes: &[u8], at: usize) -> f32 {
            f16_to_f32(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
        }

        pub(crate) fn q4_k(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes
                .chunks_exact(sz(144))
                .zip(out.chunks_exact_mut(sz(256)))
            {
                let (d, dmin) = (h(b, 0), h(b, 2));
                let scales = &b[4..16];
                let qs = &b[16..144];
                for c in 0..4 {
                    let (sc1, m1) = scale_min(2 * c, scales);
                    let (sc2, m2) = scale_min(2 * c + 1, scales);
                    let (d1, mm1) = (d * f32::from(sc1), dmin * f32::from(m1));
                    let (d2, mm2) = (d * f32::from(sc2), dmin * f32::from(m2));
                    for l in 0..32 {
                        let q = qs[c * 32 + l];
                        y[c * 64 + l] = d1 * f32::from(q & 0xf) - mm1;
                        y[c * 64 + 32 + l] = d2 * f32::from(q >> 4) - mm2;
                    }
                }
            }
        }

        pub(crate) fn q5_k(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes
                .chunks_exact(sz(176))
                .zip(out.chunks_exact_mut(sz(256)))
            {
                let (d, dmin) = (h(b, 0), h(b, 2));
                let scales = &b[4..16];
                let qh = &b[16..48];
                let ql = &b[48..176];
                for c in 0..4 {
                    let (sc1, m1) = scale_min(2 * c, scales);
                    let (sc2, m2) = scale_min(2 * c + 1, scales);
                    let (d1, mm1) = (d * f32::from(sc1), dmin * f32::from(m1));
                    let (d2, mm2) = (d * f32::from(sc2), dmin * f32::from(m2));
                    let (u1, u2) = (1u8 << (2 * c), 2u8 << (2 * c));
                    for l in 0..32 {
                        let q = ql[c * 32 + l];
                        let hi1 = if qh[l] & u1 != 0 { 16.0 } else { 0.0 };
                        let hi2 = if qh[l] & u2 != 0 { 16.0 } else { 0.0 };
                        y[c * 64 + l] = d1 * (f32::from(q & 0xf) + hi1) - mm1;
                        y[c * 64 + 32 + l] = d2 * (f32::from(q >> 4) + hi2) - mm2;
                    }
                }
            }
        }

        pub(crate) fn q6_k(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes
                .chunks_exact(sz(210))
                .zip(out.chunks_exact_mut(sz(256)))
            {
                let d = h(b, 208);
                for half in 0..2 {
                    let ql = &b[half * 64..half * 64 + 64];
                    let qh = &b[128 + half * 32..128 + half * 32 + 32];
                    let sc = &b[192 + half * 8..192 + half * 8 + 8];
                    let y = &mut y[half * 128..half * 128 + 128];
                    for l in 0..32 {
                        let is = l / 16;
                        let q1 = i32::from((ql[l] & 0xf) | ((qh[l] & 3) << 4)) - 32;
                        let q2 = i32::from((ql[l + 32] & 0xf) | (((qh[l] >> 2) & 3) << 4)) - 32;
                        let q3 = i32::from((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4)) - 32;
                        let q4 = i32::from((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4)) - 32;
                        let s = |k: usize| f32::from(sc[is + k] as i8);
                        y[l] = d * s(0) * q1 as f32;
                        y[l + 32] = d * s(2) * q2 as f32;
                        y[l + 64] = d * s(4) * q3 as f32;
                        y[l + 96] = d * s(6) * q4 as f32;
                    }
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            const ONE: u16 = 0x3c00;
            const HALF: u16 = 0x3800;

            /// Pack eight 6-bit scales and minimums into the 12-byte K-quant layout.
            fn pack_scales(sc: [u8; 8], m: [u8; 8]) -> [u8; 12] {
                let mut b = [0u8; 12];
                for j in 0..4 {
                    b[j] = sc[j] | ((sc[j + 4] >> 4) << 6);
                    b[j + 4] = m[j] | ((m[j + 4] >> 4) << 6);
                    b[8 + j] = (sc[j + 4] & 0xf) | ((m[j + 4] & 0xf) << 4);
                }
                b
            }

            #[test]
            fn scale_min_round_trips_all_six_bit_values() {
                for seed in 0..64u8 {
                    let sc: [u8; 8] = std::array::from_fn(|i| {
                        (seed.wrapping_mul(5).wrapping_add(i as u8 * 9)) & 63
                    });
                    let m: [u8; 8] = std::array::from_fn(|i| {
                        (seed.wrapping_mul(11).wrapping_add(i as u8 * 7)) & 63
                    });
                    let packed = pack_scales(sc, m);
                    for j in 0..8 {
                        assert_eq!(scale_min(j, &packed), (sc[j], m[j]), "seed {seed} j {j}");
                    }
                }
                // extremes
                let packed = pack_scales([63; 8], [0; 8]);
                for j in 0..8 {
                    assert_eq!(scale_min(j, &packed), (63, 0));
                }
                let packed = pack_scales([0; 8], [63; 8]);
                for j in 0..8 {
                    assert_eq!(scale_min(j, &packed), (0, 63));
                }
            }

            /// Logical Q4_K content: per sub-block scale and min, and 256 4-bit values.
            fn build_q4_k(d: u16, dmin: u16, sc: [u8; 8], m: [u8; 8], q: &[u8; 256]) -> Vec<u8> {
                let mut b = d.to_le_bytes().to_vec();
                b.extend_from_slice(&dmin.to_le_bytes());
                b.extend_from_slice(&pack_scales(sc, m));
                for c in 0..4 {
                    for l in 0..32 {
                        b.push(q[c * 64 + l] | (q[c * 64 + 32 + l] << 4));
                    }
                }
                b
            }

            #[test]
            fn q4_k_matches_the_formula() {
                let sc = [1, 2, 3, 4, 33, 40, 50, 63];
                let m = [0, 1, 2, 3, 34, 41, 51, 62];
                let q: [u8; 256] = std::array::from_fn(|i| ((i * 7 + i / 32) % 16) as u8);
                let b = build_q4_k(ONE, HALF, sc, m, &q);
                assert_eq!(b.len(), 144);
                let mut y = [0.0f32; 256];
                q4_k(&b, &mut y);
                for i in 0..256 {
                    let sub = i / 32;
                    let want = 1.0 * f32::from(sc[sub]) * f32::from(q[i]) - 0.5 * f32::from(m[sub]);
                    assert_eq!(y[i], want, "element {i}");
                }
            }

            #[test]
            fn q4_k_two_blocks() {
                let q: [u8; 256] = [3; 256];
                let mut b = build_q4_k(ONE, 0, [2; 8], [0; 8], &q);
                b.extend(build_q4_k(HALF, 0, [4; 8], [0; 8], &q));
                let mut y = [0.0f32; 512];
                q4_k(&b, &mut y);
                assert!(y[..256].iter().all(|v| *v == 6.0));
                assert!(y[256..].iter().all(|v| *v == 6.0)); // 0.5 * 4 * 3
            }

            fn build_q5_k(d: u16, dmin: u16, sc: [u8; 8], m: [u8; 8], q: &[u8; 256]) -> Vec<u8> {
                // q holds 5-bit values
                let mut b = d.to_le_bytes().to_vec();
                b.extend_from_slice(&dmin.to_le_bytes());
                b.extend_from_slice(&pack_scales(sc, m));
                let mut qh = [0u8; 32];
                let mut ql = Vec::with_capacity(128);
                for c in 0..4 {
                    for l in 0..32 {
                        let (a, bb) = (q[c * 64 + l], q[c * 64 + 32 + l]);
                        ql.push((a & 0xf) | ((bb & 0xf) << 4));
                        qh[l] |= ((a >> 4) & 1) << (2 * c);
                        qh[l] |= ((bb >> 4) & 1) << (2 * c + 1);
                    }
                }
                b.extend_from_slice(&qh);
                b.extend_from_slice(&ql);
                b
            }

            #[test]
            fn q5_k_matches_the_formula() {
                let sc = [5, 6, 7, 8, 9, 10, 11, 12];
                let m = [1, 1, 2, 2, 3, 3, 4, 4];
                let q: [u8; 256] = std::array::from_fn(|i| ((i * 13 + i / 7) % 32) as u8);
                let b = build_q5_k(ONE, HALF, sc, m, &q);
                assert_eq!(b.len(), 176);
                let mut y = [0.0f32; 256];
                q5_k(&b, &mut y);
                for i in 0..256 {
                    let sub = i / 32;
                    let want = f32::from(sc[sub]) * f32::from(q[i]) - 0.5 * f32::from(m[sub]);
                    assert_eq!(y[i], want, "element {i}");
                }
            }

            fn build_q6_k(d: u16, scales: [i8; 16], q: &[u8; 256]) -> Vec<u8> {
                // q holds 6-bit values (0..64); stored value is q, decoded as q - 32
                let mut ql = [0u8; 128];
                let mut qh = [0u8; 64];
                for half in 0..2 {
                    for l in 0..32 {
                        let base = half * 128;
                        let (v1, v2, v3, v4) = (
                            q[base + l],
                            q[base + l + 32],
                            q[base + l + 64],
                            q[base + l + 96],
                        );
                        ql[half * 64 + l] = (v1 & 0xf) | ((v3 & 0xf) << 4);
                        ql[half * 64 + l + 32] = (v2 & 0xf) | ((v4 & 0xf) << 4);
                        qh[half * 32 + l] = ((v1 >> 4) & 3)
                            | (((v2 >> 4) & 3) << 2)
                            | (((v3 >> 4) & 3) << 4)
                            | (((v4 >> 4) & 3) << 6);
                    }
                }
                let mut b = ql.to_vec();
                b.extend_from_slice(&qh);
                b.extend(scales.iter().map(|s| *s as u8));
                b.extend_from_slice(&d.to_le_bytes());
                b
            }

            #[test]
            fn q6_k_matches_the_formula() {
                let scales: [i8; 16] = std::array::from_fn(|i| (i as i8 - 8) * 3);
                let q: [u8; 256] = std::array::from_fn(|i| ((i * 29 + 5) % 64) as u8);
                let b = build_q6_k(HALF, scales, &q);
                assert_eq!(b.len(), 210);
                let mut y = [0.0f32; 256];
                q6_k(&b, &mut y);
                for i in 0..256 {
                    // sub-blocks of 16 elements, 16 scales in element order
                    let want = 0.5 * f32::from(scales[i / 16]) * (f32::from(q[i]) - 32.0);
                    assert_eq!(y[i], want, "element {i}");
                }
            }

            #[test]
            fn q6_k_extreme_values() {
                let b = build_q6_k(ONE, [127; 16], &[63; 256]);
                let mut y = [0.0f32; 256];
                q6_k(&b, &mut y);
                assert!(y.iter().all(|v| *v == 127.0 * 31.0));
                let b = build_q6_k(ONE, [-128; 16], &[0; 256]);
                q6_k(&b, &mut y);
                assert!(y.iter().all(|v| *v == -128.0 * -32.0));
            }

            #[test]
            fn zero_blocks_decode_to_zero_or_the_bias() {
                let mut y = [1.0f32; 256];
                q4_k(&[0u8; 144], &mut y);
                assert!(y.iter().all(|v| *v == 0.0));
                q5_k(&[0u8; 176], &mut y);
                assert!(y.iter().all(|v| *v == 0.0));
                q6_k(&[0u8; 210], &mut y); // d = 0
                assert!(y.iter().all(|v| *v == 0.0));
            }
        }
    }
    mod legacy {
        //! 32-element block formats: Q4_0, Q4_1, Q5_0, Q5_1, Q8_0.
        //!
        //! Layouts (little-endian):
        //!
        //! * `Q4_0` (18 bytes): `d: f16`, `qs: [u8; 16]`. Element `j` is the low nibble of `qs[j]`,
        //!   element `j + 16` the high nibble; value = `(nibble - 8) * d`.
        //! * `Q4_1` (20 bytes): `d: f16`, `m: f16`, `qs: [u8; 16]`; value = `nibble * d + m`.
        //! * `Q5_0` (22 bytes): `d: f16`, `qh: u32`, `qs: [u8; 16]`; the fifth bit of element `j` is bit
        //!   `j` of `qh`, of element `j + 16` bit `j + 16`; value = `(q5 - 16) * d`.
        //! * `Q5_1` (24 bytes): `d: f16`, `m: f16`, `qh: u32`, `qs: [u8; 16]`; value = `q5 * d + m`.
        //! * `Q8_0` (34 bytes): `d: f16`, `qs: [i8; 32]`; value = `q * d`.

        use crate::quant::float::f16_to_f32;
        use crate::quant::sz;

        fn h(bytes: &[u8], at: usize) -> f32 {
            f16_to_f32(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
        }

        pub(crate) fn q4_0(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes.chunks_exact(sz(18)).zip(out.chunks_exact_mut(sz(32))) {
                let d = h(b, 0);
                for j in 0..16 {
                    let q = b[2 + j];
                    y[j] = f32::from(i32::from(q & 0xf) as i16 - 8) * d;
                    y[j + 16] = f32::from(i32::from(q >> 4) as i16 - 8) * d;
                }
            }
        }

        pub(crate) fn q4_1(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes.chunks_exact(sz(20)).zip(out.chunks_exact_mut(sz(32))) {
                let (d, m) = (h(b, 0), h(b, 2));
                for j in 0..16 {
                    let q = b[4 + j];
                    y[j] = f32::from(q & 0xf) * d + m;
                    y[j + 16] = f32::from(q >> 4) * d + m;
                }
            }
        }

        pub(crate) fn q5_0(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes.chunks_exact(sz(22)).zip(out.chunks_exact_mut(sz(32))) {
                let d = h(b, 0);
                let qh = u32::from_le_bytes([b[2], b[3], b[4], b[5]]);
                for j in 0..16 {
                    let q = b[6 + j];
                    let lo = (q & 0xf) | ((((qh >> j) & 1) as u8) << 4);
                    let hi = (q >> 4) | ((((qh >> (j + 16)) & 1) as u8) << 4);
                    y[j] = (f32::from(lo) - 16.0) * d;
                    y[j + 16] = (f32::from(hi) - 16.0) * d;
                }
            }
        }

        pub(crate) fn q5_1(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes.chunks_exact(sz(24)).zip(out.chunks_exact_mut(sz(32))) {
                let (d, m) = (h(b, 0), h(b, 2));
                let qh = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                for j in 0..16 {
                    let q = b[8 + j];
                    let lo = (q & 0xf) | ((((qh >> j) & 1) as u8) << 4);
                    let hi = (q >> 4) | ((((qh >> (j + 16)) & 1) as u8) << 4);
                    y[j] = f32::from(lo) * d + m;
                    y[j + 16] = f32::from(hi) * d + m;
                }
            }
        }

        pub(crate) fn q8_0(bytes: &[u8], out: &mut [f32]) {
            for (b, y) in bytes.chunks_exact(sz(34)).zip(out.chunks_exact_mut(sz(32))) {
                let d = h(b, 0);
                for (o, q) in y.iter_mut().zip(&b[2..34]) {
                    *o = f32::from(*q as i8) * d;
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            const ONE: u16 = 0x3c00; // 1.0
            const HALF: u16 = 0x3800; // 0.5
            const TWO: u16 = 0x4000; // 2.0

            fn le(v: u16) -> [u8; 2] {
                v.to_le_bytes()
            }

            #[test]
            fn q8_0_scales_signed_bytes() {
                let mut b = le(HALF).to_vec();
                b.extend((0..32).map(|i| (i as i8 - 16) as u8));
                let mut y = [0.0f32; 32];
                q8_0(&b, &mut y);
                for (i, v) in y.iter().enumerate() {
                    assert_eq!(*v, (i as f32 - 16.0) * 0.5);
                }
            }

            #[test]
            fn q4_0_low_nibbles_first_then_high() {
                let mut b = le(TWO).to_vec();
                // qs[j]: low nibble j, high nibble 15 - j
                b.extend((0..16u8).map(|j| j | ((15 - j) << 4)));
                let mut y = [0.0f32; 32];
                q4_0(&b, &mut y);
                for j in 0..16 {
                    assert_eq!(y[j], (j as f32 - 8.0) * 2.0);
                    assert_eq!(y[j + 16], ((15 - j) as f32 - 8.0) * 2.0);
                }
            }

            #[test]
            fn q4_1_adds_minimum() {
                let mut b = le(HALF).to_vec();
                b.extend_from_slice(&le(ONE)); // m = 1.0
                b.extend((0..16u8).map(|j| j | (j << 4)));
                let mut y = [0.0f32; 32];
                q4_1(&b, &mut y);
                for j in 0..16 {
                    assert_eq!(y[j], j as f32 * 0.5 + 1.0);
                    assert_eq!(y[j + 16], j as f32 * 0.5 + 1.0);
                }
            }

            #[test]
            fn q5_0_uses_the_high_bit_from_qh() {
                let mut b = le(ONE).to_vec();
                // fifth bit set for element 3 (bit 3) and element 20 (bit 20)
                let qh: u32 = (1 << 3) | (1 << 20);
                b.extend_from_slice(&qh.to_le_bytes());
                b.extend(std::iter::repeat_n(0x00u8, 16)); // all nibbles zero
                let mut y = [0.0f32; 32];
                q5_0(&b, &mut y);
                for (i, v) in y.iter().enumerate() {
                    let q5 = if i == 3 || i == 20 { 16.0 } else { 0.0 };
                    assert_eq!(*v, q5 - 16.0, "element {i}");
                }
            }

            #[test]
            fn q5_1_combines_nibbles_bits_scale_and_min() {
                let mut b = le(HALF).to_vec();
                b.extend_from_slice(&le(TWO)); // m = 2.0
                let qh: u32 = 0xffff_0000; // fifth bit set for the upper 16 elements
                b.extend_from_slice(&qh.to_le_bytes());
                b.extend((0..16u8).map(|j| j | (0xf << 4)));
                let mut y = [0.0f32; 32];
                q5_1(&b, &mut y);
                for j in 0..16 {
                    assert_eq!(y[j], j as f32 * 0.5 + 2.0);
                    assert_eq!(y[j + 16], (15.0 + 16.0) * 0.5 + 2.0);
                }
            }

            #[test]
            fn multiple_blocks_are_laid_out_consecutively() {
                let mut b = Vec::new();
                for scale in [ONE, TWO] {
                    b.extend_from_slice(&le(scale));
                    b.extend(std::iter::repeat_n(1u8, 32));
                }
                let mut y = [0.0f32; 64];
                q8_0(&b, &mut y);
                assert!(y[..32].iter().all(|v| *v == 1.0));
                assert!(y[32..].iter().all(|v| *v == 2.0));
            }

            #[test]
            fn extreme_nibbles() {
                let mut b = le(ONE).to_vec();
                b.extend(std::iter::repeat_n(0xf0u8, 16)); // low 0, high 15
                let mut y = [0.0f32; 32];
                q4_0(&b, &mut y);
                assert!(y[..16].iter().all(|v| *v == -8.0));
                assert!(y[16..].iter().all(|v| *v == 7.0));
            }
        }
    }

    use crate::gguf::types::GgmlType;
    use std::fmt;

    pub use float::{bf16_to_f32, f16_to_f32};

    /// Dequantisation failure.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum QuantError {
        /// The element type is not implemented here.
        Unsupported(GgmlType),
        /// `n` is not a whole number of blocks for the type.
        NotBlockAligned,
        /// The byte slice length does not match `n` elements of this type.
        WrongByteLength {
            /// Bytes required.
            expected: usize,
            /// Bytes supplied.
            found: usize,
        },
        /// The output slice does not hold exactly `n` elements.
        WrongOutputLength,
        /// A size computation overflowed.
        Overflow,
    }

    impl fmt::Display for QuantError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                QuantError::Unsupported(t) => {
                    write!(f, "dequantisation of {} is not supported", t.name())
                }
                QuantError::NotBlockAligned => {
                    f.write_str("element count is not a multiple of the block size")
                }
                QuantError::WrongByteLength { expected, found } => {
                    write!(f, "expected {expected} bytes, got {found}")
                }
                QuantError::WrongOutputLength => {
                    f.write_str("output length does not match the element count")
                }
                QuantError::Overflow => f.write_str("size computation overflowed"),
            }
        }
    }

    impl std::error::Error for QuantError {}

    /// Pass-through that keeps block sizes out of constant position, so slicing loops stay generic over
    /// the block size (and clippy does not ask for `as_chunks` on toolchains that predate it).
    #[inline]
    pub(crate) fn sz(n: usize) -> usize {
        n
    }

    /// `true` if [`dequantize`] supports `ty`.
    pub fn is_supported(ty: GgmlType) -> bool {
        matches!(
            ty,
            GgmlType::F32
                | GgmlType::F16
                | GgmlType::BF16
                | GgmlType::Q4_0
                | GgmlType::Q4_1
                | GgmlType::Q5_0
                | GgmlType::Q5_1
                | GgmlType::Q8_0
                | GgmlType::Q4_K
                | GgmlType::Q5_K
                | GgmlType::Q6_K
        )
    }

    /// Bytes occupied by `n` elements of `ty`.
    pub fn byte_len(ty: GgmlType, n: usize) -> Result<usize, QuantError> {
        let block = usize::try_from(ty.block_size()).map_err(|_| QuantError::Overflow)?;
        if block == 0 || !n.is_multiple_of(block) {
            return Err(QuantError::NotBlockAligned);
        }
        let size = usize::try_from(ty.type_size()).map_err(|_| QuantError::Overflow)?;
        (n / block).checked_mul(size).ok_or(QuantError::Overflow)
    }

    /// Decode `n = out.len()` elements of type `ty` from `bytes` into `out`.
    ///
    /// `bytes` must be exactly [`byte_len`]`(ty, out.len())` long.
    pub fn dequantize(ty: GgmlType, bytes: &[u8], out: &mut [f32]) -> Result<(), QuantError> {
        if !is_supported(ty) {
            return Err(QuantError::Unsupported(ty));
        }
        let expected = byte_len(ty, out.len())?;
        if bytes.len() != expected {
            return Err(QuantError::WrongByteLength {
                expected,
                found: bytes.len(),
            });
        }
        match ty {
            GgmlType::F32 => float::f32s(bytes, out),
            GgmlType::F16 => float::f16s(bytes, out),
            GgmlType::BF16 => float::bf16s(bytes, out),
            GgmlType::Q4_0 => legacy::q4_0(bytes, out),
            GgmlType::Q4_1 => legacy::q4_1(bytes, out),
            GgmlType::Q5_0 => legacy::q5_0(bytes, out),
            GgmlType::Q5_1 => legacy::q5_1(bytes, out),
            GgmlType::Q8_0 => legacy::q8_0(bytes, out),
            GgmlType::Q4_K => kquant::q4_k(bytes, out),
            GgmlType::Q5_K => kquant::q5_k(bytes, out),
            GgmlType::Q6_K => kquant::q6_k(bytes, out),
            other => return Err(QuantError::Unsupported(other)),
        }
        Ok(())
    }

    /// Dequantise into a new vector of `n` elements.
    pub fn dequantize_vec(ty: GgmlType, bytes: &[u8], n: usize) -> Result<Vec<f32>, QuantError> {
        let mut out = vec![0.0f32; n];
        dequantize(ty, bytes, &mut out)?;
        Ok(out)
    }

    /// Dot product of one quantised row with `x` (dequantises the row first).
    pub fn dot_row(ty: GgmlType, row: &[u8], x: &[f32]) -> Result<f32, QuantError> {
        let mut tmp = vec![0.0f32; x.len()];
        dequantize(ty, row, &mut tmp)?;
        Ok(tmp.iter().zip(x).map(|(a, b)| a * b).sum())
    }

    /// Matrix-vector product with a quantised row-major matrix: `out[r] = row_r · x`.
    ///
    /// `rows * cols` elements are expected in `bytes`; `cols` must be block aligned for `ty`.
    pub fn matvec(
        ty: GgmlType,
        bytes: &[u8],
        rows: usize,
        cols: usize,
        x: &[f32],
        out: &mut [f32],
    ) -> Result<(), QuantError> {
        if x.len() != cols || out.len() != rows {
            return Err(QuantError::WrongOutputLength);
        }
        let row_bytes = byte_len(ty, cols)?;
        let total = row_bytes.checked_mul(rows).ok_or(QuantError::Overflow)?;
        if bytes.len() != total {
            return Err(QuantError::WrongByteLength {
                expected: total,
                found: bytes.len(),
            });
        }
        let mut tmp = vec![0.0f32; cols];
        for (r, o) in out.iter_mut().enumerate() {
            dequantize(ty, &bytes[r * row_bytes..(r + 1) * row_bytes], &mut tmp)?;
            *o = tmp.iter().zip(x).map(|(a, b)| a * b).sum();
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn support_table() {
            assert!(is_supported(GgmlType::Q4_K));
            assert!(!is_supported(GgmlType::Q2_K));
            assert!(!is_supported(GgmlType::I32));
            let mut out = [0.0f32; 256];
            assert_eq!(
                dequantize(GgmlType::Q2_K, &[0; 84], &mut out),
                Err(QuantError::Unsupported(GgmlType::Q2_K))
            );
        }

        #[test]
        fn byte_len_checks_alignment() {
            assert_eq!(byte_len(GgmlType::F32, 3), Ok(12));
            assert_eq!(byte_len(GgmlType::Q8_0, 64), Ok(68));
            assert_eq!(byte_len(GgmlType::Q4_K, 512), Ok(288));
            assert_eq!(
                byte_len(GgmlType::Q8_0, 33),
                Err(QuantError::NotBlockAligned)
            );
            assert_eq!(
                byte_len(GgmlType::F32, usize::MAX),
                Err(QuantError::Overflow)
            );
        }

        #[test]
        fn length_mismatches_are_errors() {
            let mut out = [0.0f32; 32];
            assert_eq!(
                dequantize(GgmlType::Q8_0, &[0; 33], &mut out),
                Err(QuantError::WrongByteLength {
                    expected: 34,
                    found: 33
                })
            );
            let mut odd = [0.0f32; 31];
            assert_eq!(
                dequantize(GgmlType::Q8_0, &[0; 34], &mut odd),
                Err(QuantError::NotBlockAligned)
            );
        }

        #[test]
        fn dot_and_matvec_agree_with_manual_sum() {
            // two Q8_0 rows of 32: scale 0.5, values 1..=32 and all 2
            fn row(scale: u16, f: impl Fn(usize) -> i8) -> Vec<u8> {
                let mut b = scale.to_le_bytes().to_vec();
                b.extend((0..32).map(|i| f(i) as u8));
                b
            }
            let half = 0x3800u16; // 0.5 in f16
            let r0 = row(half, |i| (i + 1) as i8);
            let r1 = row(half, |_| 2);
            let x: Vec<f32> = (0..32).map(|i| i as f32 * 0.1).collect();
            let want0: f32 = (0..32).map(|i| 0.5 * (i + 1) as f32 * x[i]).sum();
            let want1: f32 = (0..32).map(|i| 0.5 * 2.0 * x[i]).sum();
            assert!((dot_row(GgmlType::Q8_0, &r0, &x).unwrap() - want0).abs() < 1e-4);
            let mut bytes = r0.clone();
            bytes.extend_from_slice(&r1);
            let mut out = [0.0f32; 2];
            matvec(GgmlType::Q8_0, &bytes, 2, 32, &x, &mut out).unwrap();
            assert!((out[0] - want0).abs() < 1e-4 && (out[1] - want1).abs() < 1e-4);
            assert!(matvec(GgmlType::Q8_0, &bytes, 2, 32, &x[..31], &mut out).is_err());
            assert!(matvec(GgmlType::Q8_0, &bytes[..60], 2, 32, &x, &mut out).is_err());
        }

        #[test]
        fn errors_display() {
            assert!(QuantError::Unsupported(GgmlType::Q2_K)
                .to_string()
                .contains("Q2_K"));
            assert!(QuantError::WrongByteLength {
                expected: 1,
                found: 2
            }
            .to_string()
            .contains('2'));
        }
    }
}
pub mod tensor {
    //! MOULI tensor primitives: element types, row-major layouts, owned
    //! tensors, borrowed views with slicing, and a bump arena.
    //!
    //! Ported from the retired C core (`aeon_tensor`, `aeon_tensor_view`,
    //! `aeon_runtime_memory`). All size arithmetic is checked.
    #![forbid(unsafe_code)]

    pub mod arena {
        //! Bump arena: hands out aligned byte ranges from one fixed buffer.
        //!
        //! Allocations are offsets, not pointers, so the arena stays safe Rust.
        //! `reset` frees everything at once.

        use core::fmt;
        use core::ops::Range;

        /// Arena errors.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ArenaError {
            /// Alignment is not a power of two, or capacity is zero.
            BadConfig,
            /// Requested size is zero.
            ZeroSize,
            /// Not enough space left.
            Exhausted,
        }

        impl fmt::Display for ArenaError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self {
                    Self::BadConfig => "arena alignment or capacity invalid",
                    Self::ZeroSize => "zero-size allocation",
                    Self::Exhausted => "arena exhausted",
                })
            }
        }

        impl std::error::Error for ArenaError {}

        /// A fixed-capacity bump allocator.
        #[derive(Debug, Clone)]
        pub struct Arena {
            buf: Vec<u8>,
            used: usize,
            align: usize,
        }

        impl Arena {
            /// New zeroed arena of `capacity` bytes with power-of-two `align`.
            pub fn new(capacity: usize, align: usize) -> Result<Self, ArenaError> {
                if capacity == 0 || !align.is_power_of_two() {
                    return Err(ArenaError::BadConfig);
                }
                Ok(Self {
                    buf: vec![0; capacity],
                    used: 0,
                    align,
                })
            }

            /// Reserve `size` bytes; returns the byte range inside the buffer.
            pub fn alloc(&mut self, size: usize) -> Result<Range<usize>, ArenaError> {
                if size == 0 {
                    return Err(ArenaError::ZeroSize);
                }
                let start = self
                    .used
                    .checked_next_multiple_of(self.align)
                    .ok_or(ArenaError::Exhausted)?;
                let end = start.checked_add(size).ok_or(ArenaError::Exhausted)?;
                if end > self.buf.len() {
                    return Err(ArenaError::Exhausted);
                }
                self.used = end;
                Ok(start..end)
            }

            /// Bytes of an earlier allocation.
            pub fn get_mut(&mut self, r: Range<usize>) -> Option<&mut [u8]> {
                self.buf.get_mut(r)
            }

            /// Free everything.
            pub fn reset(&mut self) {
                self.used = 0;
            }

            /// Bytes not yet handed out (ignoring alignment padding).
            #[must_use]
            pub fn available(&self) -> usize {
                self.buf.len() - self.used
            }

            /// Total capacity.
            #[must_use]
            pub fn capacity(&self) -> usize {
                self.buf.len()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn aligned_bumps() {
                let mut a = Arena::new(64, 16).unwrap();
                assert_eq!(a.alloc(3).unwrap(), 0..3);
                assert_eq!(a.alloc(5).unwrap(), 16..21);
                assert_eq!(a.available(), 43);
                assert_eq!(a.alloc(40).unwrap_err(), ArenaError::Exhausted);
                a.reset();
                assert_eq!(a.alloc(64).unwrap(), 0..64);
            }

            #[test]
            fn bad_config() {
                assert_eq!(Arena::new(0, 8).unwrap_err(), ArenaError::BadConfig);
                assert_eq!(Arena::new(8, 3).unwrap_err(), ArenaError::BadConfig);
                let mut a = Arena::new(8, 8).unwrap();
                assert_eq!(a.alloc(0).unwrap_err(), ArenaError::ZeroSize);
            }
        }
    }
    pub mod layout {
        //! Element types and row-major layouts.

        use core::fmt;

        /// Highest supported tensor rank.
        pub const MAX_RANK: usize = 8;

        /// Element type of a dense tensor.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum DType {
            /// 32-bit IEEE float.
            F32,
            /// bfloat16.
            BF16,
            /// 16-bit IEEE float.
            F16,
            /// 8-bit float, e4m3 encoding.
            Fp8E4M3,
            /// Signed 8-bit integer.
            I8,
            /// Signed 16-bit integer.
            I16,
            /// Signed 32-bit integer.
            I32,
        }

        impl DType {
            /// Bytes per element.
            #[must_use]
            pub const fn size(self) -> usize {
                match self {
                    Self::F32 | Self::I32 => 4,
                    Self::BF16 | Self::F16 | Self::I16 => 2,
                    Self::Fp8E4M3 | Self::I8 => 1,
                }
            }

            /// Canonical upper-case name, as used in manifests.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    Self::F32 => "F32",
                    Self::BF16 => "BF16",
                    Self::F16 => "F16",
                    Self::Fp8E4M3 => "FP8_E4M3",
                    Self::I8 => "I8",
                    Self::I16 => "I16",
                    Self::I32 => "I32",
                }
            }

            /// Parse a canonical name.
            #[must_use]
            pub fn from_name(s: &str) -> Option<Self> {
                Some(match s {
                    "F32" => Self::F32,
                    "BF16" => Self::BF16,
                    "F16" => Self::F16,
                    "FP8_E4M3" => Self::Fp8E4M3,
                    "I8" => Self::I8,
                    "I16" => Self::I16,
                    "I32" => Self::I32,
                    _ => return None,
                })
            }
        }

        /// Errors from layout, tensor and view construction.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum TensorError {
            /// Rank is 0 or above [`MAX_RANK`].
            BadRank,
            /// A dimension is zero.
            ZeroDim,
            /// Size arithmetic overflowed.
            Overflow,
            /// Axis or index out of range.
            OutOfRange,
            /// Buffer length does not match the layout.
            SizeMismatch,
        }

        impl fmt::Display for TensorError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self {
                    Self::BadRank => "tensor rank out of range",
                    Self::ZeroDim => "tensor dimension is zero",
                    Self::Overflow => "tensor size overflow",
                    Self::OutOfRange => "axis or index out of range",
                    Self::SizeMismatch => "buffer size does not match layout",
                })
            }
        }

        impl std::error::Error for TensorError {}

        /// Shape, strides (in elements) and sizes of a tensor.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Layout {
            dtype: DType,
            shape: Vec<usize>,
            strides: Vec<usize>,
            elements: usize,
        }

        impl Layout {
            /// Contiguous row-major layout for `shape`.
            pub fn contiguous(dtype: DType, shape: &[usize]) -> Result<Self, TensorError> {
                if shape.is_empty() || shape.len() > MAX_RANK {
                    return Err(TensorError::BadRank);
                }
                let elements = checked_product(shape)?;
                elements
                    .checked_mul(dtype.size())
                    .ok_or(TensorError::Overflow)?;
                let mut strides = vec![1usize; shape.len()];
                let mut acc = 1usize;
                for (s, &d) in strides.iter_mut().zip(shape).rev() {
                    *s = acc;
                    acc = acc.checked_mul(d).ok_or(TensorError::Overflow)?;
                }
                Ok(Self {
                    dtype,
                    shape: shape.to_vec(),
                    strides,
                    elements,
                })
            }

            /// Element type.
            #[must_use]
            pub fn dtype(&self) -> DType {
                self.dtype
            }

            /// Dimensions.
            #[must_use]
            pub fn shape(&self) -> &[usize] {
                &self.shape
            }

            /// Strides in elements.
            #[must_use]
            pub fn strides(&self) -> &[usize] {
                &self.strides
            }

            /// Number of dimensions.
            #[must_use]
            pub fn rank(&self) -> usize {
                self.shape.len()
            }

            /// Number of elements.
            #[must_use]
            pub fn elements(&self) -> usize {
                self.elements
            }

            /// Number of bytes for a dense buffer (checked at construction).
            #[must_use]
            pub fn bytes(&self) -> usize {
                self.elements * self.dtype.size()
            }

            /// Element offset of `indices`, or `None` if any index is out of range.
            #[must_use]
            pub fn offset_of(&self, indices: &[usize]) -> Option<usize> {
                if indices.len() != self.rank() {
                    return None;
                }
                let mut off = 0usize;
                for ((&i, &n), &s) in indices.iter().zip(&self.shape).zip(&self.strides) {
                    if i >= n {
                        return None;
                    }
                    off = off.checked_add(i.checked_mul(s)?)?;
                }
                Some(off)
            }

            /// Number of elements between the first and last addressed element, plus one.
            pub fn span_elements(&self) -> Result<usize, TensorError> {
                let mut span = 1usize;
                for (&n, &s) in self.shape.iter().zip(&self.strides) {
                    if n > 1 {
                        let axis = (n - 1).checked_mul(s).ok_or(TensorError::Overflow)?;
                        span = span.checked_add(axis).ok_or(TensorError::Overflow)?;
                    }
                }
                Ok(span)
            }

            pub(crate) fn with_axis_fixed(&self, axis: usize) -> Self {
                let mut out = self.clone();
                out.elements /= out.shape[axis];
                out.shape[axis] = 1;
                out
            }
        }

        /// Product of `dims`, rejecting zero and overflow.
        pub fn checked_product(dims: &[usize]) -> Result<usize, TensorError> {
            dims.iter().try_fold(1usize, |acc, &d| {
                if d == 0 {
                    Err(TensorError::ZeroDim)
                } else {
                    acc.checked_mul(d).ok_or(TensorError::Overflow)
                }
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn row_major_strides() {
                let l = Layout::contiguous(DType::F32, &[2, 3, 4]).unwrap();
                assert_eq!(l.strides(), &[12, 4, 1]);
                assert_eq!(l.elements(), 24);
                assert_eq!(l.bytes(), 96);
                assert_eq!(l.offset_of(&[1, 2, 3]), Some(23));
                assert_eq!(l.offset_of(&[2, 0, 0]), None);
            }

            #[test]
            fn rejects_bad_shapes() {
                assert_eq!(
                    Layout::contiguous(DType::I8, &[]),
                    Err(TensorError::BadRank)
                );
                assert_eq!(
                    Layout::contiguous(DType::I8, &[1; 9]),
                    Err(TensorError::BadRank)
                );
                assert_eq!(
                    Layout::contiguous(DType::I8, &[3, 0]),
                    Err(TensorError::ZeroDim)
                );
                assert_eq!(
                    Layout::contiguous(DType::F32, &[usize::MAX / 2, 3]),
                    Err(TensorError::Overflow)
                );
            }

            #[test]
            fn dtype_names_round_trip() {
                for d in [
                    DType::F32,
                    DType::BF16,
                    DType::F16,
                    DType::Fp8E4M3,
                    DType::I8,
                    DType::I16,
                    DType::I32,
                ] {
                    assert_eq!(DType::from_name(d.name()), Some(d));
                }
                assert_eq!(DType::from_name("Q4"), None);
            }
        }
    }
    pub mod view {
        //! Owned tensors and borrowed, sliceable views.

        use crate::tensor::layout::{Layout, TensorError};

        /// A dense tensor that owns its bytes.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Tensor {
            layout: Layout,
            data: Vec<u8>,
        }

        impl Tensor {
            /// Zero-filled tensor with the given layout.
            #[must_use]
            pub fn zeroed(layout: Layout) -> Self {
                let data = vec![0u8; layout.bytes()];
                Self { layout, data }
            }

            /// Wrap existing bytes; the length must equal `layout.bytes()`.
            pub fn from_bytes(layout: Layout, data: Vec<u8>) -> Result<Self, TensorError> {
                if data.len() != layout.bytes() {
                    return Err(TensorError::SizeMismatch);
                }
                Ok(Self { layout, data })
            }

            /// Layout.
            #[must_use]
            pub fn layout(&self) -> &Layout {
                &self.layout
            }

            /// Raw bytes.
            #[must_use]
            pub fn bytes(&self) -> &[u8] {
                &self.data
            }

            /// Mutable raw bytes.
            pub fn bytes_mut(&mut self) -> &mut [u8] {
                &mut self.data
            }

            /// Set every byte to zero.
            pub fn zero(&mut self) {
                self.data.fill(0);
            }

            /// Full view over this tensor.
            #[must_use]
            pub fn view(&self) -> TensorView<'_> {
                TensorView {
                    layout: self.layout.clone(),
                    byte_offset: 0,
                    owner: &self.data,
                }
            }
        }

        /// A borrowed window into a tensor's bytes.
        ///
        /// The view keeps a reference to the whole owner buffer plus a byte offset,
        /// so its bounds can always be re-checked.
        #[derive(Debug, Clone)]
        pub struct TensorView<'a> {
            layout: Layout,
            byte_offset: usize,
            owner: &'a [u8],
        }

        impl<'a> TensorView<'a> {
            /// Layout of the view (strides are those of the owner).
            #[must_use]
            pub fn layout(&self) -> &Layout {
                &self.layout
            }

            /// Byte offset of the first element inside the owner buffer.
            #[must_use]
            pub fn byte_offset(&self) -> usize {
                self.byte_offset
            }

            /// Fix `axis` at `index`, keeping rank (that axis becomes size 1).
            pub fn slice(&self, axis: usize, index: usize) -> Result<Self, TensorError> {
                let shape = self.layout.shape();
                if axis >= shape.len() || index >= shape[axis] {
                    return Err(TensorError::OutOfRange);
                }
                let item = self.layout.dtype().size();
                let delta = index
                    .checked_mul(self.layout.strides()[axis])
                    .and_then(|e| e.checked_mul(item))
                    .and_then(|b| b.checked_add(self.byte_offset))
                    .ok_or(TensorError::Overflow)?;
                let out = Self {
                    layout: self.layout.with_axis_fixed(axis),
                    byte_offset: delta,
                    owner: self.owner,
                };
                out.validate()?;
                Ok(out)
            }

            /// Check that every addressed element lies inside the owner buffer.
            pub fn validate(&self) -> Result<(), TensorError> {
                let span = self
                    .layout
                    .span_elements()?
                    .checked_mul(self.layout.dtype().size())
                    .ok_or(TensorError::Overflow)?;
                if self.byte_offset > self.owner.len() || span > self.owner.len() - self.byte_offset
                {
                    return Err(TensorError::OutOfRange);
                }
                Ok(())
            }

            /// Bytes of the element at `indices` (relative to this view).
            #[must_use]
            pub fn element(&self, indices: &[usize]) -> Option<&'a [u8]> {
                let item = self.layout.dtype().size();
                let start = self
                    .layout
                    .offset_of(indices)?
                    .checked_mul(item)?
                    .checked_add(self.byte_offset)?;
                self.owner.get(start..start.checked_add(item)?)
            }

            /// Bytes from the first element to the end of the view's span.
            pub fn span_bytes(&self) -> Result<&'a [u8], TensorError> {
                self.validate()?;
                let len = self.layout.span_elements()? * self.layout.dtype().size();
                Ok(&self.owner[self.byte_offset..self.byte_offset + len])
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::tensor::layout::DType;

            fn counting(shape: &[usize]) -> Tensor {
                let l = Layout::contiguous(DType::I8, shape).unwrap();
                let data = (0..l.bytes()).map(|i| i as u8).collect();
                Tensor::from_bytes(l, data).unwrap()
            }

            #[test]
            fn slice_rows_and_columns() {
                let t = counting(&[3, 4]);
                let row = t.view().slice(0, 2).unwrap();
                assert_eq!(row.layout().shape(), &[1, 4]);
                assert_eq!(row.span_bytes().unwrap(), &[8, 9, 10, 11]);
                let col = t.view().slice(1, 1).unwrap();
                assert_eq!(col.element(&[2, 0]), Some(&[9u8][..]));
                assert_eq!(col.layout().elements(), 3);
            }

            #[test]
            fn slice_out_of_range() {
                let t = counting(&[2, 2]);
                assert_eq!(t.view().slice(2, 0).unwrap_err(), TensorError::OutOfRange);
                assert_eq!(t.view().slice(0, 2).unwrap_err(), TensorError::OutOfRange);
            }

            #[test]
            fn from_bytes_checks_len() {
                let l = Layout::contiguous(DType::F32, &[2]).unwrap();
                assert_eq!(
                    Tensor::from_bytes(l, vec![0; 7]).unwrap_err(),
                    TensorError::SizeMismatch
                );
            }

            #[test]
            fn zero_clears() {
                let mut t = counting(&[4]);
                t.zero();
                assert!(t.bytes().iter().all(|&b| b == 0));
            }
        }
    }

    pub use arena::{Arena, ArenaError};
    pub use layout::{DType, Layout, TensorError, MAX_RANK};
    pub use view::{Tensor, TensorView};
}
pub mod tokenizer {
    //! Tokenizers built from GGUF metadata.
    //!
    //! Two families, selected by `tokenizer.ggml.model`:
    //!
    //! * `"llama"`: SentencePiece-style. Text gets a dummy prefix and spaces become `▁`; adjacent
    //!   symbols merge greedily by vocabulary score; characters missing from the vocabulary fall back
    //!   to byte tokens `<0xXX>` (or the unknown token when those are absent too).
    //! * `"gpt2"`: byte-level BPE. A GPT-2 style pre-tokenizer splits text into words, bytes map to
    //!   printable characters, and symbols merge by merge rank from `tokenizer.ggml.merges`.
    //!
    //! Written from the algorithm descriptions and the GGUF key list, not from other implementations.
    //!
    //! # Limits (read before relying on it)
    //!
    //! * The BPE pre-tokenizer is the GPT-2 pattern. Models that define a different pre-tokenizer
    //!   (`tokenizer.ggml.pre`, e.g. llama-3 or qwen2 families) will split some inputs differently,
    //!   so token ids can differ from the reference tokenizer for those models. The key is exposed
    //!   as [`Tokenizer::pre_tokenizer_name`] so callers can detect that case.
    //! * Unicode classes use Rust's `char::is_alphabetic` / `is_numeric`, not full `\p{L}` / `\p{N}`.
    //! * No comparison against a reference tokenizer on real models has been done yet.

    mod api {
        //! Shared vocabulary, construction from GGUF, and the public encode/decode API.

        use crate::gguf::{Array, Gguf};
        use crate::tokenizer::{bpe, spm};
        use std::collections::HashMap;
        use std::fmt;

        /// Token categories (`tokenizer.ggml.token_type`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum TokenKind {
            /// Ordinary text piece.
            Normal,
            /// The unknown token.
            Unknown,
            /// Control token such as `<s>`.
            Control,
            /// User-defined special token, matched in text when special parsing is on.
            UserDefined,
            /// Unused slot.
            Unused,
            /// Byte token `<0xXX>`.
            Byte,
        }

        impl TokenKind {
            fn from_i32(v: i32) -> TokenKind {
                match v {
                    2 => TokenKind::Unknown,
                    3 => TokenKind::Control,
                    4 => TokenKind::UserDefined,
                    5 => TokenKind::Unused,
                    6 => TokenKind::Byte,
                    _ => TokenKind::Normal,
                }
            }
        }

        /// Tokenizer construction failure.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum TokenizerError {
            /// A required metadata key is missing or has the wrong type.
            Metadata(String),
            /// `tokenizer.ggml.model` is not one this crate implements.
            UnsupportedModel(String),
            /// The vocabulary is empty or has duplicate token strings.
            BadVocabulary(String),
            /// A special-token id is outside the vocabulary.
            BadTokenId(u32),
        }

        impl fmt::Display for TokenizerError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    TokenizerError::Metadata(m) => write!(f, "tokenizer metadata: {m}"),
                    TokenizerError::UnsupportedModel(m) => {
                        write!(f, "unsupported tokenizer model {m:?}")
                    }
                    TokenizerError::BadVocabulary(m) => write!(f, "bad vocabulary: {m}"),
                    TokenizerError::BadTokenId(i) => {
                        write!(f, "token id {i} is outside the vocabulary")
                    }
                }
            }
        }

        impl std::error::Error for TokenizerError {}

        /// Everything needed to build a tokenizer without a GGUF file.
        #[derive(Debug, Clone)]
        pub struct TokenizerConfig {
            /// `"llama"` (SentencePiece-style) or `"gpt2"` (byte-level BPE).
            pub model: String,
            /// Token strings, indexed by id.
            pub tokens: Vec<String>,
            /// Scores (SentencePiece); empty means all zero.
            pub scores: Vec<f32>,
            /// Token types as in GGUF; empty means all normal.
            pub types: Vec<i32>,
            /// BPE merges as `"left right"`, best first.
            pub merges: Vec<String>,
            /// Beginning-of-sequence id.
            pub bos: Option<u32>,
            /// End-of-sequence id.
            pub eos: Option<u32>,
            /// Unknown-token id.
            pub unk: Option<u32>,
            /// Padding id.
            pub pad: Option<u32>,
            /// Whether to prepend BOS by default.
            pub add_bos: bool,
            /// Whether SentencePiece adds a dummy `▁` prefix (default true).
            pub add_space_prefix: bool,
            /// `tokenizer.ggml.pre`, if present.
            pub pre: Option<String>,
        }

        enum Algo {
            Spm,
            Bpe { ranks: HashMap<String, usize> },
        }

        /// A ready-to-use tokenizer.
        pub struct Tokenizer {
            tokens: Vec<String>,
            scores: Vec<f32>,
            kinds: Vec<TokenKind>,
            index: HashMap<String, u32>,
            /// Byte value for each byte token id (SentencePiece).
            byte_ids: [Option<u32>; 256],
            /// User-defined and control tokens, longest first, for special parsing.
            specials: Vec<(String, u32)>,
            algo: Algo,
            bos: Option<u32>,
            eos: Option<u32>,
            unk: Option<u32>,
            pad: Option<u32>,
            add_bos: bool,
            add_space_prefix: bool,
            pre: Option<String>,
        }

        fn strings(a: &Array, what: &str) -> Result<Vec<String>, TokenizerError> {
            a.as_strings()
                .map(<[String]>::to_vec)
                .ok_or_else(|| TokenizerError::Metadata(format!("{what} is not a string array")))
        }

        impl Tokenizer {
            /// Build from GGUF metadata (`tokenizer.ggml.*`).
            pub fn from_gguf(g: &Gguf) -> Result<Self, TokenizerError> {
                let model = g
                    .get_str("tokenizer.ggml.model")
                    .map_err(|e| TokenizerError::Metadata(e.to_string()))?
                    .to_string();
                let tokens = strings(
                    g.get_array("tokenizer.ggml.tokens")
                        .map_err(|e| TokenizerError::Metadata(e.to_string()))?,
                    "tokenizer.ggml.tokens",
                )?;
                let scores = match g.get("tokenizer.ggml.scores").and_then(|v| v.as_array()) {
                    Some(a) => a.as_f32s().map(<[f32]>::to_vec).ok_or_else(|| {
                        TokenizerError::Metadata("scores is not an f32 array".into())
                    })?,
                    None => Vec::new(),
                };
                let types = match g
                    .get("tokenizer.ggml.token_type")
                    .and_then(|v| v.as_array())
                {
                    Some(a) => a.as_i32s().map(<[i32]>::to_vec).ok_or_else(|| {
                        TokenizerError::Metadata("token_type is not an i32 array".into())
                    })?,
                    None => Vec::new(),
                };
                let merges = match g.get("tokenizer.ggml.merges").and_then(|v| v.as_array()) {
                    Some(a) => strings(a, "tokenizer.ggml.merges")?,
                    None => Vec::new(),
                };
                let id = |k: &str| g.get(k).and_then(|v| v.as_u32());
                let cfg = TokenizerConfig {
                    model,
                    tokens,
                    scores,
                    types,
                    merges,
                    bos: id("tokenizer.ggml.bos_token_id"),
                    eos: id("tokenizer.ggml.eos_token_id"),
                    unk: id("tokenizer.ggml.unknown_token_id"),
                    pad: id("tokenizer.ggml.padding_token_id"),
                    add_bos: g
                        .get("tokenizer.ggml.add_bos_token")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                    add_space_prefix: g
                        .get("tokenizer.ggml.add_space_prefix")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                    pre: g
                        .get("tokenizer.ggml.pre")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                };
                Tokenizer::new(cfg)
            }

            /// Build from an explicit configuration.
            pub fn new(cfg: TokenizerConfig) -> Result<Self, TokenizerError> {
                if cfg.tokens.is_empty() {
                    return Err(TokenizerError::BadVocabulary("empty vocabulary".into()));
                }
                let n = u32::try_from(cfg.tokens.len())
                    .map_err(|_| TokenizerError::BadVocabulary("too many tokens".into()))?;
                for id in [cfg.bos, cfg.eos, cfg.unk, cfg.pad].into_iter().flatten() {
                    if id >= n {
                        return Err(TokenizerError::BadTokenId(id));
                    }
                }
                let mut index = HashMap::with_capacity(cfg.tokens.len());
                for (i, t) in cfg.tokens.iter().enumerate() {
                    // Duplicates exist in some real vocabularies; the first id wins for encoding.
                    index.entry(t.clone()).or_insert(i as u32);
                }
                let kinds: Vec<TokenKind> = (0..cfg.tokens.len())
                    .map(|i| {
                        cfg.types
                            .get(i)
                            .copied()
                            .map_or(TokenKind::Normal, TokenKind::from_i32)
                    })
                    .collect();
                let scores: Vec<f32> = (0..cfg.tokens.len())
                    .map(|i| cfg.scores.get(i).copied().unwrap_or(0.0))
                    .collect();

                let mut byte_ids = [None; 256];
                for (i, t) in cfg.tokens.iter().enumerate() {
                    if let Some(b) = parse_byte_token(t) {
                        if byte_ids[usize::from(b)].is_none() {
                            byte_ids[usize::from(b)] = Some(i as u32);
                        }
                    }
                }
                let mut specials: Vec<(String, u32)> = cfg
                    .tokens
                    .iter()
                    .enumerate()
                    .filter(|(i, t)| {
                        matches!(kinds[*i], TokenKind::Control | TokenKind::UserDefined)
                            && !t.is_empty()
                    })
                    .map(|(i, t)| (t.clone(), i as u32))
                    .collect();
                specials.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.1.cmp(&b.1)));

                let algo = match cfg.model.as_str() {
                    "llama" | "spm" => Algo::Spm,
                    "gpt2" | "bpe" => {
                        let mut ranks = HashMap::with_capacity(cfg.merges.len());
                        for (rank, m) in cfg.merges.iter().enumerate() {
                            if !m.contains(' ') {
                                return Err(TokenizerError::BadVocabulary(format!(
                                    "merge {rank} has no space"
                                )));
                            }
                            // Keyed by the merge line itself ("left right"); first occurrence wins.
                            ranks.entry(m.clone()).or_insert(rank);
                        }
                        Algo::Bpe { ranks }
                    }
                    other => return Err(TokenizerError::UnsupportedModel(other.to_string())),
                };
                Ok(Tokenizer {
                    tokens: cfg.tokens,
                    scores,
                    kinds,
                    index,
                    byte_ids,
                    specials,
                    algo,
                    bos: cfg.bos,
                    eos: cfg.eos,
                    unk: cfg.unk,
                    pad: cfg.pad,
                    add_bos: cfg.add_bos,
                    add_space_prefix: cfg.add_space_prefix,
                    pre: cfg.pre,
                })
            }

            /// Vocabulary size.
            pub fn vocab_size(&self) -> usize {
                self.tokens.len()
            }

            /// The text of token `id`.
            pub fn token(&self, id: u32) -> Option<&str> {
                self.tokens.get(id as usize).map(String::as_str)
            }

            /// The id of an exact token string.
            pub fn id_of(&self, token: &str) -> Option<u32> {
                self.index.get(token).copied()
            }

            /// The category of token `id`.
            pub fn kind(&self, id: u32) -> Option<TokenKind> {
                self.kinds.get(id as usize).copied()
            }

            /// Beginning-of-sequence id.
            pub fn bos(&self) -> Option<u32> {
                self.bos
            }

            /// End-of-sequence id.
            pub fn eos(&self) -> Option<u32> {
                self.eos
            }

            /// Unknown-token id.
            pub fn unk(&self) -> Option<u32> {
                self.unk
            }

            /// Padding id.
            pub fn pad(&self) -> Option<u32> {
                self.pad
            }

            /// Whether BOS is prepended by default.
            pub fn adds_bos(&self) -> bool {
                self.add_bos
            }

            /// `tokenizer.ggml.pre` from the file, if any. BPE here always uses the GPT-2 pattern, so a
            /// value other than `None` or `"gpt2"` means ids may differ from the reference tokenizer.
            pub fn pre_tokenizer_name(&self) -> Option<&str> {
                self.pre.as_deref()
            }

            /// Encode `text`. Prepends BOS if `add_bos` and the model has one. With `parse_special`,
            /// control and user-defined token strings inside the text become their single ids.
            pub fn encode(&self, text: &str, add_bos: bool, parse_special: bool) -> Vec<u32> {
                let mut out = Vec::new();
                if add_bos {
                    if let Some(b) = self.bos {
                        out.push(b);
                    }
                }
                let mut first = true;
                for (is_special, piece) in self.split_specials(text, parse_special) {
                    if is_special {
                        if let Some(id) = self.index.get(piece).copied() {
                            out.push(id);
                        }
                    } else if !piece.is_empty() {
                        match &self.algo {
                            Algo::Spm => {
                                spm::encode(self, piece, first && self.add_space_prefix, &mut out)
                            }
                            Algo::Bpe { ranks } => bpe::encode(self, ranks, piece, &mut out),
                        }
                    }
                    first = false;
                }
                out
            }

            /// Encode with the model's default BOS behaviour and no special-token parsing.
            pub fn encode_default(&self, text: &str) -> Vec<u32> {
                self.encode(text, self.add_bos, false)
            }

            fn split_specials<'a>(&'a self, text: &'a str, parse: bool) -> Vec<(bool, &'a str)> {
                if !parse || self.specials.is_empty() {
                    return vec![(false, text)];
                }
                let mut parts = Vec::new();
                let mut rest_start = 0;
                let mut i = 0;
                while i < text.len() {
                    if !text.is_char_boundary(i) {
                        i += 1;
                        continue;
                    }
                    let hit = self
                        .specials
                        .iter()
                        .find(|(s, _)| text[i..].starts_with(s.as_str()));
                    if let Some((s, _)) = hit {
                        if i > rest_start {
                            parts.push((false, &text[rest_start..i]));
                        }
                        parts.push((true, &text[i..i + s.len()]));
                        i += s.len();
                        rest_start = i;
                    } else {
                        i += 1;
                    }
                }
                if rest_start < text.len() {
                    parts.push((false, &text[rest_start..]));
                }
                parts
            }

            /// Decode token ids to text. Control and unused tokens produce nothing; invalid UTF-8
            /// (possible when a multi-byte character is split across tokens) is replaced with U+FFFD.
            pub fn decode(&self, ids: &[u32]) -> String {
                let bytes = self.decode_bytes(ids);
                String::from_utf8_lossy(&bytes).into_owned()
            }

            /// Decode token ids to raw bytes.
            pub fn decode_bytes(&self, ids: &[u32]) -> Vec<u8> {
                match &self.algo {
                    Algo::Spm => spm::decode(self, ids),
                    Algo::Bpe { .. } => bpe::decode(self, ids),
                }
            }

            // --- crate-internal access for the algorithm modules ---
            pub(crate) fn score_of(&self, id: u32) -> f32 {
                self.scores.get(id as usize).copied().unwrap_or(0.0)
            }

            pub(crate) fn lookup(&self, s: &str) -> Option<u32> {
                self.index.get(s).copied()
            }

            pub(crate) fn byte_id(&self, b: u8) -> Option<u32> {
                self.byte_ids[usize::from(b)]
            }

            pub(crate) fn raw(&self, id: u32) -> Option<&str> {
                self.token(id)
            }

            pub(crate) fn kind_of(&self, id: u32) -> TokenKind {
                self.kind(id).unwrap_or(TokenKind::Unused)
            }

            pub(crate) fn unknown_id(&self) -> Option<u32> {
                self.unk
            }

            pub(crate) fn space_prefix(&self) -> bool {
                self.add_space_prefix
            }
        }

        /// `<0xXX>` (uppercase hex) to its byte.
        pub(crate) fn parse_byte_token(t: &str) -> Option<u8> {
            let h = t.strip_prefix("<0x")?.strip_suffix('>')?;
            if h.len() != 2
                || !h
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
            {
                return None;
            }
            u8::from_str_radix(h, 16).ok()
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn byte_tokens_parse_strictly() {
                assert_eq!(parse_byte_token("<0x00>"), Some(0));
                assert_eq!(parse_byte_token("<0xFF>"), Some(255));
                assert_eq!(parse_byte_token("<0x0a>"), None); // lowercase is not a byte token
                assert_eq!(parse_byte_token("<0x1>"), None);
                assert_eq!(parse_byte_token("<0x123>"), None);
                assert_eq!(parse_byte_token("0x41"), None);
                assert_eq!(parse_byte_token("<0xZZ>"), None);
            }

            fn cfg(model: &str, tokens: &[&str]) -> TokenizerConfig {
                TokenizerConfig {
                    model: model.into(),
                    tokens: tokens.iter().map(|s| s.to_string()).collect(),
                    scores: vec![],
                    types: vec![],
                    merges: vec![],
                    bos: None,
                    eos: None,
                    unk: None,
                    pad: None,
                    add_bos: false,
                    add_space_prefix: true,
                    pre: None,
                }
            }

            #[test]
            fn construction_validates() {
                assert!(matches!(
                    Tokenizer::new(cfg("llama", &[])),
                    Err(TokenizerError::BadVocabulary(_))
                ));
                assert!(matches!(
                    Tokenizer::new(cfg("wordpiece", &["a"])),
                    Err(TokenizerError::UnsupportedModel(_))
                ));
                let mut c = cfg("llama", &["a", "b"]);
                c.bos = Some(2);
                assert!(matches!(
                    Tokenizer::new(c),
                    Err(TokenizerError::BadTokenId(2))
                ));
                let mut c = cfg("gpt2", &["a"]);
                c.merges = vec!["nospace".into()];
                assert!(matches!(
                    Tokenizer::new(c),
                    Err(TokenizerError::BadVocabulary(_))
                ));
            }

            #[test]
            fn accessors_and_kinds() {
                let mut c = cfg("llama", &["<unk>", "<s>", "a"]);
                c.types = vec![2, 3, 1];
                c.unk = Some(0);
                c.bos = Some(1);
                let t = Tokenizer::new(c).unwrap();
                assert_eq!(t.vocab_size(), 3);
                assert_eq!(t.token(2), Some("a"));
                assert_eq!(t.token(9), None);
                assert_eq!(t.id_of("<s>"), Some(1));
                assert_eq!(t.kind(0), Some(TokenKind::Unknown));
                assert_eq!(t.kind(1), Some(TokenKind::Control));
                assert_eq!(t.kind(2), Some(TokenKind::Normal));
                assert_eq!(
                    (t.bos(), t.unk(), t.eos(), t.pad()),
                    (Some(1), Some(0), None, None)
                );
                assert!(!t.adds_bos());
                assert_eq!(t.pre_tokenizer_name(), None);
            }

            #[test]
            fn error_display() {
                assert!(TokenizerError::BadTokenId(7).to_string().contains('7'));
                assert!(TokenizerError::UnsupportedModel("x".into())
                    .to_string()
                    .contains("\"x\""));
            }
        }
    }
    mod bpe {
        //! GPT-2 style byte-level BPE.

        use crate::tokenizer::api::{TokenKind, Tokenizer};
        use std::cmp::{Ordering, Reverse};
        use std::collections::{BinaryHeap, HashMap};
        use std::sync::OnceLock;

        /// The standard byte to printable-character table: printable Latin-1 bytes map to themselves,
        /// the remaining 68 bytes to U+0100 upward in order.
        fn byte_table() -> &'static ([char; 256], HashMap<char, u8>) {
            static T: OnceLock<([char; 256], HashMap<char, u8>)> = OnceLock::new();
            T.get_or_init(|| {
                let mut table = ['\0'; 256];
                let mut next = 256u32;
                for b in 0..=255u32 {
                    let keep = (33..=126).contains(&b)
                        || (161..=172).contains(&b)
                        || (174..=255).contains(&b);
                    let c = if keep {
                        char::from_u32(b)
                    } else {
                        let c = char::from_u32(next);
                        next += 1;
                        c
                    };
                    table[b as usize] = c.unwrap_or('\u{fffd}');
                }
                let rev = table
                    .iter()
                    .enumerate()
                    .map(|(b, c)| (*c, b as u8))
                    .collect();
                (table, rev)
            })
        }

        /// Split text into words with the GPT-2 pattern
        /// `'s|'t|'re|'ve|'m|'ll|'d| ?letters+| ?digits+| ?other+|spaces-not-before-a-word|spaces`.
        pub(crate) fn pre_tokenize(text: &str) -> Vec<&str> {
            let chars: Vec<(usize, char)> = text.char_indices().collect();
            let n = chars.len();
            let at = |i: usize| chars[i].0;
            let end_of = |i: usize| if i < n { chars[i].0 } else { text.len() };
            let mut out = Vec::new();
            let mut i = 0;
            while i < n {
                let c = chars[i].1;
                if c == '\'' && i + 1 < n {
                    let rest: String = chars[i + 1..n.min(i + 3)].iter().map(|(_, c)| *c).collect();
                    let take = if rest.starts_with("re")
                        || rest.starts_with("ve")
                        || rest.starts_with("ll")
                    {
                        3
                    } else if rest.starts_with(['s', 't', 'm', 'd']) {
                        2
                    } else {
                        0
                    };
                    if take > 0 {
                        out.push(&text[at(i)..end_of(i + take)]);
                        i += take;
                        continue;
                    }
                }
                let start = i;
                let mut j = i;
                if c == ' ' && i + 1 < n && !chars[i + 1].1.is_whitespace() {
                    j = i + 1;
                }
                let c2 = chars[j].1;
                let mut k = j;
                if c2.is_alphabetic() {
                    while k < n && chars[k].1.is_alphabetic() {
                        k += 1;
                    }
                } else if c2.is_numeric() {
                    while k < n && chars[k].1.is_numeric() {
                        k += 1;
                    }
                } else if !c2.is_whitespace() {
                    while k < n
                        && !chars[k].1.is_whitespace()
                        && !chars[k].1.is_alphabetic()
                        && !chars[k].1.is_numeric()
                    {
                        k += 1;
                    }
                } else {
                    while k < n && chars[k].1.is_whitespace() {
                        k += 1;
                    }
                    // `\s+(?!\S)`: leave the last space for the next word when a word follows.
                    if k < n && k - start > 1 {
                        k -= 1;
                    }
                }
                out.push(&text[at(start)..end_of(k)]);
                i = k;
            }
            out
        }

        struct Sym {
            start: usize,
            end: usize,
            prev: Option<usize>,
            next: Option<usize>,
            alive: bool,
        }

        #[derive(PartialEq, Eq)]
        struct Cand {
            rank: usize,
            left: usize,
            right: usize,
            len: usize,
        }

        impl Ord for Cand {
            /// Min rank first (BinaryHeap is a max-heap, so wrap with `Reverse` at the call site).
            fn cmp(&self, o: &Self) -> Ordering {
                self.rank.cmp(&o.rank).then_with(|| self.left.cmp(&o.left))
            }
        }
        impl PartialOrd for Cand {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }

        /// Merge the symbols of one mapped word and append token ids.
        fn merge_word(
            t: &Tokenizer,
            ranks: &HashMap<String, usize>,
            word: &str,
            out: &mut Vec<u32>,
        ) {
            let mut syms: Vec<Sym> = Vec::new();
            for (i, c) in word.char_indices() {
                let n = syms.len();
                syms.push(Sym {
                    start: i,
                    end: i + c.len_utf8(),
                    prev: n.checked_sub(1),
                    next: Some(n + 1),
                    alive: true,
                });
            }
            if syms.is_empty() {
                return;
            }
            let last = syms.len() - 1;
            syms[last].next = None;

            let mut heap: BinaryHeap<Reverse<Cand>> = BinaryHeap::new();
            let try_add = |syms: &Vec<Sym>,
                           heap: &mut BinaryHeap<Reverse<Cand>>,
                           l: Option<usize>,
                           r: Option<usize>| {
                let (Some(l), Some(r)) = (l, r) else { return };
                let key = format!(
                    "{} {}",
                    &word[syms[l].start..syms[l].end],
                    &word[syms[r].start..syms[r].end]
                );
                if let Some(rank) = ranks.get(&key) {
                    heap.push(Reverse(Cand {
                        rank: *rank,
                        left: l,
                        right: r,
                        len: syms[r].end - syms[l].start,
                    }));
                }
            };
            for i in 0..syms.len().saturating_sub(1) {
                try_add(&syms, &mut heap, Some(i), Some(i + 1));
            }
            while let Some(Reverse(c)) = heap.pop() {
                let valid = syms[c.left].alive
                    && syms[c.right].alive
                    && syms[c.left].next == Some(c.right)
                    && syms[c.right].end - syms[c.left].start == c.len;
                if !valid {
                    continue;
                }
                syms[c.left].end = syms[c.right].end;
                syms[c.right].alive = false;
                let next = syms[c.right].next;
                syms[c.left].next = next;
                if let Some(n) = next {
                    syms[n].prev = Some(c.left);
                }
                let prev = syms[c.left].prev;
                try_add(&syms, &mut heap, prev, Some(c.left));
                try_add(&syms, &mut heap, Some(c.left), next);
            }

            let mut cur = Some(0usize);
            while let Some(i) = cur {
                let piece = &word[syms[i].start..syms[i].end];
                match t.lookup(piece) {
                    Some(id) => out.push(id),
                    None => {
                        // Fall back to single mapped characters, then to the unknown token.
                        for (ci, ch) in piece.char_indices() {
                            let s = &piece[ci..ci + ch.len_utf8()];
                            if let Some(id) = t.lookup(s).or_else(|| t.unknown_id()) {
                                out.push(id);
                            }
                        }
                    }
                }
                cur = syms[i].next;
            }
        }

        /// Encode one text segment (no special tokens inside).
        pub(crate) fn encode(
            t: &Tokenizer,
            ranks: &HashMap<String, usize>,
            text: &str,
            out: &mut Vec<u32>,
        ) {
            let (table, _) = byte_table();
            for word in pre_tokenize(text) {
                let mapped: String = word.bytes().map(|b| table[usize::from(b)]).collect();
                merge_word(t, ranks, &mapped, out);
            }
        }

        /// Decode ids: mapped characters go back to bytes; special tokens contribute their text.
        pub(crate) fn decode(t: &Tokenizer, ids: &[u32]) -> Vec<u8> {
            let (_, rev) = byte_table();
            let mut out = Vec::new();
            for id in ids {
                let Some(text) = t.raw(*id) else { continue };
                match t.kind_of(*id) {
                    TokenKind::Normal | TokenKind::Byte => {
                        for c in text.chars() {
                            match rev.get(&c) {
                                Some(b) => out.push(*b),
                                None => {
                                    let mut buf = [0u8; 4];
                                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                                }
                            }
                        }
                    }
                    TokenKind::UserDefined => out.extend_from_slice(text.as_bytes()),
                    _ => {}
                }
            }
            out
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::tokenizer::api::TokenizerConfig;

            #[test]
            fn byte_table_is_the_standard_gpt2_one() {
                let (table, rev) = byte_table();
                assert_eq!(table[b'A' as usize], 'A');
                assert_eq!(table[b'!' as usize], '!');
                assert_eq!(table[b' ' as usize], '\u{120}'); // Ġ
                assert_eq!(table[b'\n' as usize], '\u{10a}'); // Ċ
                assert_eq!(table[0], '\u{100}');
                assert_eq!(table[0xad], '\u{143}'); // soft hyphen is remapped
                assert_eq!(table[0xe9], 'é');
                assert_eq!(rev.len(), 256);
                for b in 0..=255u8 {
                    assert_eq!(rev[&table[usize::from(b)]], b);
                }
            }

            #[test]
            fn pre_tokenizer_follows_the_gpt2_pattern() {
                assert_eq!(pre_tokenize("Hello world"), ["Hello", " world"]);
                assert_eq!(pre_tokenize("Hello, world!"), ["Hello", ",", " world", "!"]);
                assert_eq!(
                    pre_tokenize("I'm sure we'll go"),
                    ["I", "'m", " sure", " we", "'ll", " go"]
                );
                assert_eq!(pre_tokenize("don't"), ["don", "'t"]);
                assert_eq!(pre_tokenize("it's they've"), ["it", "'s", " they", "'ve"]);
                assert_eq!(pre_tokenize("abc123 def"), ["abc", "123", " def"]);
                assert_eq!(pre_tokenize("x  y"), ["x", " ", " y"]);
                assert_eq!(pre_tokenize("x   "), ["x", "   "]);
                assert_eq!(pre_tokenize("a\n\nb"), ["a", "\n", "\n", "b"]);
                assert_eq!(pre_tokenize("héllo wörld"), ["héllo", " wörld"]);
                assert_eq!(pre_tokenize(" 42"), [" 42"]);
                assert_eq!(pre_tokenize("$$$ ok"), ["$$$", " ok"]);
                assert_eq!(pre_tokenize("'"), ["'"]);
                assert_eq!(pre_tokenize("'x"), ["'", "x"]);
                assert!(pre_tokenize("").is_empty());
            }

            #[test]
            fn pre_tokenizer_never_loses_or_reorders_text() {
                for s in [
                    "Hello,  world!!  \n\t x'y 'z\u{3000}日本語 12.5% — done",
                    "   ",
                    "a  b  c  ",
                    "\n\n\n",
                    "''''",
                    "x'",
                    "😀😀 😀",
                ] {
                    assert_eq!(pre_tokenize(s).concat(), s, "{s:?}");
                }
            }

            /// Byte-level vocabulary: all 256 mapped bytes, then merged pieces.
            fn tokenizer(merges: &[&str], extra: &[&str]) -> Tokenizer {
                let (table, _) = byte_table();
                let mut tokens: Vec<String> = table.iter().map(|c| c.to_string()).collect();
                tokens.extend(extra.iter().map(|s| (*s).to_string()));
                Tokenizer::new(TokenizerConfig {
                    model: "gpt2".into(),
                    tokens,
                    scores: vec![],
                    types: vec![],
                    merges: merges.iter().map(|s| (*s).to_string()).collect(),
                    bos: None,
                    eos: None,
                    unk: None,
                    pad: None,
                    add_bos: false,
                    add_space_prefix: false,
                    pre: Some("gpt2".into()),
                })
                .unwrap()
            }

            fn pieces(t: &Tokenizer, ids: &[u32]) -> Vec<String> {
                ids.iter()
                    .map(|i| t.token(*i).unwrap().to_string())
                    .collect()
            }

            #[test]
            fn lowest_rank_merges_first() {
                // "abcd": rank 0 = "b c", rank 1 = "a bc", rank 2 = "abc d"
                let t = tokenizer(&["b c", "a bc", "abc d"], &["bc", "abc", "abcd"]);
                let ids = t.encode("abcd", false, false);
                assert_eq!(pieces(&t, &ids), ["abcd"]);
                // without the last merge, stops at "abc" + "d"
                let t = tokenizer(&["b c", "a bc"], &["bc", "abc"]);
                assert_eq!(pieces(&t, &t.encode("abcd", false, false)), ["abc", "d"]);
            }

            #[test]
            fn merge_order_decides_ambiguous_pairs() {
                // "aaa": merges "a a" -> "aa"; then only one "aa" + "a" remain
                let t = tokenizer(&["a a"], &["aa"]);
                assert_eq!(pieces(&t, &t.encode("aaa", false, false)), ["aa", "a"]);
                assert_eq!(pieces(&t, &t.encode("aaaa", false, false)), ["aa", "aa"]);
            }

            #[test]
            fn spaces_use_the_mapped_character() {
                let t = tokenizer(&["\u{120} t", "\u{120}t h"], &["\u{120}t", "\u{120}th"]);
                let ids = t.encode("a th", false, false);
                assert_eq!(pieces(&t, &ids), ["a", "\u{120}th"]);
                assert_eq!(t.decode(&ids), "a th");
            }

            #[test]
            fn round_trips_any_utf8_with_a_full_byte_vocabulary() {
                let t = tokenizer(&["t h", "th e"], &["th", "the"]);
                for text in [
                    "the cat",
                    "The  CAT\n",
                    "héllo wörld",
                    "日本語のテスト",
                    "emoji 😀 ok",
                    "a\u{0}b",
                    "tabs\tand\r\nnewlines",
                    "",
                    "  ",
                    "x'y 're",
                ] {
                    let ids = t.encode(text, false, false);
                    assert_eq!(t.decode(&ids), text, "text {text:?}");
                }
            }

            #[test]
            fn missing_vocabulary_entries_fall_back_to_characters() {
                // vocabulary lacks "ab" although a merge produces it
                let t = tokenizer(&["a b"], &[]);
                assert_eq!(pieces(&t, &t.encode("ab", false, false)), ["a", "b"]);
            }

            #[test]
            fn special_tokens_in_text() {
                let (table, _) = byte_table();
                let mut tokens: Vec<String> = table.iter().map(|c| c.to_string()).collect();
                tokens.push("<|end|>".into());
                let mut types = vec![1; 256];
                types.push(4);
                let t = Tokenizer::new(TokenizerConfig {
                    model: "gpt2".into(),
                    tokens,
                    scores: vec![],
                    types,
                    merges: vec![],
                    bos: None,
                    eos: Some(256),
                    unk: None,
                    pad: None,
                    add_bos: false,
                    add_space_prefix: false,
                    pre: None,
                })
                .unwrap();
                let ids = t.encode("hi<|end|>yo", false, true);
                assert_eq!(ids.iter().filter(|i| **i == 256).count(), 1);
                assert_eq!(t.decode(&ids), "hi<|end|>yo");
                let plain = t.encode("hi<|end|>yo", false, false);
                assert!(!plain.contains(&256));
                assert_eq!(t.decode(&plain), "hi<|end|>yo");
            }

            #[test]
            fn long_single_word_is_fast() {
                let t = tokenizer(&["a a", "aa aa"], &["aa", "aaaa"]);
                let text = "a".repeat(100_000);
                let ids = t.encode(&text, false, false);
                assert_eq!(t.decode(&ids), text);
                assert_eq!(ids.len(), 25_000);
            }

            #[test]
            fn decode_ignores_unknown_ids() {
                let t = tokenizer(&[], &[]);
                // ids 0..256 are the mapped bytes in byte order: 97 is "a"
                assert_eq!(t.decode(&[97, 99_999, 98]), "ab");
                assert_eq!(t.decode(&[]), "");
            }
        }
    }
    mod spm {
        //! SentencePiece-style encoding: greedy score-ordered pair merging with byte fallback.

        use crate::tokenizer::api::{TokenKind, Tokenizer};
        use std::cmp::Ordering;
        use std::collections::BinaryHeap;

        const SPACE: char = '\u{2581}'; // ▁

        struct Sym {
            start: usize,
            end: usize,
            prev: Option<usize>,
            next: Option<usize>,
            alive: bool,
        }

        struct Bigram {
            score: f32,
            left: usize,
            right: usize,
            len: usize,
        }

        impl PartialEq for Bigram {
            fn eq(&self, o: &Self) -> bool {
                self.cmp(o) == Ordering::Equal
            }
        }
        impl Eq for Bigram {}
        impl PartialOrd for Bigram {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Bigram {
            /// Max-heap: higher score first, then the leftmost pair.
            fn cmp(&self, o: &Self) -> Ordering {
                self.score
                    .total_cmp(&o.score)
                    .then_with(|| o.left.cmp(&self.left))
            }
        }

        fn mergeable(t: &Tokenizer, s: &str) -> Option<u32> {
            let id = t.lookup(s)?;
            (t.kind_of(id) == TokenKind::Normal).then_some(id)
        }

        /// Encode one text segment, appending ids to `out`.
        pub(crate) fn encode(t: &Tokenizer, text: &str, prefix: bool, out: &mut Vec<u32>) {
            let mut norm = String::with_capacity(text.len() + 4);
            if prefix {
                norm.push(SPACE);
            }
            for c in text.chars() {
                norm.push(if c == ' ' { SPACE } else { c });
            }

            let mut syms: Vec<Sym> = Vec::new();
            for (i, c) in norm.char_indices() {
                let n = syms.len();
                syms.push(Sym {
                    start: i,
                    end: i + c.len_utf8(),
                    prev: n.checked_sub(1),
                    next: Some(n + 1),
                    alive: true,
                });
            }
            if syms.is_empty() {
                return;
            }
            let last = syms.len() - 1;
            syms[last].next = None;

            let mut heap = BinaryHeap::new();
            let try_add = |syms: &Vec<Sym>,
                           heap: &mut BinaryHeap<Bigram>,
                           left: Option<usize>,
                           right: Option<usize>| {
                let (Some(l), Some(r)) = (left, right) else {
                    return;
                };
                let (start, end) = (syms[l].start, syms[r].end);
                if let Some(id) = mergeable(t, &norm[start..end]) {
                    heap.push(Bigram {
                        score: t.score_of(id),
                        left: l,
                        right: r,
                        len: end - start,
                    });
                }
            };
            for i in 0..syms.len().saturating_sub(1) {
                try_add(&syms, &mut heap, Some(i), Some(i + 1));
            }

            while let Some(b) = heap.pop() {
                let valid = syms[b.left].alive
                    && syms[b.right].alive
                    && syms[b.left].next == Some(b.right)
                    && syms[b.right].end - syms[b.left].start == b.len;
                if !valid {
                    continue;
                }
                syms[b.left].end = syms[b.right].end;
                syms[b.right].alive = false;
                let next = syms[b.right].next;
                syms[b.left].next = next;
                if let Some(n) = next {
                    syms[n].prev = Some(b.left);
                }
                let prev = syms[b.left].prev;
                try_add(&syms, &mut heap, prev, Some(b.left));
                try_add(&syms, &mut heap, Some(b.left), next);
            }

            let mut cur = Some(0usize);
            while let Some(i) = cur {
                let piece = &norm[syms[i].start..syms[i].end];
                match mergeable(t, piece).or_else(|| {
                    t.lookup(piece)
                        .filter(|id| t.kind_of(*id) != TokenKind::Byte)
                }) {
                    Some(id) => out.push(id),
                    None => {
                        for b in piece.bytes() {
                            if let Some(id) = t.byte_id(b).or_else(|| t.unknown_id()) {
                                out.push(id);
                            }
                        }
                    }
                }
                cur = syms[i].next;
            }
        }

        /// Decode ids: byte tokens give their byte, normal tokens their text with `▁` as a space,
        /// control/unknown/unused tokens nothing. A single leading space (the dummy prefix) is removed.
        pub(crate) fn decode(t: &Tokenizer, ids: &[u32]) -> Vec<u8> {
            let mut out: Vec<u8> = Vec::new();
            for id in ids {
                let Some(text) = t.raw(*id) else { continue };
                match t.kind_of(*id) {
                    TokenKind::Byte => {
                        if let Some(b) = crate::tokenizer::api::parse_byte_token(text) {
                            out.push(b);
                        }
                    }
                    TokenKind::Normal | TokenKind::UserDefined => {
                        for c in text.chars() {
                            let c = if c == SPACE { ' ' } else { c };
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                    _ => {}
                }
            }
            if t.space_prefix() && out.first() == Some(&b' ') {
                out.remove(0);
            }
            out
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::tokenizer::api::TokenizerConfig;

            /// Vocabulary: specials, all 256 byte tokens, then pieces with scores.
            fn tokenizer(pieces: &[(&str, f32)], add_prefix: bool) -> Tokenizer {
                let mut tokens = vec!["<unk>".to_string(), "<s>".to_string(), "</s>".to_string()];
                let mut types = vec![2, 3, 3];
                let mut scores = vec![0.0, 0.0, 0.0];
                for b in 0..=255u8 {
                    tokens.push(format!("<0x{b:02X}>"));
                    types.push(6);
                    scores.push(0.0);
                }
                for (p, s) in pieces {
                    tokens.push((*p).to_string());
                    types.push(1);
                    scores.push(*s);
                }
                Tokenizer::new(TokenizerConfig {
                    model: "llama".into(),
                    tokens,
                    scores,
                    types,
                    merges: vec![],
                    bos: Some(1),
                    eos: Some(2),
                    unk: Some(0),
                    pad: None,
                    add_bos: true,
                    add_space_prefix: add_prefix,
                    pre: None,
                })
                .unwrap()
            }

            fn pieces(t: &Tokenizer, ids: &[u32]) -> Vec<String> {
                ids.iter()
                    .map(|i| t.token(*i).unwrap().to_string())
                    .collect()
            }

            #[test]
            fn merges_by_score_not_by_position() {
                // "abc": ab (score 2) beats bc (score 1) even though both are available
                let t = tokenizer(
                    &[
                        ("a", 0.0),
                        ("b", 0.0),
                        ("c", 0.0),
                        ("ab", 2.0),
                        ("bc", 1.0),
                        ("xyz", -1.0),
                    ],
                    false,
                );
                assert_eq!(pieces(&t, &t.encode("abc", false, false)), ["ab", "c"]);
                // equal scores: leftmost pair wins
                let t = tokenizer(
                    &[("a", 0.0), ("b", 0.0), ("c", 0.0), ("ab", 1.0), ("bc", 1.0)],
                    false,
                );
                assert_eq!(pieces(&t, &t.encode("abc", false, false)), ["ab", "c"]);
            }

            #[test]
            fn merges_continue_until_no_pair_is_in_the_vocabulary() {
                let t = tokenizer(
                    &[
                        ("a", 0.0),
                        ("b", 0.0),
                        ("c", 0.0),
                        ("ab", 2.0),
                        ("abc", 3.0),
                    ],
                    false,
                );
                assert_eq!(pieces(&t, &t.encode("abc", false, false)), ["abc"]);
                assert_eq!(pieces(&t, &t.encode("abcab", false, false)), ["abc", "ab"]);
            }

            #[test]
            fn dummy_prefix_and_space_marker() {
                let t = tokenizer(
                    &[
                        ("\u{2581}", 0.0),
                        ("h", 0.0),
                        ("i", 0.0),
                        ("\u{2581}h", 1.0),
                        ("\u{2581}hi", 2.0),
                    ],
                    true,
                );
                assert_eq!(pieces(&t, &t.encode("hi", false, false)), ["\u{2581}hi"]);
                assert_eq!(
                    pieces(&t, &t.encode("hi hi", false, false)),
                    ["\u{2581}hi", "\u{2581}hi"]
                );
                let t = tokenizer(
                    &[
                        ("\u{2581}", 0.0),
                        ("h", 0.0),
                        ("i", 0.0),
                        ("\u{2581}h", 1.0),
                        ("hi", 2.0),
                    ],
                    false,
                );
                assert_eq!(pieces(&t, &t.encode("hi", false, false)), ["hi"]);
            }

            #[test]
            fn bos_is_prepended_on_request() {
                let t = tokenizer(&[("a", 0.0)], false);
                assert_eq!(t.encode("a", true, false)[0], 1);
                assert_ne!(t.encode("a", false, false)[0], 1);
                assert_eq!(t.encode_default("a")[0], 1);
                assert_eq!(t.encode("", true, false), vec![1]);
                assert!(t.encode("", false, false).is_empty());
            }

            #[test]
            fn unknown_characters_fall_back_to_bytes() {
                let t = tokenizer(&[("a", 0.0)], false);
                let ids = t.encode("é", false, false); // 0xC3 0xA9
                assert_eq!(pieces(&t, &ids), ["<0xC3>", "<0xA9>"]);
                assert_eq!(t.decode(&ids), "é");
            }

            #[test]
            fn without_byte_tokens_unknown_becomes_unk() {
                let t = Tokenizer::new(TokenizerConfig {
                    model: "llama".into(),
                    tokens: vec!["<unk>".into(), "a".into()],
                    scores: vec![],
                    types: vec![2, 1],
                    merges: vec![],
                    bos: None,
                    eos: None,
                    unk: Some(0),
                    pad: None,
                    add_bos: false,
                    add_space_prefix: false,
                    pre: None,
                })
                .unwrap();
                // "é" is two bytes and there are no byte tokens: one unk per byte
                assert_eq!(t.encode("é", false, false), vec![0, 0]);
                assert_eq!(t.encode("aéa", false, false), vec![1, 0, 0, 1]);
            }

            #[test]
            fn decode_round_trips_arbitrary_text() {
                let t = tokenizer(
                    &[
                        ("\u{2581}", 0.0),
                        ("t", 0.0),
                        ("h", 0.0),
                        ("e", 0.0),
                        ("\u{2581}t", 1.0),
                        ("th", 2.0),
                        ("the", 3.0),
                    ],
                    true,
                );
                for text in [
                    "the",
                    "the the",
                    " the",
                    "  the  ",
                    "héllo wörld",
                    "日本語 テスト",
                    "a\nb\tc",
                    "emoji 😀 ok",
                    "",
                ] {
                    let ids = t.encode(text, false, false);
                    assert_eq!(t.decode(&ids), text, "text {text:?}");
                }
            }

            #[test]
            fn decode_skips_control_tokens_and_handles_bad_ids() {
                let t = tokenizer(&[("a", 0.0)], false);
                let a = t.id_of("a").unwrap();
                assert_eq!(t.decode(&[1, a, 2]), "a");
                assert_eq!(t.decode(&[a, 99_999, a]), "aa");
                assert_eq!(t.decode(&[]), "");
            }

            #[test]
            fn special_tokens_are_parsed_only_on_request() {
                let mut tokens = vec![
                    "<unk>".to_string(),
                    "<s>".to_string(),
                    "<|im|>".to_string(),
                    "a".to_string(),
                ];
                let types = vec![2, 3, 4, 1];
                tokens.extend((0..=255u8).map(|b| format!("<0x{b:02X}>")));
                let mut types = types;
                types.extend(std::iter::repeat_n(6, 256));
                let t = Tokenizer::new(TokenizerConfig {
                    model: "llama".into(),
                    tokens,
                    scores: vec![],
                    types,
                    merges: vec![],
                    bos: Some(1),
                    eos: None,
                    unk: Some(0),
                    pad: None,
                    add_bos: false,
                    add_space_prefix: false,
                    pre: None,
                })
                .unwrap();
                assert_eq!(t.encode("a<|im|>a", false, true), vec![3, 2, 3]);
                // without parsing, the text is ordinary characters (via bytes here), never the special id
                assert!(!t.encode("a<|im|>a", false, false).contains(&2));
            }

            #[test]
            fn long_input_is_linearithmic_enough() {
                let t = tokenizer(&[("a", 0.0), ("aa", 1.0), ("aaaa", 2.0)], false);
                let text = "a".repeat(200_000);
                let ids = t.encode(&text, false, false);
                assert_eq!(t.decode(&ids), text);
            }
        }
    }

    pub use api::{TokenKind, Tokenizer, TokenizerConfig, TokenizerError};
}
pub mod weights {
    //! MOULI model storage layer.
    //!
    //! * [`index`]: named tensor table with overlap and bounds checks.
    //! * [`manifest`]: text manifest (`name | offset | bytes | dtype | rank | d0,d1,..`).
    //! * [`source`]: random-access byte sources (memory, file) and a bounded stream.
    //! * [`block`]: block-device (NVMe) sources, including submit/poll queues.
    //! * [`binding`]: a cursor that can only read inside one tensor's bytes.
    //! * [`cache`]: byte-budgeted LRU cache of fully loaded tensors.
    //! * [`experts`]: tiered (fast / RAM LRU / disk) mixture-of-experts store and routing.
    //!
    //! Ported from the retired C core (`aeon_model_*`, `aeon_tensor_binding`,
    //! `aeon_tensor_layout`, `aeon_expert_store`).
    #![forbid(unsafe_code)]

    pub mod binding {
        //! A reader confined to one tensor's bytes.

        use crate::weights::index::{ModelIndex, TensorMeta};
        use crate::weights::source::{ModelSource, ModelStream};
        use crate::weights::ModelError;

        /// Cursor over a single tensor inside a model stream.
        #[derive(Debug)]
        pub struct TensorReader<'a, S> {
            stream: &'a mut ModelStream<S>,
            meta: &'a TensorMeta,
            position: u64,
        }

        impl<'a, S: ModelSource> TensorReader<'a, S> {
            /// Bind `name` from `index`; the index and the tensor layout are validated.
            pub fn bind(
                stream: &'a mut ModelStream<S>,
                index: &'a ModelIndex,
                name: &str,
            ) -> Result<Self, ModelError> {
                index.validate(stream.total())?;
                let meta = index.find(name).ok_or(ModelError::NotFound)?;
                meta.validate_layout()?;
                Ok(Self {
                    stream,
                    meta,
                    position: 0,
                })
            }

            /// Tensor metadata.
            #[must_use]
            pub fn meta(&self) -> &TensorMeta {
                self.meta
            }

            /// Move inside the tensor (may equal its length).
            pub fn seek(&mut self, offset: u64) -> Result<(), ModelError> {
                if offset > self.meta.bytes {
                    return Err(ModelError::OutOfBounds);
                }
                self.position = offset;
                Ok(())
            }

            /// Fill `buf` from the current position and advance.
            pub fn read(&mut self, buf: &mut [u8]) -> Result<(), ModelError> {
                let len = buf.len() as u64;
                if len == 0 || len > self.remaining() {
                    return Err(ModelError::OutOfBounds);
                }
                self.stream.seek(self.meta.offset + self.position)?;
                self.stream.read(buf)?;
                self.position += len;
                Ok(())
            }

            /// Read the whole tensor into a new buffer.
            pub fn read_all(&mut self) -> Result<Vec<u8>, ModelError> {
                let n = usize::try_from(self.meta.bytes).map_err(|_| ModelError::OutOfBounds)?;
                let mut v = vec![0u8; n];
                self.seek(0)?;
                self.read(&mut v)?;
                Ok(v)
            }

            /// Bytes left in the tensor.
            #[must_use]
            pub fn remaining(&self) -> u64 {
                self.meta.bytes - self.position
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::weights::manifest;

            #[test]
            fn confined_reads() {
                let data: Vec<u8> = (0..12).collect();
                let ix = manifest::parse("a | 0 | 4 | I8 | 1 | 4\nb | 4 | 8 | F32 | 1 | 2\n", 12)
                    .unwrap();
                let mut s = ModelStream::new(data).unwrap();
                let mut r = TensorReader::bind(&mut s, &ix, "b").unwrap();
                let mut buf = [0u8; 3];
                r.read(&mut buf).unwrap();
                assert_eq!(buf, [4, 5, 6]);
                assert_eq!(r.remaining(), 5);
                let mut big = [0u8; 6];
                assert_eq!(r.read(&mut big), Err(ModelError::OutOfBounds));
                assert_eq!(r.read_all().unwrap(), (4..12).collect::<Vec<u8>>());
                assert!(matches!(
                    TensorReader::bind(&mut s, &ix, "zz"),
                    Err(ModelError::NotFound)
                ));
            }
        }
    }
    pub mod block {
        //! Block-device model sources (NVMe namespaces and similar).
        //!
        //! A device reads whole logical blocks (LBAs). [`BlockSource`] turns any
        //! [`BlockDevice`] into a byte-addressed [`ModelSource`], using a bounce
        //! buffer for unaligned ranges. [`QueuedDevice`] is the submit/poll shape of
        //! a hardware queue pair; [`Polled`] adapts it to a blocking [`BlockDevice`].

        use crate::weights::source::ModelSource;
        use crate::weights::ModelError;

        /// A device addressed in fixed-size logical blocks.
        pub trait BlockDevice {
            /// Bytes per block (power of two).
            fn block_bytes(&self) -> u32;
            /// Number of blocks.
            fn blocks(&self) -> u64;
            /// Read `buf.len() / block_bytes` blocks starting at `lba`.
            fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), ModelError>;
        }

        /// Byte-addressed view of a block device.
        #[derive(Debug)]
        pub struct BlockSource<D> {
            dev: D,
            len: u64,
        }

        impl<D: BlockDevice> BlockSource<D> {
            /// Wrap a device; rejects zero or non-power-of-two block sizes.
            pub fn new(dev: D) -> Result<Self, ModelError> {
                let bs = dev.block_bytes();
                if bs == 0 || !bs.is_power_of_two() || dev.blocks() == 0 {
                    return Err(ModelError::BadConfig);
                }
                let len = dev
                    .blocks()
                    .checked_mul(u64::from(bs))
                    .ok_or(ModelError::BadConfig)?;
                Ok(Self { dev, len })
            }

            /// Underlying device.
            pub fn device(&self) -> &D {
                &self.dev
            }
        }

        impl<D: BlockDevice> ModelSource for BlockSource<D> {
            fn len(&self) -> u64 {
                self.len
            }

            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ModelError> {
                let n = buf.len() as u64;
                let end = offset.checked_add(n).ok_or(ModelError::OutOfBounds)?;
                if end > self.len {
                    return Err(ModelError::OutOfBounds);
                }
                if buf.is_empty() {
                    return Ok(());
                }
                let bs = u64::from(self.dev.block_bytes());
                if offset.is_multiple_of(bs) && n.is_multiple_of(bs) {
                    return self.dev.read_blocks(offset / bs, buf);
                }
                let first = offset / bs;
                let last = (end - 1) / bs;
                let span = usize::try_from((last - first + 1) * bs)
                    .map_err(|_| ModelError::OutOfBounds)?;
                let mut bounce = vec![0u8; span];
                self.dev.read_blocks(first, &mut bounce)?;
                let skip =
                    usize::try_from(offset - first * bs).map_err(|_| ModelError::OutOfBounds)?;
                buf.copy_from_slice(&bounce[skip..skip + buf.len()]);
                Ok(())
            }
        }

        /// Outcome of polling a submitted command.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Poll {
            /// Still running.
            Pending,
            /// Completed successfully.
            Done,
            /// Completed with an error.
            Failed,
        }

        /// Submit/poll interface of a hardware queue.
        pub trait QueuedDevice {
            /// Bytes per block.
            fn block_bytes(&self) -> u32;
            /// Number of blocks.
            fn blocks(&self) -> u64;
            /// Submit a read; returns a command id.
            fn submit_read(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<u16, ModelError>;
            /// Poll command `cid`.
            fn poll(&self, cid: u16) -> Result<Poll, ModelError>;
        }

        /// Blocking adapter: submit, then poll up to `max_polls` times.
        #[derive(Debug)]
        pub struct Polled<Q> {
            q: Q,
            max_polls: u32,
        }

        impl<Q: QueuedDevice> Polled<Q> {
            /// Wrap a queue.
            pub fn new(q: Q, max_polls: u32) -> Self {
                Self {
                    q,
                    max_polls: max_polls.max(1),
                }
            }
        }

        impl<Q: QueuedDevice> BlockDevice for Polled<Q> {
            fn block_bytes(&self) -> u32 {
                self.q.block_bytes()
            }

            fn blocks(&self) -> u64 {
                self.q.blocks()
            }

            fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), ModelError> {
                let bs = self.q.block_bytes() as usize;
                if bs == 0 || buf.is_empty() || !buf.len().is_multiple_of(bs) {
                    return Err(ModelError::BadConfig);
                }
                let count = u32::try_from(buf.len() / bs).map_err(|_| ModelError::BadConfig)?;
                let cid = self.q.submit_read(lba, count, buf)?;
                for _ in 0..self.max_polls {
                    match self.q.poll(cid)? {
                        Poll::Pending => std::hint::spin_loop(),
                        Poll::Done => return Ok(()),
                        Poll::Failed => {
                            return Err(ModelError::Io(format!("read cid {cid} failed")))
                        }
                    }
                }
                Err(ModelError::Io(format!("read cid {cid} timed out")))
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use std::cell::Cell;

            struct Mem {
                data: Vec<u8>,
                bs: u32,
            }

            impl BlockDevice for Mem {
                fn block_bytes(&self) -> u32 {
                    self.bs
                }
                fn blocks(&self) -> u64 {
                    (self.data.len() / self.bs as usize) as u64
                }
                fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<(), ModelError> {
                    assert_eq!(buf.len() % self.bs as usize, 0, "unaligned device read");
                    let s = lba as usize * self.bs as usize;
                    buf.copy_from_slice(&self.data[s..s + buf.len()]);
                    Ok(())
                }
            }

            fn mem() -> Mem {
                Mem {
                    data: (0..64u8).collect(),
                    bs: 16,
                }
            }

            #[test]
            fn aligned_and_unaligned_reads() {
                let s = BlockSource::new(mem()).unwrap();
                assert_eq!(ModelSource::len(&s), 64);
                let mut a = [0u8; 16];
                s.read_at(16, &mut a).unwrap();
                assert_eq!(a[0], 16);
                let mut b = [0u8; 5];
                s.read_at(14, &mut b).unwrap();
                assert_eq!(b, [14, 15, 16, 17, 18]);
                assert_eq!(s.read_at(62, &mut b), Err(ModelError::OutOfBounds));
            }

            struct Queue {
                inner: Mem,
                pending: Cell<u32>,
                fail: bool,
            }

            impl QueuedDevice for Queue {
                fn block_bytes(&self) -> u32 {
                    self.inner.bs
                }
                fn blocks(&self) -> u64 {
                    self.inner.blocks()
                }
                fn submit_read(
                    &self,
                    lba: u64,
                    _count: u32,
                    buf: &mut [u8],
                ) -> Result<u16, ModelError> {
                    self.inner.read_blocks(lba, buf)?;
                    self.pending.set(2);
                    Ok(7)
                }
                fn poll(&self, cid: u16) -> Result<Poll, ModelError> {
                    assert_eq!(cid, 7);
                    let left = self.pending.get();
                    if left > 0 {
                        self.pending.set(left - 1);
                        return Ok(Poll::Pending);
                    }
                    Ok(if self.fail { Poll::Failed } else { Poll::Done })
                }
            }

            #[test]
            fn polled_queue() {
                let q = Queue {
                    inner: mem(),
                    pending: Cell::new(0),
                    fail: false,
                };
                let s = BlockSource::new(Polled::new(q, 8)).unwrap();
                let mut b = [0u8; 3];
                s.read_at(33, &mut b).unwrap();
                assert_eq!(b, [33, 34, 35]);

                let slow = Queue {
                    inner: mem(),
                    pending: Cell::new(0),
                    fail: false,
                };
                let d = Polled::new(slow, 1);
                assert!(d.read_blocks(0, &mut [0u8; 16]).is_err());

                let bad = Queue {
                    inner: mem(),
                    pending: Cell::new(0),
                    fail: true,
                };
                let d = Polled::new(bad, 8);
                assert!(d.read_blocks(0, &mut [0u8; 16]).is_err());
            }
        }
    }
    pub mod cache {
        //! Byte-budgeted LRU cache of materialised (fully loaded) tensors.
        //!
        //! Entries are keyed by tensor name and own their bytes. A load that hits
        //! the cache with a stale size is counted, dropped and re-read.

        use crate::weights::binding::TensorReader;
        use crate::weights::index::ModelIndex;
        use crate::weights::source::{ModelSource, ModelStream};
        use crate::weights::ModelError;
        use std::sync::Arc;

        /// Counters.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct CacheStats {
            /// Lookups served from cache.
            pub hits: u64,
            /// Lookups that missed.
            pub misses: u64,
            /// Entries inserted.
            pub inserts: u64,
            /// Entries evicted to make room.
            pub evictions: u64,
            /// Hits dropped because they no longer matched the tensor.
            pub stale: u64,
            /// Bytes resident now.
            pub bytes_used: usize,
            /// Highest `bytes_used` seen.
            pub peak_bytes: usize,
        }

        #[derive(Debug)]
        struct Entry {
            name: String,
            data: Arc<[u8]>,
            age: u64,
        }

        /// LRU tensor cache with an entry limit and a byte budget.
        #[derive(Debug)]
        pub struct TensorCache {
            entries: Vec<Entry>,
            max_entries: usize,
            budget: usize,
            clock: u64,
            stats: CacheStats,
        }

        impl TensorCache {
            /// Cache holding at most `max_entries` tensors and `budget` bytes.
            pub fn new(max_entries: usize, budget: usize) -> Result<Self, ModelError> {
                if max_entries == 0 || budget == 0 {
                    return Err(ModelError::BadConfig);
                }
                Ok(Self {
                    entries: Vec::with_capacity(max_entries),
                    max_entries,
                    budget,
                    clock: 0,
                    stats: CacheStats::default(),
                })
            }

            fn tick(&mut self) -> u64 {
                self.clock += 1;
                self.clock
            }

            fn remove_at(&mut self, i: usize) -> Entry {
                let e = self.entries.swap_remove(i);
                self.stats.bytes_used -= e.data.len();
                e
            }

            fn lru(&self) -> Option<usize> {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.age)
                    .map(|(i, _)| i)
            }

            /// Look up `name`, refreshing its age.
            pub fn get(&mut self, name: &str) -> Option<Arc<[u8]>> {
                match self.entries.iter().position(|e| e.name == name) {
                    Some(i) => {
                        self.stats.hits += 1;
                        let age = self.tick();
                        self.entries[i].age = age;
                        Some(Arc::clone(&self.entries[i].data))
                    }
                    None => {
                        self.stats.misses += 1;
                        None
                    }
                }
            }

            /// Insert, evicting least-recently-used entries until it fits.
            pub fn put(&mut self, name: &str, data: Arc<[u8]>) -> Result<(), ModelError> {
                if data.is_empty() || data.len() > self.budget {
                    return Err(ModelError::BadConfig);
                }
                if let Some(i) = self.entries.iter().position(|e| e.name == name) {
                    self.remove_at(i);
                }
                while self.entries.len() >= self.max_entries
                    || self.stats.bytes_used > self.budget - data.len()
                {
                    let i = self.lru().ok_or(ModelError::NoSlot)?;
                    self.remove_at(i);
                    self.stats.evictions += 1;
                }
                let age = self.tick();
                self.stats.bytes_used += data.len();
                self.stats.peak_bytes = self.stats.peak_bytes.max(self.stats.bytes_used);
                self.stats.inserts += 1;
                self.entries.push(Entry {
                    name: name.to_owned(),
                    data,
                    age,
                });
                Ok(())
            }

            /// Check the cache's internal invariants (the Rust form of the C `aeon_tensor_cache_validate`):
            /// resident bytes within budget, entry count within the limit, every entry non-empty with a
            /// distinct non-zero age and a unique name, and `bytes_used` equal to the sum of entry sizes.
            ///
            /// Meant for tests and debug assertions; a healthy cache always passes.
            pub fn validate(&self) -> Result<(), ModelError> {
                if self.entries.len() > self.max_entries {
                    return Err(ModelError::Inconsistent("more entries than the limit"));
                }
                if self.stats.bytes_used > self.budget {
                    return Err(ModelError::Inconsistent("bytes used exceed the budget"));
                }
                let mut resident = 0usize;
                for (i, e) in self.entries.iter().enumerate() {
                    if e.data.is_empty() {
                        return Err(ModelError::Inconsistent("empty entry"));
                    }
                    if e.age == 0 || e.age > self.clock {
                        return Err(ModelError::Inconsistent("entry age out of range"));
                    }
                    resident = resident
                        .checked_add(e.data.len())
                        .ok_or(ModelError::Inconsistent("resident bytes overflow"))?;
                    for other in &self.entries[i + 1..] {
                        if other.name == e.name {
                            return Err(ModelError::Inconsistent("duplicate entry name"));
                        }
                        if other.age == e.age {
                            return Err(ModelError::Inconsistent("duplicate entry age"));
                        }
                    }
                }
                if resident != self.stats.bytes_used {
                    return Err(ModelError::Inconsistent("bytes used do not match entries"));
                }
                Ok(())
            }

            /// Drop one entry. Returns whether it was present.
            pub fn invalidate(&mut self, name: &str) -> bool {
                match self.entries.iter().position(|e| e.name == name) {
                    Some(i) => {
                        self.remove_at(i);
                        true
                    }
                    None => false,
                }
            }

            /// Drop everything (stats other than `bytes_used` are kept).
            pub fn invalidate_all(&mut self) {
                self.entries.clear();
                self.stats.bytes_used = 0;
            }

            /// Change the budget; fails if current residency exceeds it.
            pub fn set_budget(&mut self, budget: usize) -> Result<(), ModelError> {
                if budget == 0 || budget < self.stats.bytes_used {
                    return Err(ModelError::BadConfig);
                }
                self.budget = budget;
                Ok(())
            }

            /// Current budget.
            #[must_use]
            pub fn budget(&self) -> usize {
                self.budget
            }

            /// Counters.
            #[must_use]
            pub fn stats(&self) -> CacheStats {
                self.stats
            }

            /// Return the tensor's bytes from cache, or read and cache them.
            pub fn materialize<S: ModelSource>(
                &mut self,
                stream: &mut ModelStream<S>,
                index: &ModelIndex,
                name: &str,
            ) -> Result<Arc<[u8]>, ModelError> {
                let meta = index.find(name).ok_or(ModelError::NotFound)?;
                if let Some(hit) = self.get(name) {
                    if hit.len() as u64 == meta.bytes {
                        return Ok(hit);
                    }
                    self.stats.stale += 1;
                    self.invalidate(name);
                }
                let data: Arc<[u8]> = TensorReader::bind(stream, index, name)?.read_all()?.into();
                self.put(name, Arc::clone(&data))?;
                Ok(data)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::weights::manifest;

            fn bytes(n: usize) -> Arc<[u8]> {
                vec![1u8; n].into()
            }

            #[test]
            fn lru_by_count_and_budget() {
                let mut c = TensorCache::new(2, 10).unwrap();
                c.put("a", bytes(4)).unwrap();
                c.put("b", bytes(4)).unwrap();
                assert!(c.get("a").is_some());
                c.put("c", bytes(4)).unwrap(); // evicts b (count)
                assert!(c.get("b").is_none());
                c.put("d", bytes(8)).unwrap(); // evicts a and c (budget)
                assert_eq!(c.stats().bytes_used, 8);
                assert_eq!(c.stats().evictions, 3);
                assert_eq!(c.stats().peak_bytes, 8);
                assert!(c.put("e", bytes(11)).is_err());
                assert_eq!(c.set_budget(7), Err(ModelError::BadConfig));
            }

            #[test]
            fn validate_passes_through_normal_use() {
                let mut c = TensorCache::new(3, 12).unwrap();
                c.validate().unwrap();
                for (n, size) in [("a", 4), ("b", 4), ("c", 4), ("d", 8), ("a", 3)] {
                    c.put(n, bytes(size)).unwrap();
                    c.validate().unwrap();
                    let _ = c.get("b");
                    c.validate().unwrap();
                }
                c.invalidate("a");
                c.validate().unwrap();
                c.invalidate_all();
                c.validate().unwrap();
                assert_eq!(c.stats().bytes_used, 0);
            }

            #[test]
            fn validate_detects_corruption() {
                let mut c = TensorCache::new(4, 100).unwrap();
                c.put("a", bytes(10)).unwrap();
                c.put("b", bytes(10)).unwrap();

                let mut bad = TensorCache::new(4, 100).unwrap();
                bad.put("a", bytes(10)).unwrap();
                bad.stats.bytes_used = 11;
                assert_eq!(
                    bad.validate(),
                    Err(ModelError::Inconsistent("bytes used do not match entries"))
                );
                bad.stats.bytes_used = 101;
                assert_eq!(
                    bad.validate(),
                    Err(ModelError::Inconsistent("bytes used exceed the budget"))
                );

                c.entries[1].name = "a".into();
                assert_eq!(
                    c.validate(),
                    Err(ModelError::Inconsistent("duplicate entry name"))
                );
                c.entries[1].name = "b".into();
                c.entries[1].age = c.entries[0].age;
                assert_eq!(
                    c.validate(),
                    Err(ModelError::Inconsistent("duplicate entry age"))
                );
                c.entries[1].age = 0;
                assert_eq!(
                    c.validate(),
                    Err(ModelError::Inconsistent("entry age out of range"))
                );
                c.entries[1].age = c.clock + 1;
                assert_eq!(
                    c.validate(),
                    Err(ModelError::Inconsistent("entry age out of range"))
                );
                c.entries[1].age = 2;
                c.entries[1].data = Arc::from(Vec::new());
                assert_eq!(c.validate(), Err(ModelError::Inconsistent("empty entry")));
                c.entries.push(Entry {
                    name: "x".into(),
                    data: bytes(1),
                    age: 9,
                });
                c.entries.push(Entry {
                    name: "y".into(),
                    data: bytes(1),
                    age: 10,
                });
                c.entries.push(Entry {
                    name: "z".into(),
                    data: bytes(1),
                    age: 11,
                });
                assert_eq!(
                    c.validate(),
                    Err(ModelError::Inconsistent("more entries than the limit"))
                );
            }

            #[test]
            fn inconsistent_error_displays_its_reason() {
                assert_eq!(
                    ModelError::Inconsistent("x").to_string(),
                    "inconsistent state: x"
                );
            }

            #[test]
            fn materialize_reads_once() {
                let ix = manifest::parse("w | 2 | 4 | I8 | 1 | 4\n", 8).unwrap();
                let mut s = ModelStream::new((0u8..8).collect::<Vec<u8>>()).unwrap();
                let mut c = TensorCache::new(4, 64).unwrap();
                let a = c.materialize(&mut s, &ix, "w").unwrap();
                assert_eq!(&a[..], &[2, 3, 4, 5]);
                let b = c.materialize(&mut s, &ix, "w").unwrap();
                assert!(Arc::ptr_eq(&a, &b));
                assert_eq!(c.stats().hits, 1);
                assert!(c.invalidate("w"));
                assert!(!c.invalidate("w"));
            }

            #[test]
            fn stale_entry_is_replaced() {
                let ix = manifest::parse("w | 0 | 4 | I8 | 1 | 4\n", 4).unwrap();
                let mut s = ModelStream::new(vec![9u8; 4]).unwrap();
                let mut c = TensorCache::new(4, 64).unwrap();
                c.put("w", bytes(2)).unwrap();
                let got = c.materialize(&mut s, &ix, "w").unwrap();
                assert_eq!(&got[..], &[9, 9, 9, 9]);
                assert_eq!(c.stats().stale, 1);
            }
        }
    }
    pub mod experts {
        //! Tiered mixture-of-experts store, routing and a SwiGLU expert kernel.
        //!
        //! Every routed expert of a model has the same packed size, so the store
        //! keeps two arrays of fixed-size slots:
        //!
        //! * **fast**: the hot set (pinned or GPU-visible memory), never evicted;
        //! * **ram**: an LRU cache;
        //! * **disk**: the model source, read only when the router picks an expert.
        //!
        //! An optional per-layer allow mask restricts routing to a domain hot set.

        use crate::weights::source::ModelSource;
        use crate::weights::ModelError;

        /// Upper bound on experts per layer.
        pub const MAX_EXPERTS_PER_LAYER: u32 = 256;
        const MASK_WORDS: usize = (MAX_EXPERTS_PER_LAYER as usize) / 64;

        /// Identifies one expert.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct ExpertKey {
            /// Layer index.
            pub layer: u32,
            /// Expert index within the layer.
            pub expert: u32,
        }

        /// Where an expert was served from.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Tier {
            /// Fast (pinned) tier.
            Fast,
            /// RAM cache hit.
            Ram,
            /// Just loaded from the source into RAM.
            Disk,
        }

        /// Counters.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct ExpertStats {
            /// Fast-tier hits.
            pub hit_fast: u64,
            /// RAM-tier hits.
            pub hit_ram: u64,
            /// Loads from the source.
            pub disk_loads: u64,
            /// Bytes loaded from the source.
            pub disk_bytes: u64,
            /// RAM evictions.
            pub evictions: u64,
            /// Requests refused by the allow mask.
            pub denied: u64,
        }

        /// Store sizing.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct ExpertConfig {
            /// Packed bytes per expert.
            pub expert_bytes: usize,
            /// Number of layers.
            pub layers: u32,
            /// Experts per layer (at most [`MAX_EXPERTS_PER_LAYER`]).
            pub experts_per_layer: u32,
            /// Slots in the fast tier.
            pub fast_slots: usize,
            /// Slots in the RAM tier.
            pub ram_slots: usize,
        }

        #[derive(Debug, Clone, Copy, Default)]
        struct Slot {
            key: Option<ExpertKey>,
            pinned: bool,
            age: u64,
        }

        #[derive(Debug)]
        struct TierBuf {
            data: Vec<u8>,
            slots: Vec<Slot>,
        }

        impl TierBuf {
            fn new(n: usize, expert_bytes: usize) -> Result<Self, ModelError> {
                let len = n.checked_mul(expert_bytes).ok_or(ModelError::BadConfig)?;
                Ok(Self {
                    data: vec![0; len],
                    slots: vec![Slot::default(); n],
                })
            }

            fn find(&self, k: ExpertKey) -> Option<usize> {
                self.slots.iter().position(|s| s.key == Some(k))
            }

            fn free(&self) -> Option<usize> {
                self.slots.iter().position(|s| s.key.is_none())
            }

            /// Free slot, else least-recently-used unpinned slot (flagged as eviction).
            fn victim(&self) -> Option<(usize, bool)> {
                if let Some(i) = self.free() {
                    return Some((i, false));
                }
                self.slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.pinned)
                    .min_by_key(|(_, s)| s.age)
                    .map(|(i, _)| (i, true))
            }
        }

        /// Tiered expert store over a model source.
        ///
        /// `locate` maps an expert to the byte offset of its packed weights.
        pub struct ExpertStore<S, L> {
            cfg: ExpertConfig,
            src: S,
            locate: L,
            fast: TierBuf,
            ram: TierBuf,
            masks: Option<Vec<[u64; MASK_WORDS]>>,
            clock: u64,
            stats: ExpertStats,
        }

        impl<S, L> core::fmt::Debug for ExpertStore<S, L> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct("ExpertStore")
                    .field("cfg", &self.cfg)
                    .field("stats", &self.stats)
                    .finish_non_exhaustive()
            }
        }

        impl<S: ModelSource, L: Fn(ExpertKey) -> Option<u64>> ExpertStore<S, L> {
            /// Build a store; all buffers are allocated up front.
            pub fn new(cfg: ExpertConfig, src: S, locate: L) -> Result<Self, ModelError> {
                if cfg.expert_bytes == 0
                    || cfg.layers == 0
                    || cfg.experts_per_layer == 0
                    || cfg.experts_per_layer > MAX_EXPERTS_PER_LAYER
                {
                    return Err(ModelError::BadConfig);
                }
                Ok(Self {
                    fast: TierBuf::new(cfg.fast_slots, cfg.expert_bytes)?,
                    ram: TierBuf::new(cfg.ram_slots, cfg.expert_bytes)?,
                    cfg,
                    src,
                    locate,
                    masks: None,
                    clock: 0,
                    stats: ExpertStats::default(),
                })
            }

            fn check(&self, k: ExpertKey) -> Result<(), ModelError> {
                if k.layer < self.cfg.layers && k.expert < self.cfg.experts_per_layer {
                    Ok(())
                } else {
                    Err(ModelError::BadExpert)
                }
            }

            fn tick(&mut self) -> u64 {
                self.clock += 1;
                self.clock
            }

            fn load(&mut self, k: ExpertKey, ram: bool, slot: usize) -> Result<(), ModelError> {
                let off = (self.locate)(k).ok_or(ModelError::BadExpert)?;
                let n = self.cfg.expert_bytes;
                let tier = if ram { &mut self.ram } else { &mut self.fast };
                let dst = &mut tier.data[slot * n..(slot + 1) * n];
                self.src.read_at(off, dst)?;
                self.stats.disk_loads += 1;
                self.stats.disk_bytes += n as u64;
                Ok(())
            }

            fn slice(&self, ram: bool, slot: usize) -> &[u8] {
                let n = self.cfg.expert_bytes;
                let tier = if ram { &self.ram } else { &self.fast };
                &tier.data[slot * n..(slot + 1) * n]
            }

            /// Load `k` into the fast tier, or pin it in RAM if fast is full.
            pub fn pin(&mut self, k: ExpertKey) -> Result<Tier, ModelError> {
                self.check(k)?;
                if self.fast.find(k).is_some() {
                    return Ok(Tier::Fast);
                }
                if let Some(i) = self.ram.find(k) {
                    self.ram.slots[i].pinned = true;
                    return Ok(Tier::Ram);
                }
                if let Some(i) = self.fast.free() {
                    self.load(k, false, i)?;
                    let age = self.tick();
                    self.fast.slots[i] = Slot {
                        key: Some(k),
                        pinned: true,
                        age,
                    };
                    return Ok(Tier::Fast);
                }
                let (i, evict) = self.ram.victim().ok_or(ModelError::NoSlot)?;
                self.ram.slots[i].key = None;
                self.load(k, true, i)?;
                if evict {
                    self.stats.evictions += 1;
                }
                let age = self.tick();
                self.ram.slots[i] = Slot {
                    key: Some(k),
                    pinned: true,
                    age,
                };
                Ok(Tier::Ram)
            }

            /// Packed weights of `k`, loading from the source on a miss.
            pub fn get(&mut self, k: ExpertKey) -> Result<(&[u8], Tier), ModelError> {
                self.check(k)?;
                if !self.is_allowed(k) {
                    self.stats.denied += 1;
                    return Err(ModelError::Denied);
                }
                if let Some(i) = self.fast.find(k) {
                    self.stats.hit_fast += 1;
                    let age = self.tick();
                    self.fast.slots[i].age = age;
                    return Ok((self.slice(false, i), Tier::Fast));
                }
                if let Some(i) = self.ram.find(k) {
                    self.stats.hit_ram += 1;
                    let age = self.tick();
                    self.ram.slots[i].age = age;
                    return Ok((self.slice(true, i), Tier::Ram));
                }
                let (i, evict) = self.ram.victim().ok_or(ModelError::NoSlot)?;
                self.ram.slots[i].key = None;
                self.load(k, true, i)?;
                if evict {
                    self.stats.evictions += 1;
                }
                let age = self.tick();
                self.ram.slots[i] = Slot {
                    key: Some(k),
                    pinned: false,
                    age,
                };
                Ok((self.slice(true, i), Tier::Disk))
            }

            /// Restrict `layer` to `experts` (enables masking).
            pub fn allow_only(&mut self, layer: u32, experts: &[u32]) -> Result<(), ModelError> {
                if layer >= self.cfg.layers
                    || experts.iter().any(|&e| e >= self.cfg.experts_per_layer)
                {
                    return Err(ModelError::BadExpert);
                }
                let layers = self.cfg.layers as usize;
                let masks = self
                    .masks
                    .get_or_insert_with(|| vec![[u64::MAX; MASK_WORDS]; layers]);
                let m = &mut masks[layer as usize];
                *m = [0; MASK_WORDS];
                for &e in experts {
                    m[(e / 64) as usize] |= 1u64 << (e % 64);
                }
                Ok(())
            }

            /// Remove every mask.
            pub fn allow_all(&mut self) {
                self.masks = None;
            }

            /// Whether routing may use `k`.
            #[must_use]
            pub fn is_allowed(&self, k: ExpertKey) -> bool {
                if self.check(k).is_err() {
                    return false;
                }
                match &self.masks {
                    None => true,
                    Some(m) => {
                        (m[k.layer as usize][(k.expert / 64) as usize] >> (k.expert % 64)) & 1 == 1
                    }
                }
            }

            /// Counters so far.
            #[must_use]
            pub fn stats(&self) -> ExpertStats {
                self.stats
            }

            /// Bytes held across both tiers.
            #[must_use]
            pub fn resident_bytes(&self) -> usize {
                let n = self
                    .fast
                    .slots
                    .iter()
                    .chain(&self.ram.slots)
                    .filter(|s| s.key.is_some())
                    .count();
                n * self.cfg.expert_bytes
            }
        }

        /// Softmax over `logits` (disallowed experts excluded), then top-k.
        ///
        /// Returns `(expert, weight)` pairs in descending weight order. With
        /// `renormalise`, the selected weights sum to 1.
        pub fn route(
            logits: &[f32],
            allowed: impl Fn(usize) -> bool,
            top_k: usize,
            renormalise: bool,
        ) -> Result<Vec<(usize, f32)>, ModelError> {
            if logits.is_empty()
                || logits.len() > MAX_EXPERTS_PER_LAYER as usize
                || top_k == 0
                || top_k > logits.len()
            {
                return Err(ModelError::BadConfig);
            }
            let live: Vec<usize> = (0..logits.len()).filter(|&e| allowed(e)).collect();
            let max = live
                .iter()
                .map(|&e| logits[e])
                .fold(f32::NEG_INFINITY, f32::max);
            if live.len() < top_k || !max.is_finite() {
                return Err(ModelError::Denied);
            }
            let mut probs: Vec<(usize, f32)> =
                live.iter().map(|&e| (e, (logits[e] - max).exp())).collect();
            let sum: f32 = probs.iter().map(|p| p.1).sum();
            for p in &mut probs {
                p.1 /= sum;
            }
            probs.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            probs.truncate(top_k);
            if renormalise {
                let sel: f32 = probs.iter().map(|p| p.1).sum();
                if sel > 0.0 {
                    for p in &mut probs {
                        p.1 /= sel;
                    }
                }
            }
            Ok(probs)
        }

        /// One SwiGLU expert on one token, accumulated into `out`.
        ///
        /// `packed` holds row-major f32 `gate[inter][hidden] | up[inter][hidden] |
        /// down[hidden][inter]`. Computes `out += weight * down(silu(gate x) * up x)`.
        pub fn expert_accumulate(
            packed: &[f32],
            hidden: usize,
            inter: usize,
            x: &[f32],
            weight: f32,
            out: &mut [f32],
        ) -> Result<(), ModelError> {
            let block = hidden.checked_mul(inter).ok_or(ModelError::BadConfig)?;
            let need = block.checked_mul(3).ok_or(ModelError::BadConfig)?;
            if hidden == 0
                || inter == 0
                || packed.len() < need
                || x.len() != hidden
                || out.len() != hidden
            {
                return Err(ModelError::BadConfig);
            }
            let (gate, rest) = packed.split_at(block);
            let (up, down) = rest.split_at(block);
            let dot = |row: &[f32]| row.iter().zip(x).map(|(w, v)| w * v).sum::<f32>();
            let act: Vec<f32> = gate
                .chunks_exact(hidden)
                .zip(up.chunks_exact(hidden))
                .map(|(g, u)| {
                    let a = dot(g);
                    (a / (1.0 + (-a).exp())) * dot(u)
                })
                .collect();
            for (o, row) in out.iter_mut().zip(down.chunks_exact(inter)) {
                *o += weight * row.iter().zip(&act).map(|(w, a)| w * a).sum::<f32>();
            }
            Ok(())
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn store(
                fast: usize,
                ram: usize,
            ) -> ExpertStore<Vec<u8>, impl Fn(ExpertKey) -> Option<u64>> {
                // 2 layers x 4 experts x 2 bytes; expert bytes = [layer, expert].
                let mut data = Vec::new();
                for l in 0..2u8 {
                    for e in 0..4u8 {
                        data.extend_from_slice(&[l, e]);
                    }
                }
                let cfg = ExpertConfig {
                    expert_bytes: 2,
                    layers: 2,
                    experts_per_layer: 4,
                    fast_slots: fast,
                    ram_slots: ram,
                };
                ExpertStore::new(cfg, data, |k: ExpertKey| {
                    Some(u64::from(k.layer * 4 + k.expert) * 2)
                })
                .unwrap()
            }

            fn key(layer: u32, expert: u32) -> ExpertKey {
                ExpertKey { layer, expert }
            }

            #[test]
            fn tiers_and_lru() {
                let mut s = store(1, 2);
                assert_eq!(s.pin(key(0, 0)).unwrap(), Tier::Fast);
                assert_eq!(s.get(key(0, 0)).unwrap(), (&[0u8, 0][..], Tier::Fast));
                assert_eq!(s.get(key(1, 2)).unwrap(), (&[1u8, 2][..], Tier::Disk));
                assert_eq!(s.get(key(1, 3)).unwrap().1, Tier::Disk);
                assert_eq!(s.get(key(1, 2)).unwrap().1, Tier::Ram);
                // (1,3) is now LRU and gets evicted.
                assert_eq!(s.get(key(0, 1)).unwrap().1, Tier::Disk);
                assert_eq!(s.get(key(1, 3)).unwrap().1, Tier::Disk);
                let st = s.stats();
                assert_eq!(st.evictions, 2);
                assert_eq!(st.hit_fast, 1);
                assert_eq!(s.resident_bytes(), 6);
            }

            #[test]
            fn pinned_ram_is_kept() {
                let mut s = store(0, 1);
                assert_eq!(s.pin(key(0, 1)).unwrap(), Tier::Ram);
                assert_eq!(s.get(key(0, 2)), Err(ModelError::NoSlot));
                assert_eq!(s.get(key(0, 1)).unwrap().1, Tier::Ram);
            }

            #[test]
            fn masks_deny() {
                let mut s = store(0, 2);
                s.allow_only(0, &[1, 3]).unwrap();
                assert!(s.is_allowed(key(0, 3)));
                assert!(!s.is_allowed(key(0, 0)));
                assert!(s.is_allowed(key(1, 0)));
                assert_eq!(s.get(key(0, 0)), Err(ModelError::Denied));
                assert_eq!(s.stats().denied, 1);
                s.allow_all();
                assert!(s.get(key(0, 0)).is_ok());
                assert_eq!(s.get(key(5, 0)), Err(ModelError::BadExpert));
            }

            #[test]
            fn routing_top_k() {
                let logits = [0.0f32, 2.0, 1.0, 3.0];
                let r = route(&logits, |_| true, 2, true).unwrap();
                assert_eq!(r[0].0, 3);
                assert_eq!(r[1].0, 1);
                assert!((r[0].1 + r[1].1 - 1.0).abs() < 1e-6);
                let r = route(&logits, |e| e != 3, 1, false).unwrap();
                assert_eq!(r[0].0, 1);
                assert_eq!(route(&logits, |e| e == 0, 2, true), Err(ModelError::Denied));
            }

            #[test]
            fn swiglu_expert() {
                // hidden = 1, inter = 1: gate = 1, up = 2, down = 3, x = 1.
                let packed = [1.0f32, 2.0, 3.0];
                let mut out = [0.5f32];
                expert_accumulate(&packed, 1, 1, &[1.0], 0.5, &mut out).unwrap();
                let silu = 1.0 / (1.0 + (-1.0f32).exp());
                let want = 0.5 + 0.5 * 3.0 * silu * 2.0;
                assert!((out[0] - want).abs() < 1e-6);
                assert!(expert_accumulate(&packed, 2, 1, &[1.0], 1.0, &mut out).is_err());
            }
        }
    }
    pub mod index {
        //! Named tensor table for one model file.

        use crate::tensor::{layout::checked_product, DType, MAX_RANK};
        use crate::weights::ModelError;

        /// Maximum tensors in one index.
        pub const MAX_TENSORS: usize = 4096;
        /// Maximum tensor name length in bytes.
        pub const NAME_MAX: usize = 95;

        /// Dtypes allowed for stored model weights.
        #[must_use]
        pub fn is_model_dtype(d: DType) -> bool {
            matches!(
                d,
                DType::F32 | DType::BF16 | DType::F16 | DType::Fp8E4M3 | DType::I8
            )
        }

        /// Location and shape of one tensor inside a model file.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct TensorMeta {
            /// Unique name.
            pub name: String,
            /// Byte offset in the model file.
            pub offset: u64,
            /// Byte length.
            pub bytes: u64,
            /// Element type.
            pub dtype: DType,
            /// Dimensions.
            pub shape: Vec<u64>,
        }

        impl TensorMeta {
            /// Element count, checked.
            pub fn elements(&self) -> Result<u64, ModelError> {
                if self.shape.is_empty() || self.shape.len() > MAX_RANK {
                    return Err(ModelError::BadShape);
                }
                let dims: Option<Vec<usize>> = self
                    .shape
                    .iter()
                    .map(|&d| usize::try_from(d).ok())
                    .collect();
                let dims = dims.ok_or(ModelError::BadShape)?;
                let n = checked_product(&dims).map_err(|_| ModelError::BadShape)?;
                u64::try_from(n).map_err(|_| ModelError::BadShape)
            }

            /// Bytes implied by shape and dtype.
            pub fn expected_bytes(&self) -> Result<u64, ModelError> {
                if !is_model_dtype(self.dtype) {
                    return Err(ModelError::BadDType);
                }
                self.elements()?
                    .checked_mul(self.dtype.size() as u64)
                    .ok_or(ModelError::BadShape)
            }

            /// Check that `bytes` matches shape and dtype.
            pub fn validate_layout(&self) -> Result<(), ModelError> {
                if self.bytes == 0 || self.expected_bytes()? != self.bytes {
                    return Err(ModelError::LayoutMismatch);
                }
                Ok(())
            }

            fn end(&self) -> Option<u64> {
                self.offset.checked_add(self.bytes)
            }
        }

        /// Table of tensors, unique by name.
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct ModelIndex {
            tensors: Vec<TensorMeta>,
        }

        impl ModelIndex {
            /// Empty index.
            #[must_use]
            pub fn new() -> Self {
                Self::default()
            }

            /// Add a tensor. Range checks against the file happen in [`Self::validate`].
            pub fn add(&mut self, meta: TensorMeta) -> Result<(), ModelError> {
                if self.tensors.len() >= MAX_TENSORS {
                    return Err(ModelError::Full);
                }
                if meta.name.is_empty() || meta.name.len() > NAME_MAX || meta.name.contains('\0') {
                    return Err(ModelError::BadName);
                }
                if !is_model_dtype(meta.dtype) {
                    return Err(ModelError::BadDType);
                }
                if meta.shape.is_empty() || meta.shape.len() > MAX_RANK || meta.shape.contains(&0) {
                    return Err(ModelError::BadShape);
                }
                if meta.bytes == 0 {
                    return Err(ModelError::LayoutMismatch);
                }
                if self.find(&meta.name).is_some() {
                    return Err(ModelError::DuplicateName);
                }
                self.tensors.push(meta);
                Ok(())
            }

            /// Check every tensor lies inside `model_bytes` and no two overlap.
            pub fn validate(&self, model_bytes: u64) -> Result<(), ModelError> {
                if model_bytes == 0 {
                    return Err(ModelError::BadRange);
                }
                let mut spans: Vec<(u64, u64)> = Vec::with_capacity(self.tensors.len());
                for t in &self.tensors {
                    let end = t.end().ok_or(ModelError::BadRange)?;
                    if end > model_bytes {
                        return Err(ModelError::BadRange);
                    }
                    spans.push((t.offset, end));
                }
                spans.sort_unstable();
                if spans
                    .windows(2)
                    .any(|w| w[1].0 < w[0].1 || w[1].0 == w[0].0)
                {
                    return Err(ModelError::BadRange);
                }
                Ok(())
            }

            /// Look up by name.
            #[must_use]
            pub fn find(&self, name: &str) -> Option<&TensorMeta> {
                self.tensors.iter().find(|t| t.name == name)
            }

            /// All tensors in insertion order.
            #[must_use]
            pub fn tensors(&self) -> &[TensorMeta] {
                &self.tensors
            }

            /// Number of tensors.
            #[must_use]
            pub fn len(&self) -> usize {
                self.tensors.len()
            }

            /// True when empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.tensors.is_empty()
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn meta(name: &str, offset: u64, bytes: u64) -> TensorMeta {
                TensorMeta {
                    name: name.into(),
                    offset,
                    bytes,
                    dtype: DType::I8,
                    shape: vec![bytes],
                }
            }

            #[test]
            fn add_find_validate() {
                let mut ix = ModelIndex::new();
                ix.add(meta("a", 0, 4)).unwrap();
                ix.add(meta("b", 4, 4)).unwrap();
                assert_eq!(ix.find("b").unwrap().offset, 4);
                ix.validate(8).unwrap();
                assert_eq!(ix.validate(7), Err(ModelError::BadRange));
            }

            #[test]
            fn rejects_duplicates_and_overlap() {
                let mut ix = ModelIndex::new();
                ix.add(meta("a", 0, 4)).unwrap();
                assert_eq!(ix.add(meta("a", 8, 4)), Err(ModelError::DuplicateName));
                ix.add(meta("b", 3, 4)).unwrap();
                assert_eq!(ix.validate(100), Err(ModelError::BadRange));
            }

            #[test]
            fn layout_check() {
                let mut m = meta("w", 0, 8);
                m.dtype = DType::F32;
                m.shape = vec![2];
                m.validate_layout().unwrap();
                m.bytes = 9;
                assert_eq!(m.validate_layout(), Err(ModelError::LayoutMismatch));
                m.dtype = DType::I32;
                assert_eq!(m.expected_bytes(), Err(ModelError::BadDType));
            }
        }
    }
    pub mod manifest {
        //! Plain-text tensor manifest.
        //!
        //! One record per line: `name | offset | bytes | dtype | rank | d0,d1,...`.
        //! Blank lines and lines starting with `#` are skipped.

        use crate::tensor::{DType, MAX_RANK};
        use crate::weights::index::{ModelIndex, TensorMeta};
        use crate::weights::ModelError;

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
    }
    pub mod source {
        //! Random-access byte sources and a bounded sequential stream.

        use crate::weights::ModelError;
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};
        use std::path::Path;
        use std::sync::Mutex;

        /// A fixed-length, random-access byte source (memory, file, block device).
        pub trait ModelSource {
            /// Total length in bytes.
            fn len(&self) -> u64;

            /// True when the source has no bytes.
            fn is_empty(&self) -> bool {
                self.len() == 0
            }

            /// Fill `buf` from `offset`. Must fail rather than read short.
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ModelError>;
        }

        fn range_ok(offset: u64, len: usize, total: u64) -> bool {
            u64::try_from(len)
                .ok()
                .and_then(|l| offset.checked_add(l))
                .is_some_and(|end| end <= total)
        }

        impl ModelSource for [u8] {
            fn len(&self) -> u64 {
                <[u8]>::len(self) as u64
            }

            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ModelError> {
                if !range_ok(offset, buf.len(), ModelSource::len(self)) {
                    return Err(ModelError::OutOfBounds);
                }
                let start = usize::try_from(offset).map_err(|_| ModelError::OutOfBounds)?;
                buf.copy_from_slice(&self[start..start + buf.len()]);
                Ok(())
            }
        }

        impl ModelSource for Vec<u8> {
            fn len(&self) -> u64 {
                ModelSource::len(self.as_slice())
            }

            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ModelError> {
                self.as_slice().read_at(offset, buf)
            }
        }

        /// A model file read with seek + read under a lock (portable).
        #[derive(Debug)]
        pub struct FileSource {
            file: Mutex<File>,
            len: u64,
        }

        impl FileSource {
            /// Open a file read-only.
            pub fn open(path: &Path) -> Result<Self, ModelError> {
                let file = File::open(path)?;
                let len = file.metadata()?.len();
                Ok(Self {
                    file: Mutex::new(file),
                    len,
                })
            }
        }

        impl ModelSource for FileSource {
            fn len(&self) -> u64 {
                self.len
            }

            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ModelError> {
                if !range_ok(offset, buf.len(), self.len) {
                    return Err(ModelError::OutOfBounds);
                }
                let mut f = self
                    .file
                    .lock()
                    .map_err(|_| ModelError::Io("file lock poisoned".into()))?;
                f.seek(SeekFrom::Start(offset))?;
                f.read_exact(buf)?;
                Ok(())
            }
        }

        /// Sequential cursor over a source that never reads past its end.
        #[derive(Debug)]
        pub struct ModelStream<S> {
            src: S,
            pos: u64,
        }

        impl<S: ModelSource> ModelStream<S> {
            /// Stream positioned at 0. Empty sources are rejected.
            pub fn new(src: S) -> Result<Self, ModelError> {
                if src.is_empty() {
                    return Err(ModelError::BadConfig);
                }
                Ok(Self { src, pos: 0 })
            }

            /// Underlying source.
            pub fn source(&self) -> &S {
                &self.src
            }

            /// Total bytes.
            pub fn total(&self) -> u64 {
                self.src.len()
            }

            /// Current position.
            pub fn position(&self) -> u64 {
                self.pos
            }

            /// Move to `offset` (may equal the end).
            pub fn seek(&mut self, offset: u64) -> Result<(), ModelError> {
                if offset > self.total() {
                    return Err(ModelError::OutOfBounds);
                }
                self.pos = offset;
                Ok(())
            }

            /// Fill `buf` and advance.
            pub fn read(&mut self, buf: &mut [u8]) -> Result<(), ModelError> {
                if buf.is_empty() || !range_ok(self.pos, buf.len(), self.total()) {
                    return Err(ModelError::OutOfBounds);
                }
                self.src.read_at(self.pos, buf)?;
                self.pos += buf.len() as u64;
                Ok(())
            }

            /// Bytes left.
            pub fn remaining(&self) -> u64 {
                self.total() - self.pos
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn stream_reads_and_bounds() {
                let mut s = ModelStream::new(vec![1u8, 2, 3, 4, 5]).unwrap();
                let mut b = [0u8; 2];
                s.read(&mut b).unwrap();
                assert_eq!(b, [1, 2]);
                s.seek(4).unwrap();
                assert_eq!(s.read(&mut b), Err(ModelError::OutOfBounds));
                assert_eq!(s.remaining(), 1);
                assert_eq!(s.seek(6), Err(ModelError::OutOfBounds));
                assert!(ModelStream::new(Vec::new()).is_err());
            }

            #[test]
            fn file_source_round_trip() {
                let path =
                    std::env::temp_dir().join(format!("mouli-model-src-{}", std::process::id()));
                std::fs::write(&path, b"abcdef").unwrap();
                let f = FileSource::open(&path).unwrap();
                let mut b = [0u8; 3];
                f.read_at(2, &mut b).unwrap();
                assert_eq!(&b, b"cde");
                assert_eq!(f.read_at(4, &mut b), Err(ModelError::OutOfBounds));
                drop(f);
                std::fs::remove_file(path).unwrap();
            }
        }
    }

    use core::fmt;

    /// Errors from the model storage layer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ModelError {
        /// Tensor name empty or too long.
        BadName,
        /// Tensor name already present.
        DuplicateName,
        /// Unsupported or unknown dtype.
        BadDType,
        /// Rank or shape invalid.
        BadShape,
        /// Declared byte count does not match shape and dtype.
        LayoutMismatch,
        /// Range outside the model or overlapping another tensor.
        BadRange,
        /// Index is full.
        Full,
        /// Tensor not found.
        NotFound,
        /// Manifest line malformed (1-based line number).
        Manifest(usize),
        /// Manifest has no records.
        EmptyManifest,
        /// Read would go past the end of the source or tensor.
        OutOfBounds,
        /// Underlying I/O failure.
        Io(String),
        /// Expert key outside the configured layers/experts.
        BadExpert,
        /// Expert is masked out.
        Denied,
        /// Every slot is pinned.
        NoSlot,
        /// Bad configuration or arguments.
        BadConfig,
        /// An internal invariant does not hold (the text names it).
        Inconsistent(&'static str),
    }

    impl fmt::Display for ModelError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::BadName => f.write_str("bad tensor name"),
                Self::DuplicateName => f.write_str("duplicate tensor name"),
                Self::BadDType => f.write_str("unsupported dtype"),
                Self::BadShape => f.write_str("bad tensor shape"),
                Self::LayoutMismatch => f.write_str("byte count does not match shape"),
                Self::BadRange => f.write_str("tensor range out of bounds or overlapping"),
                Self::Full => f.write_str("model index full"),
                Self::NotFound => f.write_str("tensor not found"),
                Self::Manifest(line) => write!(f, "manifest line {line} malformed"),
                Self::EmptyManifest => f.write_str("manifest has no records"),
                Self::OutOfBounds => f.write_str("read out of bounds"),
                Self::Io(e) => write!(f, "i/o error: {e}"),
                Self::BadExpert => f.write_str("expert key out of range"),
                Self::Denied => f.write_str("expert not allowed"),
                Self::NoSlot => f.write_str("no evictable expert slot"),
                Self::BadConfig => f.write_str("bad configuration"),
                Self::Inconsistent(what) => write!(f, "inconsistent state: {what}"),
            }
        }
    }

    impl std::error::Error for ModelError {}

    impl From<std::io::Error> for ModelError {
        fn from(e: std::io::Error) -> Self {
            Self::Io(e.to_string())
        }
    }
}

pub mod theory {
    //! MOULI theory layer, from "Towards a Mathematical Theory of Intelligence".
    //!
    //! MOULI uses these measures to choose, route and merge models by how they
    //! behave under shift, not by a single familiar benchmark number.
    //!
    //! * [`robust`]: closure conditions, robust score S (worst case), shortcut
    //!   reliance and the gap law that predicts accuracy under shift.
    //! * [`calibration`]: ECE, Brier score and the self-knowledge loss.
    //! * [`cascade`]: cheapest-first two-model cascades and their Pareto frontier.
    //! * [`select`]: leaderboard ranking and robust choice of a merge weight.
    //! * [`merge`]: task-vector screening and TIES / linear merging.
    //!
    //! Rule kept from the research engine: nothing is reported as measured
    //! unless it was measured; these functions only transform measurements.
    #![forbid(unsafe_code)]

    /// Options per multiple-choice item used throughout (A-D).
    pub const K: usize = 4;

    pub mod calibration {
        //! Self-knowledge: how well a model's confidence matches its correctness.

        /// Calibration metrics over scored items.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Calibration {
            /// Items scored.
            pub items: usize,
            /// Fraction correct.
            pub accuracy: f64,
            /// Mean confidence.
            pub mean_confidence: f64,
            /// Expected calibration error over equal-width bins.
            pub ece: f64,
            /// Brier score `E[(P - V)^2]`.
            pub brier: f64,
        }

        impl Calibration {
            /// Exact expected cost of acting on this confidence: `E[(P - V)^2] / 2`.
            #[must_use]
            pub fn self_knowledge_loss(&self) -> f64 {
                self.brier / 2.0
            }
        }

        /// Compute calibration from `(confidence, correct)` pairs using `bins` bins.
        ///
        /// Bin `b` holds confidences in `(b/bins, (b+1)/bins]`; a confidence of 0
        /// goes to the first bin. Returns `None` for no items or zero bins.
        #[must_use]
        pub fn calibration(records: &[(f64, bool)], bins: usize) -> Option<Calibration> {
            if records.is_empty() || bins == 0 {
                return None;
            }
            let n = records.len() as f64;
            let mut sum_p = vec![0.0f64; bins];
            let mut sum_v = vec![0.0f64; bins];
            let mut count = vec![0usize; bins];
            for &(p, v) in records {
                let p = p.clamp(0.0, 1.0);
                let b = ((p * bins as f64).ceil() as usize)
                    .saturating_sub(1)
                    .min(bins - 1);
                sum_p[b] += p;
                sum_v[b] += f64::from(u8::from(v));
                count[b] += 1;
            }
            let ece = (0..bins)
                .filter(|&b| count[b] > 0)
                .map(|b| {
                    let c = count[b] as f64;
                    c / n * (sum_p[b] / c - sum_v[b] / c).abs()
                })
                .sum();
            let brier = records
                .iter()
                .map(|&(p, v)| (p - f64::from(u8::from(v))).powi(2))
                .sum::<f64>()
                / n;
            Some(Calibration {
                items: records.len(),
                accuracy: records.iter().filter(|r| r.1).count() as f64 / n,
                mean_confidence: records.iter().map(|r| r.0).sum::<f64>() / n,
                ece,
                brier,
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn perfect_and_overconfident() {
                let perfect = [(1.0, true), (0.0, false)];
                let c = calibration(&perfect, 10).unwrap();
                assert!(c.ece.abs() < 1e-12 && c.brier.abs() < 1e-12);

                let over = [(0.9, false), (0.9, true)];
                let c = calibration(&over, 10).unwrap();
                assert!((c.ece - 0.4).abs() < 1e-12);
                assert!((c.brier - (0.81 + 0.01) / 2.0).abs() < 1e-12);
                assert!((c.self_knowledge_loss() - c.brier / 2.0).abs() < 1e-12);
                assert!(calibration(&[], 10).is_none());
            }
        }
    }

    pub mod cascade {
        //! Cheapest-first model cascades.
        //!
        //! A two-model cascade answers with the small model when its confidence is
        //! at least a threshold, otherwise it also runs the large model. Every plan
        //! is evaluated on items scored by all candidates.

        use std::collections::{BTreeMap, BTreeSet};

        /// One scored item.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Scored {
            /// Answer was correct.
            pub correct: bool,
            /// Model confidence in its answer.
            pub confidence: f64,
        }

        /// A model and its per-item results.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Candidate {
            /// Name.
            pub name: String,
            /// Cost per item (for example parameters invoked).
            pub cost: f64,
            /// Results keyed by item id.
            pub records: BTreeMap<u64, Scored>,
        }

        /// A routing plan and its measured trade-off.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Plan {
            /// Models in order.
            pub models: Vec<String>,
            /// Confidence threshold on the first model (cascades only).
            pub threshold: Option<f64>,
            /// Accuracy on shared items.
            pub accuracy: f64,
            /// Mean cost per item.
            pub avg_cost: f64,
        }

        /// Cascade planning result.
        #[derive(Debug, Clone, PartialEq)]
        pub struct CascadeReport {
            /// Shared items used.
            pub common_items: usize,
            /// Most accurate single model.
            pub best_single: Plan,
            /// Cheapest plan within `tolerance` of the best single model's accuracy.
            pub recommended: Plan,
            /// Plans not dominated on (cost, accuracy), cheapest first.
            pub frontier: Vec<Plan>,
        }

        impl CascadeReport {
            /// Fraction of compute saved by the recommendation vs. the best single model.
            #[must_use]
            pub fn saving(&self) -> f64 {
                if self.best_single.avg_cost > 0.0 {
                    1.0 - self.recommended.avg_cost / self.best_single.avg_cost
                } else {
                    0.0
                }
            }
        }

        /// Plan cascades over `candidates`, sweeping `grid` thresholds per pair.
        ///
        /// Returns `None` with fewer than two candidates or fewer than `min_common`
        /// shared items.
        #[must_use]
        pub fn plan(
            candidates: &[Candidate],
            grid: usize,
            tolerance: f64,
            min_common: usize,
        ) -> Option<CascadeReport> {
            if candidates.len() < 2 || grid < 2 {
                return None;
            }
            let mut common: BTreeSet<u64> = candidates[0].records.keys().copied().collect();
            for c in &candidates[1..] {
                common.retain(|k| c.records.contains_key(k));
            }
            if common.len() < min_common.max(1) {
                return None;
            }
            let items: Vec<u64> = common.into_iter().collect();
            let n = items.len() as f64;
            let acc_of =
                |c: &Candidate| items.iter().filter(|i| c.records[i].correct).count() as f64 / n;

            let singles: Vec<Plan> = candidates
                .iter()
                .map(|c| Plan {
                    models: vec![c.name.clone()],
                    threshold: None,
                    accuracy: acc_of(c),
                    avg_cost: c.cost,
                })
                .collect();
            let mut plans = singles.clone();

            let mut by_cost: Vec<&Candidate> = candidates.iter().collect();
            by_cost.sort_by(|a, b| a.cost.total_cmp(&b.cost));
            for (i, small) in by_cost.iter().enumerate() {
                let mut confs: Vec<f64> =
                    items.iter().map(|j| small.records[j].confidence).collect();
                confs.sort_by(f64::total_cmp);
                for large in &by_cost[i + 1..] {
                    for g in 0..grid {
                        let tau = if g + 1 == grid {
                            f64::INFINITY
                        } else {
                            confs[g * (confs.len() - 1) / (grid - 1)]
                        };
                        let (mut ok, mut cost) = (0.0f64, 0.0f64);
                        for j in &items {
                            let rs = small.records[j];
                            cost += small.cost;
                            if rs.confidence >= tau {
                                ok += f64::from(u8::from(rs.correct));
                            } else {
                                ok += f64::from(u8::from(large.records[j].correct));
                                cost += large.cost;
                            }
                        }
                        plans.push(Plan {
                            models: vec![small.name.clone(), large.name.clone()],
                            threshold: tau.is_finite().then_some(tau),
                            accuracy: ok / n,
                            avg_cost: cost / n,
                        });
                    }
                }
            }

            let mut sorted = plans.clone();
            sorted.sort_by(|a, b| {
                a.avg_cost
                    .total_cmp(&b.avg_cost)
                    .then(b.accuracy.total_cmp(&a.accuracy))
            });
            let mut frontier: Vec<Plan> = Vec::new();
            for p in sorted {
                if frontier
                    .last()
                    .is_none_or(|last| p.accuracy > last.accuracy + 1e-12)
                {
                    frontier.push(p);
                }
            }
            let best_single = singles
                .iter()
                .max_by(|a, b| a.accuracy.total_cmp(&b.accuracy))?
                .clone();
            let recommended = plans
                .iter()
                .filter(|p| p.accuracy >= best_single.accuracy - tolerance)
                .min_by(|a, b| a.avg_cost.total_cmp(&b.avg_cost))?
                .clone();
            Some(CascadeReport {
                common_items: items.len(),
                best_single,
                recommended,
                frontier,
            })
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn cand(name: &str, cost: f64, rows: &[(bool, f64)]) -> Candidate {
                Candidate {
                    name: name.into(),
                    cost,
                    records: rows
                        .iter()
                        .enumerate()
                        .map(|(i, &(correct, confidence))| {
                            (
                                i as u64,
                                Scored {
                                    correct,
                                    confidence,
                                },
                            )
                        })
                        .collect(),
                }
            }

            #[test]
            fn cascade_saves_compute() {
                // Small model is right exactly when confident; large model is always right.
                let small_rows: Vec<(bool, f64)> = (0..20)
                    .map(|i| (i < 15, if i < 15 { 0.9 } else { 0.3 }))
                    .collect();
                let large_rows: Vec<(bool, f64)> = (0..20).map(|_| (true, 0.95)).collect();
                let r = plan(
                    &[
                        cand("small", 1.0, &small_rows),
                        cand("large", 10.0, &large_rows),
                    ],
                    21,
                    0.0,
                    20,
                )
                .unwrap();
                assert_eq!(r.best_single.models, vec!["large".to_string()]);
                assert!((r.recommended.accuracy - 1.0).abs() < 1e-12);
                assert_eq!(r.recommended.models.len(), 2);
                assert!(r.saving() > 0.5);
                assert!(r.frontier.windows(2).all(|w| w[1].accuracy > w[0].accuracy));
            }

            #[test]
            fn needs_overlap() {
                let a = cand("a", 1.0, &[(true, 0.5)]);
                let b = cand("b", 2.0, &[(true, 0.5)]);
                assert!(plan(&[a.clone(), b], 5, 0.01, 20).is_none());
                assert!(plan(&[a], 5, 0.01, 1).is_none());
            }
        }
    }

    pub mod merge {
        //! Task-vector screening and model merging (TIES and linear).
        //!
        //! A task vector is `finetuned - base` for one tensor. TIES merging trims
        //! small entries per model, elects the majority sign per entry, and averages
        //! only the entries that agree with it.

        /// Magnitude threshold keeping the top `density` fraction of `tau`.
        ///
        /// `density >= 1` keeps everything (threshold 0).
        #[must_use]
        pub fn ties_threshold(tau: &[f32], density: f32) -> f32 {
            if tau.is_empty() || density >= 1.0 {
                return 0.0;
            }
            let mut mags: Vec<f32> = tau.iter().map(|x| x.abs()).collect();
            let drop = ((mags.len() as f32 * (1.0 - density.max(0.0))) as usize).max(1);
            if drop >= mags.len() {
                return mags.iter().copied().fold(0.0, f32::max);
            }
            let (_, kth, _) = mags.select_nth_unstable_by(drop - 1, f32::total_cmp);
            *kth
        }

        /// TIES-combine task vectors `taus` (equal length) with per-model thresholds.
        #[must_use]
        pub fn ties_combine(taus: &[&[f32]], thresholds: &[f32]) -> Vec<f32> {
            let Some(first) = taus.first() else {
                return Vec::new();
            };
            let n = first.len();
            (0..n)
                .map(|i| {
                    let vals: Vec<f32> = taus
                        .iter()
                        .zip(thresholds)
                        .map(|(t, &h)| {
                            let v = t.get(i).copied().unwrap_or(0.0);
                            if v.abs() >= h {
                                v
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    let sign = vals.iter().sum::<f32>().signum();
                    let (sum, cnt) = vals
                        .iter()
                        .filter(|&&v| v != 0.0 && v.signum() == sign)
                        .fold((0.0f32, 0u32), |(s, c), &v| (s + v, c + 1));
                    if cnt == 0 {
                        0.0
                    } else {
                        sum / cnt as f32
                    }
                })
                .collect()
        }

        /// Merge method.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Method {
            /// TIES with the given density.
            Ties {
                /// Fraction of entries kept per model.
                density: f32,
            },
            /// Weighted sum of task vectors.
            Linear {
                /// One weight per model.
                weights: Vec<f32>,
            },
        }

        /// Merge one tensor: `base + lambda * combine(finetuned - base)`.
        ///
        /// Returns `None` when shapes differ or the weight count is wrong; callers
        /// then keep the base tensor unchanged.
        #[must_use]
        pub fn merge_tensor(
            base: &[f32],
            finetuned: &[&[f32]],
            method: &Method,
            lambda: f32,
        ) -> Option<Vec<f32>> {
            if finetuned.is_empty() || finetuned.iter().any(|f| f.len() != base.len()) {
                return None;
            }
            let taus: Vec<Vec<f32>> = finetuned
                .iter()
                .map(|f| f.iter().zip(base).map(|(a, b)| a - b).collect())
                .collect();
            let refs: Vec<&[f32]> = taus.iter().map(Vec::as_slice).collect();
            let tau = match method {
                Method::Ties { density } => {
                    let thr: Vec<f32> = refs.iter().map(|t| ties_threshold(t, *density)).collect();
                    ties_combine(&refs, &thr)
                }
                Method::Linear { weights } => {
                    if weights.len() != refs.len() {
                        return None;
                    }
                    (0..base.len())
                        .map(|i| refs.iter().zip(weights).map(|(t, w)| w * t[i]).sum())
                        .collect()
                }
            };
            Some(base.iter().zip(tau).map(|(b, t)| b + lambda * t).collect())
        }

        /// Screening verdict for one candidate.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub enum Screen {
            /// Keep.
            Keep,
            /// Task vector is ~zero.
            IdenticalToBase,
            /// Task vector too large relative to the base.
            Diverged(f64),
            /// Near-duplicate of an earlier kept candidate (its index).
            Duplicate(usize),
        }

        /// Screen candidates given accumulated statistics over all tensors:
        /// `gram[i][j] = <tau_i, tau_j>` and `base_sq = |base|^2`.
        ///
        /// Candidates are visited in the given order (put preferred ones first).
        #[must_use]
        pub fn screen(gram: &[Vec<f64>], base_sq: f64, max_rel: f64, dup_cos: f64) -> Vec<Screen> {
            let n = gram.len();
            let norms: Vec<f64> = (0..n).map(|i| gram[i][i].max(0.0).sqrt()).collect();
            let bnorm = base_sq.max(0.0).sqrt();
            let mut kept: Vec<usize> = Vec::new();
            (0..n)
                .map(|i| {
                    let rel = if bnorm > 0.0 { norms[i] / bnorm } else { 0.0 };
                    if rel < 1e-5 {
                        return Screen::IdenticalToBase;
                    }
                    if rel > max_rel {
                        return Screen::Diverged(rel);
                    }
                    if let Some(&j) = kept
                        .iter()
                        .find(|&&j| gram[i][j] / (norms[i] * norms[j] + 1e-12) > dup_cos)
                    {
                        return Screen::Duplicate(j);
                    }
                    kept.push(i);
                    Screen::Keep
                })
                .collect()
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn threshold_keeps_top_fraction() {
                let t = [0.1, -0.4, 0.3, -0.2];
                assert!((ties_threshold(&t, 0.5) - 0.2).abs() < 1e-6);
                assert_eq!(ties_threshold(&t, 1.0), 0.0);
            }

            #[test]
            fn ties_resolves_sign_conflicts() {
                let a = [1.0f32, 1.0, -0.1];
                let b = [3.0f32, -2.0, -0.1];
                let c = [-1.0f32, -2.0, 0.0];
                let out = ties_combine(&[&a, &b, &c], &[0.0, 0.0, 0.0]);
                assert!((out[0] - 2.0).abs() < 1e-6); // + wins: mean of 1, 3
                assert!((out[1] + 2.0).abs() < 1e-6); // - wins: mean of -2, -2
                assert!((out[2] + 0.1).abs() < 1e-6);
            }

            #[test]
            fn merge_linear_and_ties() {
                let base = [1.0f32, 1.0];
                let f1 = [2.0f32, 1.0];
                let f2 = [1.0f32, 3.0];
                let lin = merge_tensor(
                    &base,
                    &[&f1, &f2],
                    &Method::Linear {
                        weights: vec![0.5, 0.5],
                    },
                    1.0,
                )
                .unwrap();
                assert_eq!(lin, vec![1.5, 2.0]);
                let ties =
                    merge_tensor(&base, &[&f1, &f2], &Method::Ties { density: 1.0 }, 1.0).unwrap();
                assert_eq!(ties, vec![2.0, 3.0]);
                assert!(
                    merge_tensor(&base, &[&[1.0f32][..]], &Method::Ties { density: 1.0 }, 1.0)
                        .is_none()
                );
            }

            #[test]
            fn screening() {
                // tau0 = (1,0), tau1 = (1,0) duplicate, tau2 = (0,0), tau3 = (10,0) diverged
                let gram = vec![
                    vec![1.0, 1.0, 0.0, 10.0],
                    vec![1.0, 1.0, 0.0, 10.0],
                    vec![0.0, 0.0, 0.0, 0.0],
                    vec![10.0, 10.0, 0.0, 100.0],
                ];
                let v = screen(&gram, 25.0, 1.0, 0.99);
                assert_eq!(v[0], Screen::Keep);
                assert_eq!(v[1], Screen::Duplicate(0));
                assert_eq!(v[2], Screen::IdenticalToBase);
                assert!(matches!(v[3], Screen::Diverged(r) if (r - 2.0).abs() < 1e-9));
            }
        }
    }

    pub mod robust {
        //! Robust score, shortcut reliance and the gap law.
        //!
        //! A closure test runs the same model under controlled hidden conditions:
        //! a clean reference, and shortcuts (answer position, planted hint) that
        //! point to the right answer with probability `p`. The robust score S is the
        //! worst accuracy over the closure conditions.

        use crate::theory::K;
        use std::collections::BTreeMap;

        /// Shortcut validity on familiar data.
        pub const FAMILIAR: f64 = 0.95;

        /// Kind of planted shortcut.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Shortcut {
            /// Correct answer tends to sit at a fixed position.
            Position,
            /// A hint names an answer.
            Hint,
        }

        /// One test condition: no shortcut, or a shortcut that is right with probability `p` (in percent).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Condition {
            /// No shortcut.
            Clean,
            /// Shortcut right with `percent`% probability.
            Cue(Shortcut, u8),
        }

        impl Condition {
            /// Probability that the cue points to the correct answer.
            #[must_use]
            pub fn p(self) -> Option<f64> {
                match self {
                    Self::Clean => None,
                    Self::Cue(_, pct) => Some(f64::from(pct) / 100.0),
                }
            }
        }

        /// Conditions whose worst case defines S.
        pub const CLOSURE: [Condition; 5] = [
            Condition::Clean,
            Condition::Cue(Shortcut::Position, 95),
            Condition::Cue(Shortcut::Position, 0),
            Condition::Cue(Shortcut::Hint, 95),
            Condition::Cue(Shortcut::Hint, 0),
        ];

        /// Held-out shift conditions (not used for S).
        pub const HELDOUT: [Condition; 2] = [
            Condition::Cue(Shortcut::Position, 25),
            Condition::Cue(Shortcut::Hint, 25),
        ];

        /// Cue informativeness: `(p - 1/K) / (1 - 1/K)`; 1 = always right, 0 = chance.
        #[must_use]
        pub fn q_of(p: f64) -> f64 {
            let k = K as f64;
            (p - 1.0 / k) / (1.0 - 1.0 / k)
        }

        /// Interventional reliance on a shortcut, measured on familiar items:
        /// how much more often the model follows the cue when it is present than in
        /// the counterfactual prompt without it, rescaled to [0, 1].
        #[must_use]
        pub fn reliance(follow: usize, follow_counterfactual: usize, n: usize) -> f64 {
            if n == 0 {
                return 0.0;
            }
            let k = K as f64;
            (follow as f64 - follow_counterfactual as f64) / n as f64 / (1.0 - 1.0 / k)
        }

        /// Gap law: predicted accuracy when the cue validity shifts from familiar to `p_new`.
        ///
        /// `predicted = acc_familiar + reliance * (q(p_new) - q(FAMILIAR)) * (1 - 1/K)`.
        #[must_use]
        pub fn gap_law(acc_familiar: f64, reliance: f64, p_new: f64) -> f64 {
            let k = K as f64;
            acc_familiar + reliance * (q_of(p_new) - q_of(FAMILIAR)) * (1.0 - 1.0 / k)
        }

        /// Gap-law check for one condition.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct GapCheck {
            /// Gap-law prediction.
            pub predicted: f64,
            /// Measured accuracy.
            pub actual: f64,
        }

        impl GapCheck {
            /// `|predicted - actual|`.
            #[must_use]
            pub fn abs_error(&self) -> f64 {
                (self.predicted - self.actual).abs()
            }
        }

        /// Summary of a closure test.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Summary {
            /// Worst closure accuracy (robust score S).
            pub robust_s: Option<f64>,
            /// Mean closure accuracy.
            pub closure_mean: Option<f64>,
            /// Mean held-out accuracy.
            pub heldout_mean: Option<f64>,
            /// Mean accuracy on the two familiar (95%) conditions.
            pub familiar_mean: Option<f64>,
            /// Gap-law predictions vs. measurements for the shifted conditions.
            pub gap: BTreeMap<Condition, GapCheck>,
        }

        fn mean(xs: &[f64]) -> Option<f64> {
            (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
        }

        /// Summarise per-condition accuracy and measured reliance.
        #[must_use]
        pub fn summarise(acc: &BTreeMap<Condition, f64>, rel: &BTreeMap<Shortcut, f64>) -> Summary {
            let pick = |cs: &[Condition]| -> Vec<f64> {
                cs.iter().filter_map(|c| acc.get(c).copied()).collect()
            };
            let closure = pick(&CLOSURE);
            let held = pick(&HELDOUT);
            let familiar = pick(&[
                Condition::Cue(Shortcut::Position, 95),
                Condition::Cue(Shortcut::Hint, 95),
            ]);
            let mut gap = BTreeMap::new();
            for (&s, &r) in rel {
                let Some(&fam) = acc.get(&Condition::Cue(s, 95)) else {
                    continue;
                };
                for pct in [25u8, 0] {
                    let c = Condition::Cue(s, pct);
                    if let Some(&actual) = acc.get(&c) {
                        gap.insert(
                            c,
                            GapCheck {
                                predicted: gap_law(fam, r, f64::from(pct) / 100.0),
                                actual,
                            },
                        );
                    }
                }
            }
            Summary {
                robust_s: closure.iter().copied().reduce(f64::min),
                closure_mean: mean(&closure),
                heldout_mean: mean(&held),
                familiar_mean: mean(&familiar),
                gap,
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn q_and_gap_law() {
                assert!((q_of(1.0) - 1.0).abs() < 1e-12);
                assert!(q_of(0.25).abs() < 1e-12);
                // No reliance: accuracy does not move under shift.
                assert!((gap_law(0.8, 0.0, 0.0) - 0.8).abs() < 1e-12);
                // Full reliance, cue flips from 95% right to never right.
                let drop = gap_law(0.95, 1.0, 0.0);
                assert!((drop - (0.95 + (q_of(0.0) - q_of(0.95)) * 0.75)).abs() < 1e-12);
                assert!(drop.abs() < 1e-9);
            }

            #[test]
            fn reliance_scale() {
                assert!((reliance(100, 25, 100) - 1.0).abs() < 1e-12);
                assert_eq!(reliance(5, 5, 10), 0.0);
                assert_eq!(reliance(1, 0, 0), 0.0);
            }

            #[test]
            fn summary_takes_worst_case() {
                let mut acc = BTreeMap::new();
                acc.insert(Condition::Clean, 0.7);
                acc.insert(Condition::Cue(Shortcut::Position, 95), 0.9);
                acc.insert(Condition::Cue(Shortcut::Position, 0), 0.4);
                acc.insert(Condition::Cue(Shortcut::Position, 25), 0.6);
                acc.insert(Condition::Cue(Shortcut::Hint, 95), 0.8);
                acc.insert(Condition::Cue(Shortcut::Hint, 0), 0.6);
                let mut rel = BTreeMap::new();
                rel.insert(Shortcut::Position, 0.5);
                let s = summarise(&acc, &rel);
                assert_eq!(s.robust_s, Some(0.4));
                assert!((s.familiar_mean.unwrap() - 0.85).abs() < 1e-12);
                assert_eq!(s.heldout_mean, Some(0.6));
                assert_eq!(s.gap.len(), 2);
                let g = s.gap[&Condition::Cue(Shortcut::Position, 0)];
                assert!((g.predicted - gap_law(0.9, 0.5, 0.0)).abs() < 1e-12);
                assert!((g.abs_error() - (g.predicted - 0.4).abs()).abs() < 1e-12);
            }
        }
    }

    pub mod select {
        //! Model selection by robust score.

        /// One evaluated model.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Entry {
            /// Name.
            pub name: String,
            /// Parameter count, if known.
            pub parameters: Option<u64>,
            /// Robust score S.
            pub robust_s: f64,
        }

        /// Rank by S (highest first) and recommend: the cheapest model with
        /// `S >= threshold`, or the highest S when no threshold is given.
        #[must_use]
        pub fn leaderboard(
            mut rows: Vec<Entry>,
            threshold: Option<f64>,
        ) -> (Vec<Entry>, Option<Entry>) {
            rows.sort_by(|a, b| b.robust_s.total_cmp(&a.robust_s));
            let rec = match threshold {
                Some(t) => rows
                    .iter()
                    .filter(|r| r.robust_s >= t)
                    .filter_map(|r| r.parameters.map(|p| (p, r)))
                    .min_by_key(|(p, _)| *p)
                    .map(|(_, r)| r.clone()),
                None => rows.first().cloned(),
            };
            (rows, rec)
        }

        /// Pick the merge weight `t` with the highest S; ties go to the `t` closest to 0.5.
        #[must_use]
        pub fn choose_merge_weight(trials: &[(f64, f64)]) -> Option<f64> {
            trials
                .iter()
                .filter(|(t, s)| t.is_finite() && s.is_finite())
                .max_by(|a, b| {
                    a.1.total_cmp(&b.1)
                        .then((b.0 - 0.5).abs().total_cmp(&(a.0 - 0.5).abs()))
                })
                .map(|&(t, _)| t)
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn e(name: &str, p: u64, s: f64) -> Entry {
                Entry {
                    name: name.into(),
                    parameters: Some(p),
                    robust_s: s,
                }
            }

            #[test]
            fn cheapest_above_threshold() {
                let rows = vec![e("big", 70, 0.8), e("mid", 8, 0.62), e("tiny", 1, 0.4)];
                let (ranked, rec) = leaderboard(rows.clone(), Some(0.6));
                assert_eq!(ranked[0].name, "big");
                assert_eq!(rec.unwrap().name, "mid");
                assert_eq!(leaderboard(rows.clone(), None).1.unwrap().name, "big");
                assert!(leaderboard(rows, Some(0.9)).1.is_none());
            }

            #[test]
            fn merge_weight_ties_prefer_half() {
                assert_eq!(
                    choose_merge_weight(&[(0.2, 0.7), (0.5, 0.7), (0.9, 0.6)]),
                    Some(0.5)
                );
                assert_eq!(choose_merge_weight(&[(0.3, 0.1), (0.8, 0.9)]), Some(0.8));
                assert_eq!(choose_merge_weight(&[]), None);
            }
        }
    }
}

pub mod compress {
    //! Weight analysis for compression planning.
    //!
    //! Model-independent numerics normalised from the 55D research engine. Each function
    //! takes plain `f32` slices, so it works on tensors read by `mouli-llm::gguf` or produced
    //! anywhere else. These functions *measure*: none changes a model, and a plan (for
    //! example a pruning threshold) is not evidence that the planned model still works.
    //!
    //! Differences from the Python original, all deliberate:
    //!
    //! * Row sampling in [`stats::near_duplicate_fraction`] is evenly strided, not random,
    //!   so results are reproducible without an RNG.
    //! * Singular values come from a Jacobi eigen-solver on the Gram matrix, so large
    //!   matrices are cropped to `max_dim` (default [`lowrank::DEFAULT_MAX_DIM`]) before
    //!   analysis; the original cropped to 2048 using LAPACK.

    use std::fmt;

    /// Invalid input to an analysis function.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AnalysisError {
        /// `data.len()` is not `rows * cols`, or a dimension is zero.
        BadShape,
        /// A parameter is out of range (the text says which).
        BadParameter(&'static str),
        /// The input contains NaN or infinity.
        NonFinite,
    }

    impl fmt::Display for AnalysisError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                AnalysisError::BadShape => f.write_str("data length does not match the shape"),
                AnalysisError::BadParameter(p) => write!(f, "invalid parameter: {p}"),
                AnalysisError::NonFinite => f.write_str("input contains NaN or infinity"),
            }
        }
    }

    impl std::error::Error for AnalysisError {}

    /// A row-major matrix view.
    #[derive(Debug, Clone, Copy)]
    pub struct Matrix<'a> {
        /// Row-major elements.
        pub data: &'a [f32],
        /// Row count (at least 1).
        pub rows: usize,
        /// Column count (at least 1).
        pub cols: usize,
    }

    impl<'a> Matrix<'a> {
        /// Validate and wrap a row-major buffer.
        pub fn new(data: &'a [f32], rows: usize, cols: usize) -> Result<Self, AnalysisError> {
            if rows == 0 || cols == 0 || rows.checked_mul(cols) != Some(data.len()) {
                return Err(AnalysisError::BadShape);
            }
            if data.iter().any(|x| !x.is_finite()) {
                return Err(AnalysisError::NonFinite);
            }
            Ok(Matrix { data, rows, cols })
        }

        /// One row.
        pub fn row(&self, r: usize) -> &'a [f32] {
            &self.data[r * self.cols..(r + 1) * self.cols]
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn matrix_validates_shape_and_values() {
            assert!(Matrix::new(&[1.0, 2.0, 3.0, 4.0], 2, 2).is_ok());
            assert_eq!(
                Matrix::new(&[1.0; 3], 2, 2).unwrap_err(),
                AnalysisError::BadShape
            );
            assert_eq!(Matrix::new(&[], 0, 2).unwrap_err(), AnalysisError::BadShape);
            assert_eq!(
                Matrix::new(&[1.0, f32::NAN], 1, 2).unwrap_err(),
                AnalysisError::NonFinite
            );
            assert_eq!(
                Matrix::new(&[1.0, f32::INFINITY], 1, 2).unwrap_err(),
                AnalysisError::NonFinite
            );
            let m = Matrix::new(&[1.0, 2.0, 3.0, 4.0], 2, 2).unwrap();
            assert_eq!(m.row(1), &[3.0, 4.0]);
        }
    }

    pub mod lowrank {
        //! Singular values, effective rank and low-rank approximation error.
        //!
        //! Singular values are computed as the square roots of the eigenvalues of the smaller
        //! Gram matrix (`AᵀA` or `AAᵀ`) with a cyclic Jacobi solver in `f64`. That is accurate
        //! enough for analysis (small singular values lose relative precision) and needs no
        //! linear-algebra dependency.

        use crate::compress::{AnalysisError, Matrix};

        /// Largest matrix side analysed by default; bigger inputs are cropped to the top-left block.
        pub const DEFAULT_MAX_DIM: usize = 256;

        const MAX_SWEEPS: usize = 64;

        /// Eigenvalues of a symmetric `n x n` matrix (row-major, destroyed), unordered.
        fn jacobi_eigenvalues(a: &mut [f64], n: usize) -> Vec<f64> {
            for _ in 0..MAX_SWEEPS {
                let mut off = 0.0f64;
                let mut diag = 0.0f64;
                for i in 0..n {
                    diag += a[i * n + i] * a[i * n + i];
                    for j in i + 1..n {
                        off += a[i * n + j] * a[i * n + j];
                    }
                }
                if off <= 1e-24 * (diag + off).max(f64::MIN_POSITIVE) {
                    break;
                }
                for p in 0..n {
                    for q in p + 1..n {
                        let apq = a[p * n + q];
                        if apq.abs() < 1e-300 {
                            continue;
                        }
                        let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                        let t = if theta == 0.0 { 1.0 } else { t };
                        let c = 1.0 / (t * t + 1.0).sqrt();
                        let s = t * c;
                        for k in 0..n {
                            let (akp, akq) = (a[k * n + p], a[k * n + q]);
                            a[k * n + p] = c * akp - s * akq;
                            a[k * n + q] = s * akp + c * akq;
                        }
                        for k in 0..n {
                            let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                            a[p * n + k] = c * apk - s * aqk;
                            a[q * n + k] = s * apk + c * aqk;
                        }
                    }
                }
            }
            (0..n).map(|i| a[i * n + i]).collect()
        }

        /// Squared singular values in descending order, from the top-left
        /// `min(rows, max_dim) x min(cols, max_dim)` block of `m`.
        pub fn singular_values_squared(
            m: Matrix<'_>,
            max_dim: usize,
        ) -> Result<Vec<f64>, AnalysisError> {
            if max_dim == 0 {
                return Err(AnalysisError::BadParameter("max_dim must be at least 1"));
            }
            let (r, c) = (m.rows.min(max_dim), m.cols.min(max_dim));
            let at = |i: usize, j: usize| f64::from(m.data[i * m.cols + j]);
            // Gram matrix of the smaller side.
            let n = r.min(c);
            let mut g = vec![0.0f64; n * n];
            if c <= r {
                for i in 0..r {
                    for p in 0..c {
                        let aip = at(i, p);
                        if aip != 0.0 {
                            for q in p..c {
                                g[p * n + q] += aip * at(i, q);
                            }
                        }
                    }
                }
            } else {
                for p in 0..r {
                    for q in p..r {
                        let mut s = 0.0;
                        for j in 0..c {
                            s += at(p, j) * at(q, j);
                        }
                        g[p * n + q] = s;
                    }
                }
            }
            for p in 0..n {
                for q in 0..p {
                    g[p * n + q] = g[q * n + p];
                }
            }
            let mut ev = jacobi_eigenvalues(&mut g, n);
            for e in &mut ev {
                if *e < 0.0 {
                    *e = 0.0; // rounding noise
                }
            }
            ev.sort_by(|a, b| b.total_cmp(a));
            Ok(ev)
        }

        /// Smallest rank whose singular values capture at least `energy_threshold` of the total
        /// squared energy (the original's `searchsorted(...) + 1`). `0` for an all-zero matrix.
        /// `energy_threshold` must be in `(0, 1]`.
        pub fn effective_rank(
            m: Matrix<'_>,
            energy_threshold: f64,
            max_dim: usize,
        ) -> Result<usize, AnalysisError> {
            if !(energy_threshold > 0.0 && energy_threshold <= 1.0) {
                return Err(AnalysisError::BadParameter(
                    "energy_threshold must be in (0, 1]",
                ));
            }
            let power = singular_values_squared(m, max_dim)?;
            let total: f64 = power.iter().sum();
            if total <= 0.0 {
                return Ok(0);
            }
            let mut cum = 0.0;
            for (i, p) in power.iter().enumerate() {
                cum += p;
                if cum / total >= energy_threshold - 1e-12 {
                    return Ok(i + 1);
                }
            }
            Ok(power.len())
        }

        /// Rank used for a given parameter ratio: `max(1, floor(min(rows, cols) * ratio))`.
        /// `ratio` must be in `(0, 1]`.
        pub fn rank_for_ratio(
            rows: usize,
            cols: usize,
            ratio: f64,
        ) -> Result<usize, AnalysisError> {
            if !(ratio > 0.0 && ratio <= 1.0) {
                return Err(AnalysisError::BadParameter("ratio must be in (0, 1]"));
            }
            Ok(((rows.min(cols) as f64 * ratio) as usize).max(1))
        }

        /// Relative Frobenius error of the best rank-`rank` approximation:
        /// `‖A - A_k‖_F / (‖A‖_F + 1e-12)`. By the Eckart–Young theorem this equals
        /// `sqrt(Σ_{i>k} σᵢ²)` over the full norm, so no reconstruction is needed.
        /// Computed on the same cropped block as [`singular_values_squared`].
        pub fn low_rank_relative_error(
            m: Matrix<'_>,
            rank: usize,
            max_dim: usize,
        ) -> Result<f64, AnalysisError> {
            if rank == 0 {
                return Err(AnalysisError::BadParameter("rank must be at least 1"));
            }
            let power = singular_values_squared(m, max_dim)?;
            let total: f64 = power.iter().sum();
            let tail: f64 = power.iter().skip(rank).sum();
            Ok(tail.sqrt() / (total.sqrt() + 1e-12))
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn mat(data: &[f32], r: usize, c: usize) -> Matrix<'_> {
                Matrix::new(data, r, c).unwrap()
            }

            #[test]
            fn diagonal_matrix_singular_values() {
                let d = [3.0f32, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 1.0];
                let s = singular_values_squared(mat(&d, 3, 3), 256).unwrap();
                for (got, want) in s.iter().zip([16.0, 9.0, 1.0]) {
                    assert!((got - want).abs() < 1e-9, "{s:?}");
                }
            }

            #[test]
            fn tall_and_wide_agree() {
                let a: Vec<f32> = (0..24).map(|i| ((i * 5) % 11) as f32 - 4.0).collect();
                let tall = singular_values_squared(mat(&a, 6, 4), 256).unwrap();
                // transpose
                let mut t = vec![0.0f32; 24];
                for i in 0..6 {
                    for j in 0..4 {
                        t[j * 6 + i] = a[i * 4 + j];
                    }
                }
                let wide = singular_values_squared(mat(&t, 4, 6), 256).unwrap();
                assert_eq!(tall.len(), 4);
                for (x, y) in tall.iter().zip(&wide) {
                    assert!((x - y).abs() < 1e-6 * x.max(1.0), "{tall:?} vs {wide:?}");
                }
            }

            #[test]
            fn energy_equals_squared_frobenius_norm() {
                let a: Vec<f32> = (0..30).map(|i| (i as f32).sin() * 3.0).collect();
                let s = singular_values_squared(mat(&a, 5, 6), 256).unwrap();
                let frob: f64 = a.iter().map(|v| f64::from(*v).powi(2)).sum();
                assert!((s.iter().sum::<f64>() - frob).abs() < 1e-6 * frob);
            }

            #[test]
            fn rank_one_matrix_has_effective_rank_one() {
                // outer product u v^T
                let u = [1.0f32, 2.0, 3.0, 4.0];
                let v = [2.0f32, -1.0, 0.5];
                let mut a = Vec::new();
                for x in u {
                    for y in v {
                        a.push(x * y);
                    }
                }
                assert_eq!(effective_rank(mat(&a, 4, 3), 0.99, 256).unwrap(), 1);
                let s = singular_values_squared(mat(&a, 4, 3), 256).unwrap();
                assert!(s[1] < 1e-9 * s[0]);
            }

            #[test]
            fn identity_needs_every_dimension_for_full_energy() {
                let mut i = vec![0.0f32; 16];
                for k in 0..4 {
                    i[k * 4 + k] = 1.0;
                }
                assert_eq!(effective_rank(mat(&i, 4, 4), 1.0, 256).unwrap(), 4);
                assert_eq!(effective_rank(mat(&i, 4, 4), 0.5, 256).unwrap(), 2);
                assert_eq!(effective_rank(mat(&i, 4, 4), 0.99, 256).unwrap(), 4);
            }

            #[test]
            fn zero_matrix_has_rank_zero() {
                assert_eq!(effective_rank(mat(&[0.0; 6], 2, 3), 0.99, 256).unwrap(), 0);
            }

            #[test]
            fn low_rank_error_is_exact_for_a_diagonal() {
                let d = [3.0f32, 0.0, 0.0, 4.0];
                // keep the largest (4): tail is 3 over sqrt(25) = 5
                let e = low_rank_relative_error(mat(&d, 2, 2), 1, 256).unwrap();
                assert!((e - 0.6).abs() < 1e-9, "{e}");
                assert!(low_rank_relative_error(mat(&d, 2, 2), 2, 256).unwrap() < 1e-9);
                assert!(low_rank_relative_error(mat(&d, 2, 2), 5, 256).unwrap() < 1e-9);
            }

            #[test]
            fn error_decreases_with_rank() {
                let a: Vec<f32> = (0..64).map(|i| ((i * 13) % 17) as f32 - 8.0).collect();
                let m = mat(&a, 8, 8);
                let mut last = f64::MAX;
                for r in 1..=8 {
                    let e = low_rank_relative_error(m, r, 256).unwrap();
                    assert!(e <= last + 1e-12);
                    last = e;
                }
                assert!(last < 1e-6);
            }

            #[test]
            fn cropping_to_max_dim() {
                let a: Vec<f32> = (0..100).map(|i| (i % 9) as f32).collect();
                let cropped = singular_values_squared(mat(&a, 10, 10), 3).unwrap();
                assert_eq!(cropped.len(), 3);
                assert!(singular_values_squared(mat(&a, 10, 10), 0).is_err());
            }

            #[test]
            fn rank_for_ratio_rules() {
                assert_eq!(rank_for_ratio(10, 20, 0.5).unwrap(), 5);
                assert_eq!(rank_for_ratio(10, 20, 0.01).unwrap(), 1);
                assert_eq!(rank_for_ratio(10, 20, 1.0).unwrap(), 10);
                assert!(rank_for_ratio(10, 20, 0.0).is_err());
                assert!(rank_for_ratio(10, 20, 1.5).is_err());
            }

            #[test]
            fn parameter_validation() {
                let m = mat(&[1.0, 0.0, 0.0, 1.0], 2, 2);
                assert!(effective_rank(m, 0.0, 8).is_err());
                assert!(effective_rank(m, 1.5, 8).is_err());
                assert!(low_rank_relative_error(m, 0, 8).is_err());
            }
        }
    }

    pub mod prune {
        //! Magnitude pruning plans.

        use crate::compress::AnalysisError;

        /// The global magnitude threshold that would prune the smallest `ratio` of all weights:
        /// the `k`-th smallest absolute value with `k = clamp(floor(ratio * n), 1, n)`.
        ///
        /// This is a *plan*. Weights with `|w| <= threshold` are the pruning candidates; whether
        /// the pruned model still works is a separate experiment. `ratio` must be in `[0, 1]`.
        /// `None` when there are no weights.
        pub fn global_magnitude_threshold(
            tensors: &[&[f32]],
            ratio: f64,
        ) -> Result<Option<f32>, AnalysisError> {
            if !(0.0..=1.0).contains(&ratio) || ratio.is_nan() {
                return Err(AnalysisError::BadParameter("ratio must be in [0, 1]"));
            }
            let mut all: Vec<f32> = Vec::with_capacity(tensors.iter().map(|t| t.len()).sum());
            for t in tensors {
                for v in *t {
                    if !v.is_finite() {
                        return Err(AnalysisError::NonFinite);
                    }
                    all.push(v.abs());
                }
            }
            if all.is_empty() {
                return Ok(None);
            }
            let n = all.len();
            let k = ((ratio * n as f64) as usize).clamp(1, n);
            let (_, kth, _) = all.select_nth_unstable_by(k - 1, |a, b| a.total_cmp(b));
            Ok(Some(*kth))
        }

        /// Fraction of weights with `|w| <= threshold` (the share a plan would prune).
        pub fn pruned_fraction(tensors: &[&[f32]], threshold: f32) -> f64 {
            let total: usize = tensors.iter().map(|t| t.len()).sum();
            if total == 0 {
                return 0.0;
            }
            let pruned: usize = tensors
                .iter()
                .map(|t| t.iter().filter(|v| v.abs() <= threshold).count())
                .sum();
            pruned as f64 / total as f64
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn threshold_is_the_kth_smallest_magnitude() {
                let a = [1.0f32, -2.0, 3.0];
                let b = [-4.0f32, 5.0, 0.5, 6.0, -7.0, 8.0, 9.0];
                let t = [&a[..], &b[..]];
                // sorted magnitudes: 0.5 1 2 3 4 5 6 7 8 9 ; 10 weights
                assert_eq!(global_magnitude_threshold(&t, 0.5).unwrap(), Some(4.0));
                assert_eq!(global_magnitude_threshold(&t, 1.0).unwrap(), Some(9.0));
                assert_eq!(global_magnitude_threshold(&t, 0.0).unwrap(), Some(0.5)); // k clamps to 1
                assert_eq!(global_magnitude_threshold(&t, 0.05).unwrap(), Some(0.5));
            }

            #[test]
            fn pruned_fraction_matches_the_plan() {
                let w: Vec<f32> = (1..=100).map(|i| i as f32).collect();
                let t = [&w[..]];
                let th = global_magnitude_threshold(&t, 0.3).unwrap().unwrap();
                assert_eq!(th, 30.0);
                assert!((pruned_fraction(&t, th) - 0.3).abs() < 1e-12);
                assert_eq!(pruned_fraction(&[], 1.0), 0.0);
            }

            #[test]
            fn empty_and_invalid() {
                assert_eq!(global_magnitude_threshold(&[], 0.5).unwrap(), None);
                assert_eq!(global_magnitude_threshold(&[&[]], 0.5).unwrap(), None);
                assert!(global_magnitude_threshold(&[&[1.0]], -0.1).is_err());
                assert!(global_magnitude_threshold(&[&[1.0]], 1.1).is_err());
                assert!(global_magnitude_threshold(&[&[1.0]], f64::NAN).is_err());
                assert_eq!(
                    global_magnitude_threshold(&[&[f32::NAN]], 0.5).unwrap_err(),
                    AnalysisError::NonFinite
                );
            }

            #[test]
            fn ties_are_handled() {
                let w = [2.0f32; 10];
                assert_eq!(
                    global_magnitude_threshold(&[&w[..]], 0.5).unwrap(),
                    Some(2.0)
                );
                assert_eq!(pruned_fraction(&[&w[..]], 2.0), 1.0);
            }
        }
    }

    pub mod quant {
        //! Quantisation error.

        use crate::compress::AnalysisError;

        /// Relative L2 error of symmetric per-tensor integer quantisation at `bits` bits:
        /// `‖dequant(quant(x)) - x‖ / (‖x‖ + 1e-12)`, with `qmax = 2^(bits-1) - 1` and
        /// `scale = max|x| / qmax` (floored at 1e-12).
        ///
        /// This measures representational error only; it never alters the weights.
        /// `bits` must be 2..=16. Empty input gives `0.0`.
        pub fn symmetric_quantization_error(x: &[f32], bits: u32) -> Result<f64, AnalysisError> {
            if !(2..=16).contains(&bits) {
                return Err(AnalysisError::BadParameter("bits must be 2..=16"));
            }
            if x.iter().any(|v| !v.is_finite()) {
                return Err(AnalysisError::NonFinite);
            }
            let qmax = f64::from((1u32 << (bits - 1)) - 1);
            let max_abs = x.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max);
            let scale = (max_abs / qmax.max(1.0)).max(1e-12);
            let (mut err, mut norm) = (0.0f64, 0.0f64);
            for v in x {
                let v = f64::from(*v);
                let q = (v / scale).round().clamp(-qmax, qmax);
                err += (q * scale - v).powi(2);
                norm += v * v;
            }
            Ok(err.sqrt() / (norm.sqrt() + 1e-12))
        }

        /// Mean and maximum of [`symmetric_quantization_error`] over several tensors, with the
        /// number measured. Empty tensors are skipped. `None` mean/max when nothing was measured.
        pub fn quantization_summary(
            tensors: &[&[f32]],
            bits: u32,
        ) -> Result<(Option<f64>, Option<f64>, usize), AnalysisError> {
            let mut errs = Vec::new();
            for t in tensors.iter().filter(|t| !t.is_empty()) {
                errs.push(symmetric_quantization_error(t, bits)?);
            }
            if errs.is_empty() {
                return Ok((None, None, 0));
            }
            let mean = errs.iter().sum::<f64>() / errs.len() as f64;
            let max = errs.iter().copied().fold(f64::MIN, f64::max);
            Ok((Some(mean), Some(max), errs.len()))
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn exactly_representable_values_have_zero_error() {
                // 8-bit: qmax 127, values k * (127/127) are exact when max_abs = 127
                let x: Vec<f32> = (-127..=127).map(|v| v as f32).collect();
                assert!(symmetric_quantization_error(&x, 8).unwrap() < 1e-9);
            }

            #[test]
            fn more_bits_means_less_error() {
                let x: Vec<f32> = (0..500)
                    .map(|i| ((i * 37) % 200) as f32 / 10.0 - 10.0)
                    .collect();
                let e4 = symmetric_quantization_error(&x, 4).unwrap();
                let e8 = symmetric_quantization_error(&x, 8).unwrap();
                let e16 = symmetric_quantization_error(&x, 16).unwrap();
                assert!(e4 > e8 && e8 > e16, "{e4} {e8} {e16}");
                assert!(e16 < 1e-4);
            }

            #[test]
            fn two_bit_is_ternary() {
                // bits=2 -> qmax 1: values snap to {-s, 0, s}, s = max|x|
                let e = symmetric_quantization_error(&[1.0, -1.0, 0.4, -0.4], 2).unwrap();
                // errors 0, 0, 0.4, 0.4 over norm sqrt(2.32)
                let expected = (0.32f64).sqrt() / (2.32f64).sqrt();
                assert!((e - expected).abs() < 1e-6, "{e} vs {expected}");
            }

            #[test]
            fn zeros_and_empty() {
                assert_eq!(symmetric_quantization_error(&[0.0; 8], 8).unwrap(), 0.0);
                assert_eq!(symmetric_quantization_error(&[], 8).unwrap(), 0.0);
            }

            #[test]
            fn rejects_bad_parameters() {
                assert!(symmetric_quantization_error(&[1.0], 1).is_err());
                assert!(symmetric_quantization_error(&[1.0], 17).is_err());
                assert_eq!(
                    symmetric_quantization_error(&[f32::NAN], 8).unwrap_err(),
                    AnalysisError::NonFinite
                );
            }

            #[test]
            fn summary_skips_empty_tensors() {
                let a: Vec<f32> = (0..64).map(|i| i as f32).collect();
                let b: Vec<f32> = (0..64).map(|i| (i * i % 13) as f32).collect();
                let (mean, max, n) = quantization_summary(&[&a, &[], &b], 4).unwrap();
                assert_eq!(n, 2);
                let (ea, eb) = (
                    symmetric_quantization_error(&a, 4).unwrap(),
                    symmetric_quantization_error(&b, 4).unwrap(),
                );
                assert!((mean.unwrap() - (ea + eb) / 2.0).abs() < 1e-12);
                assert!((max.unwrap() - ea.max(eb)).abs() < 1e-12);
                assert_eq!(quantization_summary(&[], 4).unwrap(), (None, None, 0));
            }
        }
    }

    pub mod rvq {
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
    }

    pub mod stats {
        //! Distribution statistics of weights.

        use crate::compress::{AnalysisError, Matrix};

        /// Mean and sample standard deviation (n - 1). `None` for fewer than two values.
        pub fn mean_std(x: &[f32]) -> Option<(f64, f64)> {
            if x.len() < 2 {
                return None;
            }
            let n = x.len() as f64;
            let mean = x.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
            let var = x
                .iter()
                .map(|v| (f64::from(*v) - mean).powi(2))
                .sum::<f64>()
                / (n - 1.0);
            Some((mean, var.sqrt()))
        }

        /// Shannon entropy in bits of an equal-width histogram over `[min, max]`.
        ///
        /// `None` for empty input; `0.0` when all values are (nearly) equal. The last bin
        /// includes the maximum. `bins` must be at least 1.
        pub fn entropy_bits(x: &[f32], bins: usize) -> Result<Option<f64>, AnalysisError> {
            if bins == 0 {
                return Err(AnalysisError::BadParameter("bins must be at least 1"));
            }
            if x.is_empty() {
                return Ok(None);
            }
            if x.iter().any(|v| !v.is_finite()) {
                return Err(AnalysisError::NonFinite);
            }
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for v in x {
                lo = lo.min(f64::from(*v));
                hi = hi.max(f64::from(*v));
            }
            // numpy.allclose(min, max) with its default tolerances.
            if (lo - hi).abs() <= 1e-8 + 1e-5 * hi.abs() {
                return Ok(Some(0.0));
            }
            let width = (hi - lo) / bins as f64;
            let mut counts = vec![0u64; bins];
            for v in x {
                let idx = (((f64::from(*v) - lo) / width) as usize).min(bins - 1);
                counts[idx] += 1;
            }
            let total = x.len() as f64;
            let h = counts
                .iter()
                .filter(|c| **c > 0)
                .map(|c| {
                    let p = *c as f64 / total;
                    -p * p.log2()
                })
                .sum();
            Ok(Some(h))
        }

        /// Fraction of values with `|x| > |mean| + sigma * std`. `None` when the std is zero or
        /// there are fewer than two values.
        pub fn outlier_fraction(x: &[f32], sigma: f64) -> Option<f64> {
            let (mean, std) = mean_std(x)?;
            if std <= 0.0 {
                return None;
            }
            let limit = mean.abs() + sigma * std;
            let n = x.iter().filter(|v| f64::from(v.abs()) > limit).count();
            Some(n as f64 / x.len() as f64)
        }

        /// Fraction of adjacent (after ordering) unit-normalised rows whose cosine similarity
        /// is at least `threshold`, over at most `sample_count` evenly strided rows.
        ///
        /// This is the original's approximation: sorting by the first coordinate and comparing
        /// neighbours avoids an N² comparison. Zero rows are ignored. `None` with fewer than
        /// two usable rows.
        pub fn near_duplicate_fraction(
            m: Matrix<'_>,
            threshold: f64,
            sample_count: usize,
        ) -> Option<f64> {
            if m.rows < 2 || sample_count == 0 {
                return None;
            }
            let count = sample_count.min(m.rows);
            let mut rows: Vec<Vec<f64>> = Vec::with_capacity(count);
            for i in 0..count {
                let r = m.row(i * m.rows / count);
                let norm = r.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>().sqrt();
                if norm > 1e-12 {
                    rows.push(r.iter().map(|v| f64::from(*v) / norm).collect());
                }
            }
            if rows.len() < 2 {
                return None;
            }
            rows.sort_by(|a, b| a[0].total_cmp(&b[0]));
            let hits = rows
                .windows(2)
                .filter(|w| w[0].iter().zip(&w[1]).map(|(a, b)| a * b).sum::<f64>() >= threshold)
                .count();
            Some(hits as f64 / (rows.len() - 1) as f64)
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn mean_std_is_sample_std() {
                let (m, s) = mean_std(&[1.0, 2.0, 3.0, 4.0]).unwrap();
                assert!((m - 2.5).abs() < 1e-12);
                assert!((s - (5.0f64 / 3.0).sqrt()).abs() < 1e-12);
                assert!(mean_std(&[1.0]).is_none());
                assert!(mean_std(&[]).is_none());
            }

            #[test]
            fn entropy_of_uniform_and_degenerate() {
                // 4 bins, one value each at the bin centres -> exactly 2 bits.
                let h = entropy_bits(&[0.0, 1.0, 2.0, 3.0], 4).unwrap().unwrap();
                assert!((h - 2.0).abs() < 1e-12);
                assert_eq!(entropy_bits(&[5.0, 5.0, 5.0], 16).unwrap(), Some(0.0));
                assert_eq!(entropy_bits(&[], 16).unwrap(), None);
                // everything in one bin except the max: two occupied bins
                let h = entropy_bits(&[0.0, 0.0, 0.0, 1.0], 2).unwrap().unwrap();
                assert!((h - 0.811_278_124_459_132_9).abs() < 1e-9);
            }

            #[test]
            fn entropy_rejects_bad_input() {
                assert!(entropy_bits(&[1.0], 0).is_err());
                assert_eq!(
                    entropy_bits(&[1.0, f32::NAN], 4).unwrap_err(),
                    AnalysisError::NonFinite
                );
            }

            #[test]
            fn entropy_never_exceeds_log2_bins() {
                let x: Vec<f32> = (0..1000).map(|i| ((i * 7919) % 1000) as f32).collect();
                let h = entropy_bits(&x, 128).unwrap().unwrap();
                assert!(h > 6.5 && h <= 7.0 + 1e-9, "{h}");
            }

            #[test]
            fn outliers_use_abs_mean_plus_sigma_std() {
                let mut x = vec![0.0f32; 99];
                x.push(100.0);
                // mean 1, std ~10; limit = 1 + 3*10.05 ~ 31 -> only the 100 exceeds
                let f = outlier_fraction(&x, 3.0).unwrap();
                assert!((f - 0.01).abs() < 1e-12);
                assert_eq!(outlier_fraction(&[2.0, 2.0, 2.0], 3.0), None);
                assert_eq!(outlier_fraction(&[1.0], 3.0), None);
            }

            #[test]
            fn near_duplicates_detects_repeated_rows() {
                let data = [1.0f32, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0];
                let m = Matrix::new(&data, 4, 2).unwrap();
                // sorted by first coord: (0,1),(0,1),(1,0),(1,0): pairs 1 and 3 are duplicates, 2 is not
                let f = near_duplicate_fraction(m, 0.995, 4096).unwrap();
                assert!((f - 2.0 / 3.0).abs() < 1e-12);
            }

            #[test]
            fn near_duplicates_edge_cases() {
                let one = Matrix::new(&[1.0, 2.0], 1, 2).unwrap();
                assert_eq!(near_duplicate_fraction(one, 0.99, 10), None);
                let zeros = Matrix::new(&[0.0; 6], 3, 2).unwrap();
                assert_eq!(near_duplicate_fraction(zeros, 0.99, 10), None);
                let m = Matrix::new(&[1.0, 0.0, 0.0, 1.0], 2, 2).unwrap();
                assert_eq!(near_duplicate_fraction(m, 0.99, 0), None);
                assert_eq!(near_duplicate_fraction(m, 0.99, 10), Some(0.0));
            }

            #[test]
            fn sampling_is_deterministic_and_strided() {
                let data: Vec<f32> = (0..200).map(|i| (i % 7) as f32 + 1.0).collect();
                let m = Matrix::new(&data, 100, 2).unwrap();
                assert_eq!(
                    near_duplicate_fraction(m, 0.99, 10),
                    near_duplicate_fraction(m, 0.99, 10)
                );
            }
        }
    }
}

pub mod sparse {
    //! Selective loading: decide how much of a big model has to be resident.
    //!
    //! Ported from `sparse_runtime.py` and the planning parts of `theory_forge.py` in the research
    //! toolkit. Only the *decisions* are here, as plain functions over numbers, so they run on any
    //! model and are tested without one. Measuring (running the model on calibration prompts and
    //! scoring it with [`crate::theory`]) is done by the caller and fed in.
    //!
    //! * **Mixture of experts.** [`RoutingProfile`] counts how much routing mass each expert got on
    //!   domain prompts; [`hot_set`] keeps the most used experts up to a coverage target (never fewer
    //!   than `top_k` per layer); [`HotSet::apply`] restricts an
    //!   [`ExpertStore`](crate::weights::experts::ExpertStore) to it, so cold experts are never read.
    //! * **Dense models.** [`NeuronProfile`] and [`slice_mlp`] keep the most active feed-forward
    //!   neurons and cut the weight matrices down, which gives a smaller real model.
    //! * **Acceptance.** [`smallest_within_tolerance`] takes the smallest variant whose robust score
    //!   stays within a tolerance of the full model's.
    //! * **Budgets and hardware.** [`plan_tiers`] splits memory into fast tier, RAM cache and disk;
    //!   [`plan_hardware`] and [`device_for`] choose device, precision and run sizes.
    //!
    //! Nothing here claims a smaller model is as good: that is only established by scoring it.
    #![forbid(unsafe_code)]

    use crate::weights::experts::ExpertKey;
    use crate::weights::experts::ExpertStore;
    use crate::weights::source::ModelSource;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt;

    /// Largest number of layers, experts, neurons or manifest lines accepted.
    pub const MAX_ITEMS: usize = 1 << 20;

    /// Invalid input to a planning function.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SparseError {
        /// A shape or length does not match (the text says which).
        Shape(&'static str),
        /// A parameter is outside its range (the text says which).
        Parameter(&'static str),
        /// A manifest is malformed (line number, 1-based).
        Manifest(usize),
        /// The expert store refused the hot set.
        Store(String),
    }

    impl fmt::Display for SparseError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Shape(s) => write!(f, "shape mismatch: {s}"),
                Self::Parameter(s) => write!(f, "invalid parameter: {s}"),
                Self::Manifest(n) => write!(f, "manifest line {n} is malformed"),
                Self::Store(s) => write!(f, "expert store refused the hot set: {s}"),
            }
        }
    }

    impl std::error::Error for SparseError {}

    // ------------------------------------------------------------------ expert tensor names

    /// What an expert tensor holds.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum ExpertRole {
        /// The gate projection (`w1`, `gate_proj`).
        Gate,
        /// The up projection (`w3`, `up_proj`).
        Up,
        /// The down projection (`w2`, `down_proj`).
        Down,
        /// Gate and up stacked in one tensor (`gate_up_proj`).
        GateUp,
    }

    /// A tensor that belongs to a routed expert, found from its name.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ExpertTensor {
        /// Layer index.
        pub layer: usize,
        /// Which expert, or `None` when the tensor holds every expert of the layer (fused).
        pub expert: Option<usize>,
        /// What the tensor is.
        pub role: ExpertRole,
    }

    fn number(s: &str) -> Option<usize> {
        if s.is_empty() || s.len() > 7 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }

    /// Recognises routed-expert tensors by name, in the checkpoint styles the research toolkit
    /// handled (`...layers.N.<block>.experts.E.<w1|w2|w3|gate_proj|up_proj|down_proj>.weight`, and
    /// fused `...layers.N.<block>.experts.<gate_up_proj|down_proj>`) and in GGUF style
    /// (`blk.N.ffn_<gate|up|down>_exps.weight`, which hold all experts of a layer).
    /// Anything else, including router, shared-expert and attention tensors, gives `None`.
    #[must_use]
    pub fn classify_expert_tensor(name: &str) -> Option<ExpertTensor> {
        if let Some(rest) = name.strip_prefix("blk.") {
            let (layer, tail) = rest.split_once('.')?;
            let role = match tail {
                "ffn_gate_exps.weight" => ExpertRole::Gate,
                "ffn_up_exps.weight" => ExpertRole::Up,
                "ffn_down_exps.weight" => ExpertRole::Down,
                _ => return None,
            };
            return Some(ExpertTensor {
                layer: number(layer)?,
                expert: None,
                role,
            });
        }
        let parts: Vec<&str> = name.split('.').collect();
        let l = parts.iter().position(|p| *p == "layers")?;
        let layer = number(parts.get(l + 1)?)?;
        // After the layer index comes the block name (one or more parts), then `experts`.
        let e = parts.iter().skip(l + 2).position(|p| *p == "experts")? + l + 2;
        if e == l + 2 {
            return None; // the block name is required
        }
        let after = &parts[e + 1..];
        match after {
            ["gate_up_proj"] => Some(ExpertTensor {
                layer,
                expert: None,
                role: ExpertRole::GateUp,
            }),
            ["down_proj"] => Some(ExpertTensor {
                layer,
                expert: None,
                role: ExpertRole::Down,
            }),
            [idx, role, "weight"] => {
                let role = match *role {
                    "w1" | "gate_proj" => ExpertRole::Gate,
                    "w3" | "up_proj" => ExpertRole::Up,
                    "w2" | "down_proj" => ExpertRole::Down,
                    _ => return None,
                };
                Some(ExpertTensor {
                    layer,
                    expert: Some(number(idx)?),
                    role,
                })
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------ mixture of experts

    /// How much routing mass each expert received, per layer, on the calibration prompts.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct RoutingProfile {
        mass: BTreeMap<usize, BTreeMap<usize, f64>>,
    }

    impl RoutingProfile {
        /// An empty profile.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Adds the experts chosen for one token: `(expert, weight)` pairs, as returned by
        /// [`crate::weights::experts::route`]. Non-finite or negative weights are ignored.
        pub fn record(&mut self, layer: usize, chosen: &[(usize, f32)]) {
            let layer_mass = self.mass.entry(layer).or_default();
            for &(e, w) in chosen {
                if w.is_finite() && w >= 0.0 {
                    *layer_mass.entry(e).or_insert(0.0) += f64::from(w);
                }
            }
        }

        /// Mass of one expert (0 when never chosen).
        #[must_use]
        pub fn mass(&self, layer: usize, expert: usize) -> f64 {
            self.mass
                .get(&layer)
                .and_then(|m| m.get(&expert))
                .copied()
                .unwrap_or(0.0)
        }

        /// Layers that were seen.
        #[must_use]
        pub fn layers(&self) -> Vec<usize> {
            self.mass.keys().copied().collect()
        }

        /// The experts of `layer`, most used first (ties: lower expert number first).
        #[must_use]
        pub fn ranked(&self, layer: usize) -> Vec<(usize, f64)> {
            let mut v: Vec<(usize, f64)> = self
                .mass
                .get(&layer)
                .map(|m| m.iter().map(|(&e, &w)| (e, w)).collect())
                .unwrap_or_default();
            v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            v
        }
    }

    /// The experts kept per layer.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct HotSet {
        /// Kept experts, by layer.
        pub layers: BTreeMap<usize, BTreeSet<usize>>,
    }

    impl HotSet {
        /// Total number of experts kept.
        #[must_use]
        pub fn experts_kept(&self) -> usize {
            self.layers.values().map(BTreeSet::len).sum()
        }

        /// Restricts `store` so that only these experts can be routed to, and (when `pin`) loads
        /// them into the fast tier as far as it has room. Returns how many were pinned.
        ///
        /// # Errors
        /// [`SparseError::Store`] if a layer or expert number is outside the store.
        pub fn apply<S: ModelSource, L: Fn(ExpertKey) -> Option<u64>>(
            &self,
            store: &mut ExpertStore<S, L>,
            pin: bool,
        ) -> Result<usize, SparseError> {
            let mut pinned = 0;
            for (&layer, experts) in &self.layers {
                let list: Vec<u32> = experts
                    .iter()
                    .map(|&e| u32::try_from(e).map_err(|_| SparseError::Parameter("expert number")))
                    .collect::<Result<_, _>>()?;
                let layer32 =
                    u32::try_from(layer).map_err(|_| SparseError::Parameter("layer number"))?;
                store
                    .allow_only(layer32, &list)
                    .map_err(|e| SparseError::Store(e.to_string()))?;
                if pin {
                    for &expert in &list {
                        if store
                            .pin(ExpertKey {
                                layer: layer32,
                                expert,
                            })
                            .is_ok()
                        {
                            pinned += 1;
                        }
                    }
                }
            }
            Ok(pinned)
        }
    }

    /// The hot set that covers at least `coverage` (0 to 1) of each layer's routing mass.
    ///
    /// Experts are taken most used first, stopping once the coverage is reached *and* at least
    /// `top_k` are kept; a layer is topped up to `top_k` with the lowest-numbered unused experts so
    /// the router always has enough to choose from. A layer with no recorded mass keeps `top_k`
    /// experts (the lowest-numbered). `layers` names the layers to plan.
    ///
    /// # Errors
    /// [`SparseError::Parameter`] for a coverage outside 0..=1, `top_k` of zero or above
    /// `experts_per_layer`, or too many layers.
    pub fn hot_set(
        profile: &RoutingProfile,
        layers: &[usize],
        experts_per_layer: usize,
        top_k: usize,
        coverage: f64,
    ) -> Result<HotSet, SparseError> {
        if !(0.0..=1.0).contains(&coverage) || coverage.is_nan() {
            return Err(SparseError::Parameter("coverage must be between 0 and 1"));
        }
        if top_k == 0
            || top_k > experts_per_layer
            || experts_per_layer > MAX_ITEMS
            || layers.len() > MAX_ITEMS
        {
            return Err(SparseError::Parameter("top_k and experts_per_layer"));
        }
        let mut out = HotSet::default();
        for &layer in layers {
            let ranked = profile.ranked(layer);
            let total: f64 = ranked.iter().map(|r| r.1).sum();
            let total = if total > 0.0 { total } else { 1.0 };
            let mut keep: Vec<usize> = Vec::new();
            let mut acc = 0.0;
            for (e, w) in ranked {
                if e >= experts_per_layer {
                    continue;
                }
                if acc / total >= coverage && keep.len() >= top_k {
                    break;
                }
                keep.push(e);
                acc += w;
            }
            for e in 0..experts_per_layer {
                if keep.len() >= top_k {
                    break;
                }
                if !keep.contains(&e) {
                    keep.push(e);
                }
            }
            out.layers.insert(layer, keep.into_iter().collect());
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ memory tiers

    /// How memory is divided between the always-resident part of a model (the skeleton:
    /// attention, norms, embeddings, routers, shared experts) and the expert tiers.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct TierPlan {
        /// The skeleton fits on the accelerator (otherwise it stays in RAM).
        pub skeleton_on_gpu: bool,
        /// Bytes available to the fast expert tier (accelerator memory, or 0 with no accelerator).
        pub fast_bytes: f64,
        /// Bytes available to the RAM expert cache.
        pub ram_bytes: f64,
    }

    /// Splits memory the way the research toolkit did: the skeleton goes to the accelerator when
    /// `1.3 x` its size fits in `fast_frac` of the free accelerator memory, the fast tier gets the
    /// rest of that share, and the RAM cache gets `ram_frac` of free RAM minus the skeleton when it
    /// lives there. Everything that does not fit stays on disk.
    ///
    /// # Errors
    /// [`SparseError::Parameter`] for a fraction outside 0..=1 or a negative or non-finite size.
    pub fn plan_tiers(
        skeleton_bytes: f64,
        gpu_free: f64,
        ram_free: f64,
        fast_frac: f64,
        ram_frac: f64,
    ) -> Result<TierPlan, SparseError> {
        let ok = |v: f64| v.is_finite() && v >= 0.0;
        if ![skeleton_bytes, gpu_free, ram_free].iter().all(|&v| ok(v)) {
            return Err(SparseError::Parameter(
                "sizes must be finite and not negative",
            ));
        }
        if !(0.0..=1.0).contains(&fast_frac) || !(0.0..=1.0).contains(&ram_frac) {
            return Err(SparseError::Parameter("fractions must be between 0 and 1"));
        }
        let on_gpu = gpu_free > 0.0 && skeleton_bytes * 1.3 < gpu_free * fast_frac;
        let fast = if gpu_free > 0.0 {
            (gpu_free * fast_frac - if on_gpu { skeleton_bytes * 1.3 } else { 0.0 }).max(0.0)
        } else {
            0.0
        };
        let ram = (ram_free * ram_frac - if on_gpu { 0.0 } else { skeleton_bytes }).max(0.0);
        Ok(TierPlan {
            skeleton_on_gpu: on_gpu,
            fast_bytes: fast,
            ram_bytes: ram,
        })
    }

    // ------------------------------------------------------------------ dense models

    /// Accumulates how strongly each feed-forward neuron fires on the calibration prompts.
    #[derive(Debug, Clone, PartialEq)]
    pub struct NeuronProfile {
        sums: Vec<f32>,
    }

    impl NeuronProfile {
        /// A profile for `neurons` neurons (the intermediate size).
        ///
        /// # Errors
        /// [`SparseError::Parameter`] for zero or an unreasonable count.
        pub fn new(neurons: usize) -> Result<Self, SparseError> {
            if neurons == 0 || neurons > MAX_ITEMS {
                return Err(SparseError::Parameter("neuron count"));
            }
            Ok(Self {
                sums: vec![0.0; neurons],
            })
        }

        /// Adds one token's activations (the input of the down projection): `|a|` per neuron.
        ///
        /// # Errors
        /// [`SparseError::Shape`] if the length differs from the neuron count.
        pub fn record(&mut self, activations: &[f32]) -> Result<(), SparseError> {
            if activations.len() != self.sums.len() {
                return Err(SparseError::Shape("activation length"));
            }
            for (s, a) in self.sums.iter_mut().zip(activations) {
                if a.is_finite() {
                    *s += a.abs();
                }
            }
            Ok(())
        }

        /// Importance = accumulated activation x the norm of that neuron's down-projection column.
        ///
        /// # Errors
        /// [`SparseError::Shape`] if `down_col_norms` has the wrong length.
        pub fn importance(&self, down_col_norms: &[f32]) -> Result<Vec<f32>, SparseError> {
            if down_col_norms.len() != self.sums.len() {
                return Err(SparseError::Shape("down column norms"));
            }
            Ok(self
                .sums
                .iter()
                .zip(down_col_norms)
                .map(|(a, n)| a * n)
                .collect())
        }
    }

    /// Norm of each column of a row-major `rows x cols` matrix (for the down projection, whose
    /// columns are the neurons).
    ///
    /// # Errors
    /// [`SparseError::Shape`] if `data.len() != rows * cols` or a dimension is zero.
    pub fn column_norms(data: &[f32], rows: usize, cols: usize) -> Result<Vec<f32>, SparseError> {
        if rows == 0 || cols == 0 || rows.checked_mul(cols) != Some(data.len()) {
            return Err(SparseError::Shape("matrix"));
        }
        let mut sq = vec![0.0f64; cols];
        for row in data.chunks_exact(cols) {
            for (s, &v) in sq.iter_mut().zip(row) {
                *s += f64::from(v) * f64::from(v);
            }
        }
        // Narrowing back to the model's precision is the intent here.
        #[allow(clippy::cast_possible_truncation)]
        Ok(sq.into_iter().map(|s| s.sqrt() as f32).collect())
    }

    /// How many neurons to keep out of `total` at `fraction`: rounded to a multiple of 64 (at
    /// least 64) for wide layers, plain rounding (at least 1) for narrow ones; never above `total`.
    ///
    /// # Errors
    /// [`SparseError::Parameter`] for a fraction outside (0, 1] or `total` of zero.
    pub fn keep_count(total: usize, fraction: f64) -> Result<usize, SparseError> {
        if total == 0 || total > MAX_ITEMS || !(fraction > 0.0 && fraction <= 1.0) {
            return Err(SparseError::Parameter("fraction or neuron count"));
        }
        // Counts are far below 2^52, so the float arithmetic is exact enough for rounding.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let k = if total >= 128 {
            (((total as f64 * fraction / 64.0).round() as usize) * 64).max(64)
        } else {
            ((total as f64 * fraction).round() as usize).max(1)
        };
        Ok(k.min(total))
    }

    /// The `k` most important neurons, as sorted indices (ties: lower index kept first).
    ///
    /// # Errors
    /// [`SparseError::Parameter`] if `k` is zero or above the number of neurons.
    pub fn top_neurons(importance: &[f32], k: usize) -> Result<Vec<usize>, SparseError> {
        if k == 0 || k > importance.len() {
            return Err(SparseError::Parameter("k"));
        }
        let mut idx: Vec<usize> = (0..importance.len()).collect();
        idx.sort_by(|&a, &b| importance[b].total_cmp(&importance[a]).then(a.cmp(&b)));
        idx.truncate(k);
        idx.sort_unstable();
        Ok(idx)
    }

    /// Rows `keep` of a row-major `rows x cols` matrix, in order (`index_select` on dimension 0;
    /// for the gate and up projections, whose rows are neurons).
    ///
    /// # Errors
    /// [`SparseError::Shape`] for a bad shape or an index out of range.
    pub fn select_rows(
        data: &[f32],
        rows: usize,
        cols: usize,
        keep: &[usize],
    ) -> Result<Vec<f32>, SparseError> {
        if rows == 0
            || cols == 0
            || rows.checked_mul(cols) != Some(data.len())
            || keep.iter().any(|&r| r >= rows)
        {
            return Err(SparseError::Shape("select_rows"));
        }
        let mut out = Vec::with_capacity(keep.len() * cols);
        for &r in keep {
            out.extend_from_slice(&data[r * cols..(r + 1) * cols]);
        }
        Ok(out)
    }

    /// Columns `keep` of a row-major `rows x cols` matrix (`index_select` on dimension 1; for the
    /// down projection, whose columns are neurons). The result is `rows x keep.len()`.
    ///
    /// # Errors
    /// [`SparseError::Shape`] for a bad shape or an index out of range.
    pub fn select_cols(
        data: &[f32],
        rows: usize,
        cols: usize,
        keep: &[usize],
    ) -> Result<Vec<f32>, SparseError> {
        if rows == 0
            || cols == 0
            || rows.checked_mul(cols) != Some(data.len())
            || keep.iter().any(|&c| c >= cols)
        {
            return Err(SparseError::Shape("select_cols"));
        }
        let mut out = Vec::with_capacity(rows * keep.len());
        for row in data.chunks_exact(cols) {
            out.extend(keep.iter().map(|&c| row[c]));
        }
        Ok(out)
    }

    /// One feed-forward block cut down to the kept neurons.
    #[derive(Debug, Clone, PartialEq)]
    pub struct SlicedMlp {
        /// Gate projection, `keep.len() x hidden`.
        pub gate: Vec<f32>,
        /// Up projection, `keep.len() x hidden`.
        pub up: Vec<f32>,
        /// Down projection, `hidden x keep.len()`.
        pub down: Vec<f32>,
    }

    /// Cuts a SwiGLU block to the neurons in `keep`: rows of `gate` and `up` (each
    /// `neurons x hidden`), columns of `down` (`hidden x neurons`).
    ///
    /// # Errors
    /// [`SparseError::Shape`] if the three matrices do not agree.
    pub fn slice_mlp(
        gate: &[f32],
        up: &[f32],
        down: &[f32],
        neurons: usize,
        hidden: usize,
        keep: &[usize],
    ) -> Result<SlicedMlp, SparseError> {
        Ok(SlicedMlp {
            gate: select_rows(gate, neurons, hidden, keep)?,
            up: select_rows(up, neurons, hidden, keep)?,
            down: select_cols(down, hidden, neurons, keep)?,
        })
    }

    // ------------------------------------------------------------------ acceptance

    /// The robust score measured for one variant.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Scored {
        /// The knob that produced it: coverage for experts, the kept fraction for neurons. A
        /// smaller value means a smaller model.
        pub setting: f64,
        /// Robust score S of the variant (see [`crate::theory::robust`]).
        pub robust_s: f64,
    }

    /// The smallest variant whose robust score is at least `full_s - tolerance`. Variants are tried
    /// from the smallest setting up, and the first that passes wins; `None` means none passed, so
    /// keep the full model.
    ///
    /// # Errors
    /// [`SparseError::Parameter`] for a negative or non-finite tolerance or a non-finite score.
    pub fn smallest_within_tolerance(
        full_s: f64,
        tolerance: f64,
        variants: &[Scored],
    ) -> Result<Option<Scored>, SparseError> {
        if !tolerance.is_finite() || tolerance < 0.0 || !full_s.is_finite() {
            return Err(SparseError::Parameter("tolerance or full score"));
        }
        if variants
            .iter()
            .any(|v| !v.setting.is_finite() || !v.robust_s.is_finite())
        {
            return Err(SparseError::Parameter("variant score"));
        }
        let mut sorted = variants.to_vec();
        sorted.sort_by(|a, b| a.setting.total_cmp(&b.setting));
        Ok(sorted
            .into_iter()
            .find(|v| v.robust_s >= full_s - tolerance))
    }

    // ------------------------------------------------------------------ choice scoring

    /// Probability of each answer letter after a multiple-choice prompt, from the model's logits:
    /// log-softmax over the vocabulary, log-sum-exp over the token ids that spell each letter
    /// (with and without a leading space), then softmax over the letters.
    ///
    /// # Errors
    /// [`SparseError::Shape`] for empty logits, a token id outside the vocabulary or a letter with
    /// no token ids.
    pub fn letter_probs(
        logits: &[f32],
        letter_ids: &[Vec<usize>],
    ) -> Result<Vec<f64>, SparseError> {
        if logits.is_empty()
            || letter_ids.is_empty()
            || letter_ids
                .iter()
                .any(|ids| ids.is_empty() || ids.iter().any(|&i| i >= logits.len()))
        {
            return Err(SparseError::Shape("logits or letter ids"));
        }
        let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if !max.is_finite() {
            return Err(SparseError::Shape("logits are not finite"));
        }
        let lse: f64 = logits
            .iter()
            .map(|&l| f64::from(l - max).exp())
            .sum::<f64>()
            .ln()
            + f64::from(max);
        let per_letter: Vec<f64> = letter_ids
            .iter()
            .map(|ids| {
                let m = ids
                    .iter()
                    .map(|&i| f64::from(logits[i]))
                    .fold(f64::NEG_INFINITY, f64::max);
                m + ids
                    .iter()
                    .map(|&i| (f64::from(logits[i]) - m).exp())
                    .sum::<f64>()
                    .ln()
                    - lse
            })
            .collect();
        let m = per_letter.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let exps: Vec<f64> = per_letter.iter().map(|v| (v - m).exp()).collect();
        let sum: f64 = exps.iter().sum();
        Ok(exps.into_iter().map(|e| e / sum).collect())
    }

    /// A calibration prompt: the question, the lettered choices, then `Answer:`.
    #[must_use]
    pub fn calibration_prompt(question: &str, choices: &[&str]) -> String {
        let mut s = question.trim().to_string();
        s.push('\n');
        for (letter, c) in ('A'..='Z').zip(choices) {
            s.push_str(&format!("{letter}. {c}\n"));
        }
        s.push_str("Answer:");
        s
    }

    // ------------------------------------------------------------------ the manifest

    /// What kind of model was sliced.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Chosen {
        /// A mixture of experts: the coverage reached and the experts kept.
        Experts {
            /// Routing-mass coverage of the chosen hot set.
            coverage: f64,
            /// The experts to keep.
            hot: HotSet,
        },
        /// A dense model: the fraction of neurons kept and the sliced checkpoint's name.
        Neurons {
            /// Fraction of feed-forward neurons kept.
            fraction: f64,
            /// File or folder name of the sliced checkpoint (no path separators or line breaks).
            checkpoint: String,
        },
    }

    /// The result of a selective-loading run, to be read back at load time (`hot_set.json` in the
    /// research toolkit; a line-based text format here).
    #[derive(Debug, Clone, PartialEq)]
    pub struct Manifest {
        /// Model name (no line breaks).
        pub model: String,
        /// The tolerance on the robust score that was used.
        pub tolerance: f64,
        /// What was chosen; `None` means no variant passed, so use the full model.
        pub chosen: Option<Chosen>,
    }

    const MAGIC: &str = "mouli-sparse-manifest 1";

    impl Manifest {
        /// The text form: a magic line, then `key value` lines.
        ///
        /// # Errors
        /// [`SparseError::Parameter`] if the model or checkpoint name has a line break, or a
        /// checkpoint name has a path separator.
        pub fn to_text(&self) -> Result<String, SparseError> {
            let plain = |s: &str| !s.is_empty() && !s.contains(['\n', '\r']);
            if !plain(&self.model) {
                return Err(SparseError::Parameter("model name"));
            }
            let mut t = format!(
                "{MAGIC}\nmodel {}\ntolerance {:?}\n",
                self.model, self.tolerance
            );
            match &self.chosen {
                None => t.push_str("chosen none\n"),
                Some(Chosen::Experts { coverage, hot }) => {
                    t.push_str(&format!("chosen experts\ncoverage {coverage:?}\n"));
                    for (layer, set) in &hot.layers {
                        t.push_str(&format!("layer {layer}"));
                        for e in set {
                            t.push_str(&format!(" {e}"));
                        }
                        t.push('\n');
                    }
                }
                Some(Chosen::Neurons {
                    fraction,
                    checkpoint,
                }) => {
                    if !plain(checkpoint) || checkpoint.contains(['/', '\\']) {
                        return Err(SparseError::Parameter("checkpoint name"));
                    }
                    t.push_str(&format!(
                        "chosen neurons\nfraction {fraction:?}\ncheckpoint {checkpoint}\n"
                    ));
                }
            }
            Ok(t)
        }

        /// Reads the text form strictly: unknown keys, repeated keys, missing keys, bad numbers and
        /// oversized input are all errors, reported with the line number.
        ///
        /// # Errors
        /// [`SparseError::Manifest`] with the 1-based line of the first problem.
        pub fn from_text(text: &str) -> Result<Self, SparseError> {
            let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));
            if lines.next().map(|(_, l)| l) != Some(MAGIC) {
                return Err(SparseError::Manifest(1));
            }
            let mut model = None;
            let mut tolerance = None;
            let mut kind: Option<&str> = None;
            let mut coverage = None;
            let mut fraction = None;
            let mut checkpoint = None;
            let mut hot = HotSet::default();
            let num = |s: &str, n: usize| {
                s.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or(SparseError::Manifest(n))
            };
            for (n, line) in lines {
                if n > MAX_ITEMS {
                    return Err(SparseError::Manifest(n));
                }
                let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
                match key {
                    "model" if model.is_none() && !rest.is_empty() => {
                        model = Some(rest.to_string())
                    }
                    "tolerance" if tolerance.is_none() => tolerance = Some(num(rest, n)?),
                    "chosen"
                        if kind.is_none() && matches!(rest, "none" | "experts" | "neurons") =>
                    {
                        kind = Some(rest)
                    }
                    "coverage" if coverage.is_none() => coverage = Some(num(rest, n)?),
                    "fraction" if fraction.is_none() => fraction = Some(num(rest, n)?),
                    "checkpoint" if checkpoint.is_none() && !rest.is_empty() => {
                        checkpoint = Some(rest.to_string())
                    }
                    "layer" => {
                        let mut it = rest.split(' ');
                        let layer = it.next().and_then(number).ok_or(SparseError::Manifest(n))?;
                        let mut set = BTreeSet::new();
                        for e in it {
                            set.insert(number(e).ok_or(SparseError::Manifest(n))?);
                        }
                        if hot.layers.insert(layer, set).is_some() {
                            return Err(SparseError::Manifest(n));
                        }
                    }
                    _ => return Err(SparseError::Manifest(n)),
                }
            }
            let end = text.lines().count();
            let (model, tolerance) = (
                model.ok_or(SparseError::Manifest(end))?,
                tolerance.ok_or(SparseError::Manifest(end))?,
            );
            let chosen = match kind.ok_or(SparseError::Manifest(end))? {
                "none" => None,
                "experts" => Some(Chosen::Experts {
                    coverage: coverage.ok_or(SparseError::Manifest(end))?,
                    hot,
                }),
                _ => {
                    let checkpoint = checkpoint.ok_or(SparseError::Manifest(end))?;
                    if checkpoint.contains(['/', '\\']) {
                        return Err(SparseError::Manifest(end));
                    }
                    Some(Chosen::Neurons {
                        fraction: fraction.ok_or(SparseError::Manifest(end))?,
                        checkpoint,
                    })
                }
            };
            Ok(Self {
                model,
                tolerance,
                chosen,
            })
        }
    }

    /// A row of the report: a variant and what was measured for it.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ReportRow {
        /// For example `hot set 90% routing mass`.
        pub variant: String,
        /// Size needed in GB.
        pub gigabytes: f64,
        /// Robust score S.
        pub robust_s: f64,
        /// Clean accuracy.
        pub clean: f64,
        /// Generation speed, tokens per second.
        pub tokens_per_s: f64,
    }

    /// The Markdown report: a table, then the chosen variant and the rule.
    #[must_use]
    pub fn report_markdown(
        model: &str,
        kind: &str,
        tolerance: f64,
        rows: &[ReportRow],
        chosen: Option<&str>,
    ) -> String {
        let mut md = format!("# Sparse runtime report: {model}\n\nType: {kind} | tolerance on robust S: {tolerance}\n\n");
        md.push_str("| variant | GB needed | S | clean | tokens/s |\n|---|---|---|---|---|\n");
        for r in rows {
            md.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                r.variant, r.gigabytes, r.robust_s, r.clean, r.tokens_per_s
            ));
        }
        md.push_str(&format!(
            "\n**Chosen:** {}\n\nRule: the smallest variant whose robust score S stays within the tolerance of the full model.\n",
            chosen.unwrap_or("none within tolerance - keep the full model")
        ));
        md
    }

    // ------------------------------------------------------------------ hardware plan

    /// Where the model runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Device {
        /// The processor.
        Cpu,
        /// An NVIDIA GPU.
        Cuda,
        /// An Apple GPU.
        Mps,
    }

    /// What the machine offers.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Machine {
        /// Logical processors.
        pub cpu_threads: usize,
        /// Free RAM in GB, if known.
        pub ram_free_gb: Option<f64>,
        /// Free memory of the best NVIDIA GPU in GB, if there is one.
        pub gpu_free_gb: Option<f64>,
        /// Whether that GPU supports bfloat16.
        pub gpu_bf16: bool,
        /// Whether an Apple GPU is available.
        pub mps: bool,
    }

    /// The run plan chosen from a [`Machine`].
    #[derive(Debug, Clone, PartialEq)]
    pub struct HardwarePlan {
        /// The device.
        pub device: Device,
        /// `bfloat16`, `float16` or `float32`.
        pub dtype: &'static str,
        /// Processor threads to use (all but one).
        pub threads: usize,
        /// Memory available to the model in GB.
        pub memory_budget_gb: f64,
        /// Items per theory test.
        pub theory_n: usize,
        /// Items per code test.
        pub code_n: usize,
        /// Longest context to test.
        pub max_context: usize,
    }

    /// Chooses device, precision and run sizes: an NVIDIA GPU with at least 3 GB free, else an
    /// Apple GPU, else the CPU (unless `force` names a device the machine has).
    #[must_use]
    pub fn plan_hardware(m: &Machine, force: Option<Device>) -> HardwarePlan {
        let gpu = m.gpu_free_gb.filter(|g| g.is_finite() && *g >= 0.0);
        let auto = match gpu {
            Some(g) if g >= 3.0 => Device::Cuda,
            _ if m.mps => Device::Mps,
            _ => Device::Cpu,
        };
        let mut device = force.unwrap_or(auto);
        if device == Device::Cuda && gpu.is_none() {
            device = Device::Cpu;
        }
        let (dtype, mem) = if device == Device::Cuda {
            (
                if m.gpu_bf16 { "bfloat16" } else { "float16" },
                gpu.unwrap_or(0.0),
            )
        } else {
            (
                "float32",
                m.ram_free_gb
                    .filter(|r| r.is_finite() && *r > 0.0)
                    .unwrap_or(8.0),
            )
        };
        let fast = device != Device::Cpu;
        HardwarePlan {
            device,
            dtype,
            threads: m.cpu_threads.saturating_sub(1).max(1),
            memory_budget_gb: (mem * 10.0).round() / 10.0,
            theory_n: if fast { 60 } else { 20 },
            code_n: if fast { 40 } else { 20 },
            max_context: if fast && mem >= 12.0 {
                8192
            } else if mem >= 12.0 {
                4096
            } else {
                2048
            },
        }
    }

    /// Per-model device choice: the GPU only if the weights fit with 40% headroom, else the CPU
    /// (or the Apple GPU). The second value is a warning when the CPU would need more RAM than is
    /// free for the model in float32 (twice its size on disk plus 30%).
    #[must_use]
    pub fn device_for(
        model_gb: f64,
        plan: &HardwarePlan,
        ram_free_gb: Option<f64>,
    ) -> (Device, Option<String>) {
        if plan.device == Device::Cuda && model_gb * 1.4 <= plan.memory_budget_gb {
            return (Device::Cuda, None);
        }
        let ram = ram_free_gb
            .filter(|r| r.is_finite() && *r > 0.0)
            .unwrap_or(8.0);
        let warning = (model_gb * 2.0 * 1.3 > ram).then(|| {
            format!(
                "about {:.1} GB of RAM needed in float32, {ram} GB free: may be very slow or fail",
                model_gb * 2.6
            )
        });
        (
            if plan.device == Device::Mps {
                Device::Mps
            } else {
                Device::Cpu
            },
            warning,
        )
    }

    /// The merge recipes tried by a grid search over `models` same-architecture models: the equal
    /// average, then one recipe per model that weights it 0.6 and the others share 0.4. Empty for
    /// fewer than two models (nothing to merge).
    #[must_use]
    pub fn merge_grid(models: usize) -> Vec<Vec<f64>> {
        if !(2..=64).contains(&models) {
            return Vec::new();
        }
        let others = 0.4 / (models as f64 - 1.0);
        let mut v = vec![vec![1.0; models]];
        for j in 0..models {
            v.push(
                (0..models)
                    .map(|i| if i == j { 0.6 } else { others })
                    .collect(),
            );
        }
        v
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn profile(layer0: &[(usize, f32)]) -> RoutingProfile {
            let mut p = RoutingProfile::new();
            for &(e, w) in layer0 {
                p.record(0, &[(e, w)]);
            }
            p
        }

        #[test]
        fn names_of_expert_tensors_are_recognised_in_both_checkpoint_styles() {
            let t = |n: &str| classify_expert_tensor(n);
            assert_eq!(
                t("model.layers.3.block_sparse_moe.experts.5.w1.weight"),
                Some(ExpertTensor {
                    layer: 3,
                    expert: Some(5),
                    role: ExpertRole::Gate
                })
            );
            assert_eq!(
                t("model.layers.0.mlp.experts.12.up_proj.weight")
                    .unwrap()
                    .role,
                ExpertRole::Up
            );
            assert_eq!(
                t("model.layers.0.mlp.experts.12.w2.weight").unwrap().role,
                ExpertRole::Down
            );
            assert_eq!(
                t("model.layers.7.mlp.experts.gate_up_proj"),
                Some(ExpertTensor {
                    layer: 7,
                    expert: None,
                    role: ExpertRole::GateUp
                })
            );
            assert_eq!(
                t("model.layers.7.mlp.experts.down_proj").unwrap().expert,
                None
            );
            assert_eq!(
                t("blk.4.ffn_down_exps.weight"),
                Some(ExpertTensor {
                    layer: 4,
                    expert: None,
                    role: ExpertRole::Down
                })
            );
            for not in [
                "model.layers.3.mlp.gate.weight",
                "model.layers.3.mlp.shared_expert.up_proj.weight",
                "model.layers.3.self_attn.q_proj.weight",
                "model.embed_tokens.weight",
                "blk.4.ffn_gate_inp.weight",
                "blk.x.ffn_down_exps.weight",
                "model.layers.3.experts.5.w1.weight", // no block name
                "model.layers.3.mlp.experts.5.w9.weight",
                "model.layers.3.mlp.experts.5.w1.bias",
                "model.layers.99999999.mlp.experts.5.w1.weight",
                "",
            ] {
                assert_eq!(t(not), None, "{not}");
            }
        }

        #[test]
        fn the_hot_set_covers_the_requested_share_and_never_drops_below_top_k() {
            // Masses 50, 30, 15, 5 (total 100).
            let p = profile(&[(2, 50.0), (0, 30.0), (3, 15.0), (1, 5.0)]);
            let keep = |c: f64, k: usize| {
                hot_set(&p, &[0], 4, k, c).unwrap().layers[&0]
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
            };
            assert_eq!(keep(0.5, 1), vec![2]);
            assert_eq!(keep(0.8, 1), vec![0, 2]);
            assert_eq!(keep(0.95, 1), vec![0, 2, 3]);
            assert_eq!(keep(1.0, 1), vec![0, 1, 2, 3]);
            assert_eq!(keep(0.0, 1), vec![2], "at least one even for zero coverage");
            assert_eq!(keep(0.0, 2), vec![0, 2], "top_k is a floor");
            // Smaller coverage never keeps more.
            let sizes: Vec<usize> = [0.1, 0.5, 0.8, 0.9, 0.99, 1.0]
                .iter()
                .map(|&c| keep(c, 2).len())
                .collect();
            assert!(sizes.windows(2).all(|w| w[0] <= w[1]), "{sizes:?}");
        }

        #[test]
        fn layers_without_data_and_ties_are_handled() {
            let p = profile(&[(3, 1.0)]);
            let h = hot_set(&p, &[0, 1], 8, 2, 0.9).unwrap();
            // Layer 0 has expert 3 only and tops up with the lowest numbers; layer 1 has no data.
            assert_eq!(h.layers[&0].iter().copied().collect::<Vec<_>>(), vec![0, 3]);
            assert_eq!(h.layers[&1].iter().copied().collect::<Vec<_>>(), vec![0, 1]);
            assert_eq!(h.experts_kept(), 4);
            // Ties are broken by the lower expert number.
            let tie = profile(&[(5, 1.0), (1, 1.0), (3, 1.0)]);
            assert_eq!(
                hot_set(&tie, &[0], 8, 1, 0.3).unwrap().layers[&0]
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![1]
            );
            // Experts outside the layer's range are ignored.
            let wild = profile(&[(99, 100.0), (1, 1.0)]);
            assert_eq!(
                hot_set(&wild, &[0], 4, 1, 0.5).unwrap().layers[&0]
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![1]
            );
        }

        #[test]
        fn bad_hot_set_arguments_are_refused() {
            let p = RoutingProfile::new();
            for (c, k, e) in [
                (1.5, 1, 4),
                (-0.1, 1, 4),
                (f64::NAN, 1, 4),
                (0.5, 0, 4),
                (0.5, 5, 4),
                (0.5, 1, MAX_ITEMS + 1),
            ] {
                assert!(hot_set(&p, &[0], e, k, c).is_err(), "{c} {k} {e}");
            }
        }

        #[test]
        fn recording_ignores_bad_weights_and_accumulates() {
            let mut p = RoutingProfile::new();
            p.record(
                1,
                &[
                    (2, 0.5),
                    (3, f32::NAN),
                    (4, -1.0),
                    (2, 0.25),
                    (5, f32::INFINITY),
                ],
            );
            assert!((p.mass(1, 2) - 0.75).abs() < 1e-9);
            assert_eq!(p.mass(1, 3), 0.0);
            assert_eq!(p.mass(1, 5), 0.0);
            assert_eq!(p.layers(), vec![1]);
            assert_eq!(p.ranked(1), vec![(2, 0.75)]);
            assert!(p.ranked(9).is_empty());
        }

        #[test]
        fn a_hot_set_restricts_the_expert_store() {
            use crate::weights::experts::{ExpertConfig, Tier};
            let cfg = ExpertConfig {
                layers: 2,
                experts_per_layer: 4,
                expert_bytes: 8,
                fast_slots: 4,
                ram_slots: 2,
            };
            let src = vec![7u8; 8 * 8];
            let mut store = ExpertStore::new(cfg, src, |k: ExpertKey| {
                Some(u64::from(k.layer * 4 + k.expert) * 8)
            })
            .unwrap();
            let h = HotSet {
                layers: [
                    (0usize, BTreeSet::from([1usize, 2])),
                    (1, BTreeSet::from([0usize])),
                ]
                .into(),
            };
            let pinned = h.apply(&mut store, true).unwrap();
            assert_eq!(pinned, 3);
            assert!(store.is_allowed(ExpertKey {
                layer: 0,
                expert: 1
            }));
            assert!(!store.is_allowed(ExpertKey {
                layer: 0,
                expert: 0
            }));
            assert!(!store.is_allowed(ExpertKey {
                layer: 1,
                expert: 3
            }));
            assert_eq!(
                store
                    .get(ExpertKey {
                        layer: 0,
                        expert: 2
                    })
                    .unwrap()
                    .1,
                Tier::Fast
            );
            assert!(
                store
                    .get(ExpertKey {
                        layer: 0,
                        expert: 3
                    })
                    .is_err(),
                "cold experts are never read"
            );
            // A hot set outside the store is refused.
            let bad = HotSet {
                layers: [(5usize, BTreeSet::from([0usize]))].into(),
            };
            assert!(matches!(
                bad.apply(&mut store, false),
                Err(SparseError::Store(_))
            ));
            let bad = HotSet {
                layers: [(0usize, BTreeSet::from([9usize]))].into(),
            };
            assert!(matches!(
                bad.apply(&mut store, false),
                Err(SparseError::Store(_))
            ));
        }

        #[test]
        fn memory_tiers_follow_the_research_toolkit_formulas() {
            let gb = 1u64 << 30;
            let g = |x: f64| x * gb as f64;
            // CPU only: no fast tier, RAM = 60% of free minus the skeleton.
            let p = plan_tiers(g(4.0), 0.0, g(16.0), 0.8, 0.6).unwrap();
            assert!(!p.skeleton_on_gpu);
            assert_eq!(p.fast_bytes, 0.0);
            assert!((p.ram_bytes - g(16.0 * 0.6 - 4.0)).abs() < 1.0);
            // A roomy GPU takes the skeleton (x1.3) out of its 80% share; RAM keeps its 60%.
            let p = plan_tiers(g(4.0), g(24.0), g(16.0), 0.8, 0.6).unwrap();
            assert!(p.skeleton_on_gpu);
            assert!((p.fast_bytes - g(24.0 * 0.8 - 5.2)).abs() < 1.0);
            assert!((p.ram_bytes - g(9.6)).abs() < 1.0);
            // A small GPU cannot hold the skeleton, so it all goes to experts and RAM pays for it.
            let p = plan_tiers(g(10.0), g(8.0), g(16.0), 0.8, 0.6).unwrap();
            assert!(!p.skeleton_on_gpu);
            assert!((p.fast_bytes - g(6.4)).abs() < 1.0);
            assert_eq!(
                plan_tiers(g(20.0), 0.0, g(16.0), 0.8, 0.6)
                    .unwrap()
                    .ram_bytes,
                0.0,
                "never negative"
            );
            for bad in [
                (-1.0, 0.0, 1.0, 0.5, 0.5),
                (f64::NAN, 0.0, 1.0, 0.5, 0.5),
                (1.0, 0.0, 1.0, 1.5, 0.5),
                (1.0, 0.0, 1.0, 0.5, -0.1),
            ] {
                assert!(plan_tiers(bad.0, bad.1, bad.2, bad.3, bad.4).is_err());
            }
        }

        #[test]
        fn neuron_importance_multiplies_activation_by_the_down_column_norm() {
            let mut np = NeuronProfile::new(3).unwrap();
            np.record(&[1.0, -2.0, 0.5]).unwrap();
            np.record(&[1.0, 2.0, f32::NAN]).unwrap();
            assert!(np.record(&[1.0]).is_err());
            let imp = np.importance(&[1.0, 0.5, 4.0]).unwrap();
            assert_eq!(imp, vec![2.0, 2.0, 2.0]);
            assert!(np.importance(&[1.0]).is_err());
            assert!(NeuronProfile::new(0).is_err());
            // down is 2 x 3: columns (3,4), (0,0), (6,8) have norms 5, 0, 10.
            let n = column_norms(&[3.0, 0.0, 6.0, 4.0, 0.0, 8.0], 2, 3).unwrap();
            assert_eq!(n, vec![5.0, 0.0, 10.0]);
            assert!(column_norms(&[1.0; 5], 2, 3).is_err());
            assert!(column_norms(&[], 0, 3).is_err());
        }

        #[test]
        fn the_kept_neuron_count_is_rounded_like_the_original() {
            assert_eq!(keep_count(11008, 0.5).unwrap(), 5504, "multiple of 64");
            assert_eq!(keep_count(11008, 0.65).unwrap(), 7168);
            assert_eq!(keep_count(11008, 0.001).unwrap(), 64, "at least 64");
            assert_eq!(keep_count(11008, 1.0).unwrap(), 11008);
            assert_eq!(
                keep_count(100, 0.5).unwrap(),
                50,
                "narrow layers round plainly"
            );
            assert_eq!(keep_count(100, 0.001).unwrap(), 1);
            assert_eq!(
                keep_count(130, 1.0).unwrap(),
                128,
                "a multiple of 64 below the width"
            );
            for (t, f) in [
                (0, 0.5),
                (10, 0.0),
                (10, 1.5),
                (10, f64::NAN),
                (MAX_ITEMS + 1, 0.5),
            ] {
                assert!(keep_count(t, f).is_err(), "{t} {f}");
            }
        }

        #[test]
        fn the_most_important_neurons_are_kept_in_order() {
            let imp = [0.1, 0.9, 0.5, 0.9, 0.0];
            assert_eq!(top_neurons(&imp, 2).unwrap(), vec![1, 3]);
            assert_eq!(top_neurons(&imp, 3).unwrap(), vec![1, 2, 3]);
            assert_eq!(top_neurons(&imp, 5).unwrap(), vec![0, 1, 2, 3, 4]);
            assert_eq!(
                top_neurons(&[1.0, 1.0, 1.0], 2).unwrap(),
                vec![0, 1],
                "ties keep the lower index"
            );
            assert!(top_neurons(&imp, 0).is_err());
            assert!(top_neurons(&imp, 6).is_err());
        }

        #[test]
        fn slicing_keeps_the_same_function_for_the_kept_neurons() {
            // hidden = 2, neurons = 3.
            let gate = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
            let up = [7.0, 8.0, 9.0, 10.0, 11.0, 12.0];
            let down = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 2 x 3
            let s = slice_mlp(&gate, &up, &down, 3, 2, &[0, 2]).unwrap();
            assert_eq!(s.gate, vec![1.0, 2.0, 5.0, 6.0]);
            assert_eq!(s.up, vec![7.0, 8.0, 11.0, 12.0]);
            assert_eq!(s.down, vec![1.0, 3.0, 4.0, 6.0]);
            // Keeping every neuron changes nothing.
            let all = slice_mlp(&gate, &up, &down, 3, 2, &[0, 1, 2]).unwrap();
            assert_eq!(
                (all.gate.as_slice(), all.up.as_slice(), all.down.as_slice()),
                (&gate[..], &up[..], &down[..])
            );
            // The output of the block for the kept neurons is identical before and after slicing.
            let x = [0.5f32, -1.0];
            let silu = |v: f32| v / (1.0 + (-v).exp());
            let run = |g: &[f32], u: &[f32], d: &[f32], n: usize| -> Vec<f32> {
                let h: Vec<f32> = (0..n)
                    .map(|i| {
                        silu(g[i * 2] * x[0] + g[i * 2 + 1] * x[1])
                            * (u[i * 2] * x[0] + u[i * 2 + 1] * x[1])
                    })
                    .collect();
                (0..2)
                    .map(|r| (0..n).map(|i| d[r * n + i] * h[i]).sum())
                    .collect()
            };
            let full = run(&gate, &up, &down, 3);
            let cut = run(&s.gate, &s.up, &s.down, 2);
            let drop1: Vec<f32> = {
                // Full block with neuron 1 removed by zeroing its down column.
                let mut d = down;
                d[1] = 0.0;
                d[4] = 0.0;
                run(&gate, &up, &d, 3)
            };
            assert_eq!(cut, drop1, "slicing equals removing the neuron");
            assert_ne!(cut, full);
            assert!(select_rows(&gate, 3, 2, &[3]).is_err());
            assert!(select_cols(&down, 2, 3, &[3]).is_err());
            assert!(select_rows(&gate, 2, 2, &[0]).is_err(), "shape mismatch");
            assert!(slice_mlp(&gate, &up, &down, 3, 3, &[0]).is_err());
        }

        #[test]
        fn the_smallest_variant_within_tolerance_wins() {
            let v = [
                Scored {
                    setting: 0.95,
                    robust_s: 0.69,
                },
                Scored {
                    setting: 0.8,
                    robust_s: 0.60,
                },
                Scored {
                    setting: 0.99,
                    robust_s: 0.70,
                },
                Scored {
                    setting: 0.9,
                    robust_s: 0.68,
                },
            ];
            // Full model 0.70, tolerance 0.02: 0.80 fails (0.60), 0.90 passes (0.68 >= 0.68).
            let got = smallest_within_tolerance(0.70, 0.02, &v).unwrap().unwrap();
            assert_eq!(got.setting, 0.9);
            assert!(
                smallest_within_tolerance(0.70, 0.0, &v[..2])
                    .unwrap()
                    .is_none(),
                "none passes: keep the full model"
            );
            assert!(smallest_within_tolerance(0.70, 0.02, &[])
                .unwrap()
                .is_none());
            assert!(smallest_within_tolerance(0.7, -0.1, &v).is_err());
            assert!(smallest_within_tolerance(f64::NAN, 0.1, &v).is_err());
            assert!(smallest_within_tolerance(
                0.7,
                0.1,
                &[Scored {
                    setting: 0.5,
                    robust_s: f64::NAN
                }]
            )
            .is_err());
        }

        #[test]
        fn answer_letters_are_scored_from_logits() {
            // Vocabulary of 6; letters A..D use tokens {0}, {1,2}, {3}, {4}.
            let ids = vec![vec![0], vec![1, 2], vec![3], vec![4]];
            let flat = letter_probs(&[0.0; 6], &ids).unwrap();
            // Letter B has two token ids, so it carries twice the mass of the others.
            assert!(
                (flat[1] - 0.4).abs() < 1e-9 && (flat[0] - 0.2).abs() < 1e-9,
                "{flat:?}"
            );
            assert!((flat.iter().sum::<f64>() - 1.0).abs() < 1e-9);
            let strong = letter_probs(&[0.0, 0.0, 0.0, 10.0, 0.0, 99.0], &ids).unwrap();
            assert!(
                strong[2] > 0.99,
                "token 5 is not a letter and does not count: {strong:?}"
            );
            assert!(letter_probs(&[], &ids).is_err());
            assert!(
                letter_probs(&[0.0; 3], &ids).is_err(),
                "id outside the vocabulary"
            );
            assert!(letter_probs(&[0.0; 6], &[vec![]]).is_err());
            assert!(letter_probs(&[f32::NAN; 6], &ids).is_err());
            // Large logits do not overflow.
            assert!(letter_probs(&[1e4, 0.0, 0.0, 0.0, 0.0, 0.0], &ids).unwrap()[0] > 0.99);
        }

        #[test]
        fn calibration_prompts_are_lettered() {
            assert_eq!(
                calibration_prompt("  2+2? ", &["3", "4"]),
                "2+2?\nA. 3\nB. 4\nAnswer:"
            );
        }

        #[test]
        fn manifests_round_trip_and_reject_damage() {
            let hot = HotSet {
                layers: [
                    (0usize, BTreeSet::from([1usize, 5])),
                    (3, BTreeSet::from([0usize, 2, 7])),
                ]
                .into(),
            };
            let m = Manifest {
                model: "Mixtral 8x7B".into(),
                tolerance: 0.02,
                chosen: Some(Chosen::Experts { coverage: 0.9, hot }),
            };
            let text = m.to_text().unwrap();
            assert_eq!(Manifest::from_text(&text).unwrap(), m);
            let d = Manifest {
                model: "q".into(),
                tolerance: 0.05,
                chosen: Some(Chosen::Neurons {
                    fraction: 0.65,
                    checkpoint: "q_sparse65".into(),
                }),
            };
            assert_eq!(Manifest::from_text(&d.to_text().unwrap()).unwrap(), d);
            let n = Manifest {
                model: "q".into(),
                tolerance: 0.0,
                chosen: None,
            };
            assert_eq!(Manifest::from_text(&n.to_text().unwrap()).unwrap(), n);
            // Names that could forge lines or paths are not written.
            for model in ["", "a\nchosen none"] {
                assert!(Manifest {
                    model: model.into(),
                    ..n.clone()
                }
                .to_text()
                .is_err());
            }
            for cp in ["../x", "a\\b", "a\nb", ""] {
                let bad = Manifest {
                    chosen: Some(Chosen::Neurons {
                        fraction: 0.5,
                        checkpoint: cp.into(),
                    }),
                    ..n.clone()
                };
                assert!(bad.to_text().is_err(), "{cp:?}");
            }
            // Damaged input is refused with a line number.
            for bad in [
                "",
                "wrong header\n",
                &format!("{MAGIC}\nmodel a\ntolerance x\nchosen none\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\n"),
                &format!("{MAGIC}\nmodel a\nmodel b\ntolerance 0.1\nchosen none\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\nchosen maybe\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\nchosen experts\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\nchosen experts\ncoverage 0.9\nlayer 0 1\nlayer 0 2\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\nchosen experts\ncoverage 0.9\nlayer q\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\nchosen neurons\nfraction 0.5\ncheckpoint ../evil\n"),
                &format!("{MAGIC}\nmodel a\ntolerance NaN\nchosen none\n"),
                &format!("{MAGIC}\nmodel a\ntolerance 0.1\nchosen none\nsurprise 1\n"),
            ] {
                assert!(matches!(Manifest::from_text(bad), Err(SparseError::Manifest(_))), "{bad:?}");
            }
            assert_eq!(
                Manifest::from_text(&format!("{MAGIC}\nmodel a\ntolerance x\n")),
                Err(SparseError::Manifest(3))
            );
        }

        #[test]
        fn the_report_lists_every_variant_and_the_choice() {
            let rows = [
                ReportRow {
                    variant: "full".into(),
                    gigabytes: 24.0,
                    robust_s: 0.7,
                    clean: 0.8,
                    tokens_per_s: 1.5,
                },
                ReportRow {
                    variant: "hot set 90% routing mass".into(),
                    gigabytes: 9.5,
                    robust_s: 0.69,
                    clean: 0.79,
                    tokens_per_s: 3.0,
                },
            ];
            let md = report_markdown("Mixtral", "moe", 0.02, &rows, Some("coverage 0.9"));
            assert!(md.contains("# Sparse runtime report: Mixtral"));
            assert!(md.contains("| hot set 90% routing mass | 9.500 | 0.690 | 0.790 | 3.000 |"));
            assert!(md.contains("**Chosen:** coverage 0.9"));
            assert!(report_markdown("m", "dense", 0.02, &[], None).contains("keep the full model"));
        }

        fn machine() -> Machine {
            Machine {
                cpu_threads: 8,
                ram_free_gb: Some(11.84),
                gpu_free_gb: None,
                gpu_bf16: false,
                mps: false,
            }
        }

        #[test]
        fn the_hardware_plan_follows_the_research_rules() {
            let cpu = plan_hardware(&machine(), None);
            assert_eq!(
                (cpu.device, cpu.dtype, cpu.threads, cpu.theory_n, cpu.code_n),
                (Device::Cpu, "float32", 7, 20, 20)
            );
            assert_eq!(cpu.memory_budget_gb, 11.8);
            assert_eq!(cpu.max_context, 2048);
            let big = plan_hardware(
                &Machine {
                    ram_free_gb: Some(32.0),
                    ..machine()
                },
                None,
            );
            assert_eq!(big.max_context, 4096);
            let gpu = Machine {
                gpu_free_gb: Some(24.0),
                gpu_bf16: true,
                ..machine()
            };
            let p = plan_hardware(&gpu, None);
            assert_eq!(
                (p.device, p.dtype, p.theory_n, p.max_context),
                (Device::Cuda, "bfloat16", 60, 8192)
            );
            assert_eq!(
                plan_hardware(
                    &Machine {
                        gpu_bf16: false,
                        ..gpu.clone()
                    },
                    None
                )
                .dtype,
                "float16"
            );
            // A GPU with under 3 GB is not worth it.
            assert_eq!(
                plan_hardware(
                    &Machine {
                        gpu_free_gb: Some(2.0),
                        ..machine()
                    },
                    None
                )
                .device,
                Device::Cpu
            );
            assert_eq!(
                plan_hardware(
                    &Machine {
                        mps: true,
                        ..machine()
                    },
                    None
                )
                .device,
                Device::Mps
            );
            // Forcing CUDA without a GPU falls back to the CPU; forcing the CPU is honoured.
            assert_eq!(
                plan_hardware(&machine(), Some(Device::Cuda)).device,
                Device::Cpu
            );
            assert_eq!(plan_hardware(&gpu, Some(Device::Cpu)).device, Device::Cpu);
            // Unknown RAM assumes 8 GB; a one-thread machine still gets one thread.
            let tiny = plan_hardware(
                &Machine {
                    cpu_threads: 1,
                    ram_free_gb: None,
                    ..machine()
                },
                None,
            );
            assert_eq!((tiny.threads, tiny.memory_budget_gb), (1, 8.0));
        }

        #[test]
        fn a_model_uses_the_gpu_only_if_it_fits_with_headroom() {
            let gpu = Machine {
                gpu_free_gb: Some(24.0),
                gpu_bf16: true,
                ..machine()
            };
            let plan = plan_hardware(&gpu, None);
            assert_eq!(device_for(16.0, &plan, Some(32.0)), (Device::Cuda, None)); // 22.4 <= 24
            let (d, w) = device_for(18.0, &plan, Some(64.0)); // 25.2 > 24, CPU with 64 GB
            assert_eq!((d, w), (Device::Cpu, None));
            let cpu = plan_hardware(&machine(), None);
            let (d, w) = device_for(7.0, &cpu, Some(11.8));
            assert_eq!(d, Device::Cpu);
            assert!(w.unwrap().contains("18.2 GB"), "float32 doubles the size");
            assert!(device_for(1.0, &cpu, None).1.is_none());
        }

        #[test]
        fn the_merge_grid_has_an_equal_recipe_and_one_favouring_each_model() {
            assert!(
                merge_grid(1).is_empty() && merge_grid(0).is_empty() && merge_grid(65).is_empty()
            );
            let g = merge_grid(3);
            assert_eq!(g.len(), 4);
            assert_eq!(g[0], vec![1.0, 1.0, 1.0]);
            assert_eq!(g[1], vec![0.6, 0.2, 0.2]);
            assert_eq!(g[3], vec![0.2, 0.2, 0.6]);
            for r in &g[1..] {
                assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-12);
            }
        }
    }
}

mod config {
    //! Model hyper-parameters read from GGUF metadata.

    use crate::gguf::{Gguf, GgufError};
    use crate::quant::QuantError;
    use std::fmt;

    /// How rotary position embedding pairs up dimensions.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RopeStyle {
        /// Adjacent pairs `(2i, 2i + 1)` (llama).
        Interleaved,
        /// Split halves `(i, i + dim/2)` (qwen2, "neox").
        SplitHalf,
    }

    /// Failure while loading or running a model.
    #[derive(Debug)]
    pub enum LlmError {
        /// The GGUF file is invalid or lacks a key.
        Gguf(GgufError),
        /// Dequantisation failed (unsupported tensor type or bad sizes).
        Quant(QuantError),
        /// The architecture is not supported.
        UnsupportedArch(String),
        /// A hyper-parameter is inconsistent (the text says which).
        BadConfig(&'static str),
        /// A required tensor is missing.
        MissingTensor(String),
        /// A tensor has the wrong shape.
        BadTensorShape(String),
        /// A token id is outside the vocabulary.
        BadToken(u32),
        /// The context window is full.
        ContextFull,
        /// A computation produced a non-finite value.
        NonFinite,
    }

    impl fmt::Display for LlmError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                LlmError::Gguf(e) => write!(f, "gguf: {e}"),
                LlmError::Quant(e) => write!(f, "quantisation: {e}"),
                LlmError::UnsupportedArch(a) => write!(f, "unsupported architecture {a:?}"),
                LlmError::BadConfig(m) => write!(f, "bad configuration: {m}"),
                LlmError::MissingTensor(n) => write!(f, "missing tensor {n:?}"),
                LlmError::BadTensorShape(n) => write!(f, "tensor {n:?} has an unexpected shape"),
                LlmError::BadToken(t) => write!(f, "token {t} is outside the vocabulary"),
                LlmError::ContextFull => f.write_str("context window is full"),
                LlmError::NonFinite => f.write_str("non-finite value in the forward pass"),
            }
        }
    }

    impl std::error::Error for LlmError {}

    impl From<GgufError> for LlmError {
        fn from(e: GgufError) -> Self {
            LlmError::Gguf(e)
        }
    }

    impl From<QuantError> for LlmError {
        fn from(e: QuantError) -> Self {
            LlmError::Quant(e)
        }
    }

    /// Hyper-parameters of a decoder-only transformer.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Config {
        /// Architecture name from `general.architecture`.
        pub arch: String,
        /// Embedding width.
        pub n_embd: usize,
        /// Number of layers.
        pub n_layer: usize,
        /// Attention heads.
        pub n_head: usize,
        /// Key/value heads (grouped-query attention when smaller than `n_head`).
        pub n_head_kv: usize,
        /// Width of one head.
        pub head_dim: usize,
        /// Feed-forward inner width.
        pub n_ff: usize,
        /// Trained context length.
        pub n_ctx: usize,
        /// Vocabulary size (rows of the embedding matrix).
        pub n_vocab: usize,
        /// RoPE frequency base.
        pub rope_base: f32,
        /// Number of dimensions per head that rotate (at most `head_dim`, even).
        pub rope_dim: usize,
        /// RMSNorm epsilon.
        pub rms_eps: f32,
        /// RoPE pairing.
        pub rope_style: RopeStyle,
        /// Whether Q, K and V projections carry biases.
        pub qkv_bias: bool,
    }

    impl Config {
        /// Read the hyper-parameters. `n_vocab` comes from the shape of `token_embd.weight`.
        pub fn from_gguf(g: &Gguf) -> Result<Config, LlmError> {
            let arch = g.architecture()?.to_string();
            let (rope_style, qkv_bias, default_eps) = match arch.as_str() {
                "llama" => (RopeStyle::Interleaved, false, 1e-5),
                "qwen2" => (RopeStyle::SplitHalf, true, 1e-6),
                other => return Err(LlmError::UnsupportedArch(other.to_string())),
            };
            let n_embd = g.arch_u32("embedding_length")? as usize;
            let n_layer = g.arch_u32("block_count")? as usize;
            let n_head = g.arch_u32("attention.head_count")? as usize;
            let n_head_kv = g
                .arch_u32("attention.head_count_kv")
                .map_or(n_head, |v| v as usize);
            let n_ff = g.arch_u32("feed_forward_length")? as usize;
            let n_ctx = g.arch_u32("context_length")? as usize;
            let key = |s: &str| format!("{arch}.{s}");
            let rope_base = g.get_f32(&key("rope.freq_base")).unwrap_or(10_000.0);
            let rms_eps = g
                .get_f32(&key("attention.layer_norm_rms_epsilon"))
                .unwrap_or(default_eps);
            let embd = g
                .tensor("token_embd.weight")
                .ok_or_else(|| LlmError::MissingTensor("token_embd.weight".into()))?;
            if embd.dims.len() != 2 || embd.dims[0] as usize != n_embd {
                return Err(LlmError::BadTensorShape("token_embd.weight".into()));
            }
            let n_vocab = embd.dims[1] as usize;
            if n_head == 0 || n_head_kv == 0 || n_layer == 0 || n_embd == 0 {
                return Err(LlmError::BadConfig("zero-sized dimension"));
            }
            if !n_embd.is_multiple_of(n_head) {
                return Err(LlmError::BadConfig(
                    "embedding length is not a multiple of the head count",
                ));
            }
            if !n_head.is_multiple_of(n_head_kv) {
                return Err(LlmError::BadConfig(
                    "head count is not a multiple of the kv head count",
                ));
            }
            let head_dim = n_embd / n_head;
            let rope_dim = g
                .arch_u32("rope.dimension_count")
                .map_or(head_dim, |v| v as usize);
            if rope_dim == 0 || rope_dim > head_dim || !rope_dim.is_multiple_of(2) {
                return Err(LlmError::BadConfig(
                    "rope dimension count must be even and at most the head width",
                ));
            }
            if !rope_base.is_finite() || rope_base <= 0.0 || !rms_eps.is_finite() || rms_eps < 0.0 {
                return Err(LlmError::BadConfig("rope base or epsilon out of range"));
            }
            Ok(Config {
                arch,
                n_embd,
                n_layer,
                n_head,
                n_head_kv,
                head_dim,
                n_ff,
                n_ctx,
                n_vocab,
                rope_base,
                rope_dim,
                rms_eps,
                rope_style,
                qkv_bias,
            })
        }

        /// Width of the key (and value) projection output.
        pub fn kv_dim(&self) -> usize {
            self.n_head_kv * self.head_dim
        }
    }
}
mod model {
    //! Weights: tensor lookup, shape checks and quantised matrix-vector products.

    use crate::config::{Config, LlmError};
    use crate::gguf::types::GgmlType;
    use crate::gguf::{parse, Gguf, MappedGguf};
    use std::path::Path;

    enum Backing {
        Owned(Vec<u8>),
        Mapped(MappedGguf),
    }

    /// A 2-D weight matrix inside the file bytes: `rows` output rows of `cols` inputs each.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Mat {
        pub(crate) ty: GgmlType,
        start: usize,
        end: usize,
        pub(crate) rows: usize,
        pub(crate) cols: usize,
    }

    pub(crate) struct Layer {
        pub(crate) attn_norm: Vec<f32>,
        pub(crate) wq: Mat,
        pub(crate) wk: Mat,
        pub(crate) wv: Mat,
        pub(crate) wo: Mat,
        pub(crate) bq: Option<Vec<f32>>,
        pub(crate) bk: Option<Vec<f32>>,
        pub(crate) bv: Option<Vec<f32>>,
        pub(crate) ffn_norm: Vec<f32>,
        pub(crate) w_gate: Mat,
        pub(crate) w_up: Mat,
        pub(crate) w_down: Mat,
    }

    /// A loaded model: configuration plus weight locations in the GGUF bytes.
    pub struct Model {
        cfg: Config,
        gguf: Gguf,
        backing: Backing,
        pub(crate) layers: Vec<Layer>,
        pub(crate) token_embd: Mat,
        pub(crate) output_norm: Vec<f32>,
        pub(crate) output: Mat,
    }

    impl Model {
        /// Load from file bytes held in memory.
        pub fn from_bytes(bytes: Vec<u8>) -> Result<Model, LlmError> {
            let gguf = parse(&bytes)?;
            Model::build(gguf, Backing::Owned(bytes))
        }

        /// Memory-map and load the GGUF file at `path`.
        pub fn open(path: impl AsRef<Path>) -> Result<Model, LlmError> {
            let mapped = MappedGguf::open(path)?;
            let gguf = mapped.gguf().clone();
            Model::build(gguf, Backing::Mapped(mapped))
        }

        /// The hyper-parameters.
        pub fn config(&self) -> &Config {
            &self.cfg
        }

        /// The parsed GGUF table (metadata and tensor infos), e.g. to build a tokenizer.
        pub fn gguf(&self) -> &Gguf {
            &self.gguf
        }

        pub(crate) fn bytes(&self) -> &[u8] {
            match &self.backing {
                Backing::Owned(v) => v,
                Backing::Mapped(m) => m.bytes(),
            }
        }

        fn mat(
            g: &Gguf,
            bytes_len: usize,
            name: &str,
            cols: usize,
            rows: usize,
        ) -> Result<Mat, LlmError> {
            let t = g
                .tensor(name)
                .ok_or_else(|| LlmError::MissingTensor(name.to_string()))?;
            if t.dims.len() != 2 || t.dims[0] as usize != cols || t.dims[1] as usize != rows {
                return Err(LlmError::BadTensorShape(name.to_string()));
            }
            if !crate::quant::is_supported(t.ty) {
                return Err(LlmError::Quant(crate::quant::QuantError::Unsupported(t.ty)));
            }
            let r = t.data_range(g.data_start)?;
            let (start, end) = (r.start as usize, r.end as usize);
            if end > bytes_len {
                return Err(LlmError::BadTensorShape(name.to_string()));
            }
            Ok(Mat {
                ty: t.ty,
                start,
                end,
                rows,
                cols,
            })
        }

        fn vector(g: &Gguf, bytes: &[u8], name: &str, len: usize) -> Result<Vec<f32>, LlmError> {
            let t = g
                .tensor(name)
                .ok_or_else(|| LlmError::MissingTensor(name.to_string()))?;
            if t.dims.len() != 1 || t.dims[0] as usize != len {
                return Err(LlmError::BadTensorShape(name.to_string()));
            }
            let data = g.tensor_data(bytes, name)?;
            Ok(crate::quant::dequantize_vec(t.ty, data, len)?)
        }

        fn build(gguf: Gguf, backing: Backing) -> Result<Model, LlmError> {
            let cfg = Config::from_gguf(&gguf)?;
            let bytes: &[u8] = match &backing {
                Backing::Owned(v) => v,
                Backing::Mapped(m) => m.bytes(),
            };
            let g = &gguf;
            let n = bytes.len();
            let (e, kv, ff) = (cfg.n_embd, cfg.kv_dim(), cfg.n_ff);
            let q_dim = cfg.n_head * cfg.head_dim;
            let token_embd = Model::mat(g, n, "token_embd.weight", e, cfg.n_vocab)?;
            let output_norm = Model::vector(g, bytes, "output_norm.weight", e)?;
            // Tied embeddings: no separate output matrix.
            let output = if g.tensor("output.weight").is_some() {
                Model::mat(g, n, "output.weight", e, cfg.n_vocab)?
            } else {
                token_embd
            };
            let mut layers = Vec::with_capacity(cfg.n_layer);
            for l in 0..cfg.n_layer {
                let p = |s: &str| format!("blk.{l}.{s}");
                let bias = |name: &str, len: usize| -> Result<Option<Vec<f32>>, LlmError> {
                    if cfg.qkv_bias {
                        Ok(Some(Model::vector(g, bytes, &p(name), len)?))
                    } else {
                        Ok(None)
                    }
                };
                layers.push(Layer {
                    attn_norm: Model::vector(g, bytes, &p("attn_norm.weight"), e)?,
                    wq: Model::mat(g, n, &p("attn_q.weight"), e, q_dim)?,
                    wk: Model::mat(g, n, &p("attn_k.weight"), e, kv)?,
                    wv: Model::mat(g, n, &p("attn_v.weight"), e, kv)?,
                    wo: Model::mat(g, n, &p("attn_output.weight"), q_dim, e)?,
                    bq: bias("attn_q.bias", q_dim)?,
                    bk: bias("attn_k.bias", kv)?,
                    bv: bias("attn_v.bias", kv)?,
                    ffn_norm: Model::vector(g, bytes, &p("ffn_norm.weight"), e)?,
                    w_gate: Model::mat(g, n, &p("ffn_gate.weight"), e, ff)?,
                    w_up: Model::mat(g, n, &p("ffn_up.weight"), e, ff)?,
                    w_down: Model::mat(g, n, &p("ffn_down.weight"), ff, e)?,
                });
            }
            Ok(Model {
                cfg,
                gguf,
                backing,
                layers,
                token_embd,
                output_norm,
                output,
            })
        }

        /// `out = m * x` with on-the-fly dequantisation.
        pub(crate) fn matvec(&self, m: &Mat, x: &[f32], out: &mut [f32]) -> Result<(), LlmError> {
            Ok(crate::quant::matvec(
                m.ty,
                &self.bytes()[m.start..m.end],
                m.rows,
                m.cols,
                x,
                out,
            )?)
        }

        /// The embedding row of `token`.
        pub(crate) fn embed(&self, token: u32, out: &mut [f32]) -> Result<(), LlmError> {
            let m = &self.token_embd;
            let row = token as usize;
            if row >= m.rows {
                return Err(LlmError::BadToken(token));
            }
            let rb = crate::quant::byte_len(m.ty, m.cols)?;
            let start = m.start + row * rb;
            Ok(crate::quant::dequantize(
                m.ty,
                &self.bytes()[start..start + rb],
                out,
            )?)
        }
    }
}
mod sampler {
    //! Token sampling: greedy, temperature, top-k, top-p, with a seeded generator.

    /// Chooses the next token from logits.
    #[derive(Debug, Clone)]
    pub struct Sampler {
        temperature: f32,
        top_k: usize,
        top_p: f32,
        rng: u64,
    }

    impl Sampler {
        /// Always pick the highest logit (ties go to the lowest id).
        pub fn greedy() -> Self {
            Sampler {
                temperature: 0.0,
                top_k: 0,
                top_p: 1.0,
                rng: 0,
            }
        }

        /// Random sampling. `temperature <= 0` means greedy. `top_k == 0` disables top-k;
        /// `top_p >= 1` disables nucleus filtering. The same seed gives the same draws.
        pub fn new(temperature: f32, top_k: usize, top_p: f32, seed: u64) -> Self {
            Sampler {
                temperature,
                top_k,
                top_p,
                rng: seed ^ 0x9E37_79B9_7F4A_7C15,
            }
        }

        fn next_unit(&mut self) -> f64 {
            // SplitMix64
            self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 53) as f64
        }

        /// Pick a token id. NaN logits count as negative infinity; with no usable logit (or an
        /// empty slice) the result is 0.
        pub fn sample(&mut self, logits: &[f32]) -> u32 {
            let argmax = || {
                let mut best: Option<(usize, f32)> = None;
                for (i, v) in logits.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    if best.is_none_or(|(_, b)| *v > b) {
                        best = Some((i, *v));
                    }
                }
                best.map_or(0, |(i, _)| i as u32)
            };
            if self.temperature <= 0.0 || !self.temperature.is_finite() || logits.len() < 2 {
                return argmax();
            }
            let mut cand: Vec<(usize, f32)> = logits
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, v)| !v.is_nan())
                .collect();
            if cand.is_empty() {
                return 0;
            }
            cand.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            if self.top_k > 0 && self.top_k < cand.len() {
                cand.truncate(self.top_k);
            }
            let max = cand[0].1;
            if max == f32::NEG_INFINITY {
                return argmax();
            }
            let mut probs: Vec<f64> = cand
                .iter()
                .map(|(_, v)| (f64::from(*v - max) / f64::from(self.temperature)).exp())
                .collect();
            let total: f64 = probs.iter().sum();
            for p in &mut probs {
                *p /= total;
            }
            if self.top_p > 0.0 && self.top_p < 1.0 {
                let mut cum = 0.0;
                let mut keep = probs.len();
                for (i, p) in probs.iter().enumerate() {
                    cum += p;
                    if cum >= f64::from(self.top_p) {
                        keep = i + 1;
                        break;
                    }
                }
                cand.truncate(keep);
                probs.truncate(keep);
                let t: f64 = probs.iter().sum();
                for p in &mut probs {
                    *p /= t;
                }
            }
            let u = self.next_unit();
            let mut cum = 0.0;
            for ((id, _), p) in cand.iter().zip(&probs) {
                cum += p;
                if u < cum {
                    return *id as u32;
                }
            }
            cand[cand.len() - 1].0 as u32
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn greedy_picks_the_max_and_breaks_ties_low() {
            let mut s = Sampler::greedy();
            assert_eq!(s.sample(&[0.1, 0.9, 0.5]), 1);
            assert_eq!(s.sample(&[1.0, 1.0, 0.5]), 0);
            assert_eq!(s.sample(&[f32::NAN, 0.2, f32::NAN]), 1);
            assert_eq!(s.sample(&[f32::NEG_INFINITY, f32::NEG_INFINITY]), 0);
            assert_eq!(s.sample(&[]), 0);
            assert_eq!(s.sample(&[5.0]), 0);
        }

        #[test]
        fn zero_temperature_is_greedy() {
            let mut s = Sampler::new(0.0, 5, 0.9, 1);
            for _ in 0..20 {
                assert_eq!(s.sample(&[0.0, 3.0, 1.0]), 1);
            }
        }

        #[test]
        fn same_seed_same_sequence_different_seed_differs() {
            let logits: Vec<f32> = (0..50).map(|i| (i % 7) as f32 * 0.3).collect();
            let draw = |seed| {
                let mut s = Sampler::new(1.0, 0, 1.0, seed);
                (0..40).map(|_| s.sample(&logits)).collect::<Vec<_>>()
            };
            assert_eq!(draw(5), draw(5));
            assert_ne!(draw(5), draw(6));
        }

        #[test]
        fn top_k_restricts_candidates() {
            let logits = [5.0f32, 4.0, 3.0, 2.0, 1.0, 0.0];
            let mut s = Sampler::new(5.0, 2, 1.0, 9); // hot, but only ids 0 and 1 allowed
            for _ in 0..200 {
                assert!(s.sample(&logits) <= 1);
            }
        }

        #[test]
        fn top_p_keeps_the_nucleus() {
            // probabilities ~ [0.88, 0.12, tiny...]: top_p 0.5 keeps only the first
            let logits = [4.0f32, 2.0, -5.0, -5.0];
            let mut s = Sampler::new(1.0, 0, 0.5, 3);
            for _ in 0..200 {
                assert_eq!(s.sample(&logits), 0);
            }
        }

        #[test]
        fn sampling_follows_the_distribution() {
            // two equal logits: roughly half each
            let mut s = Sampler::new(1.0, 0, 1.0, 11);
            let n = 4000;
            let ones = (0..n).filter(|_| s.sample(&[1.0, 1.0]) == 1).count();
            assert!((1700..2300).contains(&ones), "{ones}");
        }

        #[test]
        fn hot_temperature_flattens_cold_sharpens() {
            let logits = [2.0f32, 1.0, 0.0];
            let count_top = |t: f32| {
                let mut s = Sampler::new(t, 0, 1.0, 17);
                (0..2000).filter(|_| s.sample(&logits) == 0).count()
            };
            assert!(count_top(0.2) > count_top(1.0) && count_top(1.0) > count_top(5.0));
        }

        #[test]
        fn degenerate_inputs_do_not_panic() {
            let mut s = Sampler::new(1.0, 0, 0.9, 1);
            assert_eq!(s.sample(&[f32::NAN, f32::NAN]), 0);
            assert_eq!(
                s.sample(&[f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY]),
                0
            );
            let _ = s.sample(&[f32::INFINITY, 1.0]);
            let mut hot = Sampler::new(f32::NAN, 0, 1.0, 1);
            assert_eq!(hot.sample(&[0.0, 1.0]), 1);
        }
    }
}
mod session {
    //! One generation session: KV cache, position and the per-token forward pass.

    use crate::config::{LlmError, RopeStyle};
    use crate::kernels::ops;
    use crate::model::Model;
    use crate::sampler::Sampler;

    /// State of one conversation with a [`Model`]: the KV cache and the next position.
    pub struct Session<'m> {
        model: &'m Model,
        limit: usize,
        pos: usize,
        /// Keys, laid out `[layer][kv_head][position][head_dim]`.
        k: Vec<f32>,
        /// Values, same layout.
        v: Vec<f32>,
        x: Vec<f32>,
        xn: Vec<f32>,
        q: Vec<f32>,
        kbuf: Vec<f32>,
        vbuf: Vec<f32>,
        attn: Vec<f32>,
        gate: Vec<f32>,
        up: Vec<f32>,
        hidden: Vec<f32>,
        logits: Vec<f32>,
        scores: Vec<f32>,
    }

    /// Rotate the first `rope_dim` dimensions of every head.
    fn rope(
        v: &mut [f32],
        heads: usize,
        head_dim: usize,
        rope_dim: usize,
        pos: usize,
        base: f32,
        style: RopeStyle,
    ) {
        for h in 0..heads {
            let o = h * head_dim;
            for i in 0..rope_dim / 2 {
                let freq = base.powf(-((2 * i) as f32) / rope_dim as f32);
                let (s, c) = (pos as f32 * freq).sin_cos();
                let (a, b) = match style {
                    RopeStyle::Interleaved => (o + 2 * i, o + 2 * i + 1),
                    RopeStyle::SplitHalf => (o + i, o + i + rope_dim / 2),
                };
                let (x0, x1) = (v[a], v[b]);
                v[a] = x0 * c - x1 * s;
                v[b] = x0 * s + x1 * c;
            }
        }
    }

    fn map_op<T>(r: Result<T, crate::kernels::OpError>) -> Result<T, LlmError> {
        r.map_err(|_| LlmError::NonFinite)
    }

    impl<'m> Session<'m> {
        /// A fresh session that can hold up to `max_ctx` tokens (`1..=` the trained context).
        pub fn new(model: &'m Model, max_ctx: usize) -> Result<Session<'m>, LlmError> {
            let c = model.config();
            if max_ctx == 0 || max_ctx > c.n_ctx {
                return Err(LlmError::BadConfig(
                    "max_ctx must be between 1 and the trained context length",
                ));
            }
            let cache = c.n_layer * c.n_head_kv * max_ctx * c.head_dim;
            let q_dim = c.n_head * c.head_dim;
            Ok(Session {
                model,
                limit: max_ctx,
                pos: 0,
                k: vec![0.0; cache],
                v: vec![0.0; cache],
                x: vec![0.0; c.n_embd],
                xn: vec![0.0; c.n_embd],
                q: vec![0.0; q_dim],
                kbuf: vec![0.0; c.kv_dim()],
                vbuf: vec![0.0; c.kv_dim()],
                attn: vec![0.0; q_dim],
                gate: vec![0.0; c.n_ff],
                up: vec![0.0; c.n_ff],
                hidden: vec![0.0; c.n_ff],
                logits: vec![0.0; c.n_vocab],
                scores: vec![0.0; max_ctx],
            })
        }

        /// Tokens processed so far.
        pub fn position(&self) -> usize {
            self.pos
        }

        /// Tokens this session can still take.
        pub fn remaining(&self) -> usize {
            self.limit - self.pos
        }

        /// Forget everything and start again from position 0.
        pub fn reset(&mut self) {
            self.pos = 0;
        }

        fn cache_base(&self, layer: usize, kv_head: usize) -> usize {
            let c = self.model.config();
            ((layer * c.n_head_kv + kv_head) * self.limit) * c.head_dim
        }

        /// Process one token and return the logits for the next one.
        pub fn forward(&mut self, token: u32) -> Result<&[f32], LlmError> {
            let m = self.model;
            let c = m.config();
            if self.pos >= self.limit {
                return Err(LlmError::ContextFull);
            }
            let pos = self.pos;
            m.embed(token, &mut self.x)?;
            let group = c.n_head / c.n_head_kv;
            let scale = 1.0 / (c.head_dim as f32).sqrt();

            for (l, layer) in m.layers.iter().enumerate() {
                map_op(ops::rmsnorm(
                    &self.x,
                    &layer.attn_norm,
                    c.rms_eps,
                    &mut self.xn,
                ))?;
                m.matvec(&layer.wq, &self.xn, &mut self.q)?;
                m.matvec(&layer.wk, &self.xn, &mut self.kbuf)?;
                m.matvec(&layer.wv, &self.xn, &mut self.vbuf)?;
                for (buf, bias) in [
                    (&mut self.q, &layer.bq),
                    (&mut self.kbuf, &layer.bk),
                    (&mut self.vbuf, &layer.bv),
                ] {
                    if let Some(b) = bias {
                        for (v, b) in buf.iter_mut().zip(b) {
                            *v += b;
                        }
                    }
                }
                rope(
                    &mut self.q,
                    c.n_head,
                    c.head_dim,
                    c.rope_dim,
                    pos,
                    c.rope_base,
                    c.rope_style,
                );
                rope(
                    &mut self.kbuf,
                    c.n_head_kv,
                    c.head_dim,
                    c.rope_dim,
                    pos,
                    c.rope_base,
                    c.rope_style,
                );

                for h in 0..c.n_head_kv {
                    let at = self.cache_base(l, h) + pos * c.head_dim;
                    self.k[at..at + c.head_dim]
                        .copy_from_slice(&self.kbuf[h * c.head_dim..(h + 1) * c.head_dim]);
                    self.v[at..at + c.head_dim]
                        .copy_from_slice(&self.vbuf[h * c.head_dim..(h + 1) * c.head_dim]);
                }
                for h in 0..c.n_head {
                    let base = self.cache_base(l, h / group);
                    let len = (pos + 1) * c.head_dim;
                    map_op(ops::attention(
                        &self.q[h * c.head_dim..(h + 1) * c.head_dim],
                        &self.k[base..base + len],
                        &self.v[base..base + len],
                        pos + 1,
                        scale,
                        &mut self.scores,
                        &mut self.attn[h * c.head_dim..(h + 1) * c.head_dim],
                    ))?;
                }
                m.matvec(&layer.wo, &self.attn, &mut self.xn)?;
                for (x, o) in self.x.iter_mut().zip(&self.xn) {
                    *x += o;
                }

                map_op(ops::rmsnorm(
                    &self.x,
                    &layer.ffn_norm,
                    c.rms_eps,
                    &mut self.xn,
                ))?;
                m.matvec(&layer.w_gate, &self.xn, &mut self.gate)?;
                m.matvec(&layer.w_up, &self.xn, &mut self.up)?;
                map_op(ops::swiglu(&self.gate, &self.up, &mut self.hidden))?;
                m.matvec(&layer.w_down, &self.hidden, &mut self.xn)?;
                for (x, o) in self.x.iter_mut().zip(&self.xn) {
                    *x += o;
                }
            }

            map_op(ops::rmsnorm(
                &self.x,
                &m.output_norm,
                c.rms_eps,
                &mut self.xn,
            ))?;
            m.matvec(&m.output, &self.xn, &mut self.logits)?;
            if self.logits.iter().any(|v| !v.is_finite()) {
                return Err(LlmError::NonFinite);
            }
            self.pos += 1;
            Ok(&self.logits)
        }

        /// Process several tokens and return the logits after the last one.
        pub fn prefill(&mut self, tokens: &[u32]) -> Result<&[f32], LlmError> {
            if tokens.is_empty() {
                return Err(LlmError::BadConfig("prefill needs at least one token"));
            }
            if tokens.len() > self.remaining() {
                return Err(LlmError::ContextFull);
            }
            for t in &tokens[..tokens.len() - 1] {
                self.forward(*t)?;
            }
            self.forward(tokens[tokens.len() - 1])
        }

        /// Feed `prompt`, then sample up to `max_new` tokens, stopping early on a token in `stop`
        /// (the stop token is not included). Stops at the end of the context window.
        pub fn generate(
            &mut self,
            prompt: &[u32],
            max_new: usize,
            sampler: &mut Sampler,
            stop: &[u32],
        ) -> Result<Vec<u32>, LlmError> {
            let mut logits = self.prefill(prompt)?.to_vec();
            let mut out = Vec::new();
            for _ in 0..max_new {
                let next = sampler.sample(&logits);
                if stop.contains(&next) {
                    break;
                }
                out.push(next);
                if self.remaining() == 0 {
                    break;
                }
                logits = self.forward(next)?.to_vec();
            }
            Ok(out)
        }
    }
}

pub use config::{Config, LlmError, RopeStyle};
pub use model::Model;
pub use sampler::Sampler;
pub use session::Session;
