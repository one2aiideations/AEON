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
