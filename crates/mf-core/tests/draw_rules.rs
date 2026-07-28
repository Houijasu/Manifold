use mf_core::Position;

#[test]
fn insufficient_material_distinguishes_dead_positions_from_mating_material() {
    let dead_positions = [
        "8/8/8/4k3/8/8/8/4K3 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KB2 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KN2 w - - 0 1",
        "8/8/8/4k3/8/3b4/8/4KB2 w - - 0 1",
    ];
    for fen in dead_positions {
        let position = Position::from_fen(fen, false).expect("draw FEN should parse");
        assert!(
            position.is_insufficient_material(),
            "{fen} should be a dead position"
        );
    }

    let live_positions = [
        "8/8/8/4k3/8/8/8/2B1KB2 w - - 0 1",
        "8/8/8/4k3/8/8/8/3NKN2 w - - 0 1",
        "8/8/8/4k3/8/8/3n4/4KB2 w - - 0 1",
    ];
    for fen in live_positions {
        let position = Position::from_fen(fen, false).expect("live FEN should parse");
        assert!(
            !position.is_insufficient_material(),
            "{fen} retains mating material"
        );
    }
}

#[test]
fn repetition_key_ignores_only_en_passant_targets_without_a_legal_capture() {
    let uncapturable = Position::from_fen("4k3/8/8/3p4/8/8/8/4K3 w - d6 0 1", false).unwrap();
    let uncapturable_without_target =
        Position::from_fen("4k3/8/8/3p4/8/8/8/4K3 w - - 0 1", false).unwrap();
    assert_eq!(
        uncapturable.repetition_key(),
        uncapturable_without_target.repetition_key()
    );

    let pinned = Position::from_fen("k3r3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", false).unwrap();
    let pinned_without_target =
        Position::from_fen("k3r3/8/8/3pP3/8/8/8/4K3 w - - 0 1", false).unwrap();
    assert_eq!(
        pinned.repetition_key(),
        pinned_without_target.repetition_key(),
        "an en-passant capture that exposes the king is not a legal move"
    );

    let capturable = Position::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", false).unwrap();
    let capturable_without_target =
        Position::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - - 0 1", false).unwrap();
    assert_ne!(
        capturable.repetition_key(),
        capturable_without_target.repetition_key(),
        "a legal en-passant capture changes the available moves"
    );
}
