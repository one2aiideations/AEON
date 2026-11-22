//! Block-device model sources (NVMe namespaces and similar).
//!
//! A device reads whole logical blocks (LBAs). [`BlockSource`] turns any
//! [`BlockDevice`] into a byte-addressed [`ModelSource`], using a bounce
//! buffer for unaligned ranges. [`QueuedDevice`] is the submit/poll shape of
//! a hardware queue pair; [`Polled`] adapts it to a blocking [`BlockDevice`].

use crate::source::ModelSource;
use crate::ModelError;

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
        if offset % bs == 0 && n % bs == 0 {
            return self.dev.read_blocks(offset / bs, buf);
        }
        let first = offset / bs;
        let last = (end - 1) / bs;
        let span = usize::try_from((last - first + 1) * bs).map_err(|_| ModelError::OutOfBounds)?;
        let mut bounce = vec![0u8; span];
        self.dev.read_blocks(first, &mut bounce)?;
        let skip = usize::try_from(offset - first * bs).map_err(|_| ModelError::OutOfBounds)?;
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
        if bs == 0 || buf.is_empty() || buf.len() % bs != 0 {
            return Err(ModelError::BadConfig);
        }
        let count = u32::try_from(buf.len() / bs).map_err(|_| ModelError::BadConfig)?;
        let cid = self.q.submit_read(lba, count, buf)?;
        for _ in 0..self.max_polls {
            match self.q.poll(cid)? {
                Poll::Pending => std::hint::spin_loop(),
                Poll::Done => return Ok(()),
                Poll::Failed => return Err(ModelError::Io(format!("read cid {cid} failed"))),
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
        fn submit_read(&self, lba: u64, _count: u32, buf: &mut [u8]) -> Result<u16, ModelError> {
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
