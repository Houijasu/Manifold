use std::collections::BTreeSet;

use mf_core::{
    CastlingSide, MoveFlag, Position, format_uci_move, generate_legal_moves, parse_uci_move,
};

fn castling_notation(position: &Position, chess960: bool) -> BTreeSet<String> {
    generate_legal_moves(position)
        .iter()
        .copied()
        .filter(|mv| mv.flag() == MoveFlag::CASTLING)
        .map(|mv| format_uci_move(position, mv, chess960))
        .collect()
}

#[test]
fn standard_castling_notation_round_trips_through_king_destinations() {
    let position = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", false).unwrap();
    assert_eq!(
        castling_notation(&position, false),
        BTreeSet::from(["e1c1".to_string(), "e1g1".to_string()])
    );

    for notation in ["e1c1", "e1g1"] {
        let mv = parse_uci_move(&position, notation, false).expect("standard castling must parse");
        assert_eq!(mv.flag(), MoveFlag::CASTLING);
        assert_eq!(format_uci_move(&position, mv, false), notation);
    }
    assert!(parse_uci_move(&position, "e1a1", false).is_none());
    assert!(parse_uci_move(&position, "e1h1", false).is_none());
}

#[test]
fn chess960_castling_notation_round_trips_through_rook_origins() {
    let position = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1", true).unwrap();
    assert_eq!(
        castling_notation(&position, true),
        BTreeSet::from(["e1a1".to_string(), "e1h1".to_string()])
    );

    for notation in ["e1a1", "e1h1"] {
        let mv = parse_uci_move(&position, notation, true).expect("Chess960 castling must parse");
        assert_eq!(mv.flag(), MoveFlag::CASTLING);
        assert_eq!(format_uci_move(&position, mv, true), notation);
    }
    assert!(parse_uci_move(&position, "e1c1", true).is_none());
    assert!(parse_uci_move(&position, "e1g1", true).is_none());
}

#[test]
fn chess960_kq_rights_select_outermost_rooks() {
    let position = Position::from_fen("4k3/8/8/8/8/8/8/R1R1K1R1 w KQ - 0 1", true).unwrap();
    assert_eq!(
        position.castling_rook(mf_core::Color::White, CastlingSide::KingSide),
        mf_core::Square::new(6)
    );
    assert_eq!(
        position.castling_rook(mf_core::Color::White, CastlingSide::QueenSide),
        mf_core::Square::new(0)
    );
}
