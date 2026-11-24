//! Command-line entry point for the AEON updater.
#![forbid(unsafe_code)]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(aeon_update::run_cli(&args));
}
