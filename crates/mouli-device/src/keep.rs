//! Saving the audit chain and the approval queue, so a restart loses neither.
//!
//! The audit file only ever grows: new records are appended and the file is synced before the
//! call returns. The held approvals are rewritten whole (temporary file, sync, rename) because
//! they change as requests are decided.

use mouli_interface::audit::AuditRecord;
use mouli_interface::workspace::PendingCall;
use mouli_interface::Workspace;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const AUDIT_FILE: &str = "audit.log";
const PENDING_FILE: &str = "pending.txt";

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c => o.push(c),
        }
    }
    o
}

fn unesc(s: &str) -> Result<String, String> {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => o.push('\\'),
            Some('t') => o.push('\t'),
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some('q') => o.push('='),
            _ => return Err("bad escape".into()),
        }
    }
    Ok(o)
}

fn record_line(r: &AuditRecord) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        r.seq,
        r.tick,
        esc(&r.actor),
        esc(&r.action),
        esc(&r.subject),
        esc(&r.outcome),
        r.prev,
        r.hash
    )
}

fn parse_record(line: &str) -> Result<AuditRecord, String> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() != 8 {
        return Err("wrong number of fields".into());
    }
    Ok(AuditRecord {
        seq: f[0].parse().map_err(|_| "bad seq")?,
        tick: f[1].parse().map_err(|_| "bad tick")?,
        actor: unesc(f[2])?,
        action: unesc(f[3])?,
        subject: unesc(f[4])?,
        outcome: unesc(f[5])?,
        prev: f[6].into(),
        hash: f[7].into(),
    })
}

fn pending_line(c: &PendingCall) -> String {
    let args: Vec<String> = c
        .args
        .iter()
        .map(|(k, v)| format!("{}={}", esc(k).replace('=', "\\q"), esc(v)))
        .collect();
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        esc(&c.id),
        esc(&c.tenant),
        esc(&c.user),
        esc(&c.role),
        esc(&c.capability),
        esc(&c.detail),
        c.tick,
        args.join("\t")
    )
}

fn parse_pending(line: &str) -> Result<PendingCall, String> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() < 7 {
        return Err("wrong number of fields".into());
    }
    let mut args = std::collections::BTreeMap::new();
    for a in &f[7..] {
        let (k, v) = a.split_once('=').ok_or("bad argument")?;
        args.insert(unesc(k)?, unesc(v)?);
    }
    Ok(PendingCall {
        id: unesc(f[0])?,
        tenant: unesc(f[1])?,
        user: unesc(f[2])?,
        role: unesc(f[3])?,
        capability: unesc(f[4])?,
        detail: unesc(f[5])?,
        tick: f[6].parse().map_err(|_| "bad tick")?,
        args,
    })
}

fn read_lines(path: &Path) -> Result<Vec<String>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(t.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// What [`restore`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// Audit records loaded.
    pub records: usize,
    /// Held approvals put back.
    pub pending: usize,
    /// Whether the loaded audit chain verifies (a broken chain is kept and shows up as a
    /// Security finding).
    pub chain_ok: bool,
}

/// Loads the saved audit chain and approval queue into a fresh workspace. Call before
/// `Device::new`.
///
/// # Errors
/// A message when a file cannot be read or parsed, or the workspace is not empty. Nothing in
/// the files is changed.
pub fn restore(ws: &mut Workspace, dir: &Path) -> Result<Restored, String> {
    let mut records = Vec::new();
    for (i, l) in read_lines(&dir.join(AUDIT_FILE))?.iter().enumerate() {
        records.push(parse_record(l).map_err(|e| format!("{AUDIT_FILE} line {}: {e}", i + 1))?);
    }
    let mut calls = Vec::new();
    for (i, l) in read_lines(&dir.join(PENDING_FILE))?.iter().enumerate() {
        calls.push(parse_pending(l).map_err(|e| format!("{PENDING_FILE} line {}: {e}", i + 1))?);
    }
    let n = records.len();
    let p = calls.len();
    ws.load_audit(records).map_err(|e| e.to_string())?;
    ws.import_pending(calls).map_err(|e| e.to_string())?;
    Ok(Restored {
        records: n,
        pending: p,
        chain_ok: ws.audit().verify().is_ok(),
    })
}

/// Where the state is kept and how much of it is already on disk.
pub(crate) struct Keeper {
    dir: PathBuf,
    saved_records: usize,
    saved_pending: Vec<PendingCall>,
}

impl Keeper {
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let saved_records = read_lines(&dir.join(AUDIT_FILE))?.len();
        let saved_pending = read_lines(&dir.join(PENDING_FILE))?
            .iter()
            .filter_map(|l| parse_pending(l).ok())
            .collect();
        Ok(Self {
            dir: dir.to_path_buf(),
            saved_records,
            saved_pending,
        })
    }

    /// Writes what is new. Returns a message if something could not be written.
    pub(crate) fn save(&mut self, ws: &Workspace) -> Result<(), String> {
        let all = ws.audit().records();
        if all.len() > self.saved_records {
            let path = self.dir.join(AUDIT_FILE);
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let text: String = all[self.saved_records..].iter().map(record_line).collect();
            f.write_all(text.as_bytes())
                .and_then(|()| f.sync_all())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            self.saved_records = all.len();
        }
        let pending = ws.export_pending();
        if pending != self.saved_pending {
            let path = self.dir.join(PENDING_FILE);
            let tmp = self.dir.join("pending.tmp");
            let text: String = pending.iter().map(pending_line).collect();
            let write = || -> std::io::Result<()> {
                let mut f = File::create(&tmp)?;
                f.write_all(text.as_bytes())?;
                f.sync_all()?;
                fs::rename(&tmp, &path)?;
                if let Ok(d) = File::open(&self.dir) {
                    let _ = d.sync_all();
                }
                Ok(())
            };
            write().map_err(|e| format!("{}: {e}", path.display()))?;
            self.saved_pending = pending;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_round_trip() {
        for s in ["", "a\tb\nc\\d\r", "x=y", "\\q", "\\\\"] {
            assert_eq!(unesc(&esc(s)).unwrap(), s);
        }
        assert!(unesc("bad\\").is_err());
    }

    #[test]
    fn pending_lines_round_trip_with_odd_arguments() {
        let mut args = std::collections::BTreeMap::new();
        args.insert("k=1".to_string(), "v\twith\ttabs\n".to_string());
        args.insert("plain".to_string(), String::new());
        let c = PendingCall {
            id: "apr-1".into(),
            tenant: "t".into(),
            user: "u".into(),
            role: "clerk".into(),
            capability: "x.y".into(),
            args,
            detail: "a=b\tc".into(),
            tick: 7,
        };
        let line = pending_line(&c);
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(parse_pending(line.trim_end_matches('\n')).unwrap(), c);
    }

    #[test]
    fn record_lines_round_trip_and_reject_junk() {
        let r = AuditRecord {
            seq: 3,
            tick: 4,
            actor: "a\tb".into(),
            action: "act".into(),
            subject: "s\nt".into(),
            outcome: "ok".into(),
            prev: "0".repeat(64),
            hash: "1".repeat(64),
        };
        assert_eq!(parse_record(record_line(&r).trim_end()).unwrap(), r);
        assert!(parse_record("1\t2\t3").is_err());
        assert!(parse_record("x\t2\ta\tb\tc\td\te\tf").is_err());
    }
}
