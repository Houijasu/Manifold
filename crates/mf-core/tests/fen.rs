use mf_core::Position;

#[test]
fn an_en_passant_target_no_pawn_could_capture_is_dropped_rather_than_rejected() {
    // EPD suites and several GUIs emit the square after any double push, and some
    // emit a stale one outright. Rejecting the FEN fails the whole `position`
    // command, so a UCI engine keeps its previous board and answers with a move that
    // is illegal in the GUI's position. The target only ever enables a capture, so
    // dropping an uncapturable one removes no legal move.
    for fen in [
        "4k3/3pP3/4n3/8/8/8/8/4K3 b - e6 0 1",
        "4k3/8/8/4p3/8/8/8/4K3 b - e3 0 1",
        "4k3/8/8/8/8/8/8/4K3 w - e6 0 1",
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq e6 0 1",
    ] {
        let position = Position::from_fen(fen, false)
            .unwrap_or_else(|error| panic!("a stale en-passant target must parse: {fen}: {error}"));
        assert_eq!(
            position.en_passant(),
            None,
            "an uncapturable en-passant target must be normalized away: {fen}"
        );
    }

    // A malformed square is still a parse error, and a real double push still keeps
    // its target so the capture remains generatable.
    assert!(Position::from_fen("4k3/8/8/8/8/8/8/4K3 w - e4 0 1", false).is_err());
    let real = Position::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", false)
        .expect("a genuine double push must parse");
    assert!(real.en_passant().is_some());
}

#[test]
fn a_piece_count_the_material_key_cannot_index_is_rejected_rather_than_panicked_on() {
    // `ZobristKeys::toggle_material_count` indexes a table of 17 entries, so 17 of one
    // non-pawn kind indexes past it. Reaching that from a parser is a panic on
    // untrusted input at a system boundary, and it aborted a real conversion of the
    // CC0 Lichess evaluation database with
    // `index out of bounds: the len is 17 but the index is 17`.
    for fen in [
        "QQQQQQQQ/QQQQQQQQ/Q7/8/8/8/4K3/4k3 w - - 0 1",
        "qqqqqqqq/qqqqqqqq/q7/8/8/8/4K3/4k3 w - - 0 1",
        "NNNNNNNN/NNNNNNNN/N7/8/8/8/4K3/4k3 w - - 0 1",
    ] {
        let error = Position::from_fen(fen, false)
            .err()
            .unwrap_or_else(|| panic!("an unindexable piece count must be an error: {fen}"));
        assert!(
            format!("{error}").contains("material key"),
            "the error must name the real limit, not something incidental: {error}"
        );
    }

    // Sixteen is representable and must still parse, so the guard is a bound rather
    // than a blanket rejection of unusual material.
    assert!(
        Position::from_fen("QQQQQQQQ/QQQQQQQQ/8/8/8/8/4K3/4k3 w - - 0 1", false).is_ok(),
        "sixteen queens sit exactly on the bound and must remain parseable"
    );
}

#[test]
fn a_board_a_promotion_could_push_past_the_material_key_is_rejected_at_parse_time() {
    // The count the FEN places is only half the problem: `make_move` promotes by
    // *adding* a piece, so a board carrying sixteen queens AND a pawn parses and then
    // panics one move later, inside move generation, where nothing names the FEN that
    // caused it. This exact shape appears in the CC0 Lichess evaluation database.
    for fen in [
        "1QQQQQQQ/P7/QQQQQQQQ/7Q/8/8/8/K6k w - - 0 1",
        "1qqqqqqq/p7/qqqqqqqq/7q/8/8/8/K6k b - - 0 1",
    ] {
        let error = Position::from_fen(fen, false)
            .err()
            .unwrap_or_else(|| panic!("a promotable overflow must be an error: {fen}"));
        assert!(
            format!("{error}").contains("material key"),
            "the error must name the real limit: {error}"
        );
    }

    // A pawn is only a threat to its own side's count, so the mirrored material must
    // still parse: sixteen white queens with a BLACK pawn overflows nothing.
    assert!(
        Position::from_fen("1QQQQQQQ/p7/QQQQQQQQ/7Q/8/8/8/K6k w - - 0 1", false).is_ok(),
        "a pawn cannot promote into the other side's material count"
    );

    // And the ordinary case must be untouched: eight queens and eight pawns is 16,
    // exactly on the bound.
    assert!(
        Position::from_fen("QQQQQQQQ/PPPPPPPP/8/8/8/8/4K3/4k3 w - - 0 1", false).is_ok(),
        "a full set of promotions landing exactly on the bound must remain parseable"
    );
}
