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
