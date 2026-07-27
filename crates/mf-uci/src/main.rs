use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    if let Some(command) = arguments.next() {
        let stdout = io::stdout();
        let result = match command.as_str() {
            "perft" => mf_uci::run_perft_subcommand(arguments, stdout.lock()),
            "bench" => mf_uci::run_bench_subcommand(arguments, stdout.lock()),
            _ => {
                eprintln!("Error: unknown command '{command}'");
                eprintln!(
                    "Hint: use 'manifold perft --help', 'manifold bench', or run without arguments for UCI mode."
                );
                return ExitCode::FAILURE;
            }
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let stdin = io::stdin();
    let stdout = io::stdout();

    match mf_uci::run(stdin.lock(), stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("UCI I/O error: {error}");
            ExitCode::FAILURE
        }
    }
}
