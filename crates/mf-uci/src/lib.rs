//! Universal Chess Interface protocol handling for Manifold.

use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use mf_core::{Position, format_uci_move, parse_uci_move, perft, perft_divide};
use mf_search::{
    SearchLimits, SearchResult, TranspositionTable, clamp_centipawn_score, score_to_uci_mate,
    search,
};

const DEFAULT_HASH_MIB: usize = 16;
const MIN_HASH_MIB: i128 = 1;
const MAX_HASH_MIB: i128 = 1_048_576;
const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Houijasu",
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
const DEFAULT_MOVES_TO_GO: u64 = 30;
const TIME_OVERHEAD_MILLIS: u64 = 10;

struct EngineState {
    position: Position,
    chess960: bool,
    threads: usize,
    transposition_table: TranspositionTable,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            position: Position::startpos(),
            chess960: false,
            threads: 1,
            transposition_table: TranspositionTable::new(DEFAULT_HASH_MIB)
                .expect("the default transposition table should allocate"),
        }
    }
}

impl EngineState {
    fn new_game(&mut self) {
        self.position = Position::startpos();
        self.transposition_table.clear();
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
            handle_setoption(command, &mut state, &mut writer)?;
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

fn handle_setoption<W: Write>(
    command: &str,
    state: &mut EngineState,
    writer: &mut W,
) -> io::Result<()> {
    let tokens: Vec<_> = command.split_whitespace().collect();
    if tokens.len() < 5
        || !tokens[0].eq_ignore_ascii_case("setoption")
        || !tokens[1].eq_ignore_ascii_case("name")
    {
        return Ok(());
    }
    let Some(value_index) = tokens[2..]
        .iter()
        .position(|token| token.eq_ignore_ascii_case("value"))
        .map(|index| index + 2)
    else {
        return Ok(());
    };
    let name = tokens[2..value_index].join(" ");
    let value = tokens[value_index + 1..].join(" ");

    if name.eq_ignore_ascii_case("UCI_Chess960") {
        if value.eq_ignore_ascii_case("true") {
            state.chess960 = true;
        } else if value.eq_ignore_ascii_case("false") {
            state.chess960 = false;
        }
    } else if name.eq_ignore_ascii_case("Hash") {
        let Ok(requested) = value.parse::<i128>() else {
            writeln!(writer, "info string invalid Hash value '{value}'")?;
            return Ok(());
        };
        if requested < MIN_HASH_MIB {
            writeln!(
                writer,
                "info string invalid Hash value '{value}': minimum is {MIN_HASH_MIB}"
            )?;
            return Ok(());
        }

        let clamped = requested.min(MAX_HASH_MIB) as usize;
        match TranspositionTable::new(clamped) {
            Ok(table) => {
                state.transposition_table = table;
                writeln!(writer, "info string hash resized to {clamped} MB")?;
            }
            Err(error) => {
                writeln!(
                    writer,
                    "info string unable to allocate Hash {clamped} MB: {error}"
                )?;
            }
        }
    } else if name.eq_ignore_ascii_case("Threads")
        && let Ok(requested) = value.parse::<usize>()
    {
        state.threads = requested.clamp(1, 256);
    }
    Ok(())
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
    let tokens: Vec<_> = command.split_whitespace().collect();
    if tokens
        .first()
        .is_none_or(|token| !token.eq_ignore_ascii_case("go"))
    {
        return Ok(());
    }
    if tokens.len() == 3 && tokens[1].eq_ignore_ascii_case("perft") {
        if let Ok(depth) = tokens[2].parse::<u32>() {
            return write_perft(writer, &mut state.position, depth, state.chess960);
        }
        return Ok(());
    }

    let Some(parameters) = GoParameters::parse(&tokens[1..]) else {
        return Ok(());
    };
    let limits = parameters.search_limits(&state.position);
    let result = search(&state.position, &state.transposition_table, limits);
    write_search_result(writer, &state.position, &result, state.chess960)
}

#[derive(Default)]
struct GoParameters {
    depth: Option<u32>,
    nodes: Option<u64>,
    movetime: Option<u64>,
    wtime: Option<u64>,
    btime: Option<u64>,
    winc: Option<u64>,
    binc: Option<u64>,
    movestogo: Option<u64>,
}

impl GoParameters {
    fn parse(tokens: &[&str]) -> Option<Self> {
        let mut parameters = Self::default();
        let mut index = 0;
        while index < tokens.len() {
            let key = tokens[index];
            index += 1;
            let value = *tokens.get(index)?;
            index += 1;

            if key.eq_ignore_ascii_case("depth") {
                parameters.depth = value.parse().ok();
            } else if key.eq_ignore_ascii_case("nodes") {
                parameters.nodes = value.parse().ok();
            } else if key.eq_ignore_ascii_case("movetime") {
                parameters.movetime = value.parse().ok();
            } else if key.eq_ignore_ascii_case("wtime") {
                parameters.wtime = value.parse().ok();
            } else if key.eq_ignore_ascii_case("btime") {
                parameters.btime = value.parse().ok();
            } else if key.eq_ignore_ascii_case("winc") {
                parameters.winc = value.parse().ok();
            } else if key.eq_ignore_ascii_case("binc") {
                parameters.binc = value.parse().ok();
            } else if key.eq_ignore_ascii_case("movestogo") {
                parameters.movestogo = value.parse().ok();
            } else {
                return None;
            }
        }

        (parameters.depth.is_some()
            || parameters.nodes.is_some()
            || parameters.movetime.is_some()
            || parameters.wtime.is_some()
            || parameters.btime.is_some())
        .then_some(parameters)
    }

    fn search_limits(&self, position: &Position) -> SearchLimits {
        let (soft_time, hard_time) = if self.nodes.is_some() {
            (None, None)
        } else if let Some(millis) = self.movetime {
            let hard = millis.saturating_sub(TIME_OVERHEAD_MILLIS).max(1);
            (
                Some(Duration::from_millis(hard)),
                Some(Duration::from_millis(hard)),
            )
        } else {
            self.clock_limits(position)
        };
        SearchLimits {
            depth: self.depth,
            nodes: self.nodes,
            soft_time,
            hard_time,
        }
    }

    fn clock_limits(&self, position: &Position) -> (Option<Duration>, Option<Duration>) {
        let white = position.side_to_move() == mf_core::Color::White;
        let remaining = if white { self.wtime } else { self.btime };
        let Some(remaining) = remaining else {
            return (None, None);
        };
        let increment = if white {
            self.winc.unwrap_or(0)
        } else {
            self.binc.unwrap_or(0)
        };
        let moves = self.movestogo.unwrap_or(DEFAULT_MOVES_TO_GO).max(1);
        let ideal = (remaining / moves)
            .saturating_add(increment.saturating_mul(3) / 4)
            .max(1);
        let hard = ideal
            .saturating_mul(5)
            .checked_div(4)
            .unwrap_or(ideal)
            .saturating_sub(TIME_OVERHEAD_MILLIS)
            .max(1)
            .min(remaining.saturating_sub(TIME_OVERHEAD_MILLIS).max(1));
        (
            Some(Duration::from_millis(ideal.min(hard))),
            Some(Duration::from_millis(hard)),
        )
    }
}

fn write_search_result<W: Write>(
    writer: &mut W,
    position: &Position,
    result: &SearchResult,
    chess960: bool,
) -> io::Result<()> {
    for iteration in &result.iterations {
        let elapsed_millis = iteration.elapsed.as_millis() as u64;
        let nps = iteration
            .nodes
            .saturating_mul(1_000)
            .checked_div(elapsed_millis.max(1))
            .unwrap_or(0);
        let score = format_score(iteration.score);
        let pv = format_pv(position, &iteration.pv, chess960);
        writeln!(
            writer,
            "info depth {} seldepth {} score {} nodes {} nps {} time {} pv {}",
            iteration.depth, iteration.seldepth, score, iteration.nodes, nps, elapsed_millis, pv
        )?;
    }

    if result.iterations.is_empty() {
        let score = format_score(result.score);
        let pv = format_pv(position, &result.pv, chess960);
        writeln!(
            writer,
            "info depth {} seldepth {} score {} nodes {} nps 0 time {} pv {}",
            result.depth,
            result.seldepth,
            score,
            result.nodes,
            result.elapsed.as_millis(),
            pv
        )?;
    } else if result
        .iterations
        .last()
        .is_some_and(|iteration| iteration.nodes != result.nodes)
    {
        let elapsed_millis = result.elapsed.as_millis() as u64;
        let nps = result
            .nodes
            .saturating_mul(1_000)
            .checked_div(elapsed_millis.max(1))
            .unwrap_or(0);
        let score = format_score(result.score);
        let pv = format_pv(position, &result.pv, chess960);
        writeln!(
            writer,
            "info depth {} seldepth {} score {} nodes {} nps {} time {} pv {}",
            result.depth, result.seldepth, score, result.nodes, nps, elapsed_millis, pv
        )?;
    }

    let bestmove = result
        .best_move
        .map(|mv| format_uci_move(position, mv, chess960))
        .unwrap_or_else(|| "(none)".to_string());
    writeln!(writer, "bestmove {bestmove}")
}

fn format_score(score: i32) -> String {
    score_to_uci_mate(score).map_or_else(
        || format!("cp {}", clamp_centipawn_score(score)),
        |moves| format!("mate {moves}"),
    )
}

fn format_pv(position: &Position, pv: &[mf_core::Move], chess960: bool) -> String {
    let mut replay = position.clone();
    let mut notation = Vec::with_capacity(pv.len());
    for &mv in pv {
        notation.push(format_uci_move(&replay, mv, chess960));
        replay.make_move(mv);
    }
    notation.join(" ")
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

#[cfg(test)]
mod tests {
    use mf_core::generate_legal_moves;
    use mf_search::{Bound, EntryData};

    use super::*;

    #[test]
    fn ucinewgame_clears_the_transposition_table_without_changing_its_size() {
        let mut state = EngineState::default();
        let key = state.position.zobrist().main();
        let allocated_bytes = state.transposition_table.allocated_bytes();
        let data = EntryData {
            best_move: generate_legal_moves(&state.position).first().copied(),
            score: 31,
            static_eval: 18,
            depth: 12,
            bound: Bound::Exact,
            age: 3,
            pv: true,
        };
        state.transposition_table.store(key, data);
        assert_eq!(state.transposition_table.probe(key), Some(data));

        state.new_game();

        assert_eq!(state.transposition_table.probe(key), None);
        assert_eq!(state.transposition_table.allocated_bytes(), allocated_bytes);
    }

    #[test]
    fn failed_hash_resize_preserves_the_existing_usable_table() {
        let mut state = EngineState::default();
        let original_bytes = state.transposition_table.allocated_bytes();
        let mut output = Vec::new();

        handle_setoption("setoption name Hash value 1048576", &mut state, &mut output)
            .expect("setoption output should be writable");

        assert_eq!(state.transposition_table.allocated_bytes(), original_bytes);
        assert!(
            String::from_utf8(output)
                .expect("protocol output should be UTF-8")
                .contains("unable to allocate Hash 1048576 MB")
        );
    }

    #[test]
    fn successful_hash_resize_replaces_the_table_and_starts_empty() {
        let mut state = EngineState::default();
        let key = state.position.zobrist().main();
        state.transposition_table.store(
            key,
            EntryData {
                best_move: None,
                score: 1,
                static_eval: 2,
                depth: 3,
                bound: Bound::Lower,
                age: 4,
                pv: false,
            },
        );
        let mut output = Vec::new();

        handle_setoption("setoption name Hash value 3", &mut state, &mut output)
            .expect("setoption output should be writable");

        assert_eq!(state.transposition_table.allocated_bytes(), 3 * 1024 * 1024);
        assert_eq!(state.transposition_table.probe(key), None);
    }
}
