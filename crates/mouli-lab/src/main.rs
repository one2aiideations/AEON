//! The `mouli-lab` command; see `mouli-lab --help`.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match mouli_lab::cli::run(&args) {
        Ok(text) => print!("{text}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
