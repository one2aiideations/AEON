//! AEON-SHEET spreadsheet core.
//!
//! Sparse sheets, a formula language (arithmetic, comparison, `&`
//! concatenation, `%`, ranges and common functions), and recalculation of
//! every formula in dependency order after each edit. Cells in a reference
//! cycle evaluate to `#CYCLE!`.
//!
//! Not yet supported: cross-sheet references (`Sheet2!A1`), named ranges in
//! formulas, file import/export and the desktop UI.
#![forbid(unsafe_code)]

pub mod formula;
pub mod value;
pub mod workbook;

pub use value::CellValue;
pub use workbook::{Cell, Sheet, Workbook};

/// Column letters for a 0-based column (`0` -> `A`, `27` -> `AB`).
#[must_use]
pub fn col_name(mut n: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// 0-based column for letters (`A` -> `0`), or `None` if invalid or too large.
#[must_use]
pub fn col_index(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 7 {
        return None;
    }
    let mut acc: u32 = 0;
    for c in s.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        acc = acc
            .checked_mul(26)?
            .checked_add(u32::from(c.to_ascii_uppercase() as u8 - b'A') + 1)?;
    }
    acc.checked_sub(1)
}

/// Parse `A1`, `$B$7`, ... into 0-based `(row, col)`.
#[must_use]
pub fn parse_cell_name(name: &str) -> Option<(u32, u32)> {
    formula::parse_cellref(name).map(|r| (r.row, r.col))
}

/// Name of a 0-based cell (`(0, 0)` -> `A1`).
#[must_use]
pub fn cell_name(row: u32, col: u32) -> String {
    format!("{}{}", col_name(col), u64::from(row) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_round_trip() {
        for n in [0, 1, 25, 26, 27, 51, 52, 701, 702, 16_383] {
            assert_eq!(col_index(&col_name(n)), Some(n), "{n}");
        }
        assert_eq!(col_name(27), "AB");
        assert_eq!(cell_name(9, 2), "C10");
        assert_eq!(parse_cell_name("$c$10"), Some((9, 2)));
        assert_eq!(col_index("A1"), None);
    }
}
