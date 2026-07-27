use mf_core::{Position, parse_uci_move, static_exchange_evaluation};

fn assert_see(fen: &str, notation: &str, expected: i32) {
    let position = Position::from_fen(fen, false).expect("test FEN should be valid");
    let before = position.clone();
    let mv = parse_uci_move(&position, notation, false)
        .unwrap_or_else(|| panic!("{notation} should be legal in {fen}"));

    assert_eq!(
        static_exchange_evaluation(&position, mv),
        expected,
        "{notation} in {fen}"
    );
    assert_eq!(position, before, "SEE must not mutate the position");
}

#[test]
fn see_matches_hand_computed_exchange_sequences() {
    let cases = [
        ("7k/8/8/3p4/4P3/8/8/K7 w - - 0 1", "e4d5", 100),
        ("7k/8/4p3/3p4/4P3/8/8/K7 w - - 0 1", "e4d5", 0),
        ("7k/8/3p4/4p3/8/5N2/8/K7 w - - 0 1", "f3e5", -220),
        ("3r3k/8/8/3q4/8/3R4/8/3R3K w - - 0 1", "d3d5", 900),
        ("7k/8/5n2/3r4/3Q4/8/1B6/K7 w - - 0 1", "d4d5", 500),
        ("4k3/4q3/8/8/7B/8/8/K3R3 w - - 0 1", "e1e7", 900),
        ("4k3/4p3/8/8/8/8/8/K3R3 w - - 0 1", "e1e7", -400),
        ("7k/8/8/3pP3/8/8/8/K7 w - d6 0 1", "e5d6", 100),
        ("4k2r/6P1/8/8/8/8/8/4K3 w - - 0 1", "g7h8q", 1_300),
        ("4k2r/6P1/8/8/8/8/8/4K3 w - - 0 1", "g7h8n", 720),
        ("R7/7k/8/8/8/8/1p6/q6K w - - 0 1", "a8a1", -400),
        ("7k/8/3p4/8/8/5N2/8/K7 w - - 0 1", "f3e5", -320),
    ];

    for (fen, notation, expected) in cases {
        assert_see(fen, notation, expected);
    }
}
