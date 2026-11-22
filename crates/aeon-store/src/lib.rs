//! AEON system state store.
//!
//! Settings, agent jobs and other small OS state live in memory for speed
//! and are made durable through an append-only log:
//!
//! ```text
//! record = crc32 (u32 LE) | op (u8: 1 put, 2 delete) | key_len (u32 LE) | val_len (u32 LE) | key | val
//! ```
//!
//! Replay stops at the first torn or corrupt record (a crash mid-write), so
//! the store always recovers to the last complete write. [`Store::compact`]
//! rewrites the log with only live keys.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// CRC-32 (IEEE, reflected) without tables.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            let mask = (c & 1).wrapping_neg();
            c = (c >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !c
}

const PUT: u8 = 1;
const DEL: u8 = 2;
const HEADER: usize = 13;

fn encode(op: u8, key: &[u8], val: &[u8]) -> io::Result<Vec<u8>> {
    let kl = u32::try_from(key.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "key too long"))?;
    let vl = u32::try_from(val.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "value too long"))?;
    let mut body = Vec::with_capacity(HEADER - 4 + key.len() + val.len());
    body.push(op);
    body.extend_from_slice(&kl.to_le_bytes());
    body.extend_from_slice(&vl.to_le_bytes());
    body.extend_from_slice(key);
    body.extend_from_slice(val);
    let mut rec = crc32(&body).to_le_bytes().to_vec();
    rec.extend_from_slice(&body);
    Ok(rec)
}

/// Apply every complete record in `log`; returns the map and the number of
/// valid bytes (the rest is a torn tail).
#[must_use]
pub fn replay(log: &[u8]) -> (BTreeMap<Vec<u8>, Vec<u8>>, usize) {
    let mut map = BTreeMap::new();
    let mut pos = 0;
    while log.len() - pos >= HEADER {
        let crc = u32::from_le_bytes([log[pos], log[pos + 1], log[pos + 2], log[pos + 3]]);
        let op = log[pos + 4];
        let kl = u32::from_le_bytes([log[pos + 5], log[pos + 6], log[pos + 7], log[pos + 8]]) as usize;
        let vl = u32::from_le_bytes([log[pos + 9], log[pos + 10], log[pos + 11], log[pos + 12]]) as usize;
        let Some(end) = (pos + HEADER).checked_add(kl).and_then(|x| x.checked_add(vl)) else {
            break;
        };
        if end > log.len() || crc32(&log[pos + 4..end]) != crc {
            break;
        }
        let key = log[pos + HEADER..pos + HEADER + kl].to_vec();
        match op {
            PUT => {
                map.insert(key, log[pos + HEADER + kl..end].to_vec());
            }
            DEL => {
                map.remove(&key);
            }
            _ => break,
        }
        pos = end;
    }
    (map, pos)
}

/// Durable key-value store.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    file: File,
    map: BTreeMap<Vec<u8>, Vec<u8>>,
    log_bytes: u64,
}

impl Store {
    /// Open or create the log at `path`, recovering its contents. A torn
    /// tail is truncated away.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut data = Vec::new();
        if path.exists() {
            File::open(path)?.read_to_end(&mut data)?;
        }
        let (map, valid) = replay(&data);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        if valid < data.len() {
            file.set_len(valid as u64)?;
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            map,
            log_bytes: valid as u64,
        })
    }

    fn append(&mut self, rec: &[u8]) -> io::Result<()> {
        self.file.write_all(rec)?;
        self.file.sync_data()?;
        self.log_bytes += rec.len() as u64;
        Ok(())
    }

    /// Set `key` to `val` (durable when this returns).
    pub fn put(&mut self, key: &[u8], val: &[u8]) -> io::Result<()> {
        let rec = encode(PUT, key, val)?;
        self.append(&rec)?;
        self.map.insert(key.to_vec(), val.to_vec());
        Ok(())
    }

    /// Remove `key`. Returns whether it existed.
    pub fn delete(&mut self, key: &[u8]) -> io::Result<bool> {
        if !self.map.contains_key(key) {
            return Ok(false);
        }
        let rec = encode(DEL, key, &[])?;
        self.append(&rec)?;
        self.map.remove(key);
        Ok(true)
    }

    /// Value of `key`.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.map.get(key).map(Vec::as_slice)
    }

    /// Keys starting with `prefix`, in order.
    pub fn scan<'a>(&'a self, prefix: &'a [u8]) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + 'a {
        self.map
            .range(prefix.to_vec()..)
            .take_while(move |(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
    }

    /// Number of live keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Bytes in the log file.
    #[must_use]
    pub fn log_bytes(&self) -> u64 {
        self.log_bytes
    }

    /// Rewrite the log with only live keys (write to a temp file, then rename).
    pub fn compact(&mut self) -> io::Result<()> {
        let tmp = self.path.with_extension("compact");
        {
            let mut f = File::create(&tmp)?;
            for (k, v) in &self.map {
                f.write_all(&encode(PUT, k, v)?)?;
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        self.log_bytes = self.file.metadata()?.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aeon-store-{}-{name}", std::process::id()));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn persists_and_recovers() {
        let p = tmp("persist");
        {
            let mut s = Store::open(&p).unwrap();
            s.put(b"agent/1", b"daily digest").unwrap();
            s.put(b"agent/2", b"backup").unwrap();
            s.put(b"theme", b"dark").unwrap();
            assert!(s.delete(b"agent/2").unwrap());
            assert!(!s.delete(b"nope").unwrap());
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.get(b"agent/1"), Some(&b"daily digest"[..]));
        assert_eq!(s.get(b"agent/2"), None);
        let agents: Vec<&[u8]> = s.scan(b"agent/").map(|(k, _)| k).collect();
        assert_eq!(agents, vec![&b"agent/1"[..]]);
        drop(s);
        fs::remove_file(&p).unwrap();
    }

    #[test]
    fn torn_tail_is_dropped() {
        let p = tmp("torn");
        {
            let mut s = Store::open(&p).unwrap();
            s.put(b"a", b"1").unwrap();
            s.put(b"b", b"2").unwrap();
        }
        let full = fs::read(&p).unwrap();
        fs::write(&p, &full[..full.len() - 1]).unwrap();
        let mut s = Store::open(&p).unwrap();
        assert_eq!(s.get(b"a"), Some(&b"1"[..]));
        assert_eq!(s.get(b"b"), None);
        s.put(b"c", b"3").unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.len(), 2);
        drop(s);
        fs::remove_file(&p).unwrap();
    }

    #[test]
    fn compaction_shrinks_log() {
        let p = tmp("compact");
        let mut s = Store::open(&p).unwrap();
        for i in 0..50u8 {
            s.put(b"counter", &[i]).unwrap();
        }
        let before = s.log_bytes();
        s.compact().unwrap();
        assert!(s.log_bytes() < before / 10);
        s.put(b"x", b"y").unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.get(b"counter"), Some(&[49u8][..]));
        assert_eq!(s.get(b"x"), Some(&b"y"[..]));
        drop(s);
        fs::remove_file(&p).unwrap();
    }
}
