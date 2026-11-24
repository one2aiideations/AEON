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

mod cursor;
pub mod error;
pub mod mmap;
pub mod reader;
pub mod tensor;
pub mod types;
pub mod value;
pub mod writer;

pub use error::GgufError;
pub use mmap::MappedGguf;
pub use reader::{parse, parse_header_only, Gguf, ModelSummary};
pub use tensor::TensorInfo;
pub use types::{GgmlType, ValueType};
pub use value::{Array, Value};
pub use writer::GgufWriter;
