//! Universal Chess Interface protocol handling for Manifold.

use std::io::{self, BufRead, Write};

const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Manifold contributors",
    "option name Hash type spin default 16 min 1 max 1048576",
    "option name Threads type spin default 1 min 1 max 256",
    "option name UCI_Chess960 type check default false",
    "option name EvalFile type string default <empty>",
    "uciok",
];

/// Serves UCI commands until `quit` or end-of-file.
pub fn run<R: BufRead, W: Write>(reader: R, mut writer: W) -> io::Result<()> {
    for line in reader.lines() {
        let command = line?;

        match command.trim() {
            "uci" => {
                for response in UCI_RESPONSE {
                    writeln!(writer, "{response}")?;
                }
                writer.flush()?;
            }
            "isready" => {
                writeln!(writer, "readyok")?;
                writer.flush()?;
            }
            "quit" => break,
            _ => {}
        }
    }

    Ok(())
}
