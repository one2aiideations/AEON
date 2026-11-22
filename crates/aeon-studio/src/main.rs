//! `aeon-studio` command line. See the library docs for status.
#![forbid(unsafe_code)]

use aeon_studio::{
    calc, capabilities, launch_gui, open_any, run_api, run_batch, Block, Document, Opened, SpreadsheetDoc, MODES,
    VERSION,
};
use std::path::PathBuf;

fn usage() {
    eprintln!("aeon-studio {VERSION}");
    eprintln!("  gui                         Launch desktop shell (not linked in scaffold)");
    eprintln!("  calc <formula>              Evaluate a formula, e.g. calc \"SUM(1,2)*3\"");
    eprintln!("  new <path> <title>          Create a .aeonstudio document");
    eprintln!("  new-sheet <path>            Create an empty .aeonstudio spreadsheet");
    eprintln!("  set <path> <cell> <value>   Set a cell in a .aeonstudio spreadsheet");
    eprintln!("  cat <path>                  Print a .aeonstudio file");
    eprintln!("  modes | status              List modes / implementation status");
    eprintln!("  api <port> | batch <dir> <op>  (not implemented yet)");
}

fn arg(args: &[String], i: usize, what: &str) -> Result<String, String> {
    args.get(i).cloned().ok_or_else(|| format!("missing {what}"))
}

fn run(args: &[String]) -> Result<(), String> {
    let e = |e: aeon_studio::AeonError| e.to_string();
    match args.get(1).map(String::as_str) {
        Some("gui") => launch_gui().map_err(e),
        Some("calc") => {
            println!("{}", calc(&arg(args, 2, "formula")?).display());
            Ok(())
        }
        Some("new") => {
            let p = PathBuf::from(arg(args, 2, "path")?);
            let title = args.get(3).cloned().unwrap_or_else(|| "Untitled".into());
            Document {
                blocks: vec![Block::Heading(title), Block::Paragraph("Start writing.".into())],
            }
            .save(&p)
            .map_err(e)?;
            println!("wrote {}", p.display());
            Ok(())
        }
        Some("new-sheet") => {
            let p = PathBuf::from(arg(args, 2, "path")?);
            SpreadsheetDoc::default().save(&p).map_err(e)?;
            println!("wrote {}", p.display());
            Ok(())
        }
        Some("set") => {
            let p = PathBuf::from(arg(args, 2, "path")?);
            let cell = arg(args, 3, "cell")?;
            let value = args.get(4..).map(|v| v.join(" ")).unwrap_or_default();
            let mut s = SpreadsheetDoc::load(&p).map_err(e)?;
            s.set(&cell, &value).map_err(e)?;
            s.save(&p).map_err(e)?;
            println!("{cell} = {}", s.get(&cell).map_err(e)?.display());
            Ok(())
        }
        Some("cat") => {
            match open_any(&PathBuf::from(arg(args, 2, "path")?)).map_err(e)? {
                Opened::Document(d) => println!("{}", d.render_plain()),
                Opened::Spreadsheet(s) => {
                    for sh in &s.workbook.sheets {
                        println!("[{}]", sh.name);
                        let mut keys: Vec<_> = sh.cells.keys().copied().collect();
                        keys.sort_unstable();
                        for (r, c) in keys {
                            let cell = &sh.cells[&(r, c)];
                            println!("{} = {}    ({})", aeon_sheet::cell_name(r, c), cell.value.display(), cell.raw);
                        }
                    }
                }
            }
            Ok(())
        }
        Some("modes") => {
            MODES.iter().for_each(|m| println!("{m:?}"));
            Ok(())
        }
        Some("status") => {
            capabilities().iter().for_each(|(area, cap)| println!("{cap:?}\t{area}"));
            Ok(())
        }
        Some("api") => {
            let port: u16 = arg(args, 2, "port")?.parse().map_err(|_| "bad port".to_string())?;
            run_api(port).map_err(e)
        }
        Some("batch") => run_batch(&PathBuf::from(arg(args, 2, "dir")?), &arg(args, 3, "op")?).map_err(e),
        _ => {
            usage();
            Ok(())
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(msg) = run(&args) {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
}
