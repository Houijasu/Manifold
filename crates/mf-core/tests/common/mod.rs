use std::fs;
use std::path::Path;

use mf_core::{Position, format_uci_move, perft, perft_divide};

pub const TESTDATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/testdata");

pub fn position(fen: &str, chess960: bool) -> Position {
    Position::from_fen(fen, chess960).unwrap_or_else(|error| panic!("invalid test FEN: {error}"))
}

#[allow(dead_code)]
pub fn suite(path: &Path, maximum_depth: u32, chess960: bool) {
    suite_range(path, maximum_depth, chess960, 0, usize::MAX);
}

#[allow(dead_code)]
pub fn suite_range(
    path: &Path,
    maximum_depth: u32,
    chess960: bool,
    start_line: usize,
    end_line: usize,
) {
    let contents = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));

    for (line_number, line) in contents
        .lines()
        .enumerate()
        .skip(start_line)
        .take(end_line.saturating_sub(start_line))
    {
        let mut fields = line.split(';');
        let fen = fields.next().expect("suite line must contain a FEN").trim();
        let mut position = position(fen, chess960);

        for expected in fields {
            let mut tokens = expected.split_whitespace();
            let depth = tokens
                .next()
                .and_then(|token| token.strip_prefix('D'))
                .and_then(|depth| depth.parse::<u32>().ok())
                .expect("suite entry must contain a D<depth> label");
            let nodes = tokens
                .next()
                .and_then(|nodes| nodes.parse::<u64>().ok())
                .expect("suite entry must contain a node count");

            if depth <= maximum_depth {
                let before = position.clone();
                let actual = perft(&mut position, depth);
                assert_eq!(
                    position,
                    before,
                    "{} line {} depth {depth} did not restore the root position: {fen}",
                    path.display(),
                    line_number + 1
                );
                let divide = (actual != nodes).then(|| {
                    perft_divide(&mut position, depth)
                        .into_iter()
                        .map(|(mv, count)| {
                            format!("{}={count}", format_uci_move(&position, mv, chess960))
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                });
                assert_eq!(
                    actual,
                    nodes,
                    "{} line {} depth {depth}: {fen}\ndivide: {}",
                    path.display(),
                    line_number + 1,
                    divide.as_deref().unwrap_or_default()
                );
            }
        }
    }
}
