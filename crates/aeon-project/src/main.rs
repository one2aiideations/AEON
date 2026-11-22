//! `aeon-project` entry point.
//!
//! - `aeon-project serve [ADDR]`: headless REST API (default 127.0.0.1:8765).
//! - `aeon-project gui`: desktop UI with the API in the background
//!   (requires building with `--features gui`).
//!
//! Data lives in `AEON_PROJECT_DB` (default `aeon-project.db`).
#![forbid(unsafe_code)]

use aeon_project::api::{self, AppState};
use aeon_project::db::Db;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn open_state() -> Result<AppState, String> {
    let path = std::env::var("AEON_PROJECT_DB")
        .map_or_else(|_| PathBuf::from("aeon-project.db"), PathBuf::from);
    let db = Db::open(&path).map_err(|e| e.to_string())?;
    let store = db.load().map_err(|e| e.to_string())?;
    Ok(AppState {
        store: Arc::new(Mutex::new(store)),
        db: Arc::new(Mutex::new(db)),
    })
}

fn main() {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let state = match open_state() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open database: {e}");
            std::process::exit(1);
        }
    };
    let result = match args.get(1).map(String::as_str) {
        Some("serve") | None => {
            let addr = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "127.0.0.1:8765".into());
            match tokio::runtime::Runtime::new() {
                Ok(rt) => {
                    eprintln!("{} API on http://{addr}", aeon_project::APP_NAME);
                    rt.block_on(api::serve(state, &addr))
                        .map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        #[cfg(feature = "gui")]
        Some("gui") => aeon_project::gui::run(state),
        #[cfg(not(feature = "gui"))]
        Some("gui") => Err("built without the `gui` feature; rebuild with --features gui".into()),
        Some(other) => Err(format!("unknown mode {other}; use `serve [ADDR]` or `gui`")),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
