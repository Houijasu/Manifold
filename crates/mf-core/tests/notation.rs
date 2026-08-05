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
    // The other castling dialect resolves to the same move rather than being rejected:
    // GUIs do not reliably follow `UCI_Chess960`, and a rejected move fails the whole
    // `position` command and strands the engine on its previous board.
    for notation in ["e1a1", "e1h1"] {
        let mv = parse_uci_move(&position, notation, false)
            .expect("the opposite castling dialect must still resolve");
        assert_eq!(mv.flag(), MoveFlag::CASTLING);
    }
    assert_eq!(
        parse_uci_move(&position, "e1h1", false),
        parse_uci_move(&position, "e1g1", false),
        "both spellings must name the same king-side castle"
    );
    assert!(parse_uci_move(&position, "e1b1", false).is_none());
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
    for notation in ["e1c1", "e1g1"] {
        let mv = parse_uci_move(&position, notation, true)
            .expect("the opposite castling dialect must still resolve");
        assert_eq!(mv.flag(), MoveFlag::CASTLING);
    }
    assert!(parse_uci_move(&position, "e1b1", true).is_none());
}

#[test]
fn promotion_and_castling_notation_are_case_insensitive() {
    // `b7b8Q` is what several GUIs emit. Rejecting it fails the whole `position`
    // command, so the engine keeps its previous board and answers with a move that is
    // illegal in the position the GUI is actually showing.
    let promotion = Position::from_fen("4k3/1P6/8/8/8/8/8/4K3 w - - 0 1", false).unwrap();
    for notation in ["b7b8Q", "b7b8q", "B7B8Q"] {
        let mv = parse_uci_move(&promotion, notation, false)
            .unwrap_or_else(|| panic!("promotion '{notation}' must parse"));
        assert_eq!(format_uci_move(&promotion, mv, false), "b7b8q");
    }

    let castling = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", false).unwrap();
    let mv = parse_uci_move(&castling, "E1G1", false).expect("uppercase castling must parse");
    assert_eq!(mv.flag(), MoveFlag::CASTLING);
}

#[test]
fn a_chess960_king_move_is_not_shadowed_by_the_other_castling_dialect() {
    // With the king on f1 and a rook on h1, `f1g1` is a legal quiet king move *and*
    // would be the standard-dialect spelling of the king-side castle. The configured
    // dialect must win, so the quiet move is what resolves.
    let position = Position::from_fen("4k3/8/8/8/8/8/8/5K1R w H - 0 1", true).unwrap();
    let mv = parse_uci_move(&position, "f1g1", true).expect("the quiet king move must parse");
    assert_ne!(
        mv.flag(),
        MoveFlag::CASTLING,
        "an exact match in the configured dialect must not be shadowed by a castle"
    );
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
