//! Universal Chess Interface protocol handling for Manifold.

use std::io::{self, BufRead, Write};

use mf_core::{Position, format_uci_move, parse_uci_move, perft_divide};

const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Manifold contributors",
    "option name Hash type spin default 16 min 1 max 1048576",
    "option name Threads type spin default 1 min 1 max 256",
    "option name UCI_Chess960 type check default false",
    "option name EvalFile type string default <empty>",
    "uciok",
];

struct EngineState {
    position: Position,
    chess960: bool,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            position: Position::startpos(),
            chess960: false,
        }
    }
}

/// Serves UCI commands until `quit` or end-of-file.
pub fn run<R: BufRead, W: Write>(reader: R, mut writer: W) -> io::Result<()> {
    let mut state = EngineState::default();

    for line in reader.lines() {
        let command = line?;
        let command = command.trim();

        match command {
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
            _ if command.starts_with("setoption ") => handle_setoption(command, &mut state),
            _ if command.starts_with("position ") => {
                let _ = handle_position(command, &mut state);
            }
            _ if command.starts_with("go perft ") => {
                if let Some(depth) = command
                    .strip_prefix("go perft ")
                    .and_then(|depth| depth.parse::<u32>().ok())
                {
                    write_perft(&mut writer, &mut state.position, depth, state.chess960)?;
                    writer.flush()?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Runs the standalone `perft` subcommand arguments.
pub fn run_perft_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let mut arguments = arguments.into_iter().map(Into::into);
    let Some(depth) = arguments.next() else {
        return Err(perft_usage("missing perft depth"));
    };
    if matches!(depth.as_str(), "-h" | "--help") {
        writeln!(writer, "{}", perft_help()).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let depth = depth
        .parse::<u32>()
        .map_err(|_| perft_usage(&format!("invalid perft depth '{depth}'")))?;

    let mut chess960 = false;
    let mut fen = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--chess960" => chess960 = true,
            "--fen" => {
                fen = Some(
                    arguments
                        .next()
                        .ok_or_else(|| perft_usage("--fen requires a FEN value"))?,
                );
            }
            unknown => {
                return Err(perft_usage(&format!("unknown perft argument '{unknown}'")));
            }
        }
    }

    let mut position = match fen {
        Some(fen) => Position::from_fen(&fen, chess960)
            .map_err(|error| perft_usage(&format!("invalid FEN: {error}")))?,
        None => Position::startpos(),
    };
    write_perft(&mut writer, &mut position, depth, chess960).map_err(|error| error.to_string())
}

fn handle_setoption(command: &str, state: &mut EngineState) {
    let Some(value) = command.strip_prefix("setoption name UCI_Chess960 value ") else {
        return;
    };
    match value.trim() {
        "true" => state.chess960 = true,
        "false" => state.chess960 = false,
        _ => {}
    }
}

fn handle_position(command: &str, state: &mut EngineState) -> Result<(), String> {
    let mut tokens = command
        .strip_prefix("position ")
        .ok_or_else(|| "missing position arguments".to_string())?
        .split_whitespace();
    let kind = tokens
        .next()
        .ok_or_else(|| "missing position type".to_string())?;

    let mut position = match kind {
        "startpos" => Position::startpos(),
        "fen" => {
            let fen = (0..6)
                .map(|_| {
                    tokens
                        .next()
                        .ok_or_else(|| "FEN requires six fields".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(" ");
            Position::from_fen(&fen, state.chess960).map_err(|error| error.to_string())?
        }
        _ => return Err(format!("unknown position type '{kind}'")),
    };

    if let Some(separator) = tokens.next() {
        if separator != "moves" {
            return Err(format!("unexpected position argument '{separator}'"));
        }
        for notation in tokens {
            let mv = parse_uci_move(&position, notation, state.chess960)
                .ok_or_else(|| format!("illegal move '{notation}'"))?;
            position.make_move(mv);
        }
    }

    state.position = position;
    Ok(())
}

fn write_perft<W: Write>(
    writer: &mut W,
    position: &mut Position,
    depth: u32,
    chess960: bool,
) -> io::Result<()> {
    if depth == 0 {
        writeln!(writer, "Nodes searched: 1")?;
        return Ok(());
    }

    let mut rows: Vec<_> = perft_divide(position, depth)
        .into_iter()
        .map(|(mv, nodes)| (format_uci_move(position, mv, chess960), nodes))
        .collect();
    rows.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let total = rows.iter().map(|(_, nodes)| *nodes).sum::<u64>();
    for (mv, nodes) in rows {
        writeln!(writer, "{mv}: {nodes}")?;
    }
    writeln!(writer, "Nodes searched: {total}")
}

fn perft_usage(message: &str) -> String {
    format!("{message}\n\n{}", perft_help())
}

fn perft_help() -> &'static str {
    "Usage: manifold perft <depth> [--fen <FEN>] [--chess960]\n\
     \n\
     Examples:\n\
       manifold perft 6\n\
       manifold perft 5 --fen \"r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1\"\n\
       manifold perft 4 --chess960 --fen \"rk6/8/8/8/8/8/8/RK6 w Aa - 0 1\""
}
