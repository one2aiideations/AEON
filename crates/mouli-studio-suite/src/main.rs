//! The `mouli-studio` command: documents, spreadsheets, presentations, PDF and chat from a terminal.

fn main() {
    match mouli_studio_suite::cli::process() {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("mouli-studio: {error}");
            std::process::exit(2);
        }
    }
}
