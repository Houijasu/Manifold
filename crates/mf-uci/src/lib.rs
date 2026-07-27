//! Universal Chess Interface protocol handling for Manifold.

use std::io::{self, BufRead, Write};
use std::time::Instant;

use mf_core::{
    Position, format_uci_move, generate_legal_moves, parse_uci_move, perft, perft_divide,
};

const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Manifold contributors",
    "option name Hash type spin default 16 min 1 max 1048576",
    "option name Threads type spin default 1 min 1 max 256",
    "option name UCI_Chess960 type check default false",
    "option name EvalFile type string default <empty>",
    "uciok",
];
const BENCH_CASES: [(&str, u64); 6] = [
    (
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        197_281,
    ),
    (
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        4_085_603,
    ),
    ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", 43_238),
    (
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        422_333,
    ),
    (
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        2_103_487,
    ),
    (
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
        3_894_594,
    ),
];
const BENCH_DEPTH: u32 = 4;
const NODE_SEARCH_MAX_PLY: u32 = 64;

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

impl EngineState {
    fn new_game(&mut self) {
        self.position = Position::startpos();
    }
}

/// Serves UCI commands until `quit` or end-of-file.
pub fn run<R: BufRead, W: Write>(reader: R, mut writer: W) -> io::Result<()> {
    let mut state = EngineState::default();

    for line in reader.lines() {
        let command = line?;
        let command = command.trim();
        let keyword = command.split_whitespace().next().unwrap_or_default();

        if keyword.eq_ignore_ascii_case("uci") {
            if command.split_whitespace().count() == 1 {
                for response in UCI_RESPONSE {
                    writeln!(writer, "{response}")?;
                }
                writer.flush()?;
            }
        } else if keyword.eq_ignore_ascii_case("isready") {
            if command.split_whitespace().count() == 1 {
                writeln!(writer, "readyok")?;
                writer.flush()?;
            }
        } else if keyword.eq_ignore_ascii_case("quit") {
            break;
        } else if keyword.eq_ignore_ascii_case("ucinewgame") {
            state.new_game();
        } else if keyword.eq_ignore_ascii_case("setoption") {
            handle_setoption(command, &mut state);
        } else if keyword.eq_ignore_ascii_case("position") {
            let _ = handle_position(command, &mut state);
        } else if keyword.eq_ignore_ascii_case("go") {
            handle_go(command, &mut writer, &mut state)?;
            writer.flush()?;
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

/// Runs the deterministic standalone `bench` subcommand.
pub fn run_bench_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    if !arguments.is_empty() {
        return Err(bench_usage("bench does not accept arguments"));
    }

    let mut positions = BENCH_CASES
        .iter()
        .map(|(fen, expected)| {
            Position::from_fen(fen, false)
                .map(|position| (position, *expected))
                .map_err(|error| format!("invalid built-in bench FEN: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let started = Instant::now();
    let mut total = 0u64;
    for (position, expected) in &mut positions {
        let nodes = perft(position, BENCH_DEPTH);
        if nodes != *expected {
            return Err(format!(
                "bench self-check failed: expected {expected} nodes, found {nodes}"
            ));
        }
        total += nodes;
    }
    let elapsed = started.elapsed();
    let nanos = elapsed.as_nanos().max(1);
    let nps = ((u128::from(total) * 1_000_000_000) / nanos) as u64;

    writeln!(writer, "Positions: {}", positions.len()).map_err(|error| error.to_string())?;
    writeln!(writer, "Nodes searched: {total}").map_err(|error| error.to_string())?;
    writeln!(writer, "Time (ms): {}", elapsed.as_millis()).map_err(|error| error.to_string())?;
    writeln!(writer, "NPS: {nps}").map_err(|error| error.to_string())
}

fn handle_setoption(command: &str, state: &mut EngineState) {
    let tokens: Vec<_> = command.split_whitespace().collect();
    if tokens.len() < 5
        || !tokens[0].eq_ignore_ascii_case("setoption")
        || !tokens[1].eq_ignore_ascii_case("name")
    {
        return;
    }
    let Some(value_index) = tokens[2..]
        .iter()
        .position(|token| token.eq_ignore_ascii_case("value"))
        .map(|index| index + 2)
    else {
        return;
    };
    let name = tokens[2..value_index].join(" ");
    let value = tokens[value_index + 1..].join(" ");

    if name.eq_ignore_ascii_case("UCI_Chess960") {
        if value.eq_ignore_ascii_case("true") {
            state.chess960 = true;
        } else if value.eq_ignore_ascii_case("false") {
            state.chess960 = false;
        }
    }
}

fn handle_position(command: &str, state: &mut EngineState) -> Result<(), String> {
    let mut tokens = command.split_whitespace();
    if !tokens
        .next()
        .is_some_and(|token| token.eq_ignore_ascii_case("position"))
    {
        return Err("missing position command".to_string());
    }
    let kind = tokens
        .next()
        .ok_or_else(|| "missing position type".to_string())?;

    let mut position = if kind.eq_ignore_ascii_case("startpos") {
        Position::startpos()
    } else if kind.eq_ignore_ascii_case("fen") {
        let fen = (0..6)
            .map(|_| {
                tokens
                    .next()
                    .ok_or_else(|| "FEN requires six fields".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        Position::from_fen(&fen, state.chess960).map_err(|error| error.to_string())?
    } else {
        return Err(format!("unknown position type '{kind}'"));
    };

    if let Some(separator) = tokens.next() {
        if !separator.eq_ignore_ascii_case("moves") {
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

fn handle_go<W: Write>(command: &str, writer: &mut W, state: &mut EngineState) -> io::Result<()> {
    let mut tokens = command.split_whitespace();
    if !tokens
        .next()
        .is_some_and(|token| token.eq_ignore_ascii_case("go"))
    {
        return Ok(());
    }
    let Some(kind) = tokens.next() else {
        return Ok(());
    };
    let Some(value) = tokens.next() else {
        return Ok(());
    };
    if tokens.next().is_some() {
        return Ok(());
    }

    if kind.eq_ignore_ascii_case("perft") {
        if let Ok(depth) = value.parse::<u32>() {
            write_perft(writer, &mut state.position, depth, state.chess960)?;
        }
    } else if kind.eq_ignore_ascii_case("depth") {
        if let Ok(depth) = value.parse::<u32>() {
            write_depth_search(writer, &mut state.position, depth, state.chess960)?;
        }
    } else if kind.eq_ignore_ascii_case("nodes")
        && let Ok(nodes) = value.parse::<u64>()
    {
        write_node_search(writer, &mut state.position, nodes, state.chess960)?;
    }
    Ok(())
}

fn write_depth_search<W: Write>(
    writer: &mut W,
    position: &mut Position,
    depth: u32,
    chess960: bool,
) -> io::Result<()> {
    let nodes = perft(position, depth);
    write_search_result(writer, position, depth, nodes, chess960)
}

fn write_node_search<W: Write>(
    writer: &mut W,
    position: &mut Position,
    budget: u64,
    chess960: bool,
) -> io::Result<()> {
    let mut max_ply = 0;
    let nodes = consume_nodes(position, budget, 0, &mut max_ply);
    write_search_result(writer, position, max_ply, nodes, chess960)
}

fn consume_nodes(position: &mut Position, budget: u64, ply: u32, max_ply: &mut u32) -> u64 {
    if budget == 0 {
        return 0;
    }
    *max_ply = (*max_ply).max(ply);
    if budget == 1 || ply == NODE_SEARCH_MAX_PLY {
        return 1;
    }

    let moves = generate_legal_moves(position);
    let mut nodes = 1;
    for &mv in &moves {
        if nodes == budget {
            break;
        }
        let undo = position.make_move(mv);
        nodes += consume_nodes(position, budget - nodes, ply + 1, max_ply);
        position.unmake_move(mv, undo);
    }
    nodes
}

fn write_search_result<W: Write>(
    writer: &mut W,
    position: &Position,
    depth: u32,
    nodes: u64,
    chess960: bool,
) -> io::Result<()> {
    let bestmove = generate_legal_moves(position)
        .iter()
        .copied()
        .map(|mv| format_uci_move(position, mv, chess960))
        .min()
        .unwrap_or_else(|| "(none)".to_string());
    writeln!(writer, "info depth {depth} nodes {nodes}")?;
    writeln!(writer, "bestmove {bestmove}")
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

fn bench_usage(message: &str) -> String {
    format!("{message}\n\nUsage: manifold bench")
}

fn perft_help() -> &'static str {
    "Usage: manifold perft <depth> [--fen <FEN>] [--chess960]\n\
     \n\
     Examples:\n\
       manifold perft 6\n\
       manifold perft 5 --fen \"r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1\"\n\
       manifold perft 4 --chess960 --fen \"rk6/8/8/8/8/8/8/RK6 w Aa - 0 1\""
}
