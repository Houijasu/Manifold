//! A mate search must not walk into a position with a king missing.
//!
//! `8/8/8/8/8/6K1/6R1/7k w - - 0 1` is a legal mate-in-2. It panicked in
//! `Position::king_square` once the NNUE evaluator was actually reachable, which only
//! happened after network discovery started working from a GUI's working directory.
//! `is_in_check` reports `false` for an *absent* king, so a king-capturing reply passed
//! the root legality filter and reached the accumulator refresh.

use mf_core::Position;
use mf_nnue::resolve_network;
use mf_search::{SearchLimits, SearchOptions, TranspositionTable, search};

// The composed position this test started with, `.../6R1/6Rk w`, had the black king
// in check with WHITE to move -- unreachable by any legal sequence, and rejected by
// `Position::from_fen` since the review's finding-1/18 cluster landed. This is its
// legal sibling: one rook fewer, king/rook box intact, 1. Rh2+ Kg1 2. Rh1#.
const MATE_IN_TWO: &str = "8/8/8/8/8/6K1/6R1/7k w - - 0 1";

#[test]
fn a_mate_search_does_not_panic_on_a_king_capture() {
    let (network, _) = resolve_network(None)
        .expect("network resolution should not fail")
        .into_parts();

    let position = Position::from_fen(MATE_IN_TWO, false).expect("FEN should parse");
    let table = TranspositionTable::new(16).expect("table should allocate");

    let result = search(
        &position,
        &table,
        SearchLimits {
            depth: Some(12),
            ..SearchLimits::default()
        },
        SearchOptions::default(),
        &network,
    );

    assert!(result.best_move.is_some(), "the search must return a move");
}
