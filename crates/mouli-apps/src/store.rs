//! Saving and loading the applications' data.
//!
//! Two small text files in a data directory: `sheet.mouli` and `document.mouli`. Writes are atomic
//! (a temporary file is written and synced, then renamed over the old one), so a power cut leaves
//! either the old file or the new one, never half of each. Loading checks every line and refuses
//! damaged files with a message instead of guessing.

use crate::Apps;
use mouli_sheet::Workbook;
use mouli_studio::Document;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::Path;

const SHEET_FILE: &str = "sheet.mouli";
const DOC_FILE: &str = "document.mouli";
const SHEET_MAGIC: &str = "MOULI-SHEET 1";
/// Most cells a saved workbook may hold; stops a damaged file from using all memory.
const MAX_CELLS: usize = 200_000;
const MAX_SHEETS: usize = 256;

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
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
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some('t') => o.push('\t'),
            other => return Err(format!("bad escape {other:?}")),
        }
    }
    Ok(o)
}

/// The workbook as text.
pub fn workbook_text(book: &Workbook) -> String {
    let mut s = format!("{SHEET_MAGIC}\n");
    for sh in &book.sheets {
        let _ = writeln!(s, "S\t{}", esc(&sh.name));
    }
    let mut cells: Vec<_> = Vec::new();
    for (i, sh) in book.sheets.iter().enumerate() {
        for (&(r, c), cell) in &sh.cells {
            cells.push((i, r, c, cell.raw.as_str()));
        }
    }
    cells.sort_unstable();
    for (i, r, c, raw) in cells {
        let _ = writeln!(s, "C\t{i}\t{r}\t{c}\t{}", esc(raw));
    }
    for (name, (sh, a, b)) in &book.names {
        let _ = writeln!(
            s,
            "N\t{}\t{sh}\t{}\t{}\t{}\t{}",
            esc(name),
            a.0,
            a.1,
            b.0,
            b.1
        );
    }
    let _ = writeln!(s, "A\t{}", book.active);
    s
}

fn num<T: std::str::FromStr>(f: Option<&str>, what: &str) -> Result<T, String> {
    f.ok_or_else(|| format!("missing {what}"))?
        .parse()
        .map_err(|_| format!("bad {what}"))
}

/// Reads a workbook back from [`workbook_text`].
///
/// # Errors
/// A message naming the first bad line.
pub fn parse_workbook(text: &str) -> Result<Workbook, String> {
    let mut lines = text.lines();
    if lines.next() != Some(SHEET_MAGIC) {
        return Err("not a MOULI sheet file".into());
    }
    let mut book = Workbook::new();
    book.sheets.clear();
    let (mut plain, mut formulas) = (Vec::new(), Vec::new());
    let mut active = 0usize;
    for (n, line) in lines.enumerate() {
        let at = |e: String| format!("line {}: {e}", n + 2);
        let mut f = line.split('\t');
        match f.next() {
            Some("S") => {
                if book.sheets.len() >= MAX_SHEETS {
                    return Err(at("too many sheets".into()));
                }
                let name = unesc(f.next().unwrap_or("")).map_err(at)?;
                book.add_sheet(&name).map_err(at)?;
            }
            Some("C") => {
                if plain.len() + formulas.len() >= MAX_CELLS {
                    return Err(at("too many cells".into()));
                }
                let (i, r, c): (usize, u32, u32) = (
                    num(f.next(), "sheet").map_err(at)?,
                    num(f.next(), "row").map_err(at)?,
                    num(f.next(), "column").map_err(at)?,
                );
                let raw = unesc(f.next().unwrap_or("")).map_err(at)?;
                if i >= book.sheets.len() {
                    return Err(at("cell on a sheet that is not declared".into()));
                }
                let cell = (i, r, c, raw);
                if cell.3.starts_with('=') {
                    formulas.push(cell);
                } else {
                    plain.push(cell);
                }
            }
            Some("N") => {
                let name = unesc(f.next().unwrap_or("")).map_err(at)?;
                let sh: usize = num(f.next(), "sheet").map_err(at)?;
                let (a0, a1, b0, b1): (u32, u32, u32, u32) = (
                    num(f.next(), "row").map_err(at)?,
                    num(f.next(), "column").map_err(at)?,
                    num(f.next(), "row").map_err(at)?,
                    num(f.next(), "column").map_err(at)?,
                );
                book.define_name(&name, sh, (a0, a1), (b0, b1))
                    .map_err(at)?;
            }
            Some("A") => active = num(f.next(), "active sheet").map_err(at)?,
            Some("") | None => {}
            Some(other) => return Err(at(format!("unknown record {other:?}"))),
        }
    }
    if book.sheets.is_empty() {
        return Err("the file has no sheets".into());
    }
    for (i, r, c, raw) in plain.into_iter().chain(formulas) {
        book.set_cell(i, r, c, &raw)?;
    }
    book.active = active.min(book.sheets.len() - 1);
    Ok(book)
}

/// Writes `bytes` to `path` so that a crash leaves the old content or the new, never a mix.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    // Make the rename itself durable.
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

