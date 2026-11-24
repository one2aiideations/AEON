//! Headless MOULI-SHEET shell. Commands: `set A1 <value>`, `get A1`, `demo`, `quit`.
#![forbid(unsafe_code)]

use mouli_sheet::{parse_cell_name, Workbook};
use std::io::{BufRead, Write};

fn main() {
    println!("MOULI-SHEET core (headless). Commands: set A1 <value> | get A1 | demo | quit");
    let mut wb = Workbook::new();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let reply = match parts.as_slice() {
            ["quit"] => break,
            ["demo"] => {
                for (r, raw) in [(0, "10"), (1, "20"), (2, "=SUM(A1:A2)")] {
                    let _ = wb.set_cell(0, r, 0, raw);
                }
                format!("A3 = {}", wb.get_cell(0, 2, 0).display())
            }
            ["set", cell, rest @ ..] => match parse_cell_name(cell) {
                Some((row, col)) => match wb.set_cell(0, row, col, &rest.join(" ")) {
                    Ok(()) => format!("{cell} = {}", wb.get_cell(0, row, col).display()),
                    Err(e) => format!("error: {e}"),
                },
                None => format!("bad cell {cell}"),
            },
            ["get", cell] => match parse_cell_name(cell) {
                Some((row, col)) => wb.get_cell(0, row, col).display(),
                None => format!("bad cell {cell}"),
            },
            [] => continue,
            _ => "unknown command".to_owned(),
        };
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
}
