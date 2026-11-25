//! The Sheet application as capabilities.

use mouli_interface::{Application, Call, CapabilitySpec, Error, Output, Result, Risk};
use mouli_sheet::{cell_name, parse_cell_name, Workbook};
use std::cell::RefCell;
use std::rc::Rc;

/// The workbook, shared between the adapter and the screen.
pub type SharedBook = Rc<RefCell<Workbook>>;

/// Most cells a single `show` or `clear` may touch.
const MAX_CELLS: u32 = 10_000;

/// The Sheet application.
pub struct SheetApp {
    book: SharedBook,
}

impl SheetApp {
    /// An application editing `book`.
    pub fn new(book: SharedBook) -> Self {
        Self { book }
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

fn cell_arg(call: &Call, key: &str) -> Result<(u32, u32)> {
    let v = call
        .args
        .get(key)
        .ok_or_else(|| bad(format!("{key} is required")))?;
    parse_cell_name(v).ok_or_else(|| bad(format!("{v:?} is not a cell name like B2")))
}

/// Parses `A1` or `A1:C3` into the top-left and bottom-right corners (row, column).
fn range_arg(call: &Call, key: &str) -> Result<((u32, u32), (u32, u32))> {
    let v = call
        .args
        .get(key)
        .ok_or_else(|| bad(format!("{key} is required")))?;
    let (a, b) = v.split_once(':').unwrap_or((v, v));
    let (a, b) = (
        parse_cell_name(a).ok_or_else(|| bad(format!("{a:?} is not a cell name")))?,
        parse_cell_name(b).ok_or_else(|| bad(format!("{b:?} is not a cell name")))?,
    );
    let (top, bottom) = (a.0.min(b.0), a.0.max(b.0));
    let (left, right) = (a.1.min(b.1), a.1.max(b.1));
    let cells = u64::from(bottom - top + 1) * u64::from(right - left + 1);
    if cells > u64::from(MAX_CELLS) {
        return Err(bad(format!(
            "{cells} cells is more than the {MAX_CELLS} allowed at once"
        )));
    }
    Ok(((top, left), (bottom, right)))
}

fn sheet_index(book: &Workbook, call: &Call) -> Result<usize> {
    match call.args.get("sheet") {
        None => Ok(book.active.min(book.sheets.len().saturating_sub(1))),
        Some(name) => book
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::NotFound(format!("sheet {name}"))),
    }
}

/// The cells of a range as text, one row per line, columns separated by ` | `, each row led by
/// its row number.
pub fn range_text(book: &Workbook, sheet: usize, from: (u32, u32), to: (u32, u32)) -> String {
    let mut out = String::new();
    for row in from.0..=to.0 {
        let cells: Vec<String> = (from.1..=to.1)
            .map(|col| book.get_cell(sheet, row, col).display())
            .collect();
        out.push_str(&format!("{:>4}: {}\n", row + 1, cells.join(" | ")));
    }
    out
}

impl Application for SheetApp {
    fn name(&self) -> &str {
        "sheet"
    }

    fn capabilities(&self) -> Vec<CapabilitySpec> {
        vec![
            CapabilitySpec::new("sheet.list", "list the sheets", Risk::Read),
            CapabilitySpec::new(
                "sheet.get",
                "read one cell (cell=B2 [sheet=Name])",
                Risk::Read,
            )
            .requires(&["cell"]),
            CapabilitySpec::new(
                "sheet.show",
                "show a range (range=A1:D10 [sheet=Name])",
                Risk::Read,
            )
            .requires(&["range"]),
            CapabilitySpec::new(
                "sheet.set",
                "set a cell to a value or =formula (cell=B2 value=42)",
                Risk::Write,
            )
            .requires(&["cell", "value"]),
            CapabilitySpec::new("sheet.add", "add a sheet (name=Budget)", Risk::Write)
                .requires(&["name"]),
            CapabilitySpec::new(
                "sheet.clear",
                "erase a range for good (range=A1:D10); needs approval",
                Risk::Consequential,
            )
            .requires(&["range"]),
        ]
    }

