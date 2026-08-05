//! Which real-world FEN spellings does `from_fen` accept?
//!
//! A rejected FEN is not a harmless no-op in a UCI engine: `handle_position` keeps the
//! *previous* board, so the next `go` answers with a move that is illegal in the
//! position the GUI is showing. Every dialect a GUI can emit therefore has to parse.

use mf_core::Position;

/// Reports which of these parse, so the failures are visible in one place.
fn report(cases: &[(&str, &str, bool)]) -> Vec<String> {
    let mut failures = Vec::new();
    for (label, fen, chess960) in cases {
        if let Err(error) = Position::from_fen(fen, *chess960) {
            failures.push(format!("{label}: {fen}\n    -> {error}"));
        }
    }
    failures
}

#[test]
fn accepts_the_castling_dialects_guis_emit() {
    let cases = [
        (
            "X-FEN / Shredder uppercase files, standard mode",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w AHah - 0 1",
            false,
        ),
        (
            "Shredder file letters, standard start position",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w HAha - 0 1",
            false,
        ),
        (
            "classic KQkq, standard mode",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            false,
        ),
        (
            "classic KQkq, chess960 mode",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            true,
        ),
    ];

    let failures = report(&cases);
    assert!(
        failures.is_empty(),
        "castling dialects rejected:\n{}",
        failures.join("\n")
    );
}

#[test]
fn shredder_castling_names_the_same_rooks_as_the_classic_spelling() {
    // Accepting the file-letter spelling is only useful if it means the same thing:
    // `HAha` and `KQkq` describe the identical standard start position.
    let classic = Position::from_fen(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        false,
    )
    .expect("classic castling should parse");
    let shredder = Position::from_fen(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w HAha - 0 1",
        false,
    )
    .expect("shredder castling should parse");

    assert_eq!(classic.castling_rights(), shredder.castling_rights());
    assert_eq!(classic.zobrist(), shredder.zobrist());
}

#[test]
fn zeroed_counters_normalize_to_the_game_start() {
    // Study and exercise FENs routinely carry `0 0` counters, and GUI position-setup
    // dialogs leave them at zero. The fullmove number only feeds move-count heuristics,
    // so "unknown" normalizes to the start of the game instead of failing the parse.
    let zeroed = Position::from_fen(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 0",
        false,
    )
    .expect("zeroed counters should parse");
    assert_eq!(zeroed.fullmove_number(), 1);
    assert_eq!(zeroed.halfmove_clock(), 0);

    let reference = Position::startpos();
    assert_eq!(zeroed.zobrist(), reference.zobrist());
}

#[test]
fn a_genuinely_malformed_castling_field_is_still_rejected() {
    // Widening the dialect must not turn the parser into one that accepts anything.
    for fen in [
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkqX - 0 1",
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w I - 0 1",
        // File letters naming a square with no rook on it.
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w D - 0 1",
    ] {
        assert!(
            Position::from_fen(fen, false).is_err(),
            "'{fen}' should still be rejected"
        );
    }
}
