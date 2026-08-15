use mf_core::Position;

#[test]
fn to_fen_round_trips_through_from_fen_over_a_representative_corpus() {
    // Standard positions: castling subsets, en passant, counters, promotions-in-hand.
    let standard = [
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 10",
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        "4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 3",
        "rnbqkbnr/ppppppp1/8/7p/6P1/8/PPPPPP1P/RNBQKBNR w KQkq h6 0 2",
        "4k3/8/8/8/8/8/8/4K3 b - - 42 99",
        "r3k2r/8/8/8/8/8/8/R3K2R b Kq - 3 17",
    ];
    for fen in standard {
        let position = Position::from_fen(fen, false)
            .unwrap_or_else(|error| panic!("corpus FEN must parse: {fen}: {error}"));
        let emitted = position.to_fen(false);
        assert_eq!(emitted, fen, "to_fen must reproduce the canonical FEN");
        let round_tripped = Position::from_fen(&emitted, false)
            .unwrap_or_else(|error| panic!("emitted FEN must parse: {emitted}: {error}"));
        assert_eq!(
            round_tripped, position,
            "round trip must be bit-for-bit: {fen}"
        );
    }

    // Chess960: rook-file rights, including inner-rook layouts where KQkq would be
    // ambiguous, plus an en-passant target.
    let chess960 = [
        "r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1",
        "rk6/8/8/8/8/8/8/RK6 w Aa - 0 1",
        "1rkr4/8/8/8/8/8/8/1RKR4 w DBdb - 5 12",
        "nrk1rbbq/pppppppp/8/8/8/8/PPPPPPPP/NRK1RBBQ w EBeb - 0 1",
        "rk2r3/8/8/8/3pP3/8/8/RK2R3 b EAea e3 0 9",
    ];
    for fen in chess960 {
        let position = Position::from_fen(fen, true)
            .unwrap_or_else(|error| panic!("Chess960 corpus FEN must parse: {fen}: {error}"));
        let emitted = position.to_fen(true);
        assert_eq!(emitted, fen, "to_fen must reproduce the X-FEN rook files");
        let round_tripped = Position::from_fen(&emitted, true)
            .unwrap_or_else(|error| panic!("emitted X-FEN must parse: {emitted}: {error}"));
        assert_eq!(
            round_tripped, position,
            "round trip must be bit-for-bit: {fen}"
        );
    }
}

#[test]
fn a_chess960_position_emitted_in_standard_mode_still_round_trips() {
    // Standard-mode letters cannot name an inner rook, but the parser accepts
    // Shredder file letters in both modes, so the emitted spelling must be chosen
    // per the position rather than per the flag only when it stays unambiguous.
    // The standard layout is the case standard mode must get right.
    let position =
        Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", false).expect("must parse");
    let emitted = position.to_fen(false);
    assert_eq!(emitted, "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1");
    assert_eq!(
        Position::from_fen(&emitted, false).expect("must re-parse"),
        position
    );
}

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
    // than a blanket rejection of unusual material. The black king hides behind the
    // white one on the g-file: with sixteen queens covering every file, that is the
    // one square where the side not to move is not in check.
    assert!(
        Position::from_fen("QQQQQQQQ/1QQQQQQQ/Q7/8/8/6K1/8/6k1 w - - 0 1", false).is_ok(),
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
        Position::from_fen("QQQQQQQQ/1QQQQQQQ/Q7/p7/8/6K1/8/6k1 w - - 0 1", false).is_ok(),
        "a pawn cannot promote into the other side's material count"
    );

    // And the ordinary case must be untouched: eight queens and eight pawns is 16,
    // exactly on the bound.
    assert!(
        Position::from_fen("QQQQQQQQ/PPPPPPPP/8/8/8/4K3/8/4k3 w - - 0 1", false).is_ok(),
        "a full set of promotions landing exactly on the bound must remain parseable"
    );
}

#[test]
fn a_pawn_on_a_back_rank_is_rejected() {
    // A pawn on rank 1 or 8 is unreachable in a legal game and breaks the move
    // generator's assumption that a single push always has a target square.
    for fen in [
        "P7/8/8/8/8/8/8/K6k w - - 0 1",
        "8/8/8/8/8/8/8/KP5k w - - 0 1",
        "p7/8/8/8/8/8/8/K6k w - - 0 1",
        "8/8/8/8/8/8/8/Kp5k w - - 0 1",
    ] {
        assert!(
            Position::from_fen(fen, false).is_err(),
            "a pawn on a back rank must be rejected: {fen}"
        );
    }
}

#[test]
fn a_position_where_the_side_not_to_move_is_in_check_is_rejected() {
    // If the side NOT to move is in check, the previous move left the moving king
    // en prise, so the position is unreachable by any legal sequence. UCI and EPD
    // feeds do occasionally carry these, and the search used to special-case them
    // rather than the parser rejecting them.
    for fen in [
        // White to move, but it is the BLACK king that is in check.
        "4k3/8/8/8/8/8/4R3/4K3 w - - 0 1",
        "8/8/8/8/8/2k5/1q6/K7 b - - 0 1",
    ] {
        assert!(
            Position::from_fen(fen, false).is_err(),
            "the side not to move must not start in check: {fen}"
        );
    }

    // The side TO move being in check is the ordinary legal case and must parse.
    assert!(
        Position::from_fen("4k3/8/8/8/8/8/4r3/4K3 w - - 0 1", false).is_ok(),
        "the side to move starting in check is legal and must remain parseable"
    );
}
