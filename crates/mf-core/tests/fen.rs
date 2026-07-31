use mf_core::Position;

#[test]
fn en_passant_target_must_match_the_board_and_side_to_move() {
    for fen in [
        "4k3/3pP3/4n3/8/8/8/8/4K3 b - e6 0 1",
        "4k3/8/8/4p3/8/8/8/4K3 b - e3 0 1",
        "4k3/8/8/8/8/8/8/4K3 w - e6 0 1",
    ] {
        assert!(
            Position::from_fen(fen, false).is_err(),
            "invalid en-passant state was accepted: {fen}"
        );
    }
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
