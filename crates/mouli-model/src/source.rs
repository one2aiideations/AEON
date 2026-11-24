//! Random-access byte sources and a bounded sequential stream.

use crate::ModelError;
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
        let path = std::env::temp_dir().join(format!("mouli-model-src-{}", std::process::id()));
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
