use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let stdout = io::stdout();
    match mf_tune::run_cli(std::env::args().skip(1), stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}
