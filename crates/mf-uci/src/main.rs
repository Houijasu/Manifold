use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    if let Some(command) = arguments.next() {
        if command == "perft" {
            let stdout = io::stdout();
            return match mf_uci::run_perft_subcommand(arguments, stdout.lock()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Error: {error}");
                    ExitCode::FAILURE
                }
            };
        }

        eprintln!("Error: unknown command '{command}'");
        eprintln!("Hint: use 'manifold perft --help' or run without arguments for UCI mode.");
        return ExitCode::FAILURE;
    }

    let stdin = io::stdin();
    let stdout = io::stdout();

    match mf_uci::run(stdin.lock(), stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("UCI I/O error: {error}");
            ExitCode::FAILURE
        }
    }
}
