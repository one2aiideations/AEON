//! Sparse sheets and workbook with dependency-ordered recalculation.

use crate::formula::{self, Expr, Grid};
use crate::value::{err, CellValue};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Cells a single range may expand to when tracking dependencies.
pub const MAX_RANGE_CELLS: usize = 1_000_000;

/// One cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    /// What the user typed (`=SUM(A1:A3)`, `42`, `hi`).
    pub raw: String,
    /// Cached evaluated value.
    pub value: CellValue,
    formula: Option<Expr>,
}

impl Cell {
    /// Parsed formula, if the cell holds one.
    #[must_use]
    pub fn formula(&self) -> Option<&Expr> {
        self.formula.as_ref()
    }
}

/// A sheet: sparse `(row, col)` map, 0-based.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    /// Name.
    pub name: String,
    /// Cells.
    pub cells: HashMap<(u32, u32), Cell>,
}

impl Grid for Sheet {
    fn value(&self, row: u32, col: u32) -> CellValue {
        self.cells.get(&(row, col)).map(|c| c.value.clone()).unwrap_or_default()
    }
}

impl Sheet {
    /// Empty sheet.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            cells: HashMap::new(),
        }
    }

    /// Re-evaluate every formula in dependency order. Formulas on a cycle
    /// (or depending on one) get `#CYCLE!`.
    pub fn recalc(&mut self) {
        let formulas: Vec<(u32, u32)> = self
            .cells
            .iter()
            .filter(|(_, c)| c.formula.is_some())
            .map(|(k, _)| *k)
            .collect();
        let formula_set: HashSet<(u32, u32)> = formulas.iter().copied().collect();
        // deps[x] = formula cells x reads; users[y] = formula cells reading y.
        let mut indeg: HashMap<(u32, u32), usize> = HashMap::new();
        let mut users: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
        for &k in &formulas {
            let Some(e) = self.cells.get(&k).and_then(|c| c.formula.as_ref()) else {
                continue;
            };
            let deps: HashSet<(u32, u32)> = formula::references(e, MAX_RANGE_CELLS)
                .into_iter()
                .filter(|d| formula_set.contains(d))
                .collect();
            indeg.insert(k, deps.len());
            for d in deps {
                users.entry(d).or_default().push(k);
            }
        }
        let mut ready: VecDeque<(u32, u32)> = formulas.iter().copied().filter(|k| indeg[k] == 0).collect();
        let mut done: HashSet<(u32, u32)> = HashSet::new();
        while let Some(k) = ready.pop_front() {
            let value = self
                .cells
                .get(&k)
                .and_then(|c| c.formula.as_ref())
                .map(|e| formula::eval(self, e))
                .unwrap_or_default();
            if let Some(c) = self.cells.get_mut(&k) {
                c.value = value;
            }
            done.insert(k);
            for u in users.get(&k).cloned().unwrap_or_default() {
                if let Some(d) = indeg.get_mut(&u) {
                    *d -= 1;
                    if *d == 0 {
                        ready.push_back(u);
                    }
                }
            }
        }
        for k in formulas.into_iter().filter(|k| !done.contains(k)) {
            if let Some(c) = self.cells.get_mut(&k) {
                c.value = CellValue::error(err::CYCLE);
            }
        }
    }

    /// Whether `(row, col)` is part of, or depends on, a reference cycle.
    #[must_use]
    pub fn in_cycle(&self, row: u32, col: u32) -> bool {
        self.cells.get(&(row, col)).is_some_and(|c| c.value == CellValue::error(err::CYCLE))
    }
}

