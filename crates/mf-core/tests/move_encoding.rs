use core::mem::size_of;

use mf_core::{Move, MoveFlag, PieceKind, Square};

#[test]
fn move_is_exactly_sixteen_bits_and_round_trips_every_field() {
    assert_eq!(size_of::<Move>(), 2);

    for from in 0..64 {
        for to in 0..64 {
            for flag in [0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 13, 14, 15] {
                let from = Square::new(from).unwrap();
                let to = Square::new(to).unwrap();
                let flag = MoveFlag::new(flag).unwrap();
                let mv = Move::new(from, to, flag);

                assert_eq!(mv.from(), from);
                assert_eq!(mv.to(), to);
                assert_eq!(mv.flag(), flag);
                assert_eq!(Move::from_raw(mv.raw()), Some(mv));
            }
        }
    }

    for undefined in [5, 6, 7, 16] {
        assert_eq!(MoveFlag::new(undefined), None);
        if undefined < 16 {
            assert_eq!(Move::from_raw(u16::from(undefined) << 12), None);
        }
    }
}

#[test]
fn move_flags_preserve_special_move_semantics() {
    assert!(MoveFlag::CAPTURE.is_capture());
    assert!(MoveFlag::EN_PASSANT.is_capture());
    assert!(MoveFlag::EN_PASSANT.is_en_passant());
    assert!(MoveFlag::CASTLING.is_castling());
    assert!(MoveFlag::DOUBLE_PAWN_PUSH.is_double_pawn_push());

    let promotions = [
        (MoveFlag::KNIGHT_PROMOTION, PieceKind::Knight, false),
        (MoveFlag::BISHOP_PROMOTION, PieceKind::Bishop, false),
        (MoveFlag::ROOK_PROMOTION, PieceKind::Rook, false),
        (MoveFlag::QUEEN_PROMOTION, PieceKind::Queen, false),
        (MoveFlag::KNIGHT_PROMOTION_CAPTURE, PieceKind::Knight, true),
        (MoveFlag::BISHOP_PROMOTION_CAPTURE, PieceKind::Bishop, true),
        (MoveFlag::ROOK_PROMOTION_CAPTURE, PieceKind::Rook, true),
        (MoveFlag::QUEEN_PROMOTION_CAPTURE, PieceKind::Queen, true),
    ];

    for (flag, piece, capture) in promotions {
        assert_eq!(flag.promotion(), Some(piece));
        assert_eq!(flag.is_capture(), capture);
    }
}

#[test]
fn chess960_castling_encodes_king_to_rook_origin() {
    let king = Square::new(1).unwrap();
    let queenside_rook = Square::new(0).unwrap();
    let mv = Move::new(king, queenside_rook, MoveFlag::CASTLING);

    assert_eq!(mv.from(), king);
    assert_eq!(mv.to(), queenside_rook);
    assert!(mv.flag().is_castling());
}
