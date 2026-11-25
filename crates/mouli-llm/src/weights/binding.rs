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
        let ix = manifest::parse("a | 0 | 4 | I8 | 1 | 4\nb | 4 | 8 | F32 | 1 | 2\n", 12).unwrap();
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