/// A workbook.
#[derive(Clone, Debug)]
pub struct Workbook {
    /// Sheets (at least one).
    pub sheets: Vec<Sheet>,
    /// Active sheet index.
    pub active: usize,
    /// Named ranges: name -> (sheet, top-left, bottom-right).
    pub names: BTreeMap<String, (usize, (u32, u32), (u32, u32))>,
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

impl Workbook {
    /// Workbook with one empty sheet named `Sheet1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sheets: vec![Sheet::new("Sheet1")],
            active: 0,
            names: BTreeMap::new(),
        }
    }

    /// Add a sheet; returns its index.
    pub fn add_sheet(&mut self, name: &str) -> Result<usize, String> {
        if name.trim().is_empty() || self.sheets.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
            return Err(format!("sheet name {name:?} empty or taken"));
        }
        self.sheets.push(Sheet::new(name));
        Ok(self.sheets.len() - 1)
    }

    /// Value of a cell (empty if missing).
    #[must_use]
    pub fn get_cell(&self, sheet: usize, row: u32, col: u32) -> CellValue {
        self.sheets.get(sheet).map(|s| s.value(row, col)).unwrap_or_default()
    }

    /// Raw input of a cell.
    #[must_use]
    pub fn get_raw(&self, sheet: usize, row: u32, col: u32) -> Option<&str> {
        self.sheets.get(sheet)?.cells.get(&(row, col)).map(|c| c.raw.as_str())
    }

    /// Set what the user typed and recalculate the sheet. Empty input clears the cell.
    pub fn set_cell(&mut self, sheet: usize, row: u32, col: u32, raw: &str) -> Result<(), String> {
        let sh = self.sheets.get_mut(sheet).ok_or_else(|| format!("no sheet {sheet}"))?;
        if raw.trim().is_empty() {
            sh.cells.remove(&(row, col));
        } else if let Some(body) = raw.strip_prefix('=') {
            let (formula, value) = match formula::parse(body) {
                Ok(e) => (Some(e), CellValue::Empty),
                Err(_) => (None, CellValue::error(err::PARSE)),
            };
            sh.cells.insert(
                (row, col),
                Cell {
                    raw: raw.to_owned(),
                    value,
                    formula,
                },
            );
        } else {
            sh.cells.insert(
                (row, col),
                Cell {
                    raw: raw.to_owned(),
                    value: CellValue::from_input(raw),
                    formula: None,
                },
            );
        }
        sh.recalc();
        Ok(())
    }

    /// Define a named range.
    pub fn define_name(&mut self, name: &str, sheet: usize, from: (u32, u32), to: (u32, u32)) -> Result<(), String> {
        if sheet >= self.sheets.len() || name.is_empty() || crate::parse_cell_name(name).is_some() {
            return Err(format!("invalid name {name:?}"));
        }
        self.names.insert(name.to_uppercase(), (sheet, from, to));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_range_and_dependents_update() {
        let mut wb = Workbook::new();
        wb.set_cell(0, 0, 0, "1").unwrap();
        wb.set_cell(0, 1, 0, "2").unwrap();
        wb.set_cell(0, 2, 0, "=SUM(A1:A2)").unwrap();
        wb.set_cell(0, 3, 0, "=A3*10").unwrap();
        assert_eq!(wb.get_cell(0, 2, 0), CellValue::Number(3.0));
        assert_eq!(wb.get_cell(0, 3, 0), CellValue::Number(30.0));
        wb.set_cell(0, 0, 0, "5").unwrap();
        assert_eq!(wb.get_cell(0, 2, 0), CellValue::Number(7.0), "dependent recalculated");
        assert_eq!(wb.get_cell(0, 3, 0), CellValue::Number(70.0), "chain recalculated");
        // Formula entered before its inputs exist.
        wb.set_cell(0, 0, 5, "=G1+1").unwrap();
        wb.set_cell(0, 0, 6, "41").unwrap();
        assert_eq!(wb.get_cell(0, 0, 5), CellValue::Number(42.0));
    }

    #[test]
    fn div_zero_and_parse_errors() {
        let mut wb = Workbook::new();
        wb.set_cell(0, 0, 0, "=1/0").unwrap();
        assert_eq!(wb.get_cell(0, 0, 0), CellValue::error(err::DIV0));
        wb.set_cell(0, 0, 1, "=1+").unwrap();
        assert_eq!(wb.get_cell(0, 0, 1), CellValue::error(err::PARSE));
        wb.set_cell(0, 0, 2, "=B1+1").unwrap();
        assert_eq!(wb.get_cell(0, 0, 2), CellValue::error(err::PARSE), "errors propagate");
    }

    #[test]
    fn cycles_are_marked_not_looped() {
        let mut wb = Workbook::new();
        wb.set_cell(0, 0, 0, "=A2").unwrap();
        wb.set_cell(0, 1, 0, "=A1").unwrap();
        wb.set_cell(0, 2, 0, "=A1+1").unwrap();
        wb.set_cell(0, 3, 0, "=SUM(A4:A5)").unwrap();
        assert!(wb.sheets[0].in_cycle(0, 0));
        assert!(wb.sheets[0].in_cycle(1, 0));
        assert!(wb.sheets[0].in_cycle(2, 0), "depends on a cycle");
        assert!(wb.sheets[0].in_cycle(3, 0), "self-referencing range");
        wb.set_cell(0, 1, 0, "3").unwrap();
        assert_eq!(wb.get_cell(0, 0, 0), CellValue::Number(3.0), "cycle broken");
        assert_eq!(wb.get_cell(0, 2, 0), CellValue::Number(4.0));
    }

    #[test]
    fn pmt_and_clearing() {
        let mut wb = Workbook::new();
        wb.set_cell(0, 0, 0, "=PMT(0.01,12,1000)").unwrap();
        match wb.get_cell(0, 0, 0) {
            CellValue::Number(n) => assert!((n + 88.8488).abs() < 1e-3, "{n}"),
            other => panic!("{other:?}"),
        }
        wb.set_cell(0, 0, 0, "").unwrap();
        assert_eq!(wb.get_raw(0, 0, 0), None);
        assert!(wb.set_cell(9, 0, 0, "1").is_err());
        assert!(wb.add_sheet("sheet1").is_err());
        assert_eq!(wb.add_sheet("Budget").unwrap(), 1);
        assert!(wb.define_name("A1", 0, (0, 0), (1, 1)).is_err());
        wb.define_name("rates", 0, (0, 0), (9, 0)).unwrap();
    }
}
