//! Castling notation must name exactly one move.
//!
//! `format_uci_move` spells a castle king-to-file in standard mode and king-takes-rook in
//! Chess960 mode. In some Chess960 layouts the king-to-file spelling is not a unique name
//! for the castle -- it can collide with a legal quiet king move, or degenerate into a
//! null move when the king already stands on its destination. A GUI receiving such a
//! string either plays a different move than the engine searched or rejects it outright.

use mf_core::{Move, Position, format_uci_move, generate_legal_moves, parse_uci_move};

fn castles(position: &Position) -> Vec<Move> {
    generate_legal_moves(position)
        .iter()
        .copied()
        .filter(|mv| mv.flag().is_castling())
        .collect()
}

/// Standard chess is untouched: the king always travels two squares from `e1`/`e8`, so
/// the king-to-file spelling is never ambiguous and stays exactly as it was.
#[test]
fn standard_castling_keeps_the_king_to_file_spelling() {
    let position = Position::from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", false)
        .expect("standard castling FEN should parse");
    let spellings: Vec<_> = castles(&position)
        .into_iter()
        .map(|mv| format_uci_move(&position, mv, false))
        .collect();

    assert!(
        spellings.contains(&"e1g1".to_string()),
        "king-side castle should spell e1g1, got {spellings:?}"
    );
    assert!(
        spellings.contains(&"e1c1".to_string()),
        "queen-side castle should spell e1c1, got {spellings:?}"
    );
}

/// The Chess960 spelling is likewise untouched.
#[test]
fn chess960_castling_keeps_the_king_takes_rook_spelling() {
    let position = Position::from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", true)
        .expect("standard castling FEN should parse in 960 mode");
    let spellings: Vec<_> = castles(&position)
        .into_iter()
        .map(|mv| format_uci_move(&position, mv, true))
        .collect();

    assert!(
        spellings.contains(&"e1h1".to_string()) && spellings.contains(&"e1a1".to_string()),
        "960 castles should spell king-takes-rook, got {spellings:?}"
    );
}

/// A castle must never be spelled as a string that also names a different legal move.
///
/// With the king on b1 and a rook on a1, the a-side castle's king destination is c1 --
/// but `b1c1` is also a legal quiet king move. Emitting it would make the GUI play the
/// king step instead of the castle.
#[test]
fn a_castle_is_never_spelled_as_another_legal_move() {
    // King b1, rooks a1/e1: a 960 geometry, loaded while the engine is in standard mode.
    let position = Position::from_fen("rk2r3/8/8/8/8/8/8/RK2R3 w AEae - 0 1", false)
        .expect("file-letter castling rights should parse");
    let legal = generate_legal_moves(&position);

    for castle in castles(&position) {
        let spelling = format_uci_move(&position, castle, false);
        assert_ne!(
            &spelling[0..2],
            &spelling[2..4],
            "castle spelled as the null move {spelling}"
        );
        let named_by: Vec<_> = legal
            .iter()
            .copied()
            .filter(|&mv| format_uci_move(&position, mv, false) == spelling)
            .collect();
        assert_eq!(
            named_by.len(),
            1,
            "'{spelling}' names {} different legal moves",
            named_by.len()
        );
    }
}

/// Whatever we emit, we must read back as the same move.
#[test]
fn every_castling_spelling_round_trips_in_both_dialects() {
    let fens = [
        "4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1",
        "rk2r3/8/8/8/8/8/8/RK2R3 w AEae - 0 1",
        "1r2k1r1/8/8/8/8/8/8/1R2K1R1 w BGbg - 0 1",
        "1rk1r3/8/8/8/8/8/8/1RK1R3 w BEbe - 0 1",
    ];
    for fen in fens {
        for chess960 in [false, true] {
            let Ok(position) = Position::from_fen(fen, chess960) else {
                continue;
            };
            for castle in castles(&position) {
                let spelling = format_uci_move(&position, castle, chess960);
                assert_eq!(
                    parse_uci_move(&position, &spelling, chess960),
                    Some(castle),
                    "'{spelling}' did not round-trip (chess960={chess960}) in {fen}"
                );
            }
        }
    }
}