    fn invoke(&mut self, call: &Call) -> Result<Output> {
        let mut book = self.book.borrow_mut();
        let text = match call.capability.as_str() {
            "sheet.list" => book
                .sheets
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    format!(
                        "{}{} ({} cells)",
                        if i == book.active { "* " } else { "  " },
                        s.name,
                        s.cells.len()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            "sheet.get" => {
                let (row, col) = cell_arg(call, "cell")?;
                let i = sheet_index(&book, call)?;
                let raw = book.get_raw(i, row, col).unwrap_or("").to_string();
                let value = book.get_cell(i, row, col).display();
                if raw.starts_with('=') {
                    format!("{} = {value}   ({raw})", cell_name(row, col))
                } else {
                    format!("{} = {value}", cell_name(row, col))
                }
            }
            "sheet.show" => {
                let (from, to) = range_arg(call, "range")?;
                let i = sheet_index(&book, call)?;
                range_text(&book, i, from, to)
            }
            "sheet.set" => {
                let (row, col) = cell_arg(call, "cell")?;
                let i = sheet_index(&book, call)?;
                let value = call.args.get("value").map_or("", String::as_str);
                book.set_cell(i, row, col, value).map_err(Error::Handler)?;
                format!(
                    "{} = {}",
                    cell_name(row, col),
                    book.get_cell(i, row, col).display()
                )
            }
            "sheet.add" => {
                let name = call.args.get("name").map_or("", String::as_str);
                let i = book.add_sheet(name).map_err(Error::Handler)?;
                book.active = i;
                format!("added sheet {name}")
            }
            "sheet.clear" => {
                let (from, to) = range_arg(call, "range")?;
                let i = sheet_index(&book, call)?;
                let mut n = 0;
                for row in from.0..=to.0 {
                    for col in from.1..=to.1 {
                        if book.get_raw(i, row, col).is_some() {
                            book.set_cell(i, row, col, "").map_err(Error::Handler)?;
                            n += 1;
                        }
                    }
                }
                format!("cleared {n} cell(s)")
            }
            other => return Err(Error::NotFound(format!("capability {other}"))),
        };
        Ok(Output {
            text,
            artifacts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn app() -> (SheetApp, SharedBook) {
        let book: SharedBook = Rc::new(RefCell::new(Workbook::new()));
        (SheetApp::new(book.clone()), book)
    }

    fn call(cap: &str, args: &[(&str, &str)]) -> Call {
        Call {
            capability: cap.into(),
            args: args
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn set_get_and_formulas_work_and_recalculate() {
        let (mut a, book) = app();
        assert_eq!(
            a.invoke(&call("sheet.set", &[("cell", "A1"), ("value", "10")]))
                .unwrap()
                .text,
            "A1 = 10"
        );
        a.invoke(&call("sheet.set", &[("cell", "A2"), ("value", "32")]))
            .unwrap();
        let r = a
            .invoke(&call("sheet.set", &[("cell", "A3"), ("value", "=A1+A2")]))
            .unwrap();
        assert_eq!(r.text, "A3 = 42");
        // Changing an input recalculates the formula.
        a.invoke(&call("sheet.set", &[("cell", "A1"), ("value", "20")]))
            .unwrap();
        let got = a
            .invoke(&call("sheet.get", &[("cell", "A3")]))
            .unwrap()
            .text;
        assert!(
            got.starts_with("A3 = 52") && got.contains("=A1+A2"),
            "{got}"
        );
        assert_eq!(book.borrow().get_raw(0, 0, 0), Some("20"));
    }

    #[test]
    fn show_prints_rows_and_a_range_can_be_given_in_any_order() {
        let (mut a, _) = app();
        for (c, v) in [("A1", "1"), ("B1", "2"), ("A2", "3"), ("B2", "4")] {
            a.invoke(&call("sheet.set", &[("cell", c), ("value", v)]))
                .unwrap();
        }
        let t = a
            .invoke(&call("sheet.show", &[("range", "B2:A1")]))
            .unwrap()
            .text;
        assert_eq!(t, "   1: 1 | 2\n   2: 3 | 4\n");
    }

    #[test]
    fn bad_input_is_refused_with_a_reason_and_changes_nothing() {
        let (mut a, book) = app();
        assert!(matches!(
            a.invoke(&call("sheet.set", &[("cell", "ZZZ",), ("value", "1")])),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            a.invoke(&call("sheet.get", &[("cell", "not a cell")])),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            a.invoke(&call("sheet.get", &[("cell", "A1"), ("sheet", "Nope")])),
            Err(Error::NotFound(_))
        ));
        assert!(
            matches!(
                a.invoke(&call("sheet.show", &[("range", "A1:ZZ99999")])),
                Err(Error::Invalid(_))
            ),
            "too many cells"
        );
        assert!(matches!(
            a.invoke(&call("sheet.set", &[("value", "1")])),
            Err(Error::Invalid(_))
        ));
        assert!(book.borrow().sheets[0].cells.is_empty());
    }

    #[test]
    fn clear_erases_only_the_range_and_reports_how_many() {
        let (mut a, book) = app();
        for (c, v) in [("A1", "1"), ("A2", "2"), ("C3", "3")] {
            a.invoke(&call("sheet.set", &[("cell", c), ("value", v)]))
                .unwrap();
        }
        let r = a
            .invoke(&call("sheet.clear", &[("range", "A1:B2")]))
            .unwrap();
        assert_eq!(r.text, "cleared 2 cell(s)");
        assert_eq!(
            book.borrow().get_raw(0, 2, 2),
            Some("3"),
            "outside the range is untouched"
        );
    }

    #[test]
    fn sheets_can_be_added_listed_and_addressed_by_name() {
        let (mut a, book) = app();
        a.invoke(&call("sheet.add", &[("name", "Budget")])).unwrap();
        assert!(
            a.invoke(&call("sheet.add", &[("name", "budget")])).is_err(),
            "names are unique, ignoring case"
        );
        a.invoke(&call(
            "sheet.set",
            &[("cell", "A1"), ("value", "7"), ("sheet", "Sheet1")],
        ))
        .unwrap();
        let listing = a.invoke(&call("sheet.list", &[])).unwrap().text;
        assert!(
            listing.contains("Sheet1 (1 cells)") && listing.contains("* Budget"),
            "{listing}"
        );
        assert_eq!(book.borrow().get_raw(0, 0, 0), Some("7"));
    }

    #[test]
    fn a_formula_error_is_shown_as_a_value_not_a_failure() {
        let (mut a, _) = app();
        let r = a
            .invoke(&call("sheet.set", &[("cell", "A1"), ("value", "=1/0")]))
            .unwrap();
        assert!(r.text.starts_with("A1 = #"), "{}", r.text);
    }

    #[test]
    fn capability_names_all_belong_to_the_application() {
        let (a, _) = app();
        assert!(a
            .capabilities()
            .iter()
            .all(|c| c.name.starts_with("sheet.")));
        assert!(a
            .capabilities()
            .iter()
            .any(|c| c.name == "sheet.clear" && c.risk == Risk::Consequential));
    }
}
