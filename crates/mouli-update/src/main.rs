//! Command-line entry point for the MOULI updater.
#![forbid(unsafe_code)]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(mouli_update::run_cli(&args));
}