impl Apps {
    /// Saves the workbook and the document into `dir` (created if missing).
    ///
    /// # Errors
    /// The file system's message.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let io = |e: std::io::Error| e.to_string();
        fs::create_dir_all(dir).map_err(io)?;
        write_atomic(
            &dir.join(SHEET_FILE),
            workbook_text(&self.book.borrow()).as_bytes(),
        )
        .map_err(io)?;
        write_atomic(
            &dir.join(DOC_FILE),
            self.doc.borrow().to_container().as_bytes(),
        )
        .map_err(io)
    }

    /// Loads what [`Apps::save`] wrote. A missing file leaves that application empty; a damaged
    /// file is an error and nothing is changed.
    ///
    /// # Errors
    /// A message naming the file and the problem.
    pub fn load(&self, dir: &Path) -> Result<(), String> {
        let read = |name: &str| match fs::read_to_string(dir.join(name)) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{name}: {e}")),
        };
        let book = read(SHEET_FILE)?
            .map(|t| parse_workbook(&t).map_err(|e| format!("{SHEET_FILE}: {e}")))
            .transpose()?;
        let doc = read(DOC_FILE)?
            .map(|t| Document::from_container(&t).map_err(|e| format!("{DOC_FILE}: {e}")))
            .transpose()?;
        if let Some(b) = book {
            *self.book.borrow_mut() = b;
        }
        if let Some(d) = doc {
            *self.doc.borrow_mut() = d;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register;
    use mouli_interface::{Policy, Workspace};

    fn apps() -> Apps {
        let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
        register(&mut ws).unwrap()
    }

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mouli-apps-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_workbook_survives_a_round_trip_with_formulas_and_awkward_text() {
        let a = apps();
        {
            let mut b = a.book.borrow_mut();
            b.set_cell(0, 0, 0, "12").unwrap();
            b.set_cell(0, 1, 0, "30").unwrap();
            b.set_cell(0, 2, 0, "=SUM(A1:A2)").unwrap();
            b.set_cell(0, 0, 1, "tab\there\nline \\ slash").unwrap();
            b.add_sheet("Budget").unwrap();
            b.set_cell(1, 4, 2, "=Sheet1!A3").ok();
            b.define_name("TOTAL", 0, (0, 0), (1, 0)).unwrap();
            b.active = 1;
        }
        let d = dir("rt");
        a.save(&d).unwrap();
        let b = apps();
        b.load(&d).unwrap();
        let (x, y) = (a.book.borrow(), b.book.borrow());
        assert_eq!(workbook_text(&x), workbook_text(&y));
        assert_eq!(y.get_raw(0, 0, 1), Some("tab\there\nline \\ slash"));
        assert_eq!(
            y.get_cell(0, 2, 0),
            x.get_cell(0, 2, 0),
            "formula recomputed"
        );
        assert_eq!(y.active, 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_document_survives_a_round_trip() {
        let a = apps();
        a.doc
            .borrow_mut()
            .blocks
            .push(mouli_studio::Block::Heading("Plan".into()));
        a.doc
            .borrow_mut()
            .blocks
            .push(mouli_studio::Block::Paragraph("one\ntwo".into()));
        let d = dir("doc");
        a.save(&d).unwrap();
        let b = apps();
        b.load(&d).unwrap();
        assert_eq!(*a.doc.borrow(), *b.doc.borrow());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_directory_loads_as_empty_and_leaves_state_alone() {
        let a = apps();
        a.book.borrow_mut().set_cell(0, 0, 0, "7").unwrap();
        let d = dir("empty");
        fs::create_dir_all(&d).unwrap();
        a.load(&d).unwrap();
        assert_eq!(a.book.borrow().get_raw(0, 0, 0), Some("7"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn damaged_files_are_refused_and_change_nothing() {
        let a = apps();
        a.book.borrow_mut().set_cell(0, 0, 0, "7").unwrap();
        for bad in [
            "garbage",
            "MOULI-SHEET 1\n",
            "MOULI-SHEET 1\nS\tA\nC\t0\tx\t0\t1\n",
            "MOULI-SHEET 1\nS\tA\nC\t5\t0\t0\t1\n",
            "MOULI-SHEET 1\nS\tA\nC\t0\t0\t0\tbad\\q\n",
            "MOULI-SHEET 1\nS\tA\nZ\t1\n",
            "MOULI-SHEET 1\nS\tA\nS\ta\n",
        ] {
            assert!(parse_workbook(bad).is_err(), "{bad:?}");
        }
        let d = dir("bad");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(SHEET_FILE), "garbage").unwrap();
        let e = a.load(&d).unwrap_err();
        assert!(e.contains("sheet.mouli"), "{e}");
        assert_eq!(a.book.borrow().get_raw(0, 0, 0), Some("7"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn saving_replaces_the_old_file_and_leaves_no_temporary_file() {
        let a = apps();
        let d = dir("atomic");
        a.save(&d).unwrap();
        a.book.borrow_mut().set_cell(0, 0, 0, "1").unwrap();
        a.save(&d).unwrap();
        let names: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
        let b = apps();
        b.load(&d).unwrap();
        assert_eq!(b.book.borrow().get_raw(0, 0, 0), Some("1"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_large_sheet_loads_in_reasonable_time() {
        let a = apps();
        {
            let mut b = a.book.borrow_mut();
            for r in 0..1000 {
                b.set_cell(0, r, 0, &r.to_string()).unwrap();
            }
            b.set_cell(0, 0, 1, "=SUM(A1:A1000)").unwrap();
        }
        let d = dir("big");
        a.save(&d).unwrap();
        let t = std::time::Instant::now();
        let b = apps();
        b.load(&d).unwrap();
        assert!(t.elapsed().as_secs() < 10);
        assert_eq!(
            b.book.borrow().get_cell(0, 0, 1),
            a.book.borrow().get_cell(0, 0, 1)
        );
        let _ = fs::remove_dir_all(&d);
    }
}
