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
