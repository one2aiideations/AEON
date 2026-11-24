//! M030: incremental SQLite WAL commit-frame parser and bounded CDC fan-out.
//! Emits committed page-frame batches. A database-specific decoder can translate
//! these page images into row-level INSERT/UPDATE/DELETE events before publishing.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

const WAL_HEADER_BYTES: usize = 32;
const FRAME_HEADER_BYTES: usize = 24;
const MIN_PAGE_SIZE: u32 = 512;
const MAX_PAGE_SIZE: u32 = 65_536;
const MAX_BATCH_FRAMES: usize = 65_536;
const MAX_BATCH_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalHeader {
    pub magic: u32,
    pub version: u32,
    pub page_size: u32,
    pub checkpoint_sequence: u32,
    pub salt1: u32,
    pub salt2: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageFrame {
    pub frame_number: u64,
    pub page_number: u32,
    pub database_size_after_commit: u32,
    pub page_data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitBatch {
    pub epoch: u64,
    pub commit_sequence: u64,
    pub database_pages: u32,
    pub frames: Vec<PageFrame>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalError {
    InvalidHeader,
    UnsupportedVersion,
    InvalidPageSize,
    InvalidFrame,
    SaltMismatch,
    FrameLimitExceeded,
    BatchByteLimitExceeded,
    EpochExhausted,
    SequenceExhausted,
    SubscriberLagged,
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    let array: [u8; 4] = slice.try_into().ok()?;
    Some(u32::from_be_bytes(array))
}

impl WalHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self, WalError> {
        if bytes.len() < WAL_HEADER_BYTES {
            return Err(WalError::InvalidHeader);
        }
        let magic = be_u32(bytes, 0).ok_or(WalError::InvalidHeader)?;
        if magic != 0x377f0682 && magic != 0x377f0683 {
            return Err(WalError::InvalidHeader);
        }
        let version = be_u32(bytes, 4).ok_or(WalError::InvalidHeader)?;
        if version != 3_007_000 {
            return Err(WalError::UnsupportedVersion);
        }
        let page_size = be_u32(bytes, 8).ok_or(WalError::InvalidHeader)?;
        if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) || !page_size.is_power_of_two() {
            return Err(WalError::InvalidPageSize);
        }
        Ok(Self {
            magic,
            version,
            page_size,
            checkpoint_sequence: be_u32(bytes, 12).ok_or(WalError::InvalidHeader)?,
            salt1: be_u32(bytes, 16).ok_or(WalError::InvalidHeader)?,
            salt2: be_u32(bytes, 20).ok_or(WalError::InvalidHeader)?,
        })
    }
    pub fn frame_size(&self) -> Option<usize> {
        FRAME_HEADER_BYTES.checked_add(usize::try_from(self.page_size).ok()?)
    }
}

pub struct WalTailer {
    header: Option<WalHeader>,
    epoch: u64,
    next_frame: u64,
    next_commit: u64,
    pending: Vec<PageFrame>,
    pending_bytes: usize,
    buffer: Vec<u8>,
}
impl Default for WalTailer {
    fn default() -> Self {
        Self {
            header: None,
            epoch: 0,
            next_frame: 1,
            next_commit: 1,
            pending: Vec::new(),
            pending_bytes: 0,
            buffer: Vec::new(),
        }
    }
}
impl WalTailer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed any contiguous WAL byte range. Incomplete header/frame bytes are retained.
    /// Call reset_epoch after detecting a WAL truncate/restart or changed salts.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<CommitBatch>, WalError> {
        let combined = self
            .buffer
            .len()
            .checked_add(bytes.len())
            .ok_or(WalError::BatchByteLimitExceeded)?;
        if combined
            > MAX_BATCH_BYTES
                .saturating_add(WAL_HEADER_BYTES)
                .saturating_add(65_560)
        {
            return Err(WalError::BatchByteLimitExceeded);
        }
        self.buffer.extend_from_slice(bytes);
        let mut committed = Vec::new();

        if self.header.is_none() {
            if self.buffer.len() < WAL_HEADER_BYTES {
                return Ok(committed);
            }
            let header = WalHeader::parse(&self.buffer)?;
            self.buffer.drain(..WAL_HEADER_BYTES);
            self.header = Some(header);
        }

        while let Some(header) = self.header.as_ref() {
            let frame_size = header.frame_size().ok_or(WalError::InvalidFrame)?;
            if self.buffer.len() < frame_size {
                break;
            }
            let page_number = be_u32(&self.buffer, 0).ok_or(WalError::InvalidFrame)?;
            let database_size = be_u32(&self.buffer, 4).ok_or(WalError::InvalidFrame)?;
            let salt1 = be_u32(&self.buffer, 8).ok_or(WalError::InvalidFrame)?;
            let salt2 = be_u32(&self.buffer, 12).ok_or(WalError::InvalidFrame)?;
            if page_number == 0 {
                return Err(WalError::InvalidFrame);
            }
            if salt1 != header.salt1 || salt2 != header.salt2 {
                return Err(WalError::SaltMismatch);
            }
            if self.pending.len() >= MAX_BATCH_FRAMES {
                return Err(WalError::FrameLimitExceeded);
            }
            let page_start = FRAME_HEADER_BYTES;
            let page_end = page_start
                .checked_add(header.page_size as usize)
                .ok_or(WalError::InvalidFrame)?;
            let page_data = self
                .buffer
                .get(page_start..page_end)
                .ok_or(WalError::InvalidFrame)?
                .to_vec();
            self.buffer.drain(..frame_size);
            let page_bytes = page_data.len();
            self.pending_bytes = self
                .pending_bytes
                .checked_add(page_bytes)
                .ok_or(WalError::BatchByteLimitExceeded)?;
            if self.pending_bytes > MAX_BATCH_BYTES {
                return Err(WalError::BatchByteLimitExceeded);
            }
            let frame_number = self.next_frame;
            self.next_frame = self
                .next_frame
                .checked_add(1)
                .ok_or(WalError::SequenceExhausted)?;
            self.pending.push(PageFrame {
                frame_number,
                page_number,
                database_size_after_commit: database_size,
                page_data,
            });
            if database_size != 0 {
                let batch = CommitBatch {
                    epoch: self.epoch,
                    commit_sequence: self.next_commit,
                    database_pages: database_size,
                    frames: std::mem::take(&mut self.pending),
                };
                self.next_commit = self
                    .next_commit
                    .checked_add(1)
                    .ok_or(WalError::SequenceExhausted)?;
                self.pending_bytes = 0;
                committed.push(batch);
            }
        }
        Ok(committed)
    }

    /// Starts a new WAL salt epoch. Pending uncommitted frames are discarded.
    pub fn reset_epoch(&mut self) -> Result<(), WalError> {
        self.epoch = self.epoch.checked_add(1).ok_or(WalError::EpochExhausted)?;
        self.header = None;
        self.next_frame = 1;
        self.next_commit = 1;
        self.pending.clear();
        self.pending_bytes = 0;
        self.buffer.clear();
        Ok(())
    }

    pub fn pending_frame_count(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowOperation {
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowChange {
    pub table: String,
    pub operation: RowOperation,
    pub primary_key: Vec<u8>,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdcEvent {
    pub tenant_id: u64,
    pub source_id: String,
    pub epoch: u64,
    pub commit_sequence: u64,
    pub changes: Vec<RowChange>,
}

pub struct Subscriber {
    pub id: u64,
    sender: SyncSender<CdcEvent>,
}
#[derive(Default)]
pub struct EventHub {
    subscribers: BTreeMap<u64, Subscriber>,
    next_id: u64,
}
impl EventHub {
    pub fn subscribe(&mut self, capacity: usize) -> Result<(u64, Receiver<CdcEvent>), WalError> {
        if capacity == 0 || capacity > 65_536 {
            return Err(WalError::SubscriberLagged);
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(WalError::SequenceExhausted)?;
        let id = self.next_id;
        let (tx, rx) = mpsc::sync_channel(capacity);
        self.subscribers.insert(id, Subscriber { id, sender: tx });
        Ok((id, rx))
    }
    pub fn unsubscribe(&mut self, id: u64) -> bool {
        self.subscribers.remove(&id).is_some()
    }

    /// Fan-out is non-blocking. Full or disconnected subscribers are evicted so
    /// a slow consumer cannot stall WAL ingestion; it must resnapshot/reconnect.
    pub fn publish(&mut self, event: CdcEvent) -> usize {
        let mut evict = Vec::new();
        let mut delivered = 0usize;
        for (id, subscriber) in &self.subscribers {
            match subscriber.sender.try_send(event.clone()) {
                Ok(()) => delivered = delivered.saturating_add(1),
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => evict.push(*id),
            }
        }
        for id in evict {
            self.subscribers.remove(&id);
        }
        delivered
    }
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }
}

/// Converts decoded row changes into a tenant-scoped event. Page-to-row decoding
/// is deliberately delegated to a SQLite-aware adapter.
pub fn make_event(
    tenant_id: u64,
    source_id: String,
    batch: &CommitBatch,
    changes: Vec<RowChange>,
) -> Result<CdcEvent, WalError> {
    if source_id.is_empty() || source_id.len() > 256 {
        return Err(WalError::InvalidFrame);
    }
    if changes.len() > 1_000_000 {
        return Err(WalError::FrameLimitExceeded);
    }
    for change in &changes {
        if change.table.is_empty() || change.table.len() > 256 || change.primary_key.len() > 4096 {
            return Err(WalError::InvalidFrame);
        }
    }
    Ok(CdcEvent {
        tenant_id,
        source_id,
        epoch: batch.epoch,
        commit_sequence: batch.commit_sequence,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wal_header() -> Vec<u8> {
        let mut h = vec![0u8; 32];
        h[0..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
        h[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        h[8..12].copy_from_slice(&512u32.to_be_bytes());
        h[16..20].copy_from_slice(&11u32.to_be_bytes());
        h[20..24].copy_from_slice(&22u32.to_be_bytes());
        h
    }
    fn frame(commit: u32, page: u8) -> Vec<u8> {
        let mut f = vec![0u8; 24 + 512];
        f[0..4].copy_from_slice(&1u32.to_be_bytes());
        f[4..8].copy_from_slice(&commit.to_be_bytes());
        f[8..12].copy_from_slice(&11u32.to_be_bytes());
        f[12..16].copy_from_slice(&22u32.to_be_bytes());
        f[24..].fill(page);
        f
    }
    #[test]
    fn incremental_input_emits_only_at_commit_marker() {
        let mut tail = WalTailer::new();
        let header = wal_header();
        assert!(tail.push(&header).expect("header").is_empty());
        assert!(tail.push(&frame(0, 1)).expect("frame").is_empty());
        assert_eq!(tail.pending_frame_count(), 1);
        let batches = tail.push(&frame(3, 2)).expect("commit");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].frames.len(), 2);
        assert_eq!(batches[0].database_pages, 3);
    }
    #[test]
    fn split_frame_is_buffered() {
        let mut tail = WalTailer::new();
        let mut data = wal_header();
        data.extend(frame(2, 7));
        let split = data.len() - 20;
        assert!(tail.push(&data[..split]).expect("partial").is_empty());
        assert_eq!(tail.push(&data[split..]).expect("rest").len(), 1);
    }
    #[test]
    fn slow_subscriber_is_evicted_without_blocking() {
        let mut hub = EventHub::default();
        let (id, _rx) = hub.subscribe(1).expect("subscribe");
        let event = CdcEvent {
            tenant_id: 1,
            source_id: "db".into(),
            epoch: 0,
            commit_sequence: 1,
            changes: Vec::new(),
        };
        assert_eq!(hub.publish(event.clone()), 1);
        assert_eq!(hub.publish(event), 0);
        assert_eq!(hub.subscriber_count(), 0);
        assert!(!hub.unsubscribe(id));
    }
}
