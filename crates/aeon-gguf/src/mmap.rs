//! Memory-mapped GGUF files.
//!
//! This module holds the crate's only `unsafe` code: one `Mmap::map` call.
#![allow(unsafe_code)]

use crate::error::GgufError;
use crate::reader::{parse, Gguf};
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

/// A GGUF file mapped read-only into memory, with its parsed table.
///
/// The file must not be truncated or modified while it is mapped. Truncating a mapped
/// file makes reads of the removed pages fault (`SIGBUS` on Linux), which aborts the
/// process; that cannot be prevented from inside this crate. AEON model files are
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
    use crate::types::GgmlType;
    use crate::value::Value;
    use crate::writer::GgufWriter;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("aeon-gguf-{tag}-{}-{n}.gguf", std::process::id()))
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
            MappedGguf::open("/nonexistent/aeon/model.gguf"),
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
