use mf_core::{CastlingSide, Color, Move, MoveFlag, Piece, PieceKind, Position, Square};

fn square(index: u8) -> Square {
    Square::new(index).unwrap()
}

fn assert_round_trip(mut position: Position, mv: Move) {
    let before = position.clone();
    let undo = position.make_move(mv);
    assert_eq!(position.zobrist(), position.recompute_zobrist());
    position.unmake_move(mv, undo);
    assert_eq!(position, before);
    assert_eq!(position.zobrist(), position.recompute_zobrist());
}

#[test]
fn startpos_has_canonical_state_and_hashes() {
    let position = Position::startpos();

    assert_eq!(position.side_to_move(), Color::White);
    assert_eq!(
        position.piece_at(square(4)),
        Some(Piece::new(Color::White, PieceKind::King))
    );
    assert_eq!(
        position.piece_at(square(60)),
        Some(Piece::new(Color::Black, PieceKind::King))
    );
    assert_eq!(
        position.castling_rook(Color::White, CastlingSide::QueenSide),
        Some(square(0))
    );
    assert_eq!(
        position.castling_rook(Color::White, CastlingSide::KingSide),
        Some(square(7))
    );
    assert_eq!(
        position.castling_rook(Color::Black, CastlingSide::QueenSide),
        Some(square(56))
    );
    assert_eq!(
        position.castling_rook(Color::Black, CastlingSide::KingSide),
        Some(square(63))
    );
    assert_eq!(position.en_passant(), None);
    assert_eq!(position.halfmove_clock(), 0);
    assert_eq!(position.fullmove_number(), 1);
    assert_eq!(position.zobrist(), position.recompute_zobrist());
}

#[test]
fn make_unmake_restores_every_special_move_bit_for_bit() {
    let mut quiet = Position::empty(Color::White);
    quiet.place_piece(square(1), Piece::new(Color::White, PieceKind::Knight));
    assert_round_trip(quiet, Move::new(square(1), square(18), MoveFlag::QUIET));

    let mut capture = Position::empty(Color::White);
    capture.place_piece(square(0), Piece::new(Color::White, PieceKind::Rook));
    capture.place_piece(square(56), Piece::new(Color::Black, PieceKind::Rook));
    assert_round_trip(capture, Move::new(square(0), square(56), MoveFlag::CAPTURE));

    let mut double_push = Position::empty(Color::White);
    double_push.place_piece(square(12), Piece::new(Color::White, PieceKind::Pawn));
    assert_round_trip(
        double_push,
        Move::new(square(12), square(28), MoveFlag::DOUBLE_PAWN_PUSH),
    );

    let mut en_passant = Position::empty(Color::White);
    en_passant.place_piece(square(36), Piece::new(Color::White, PieceKind::Pawn));
    en_passant.place_piece(square(35), Piece::new(Color::Black, PieceKind::Pawn));
    en_passant.set_en_passant(Some(square(43)));
    assert_round_trip(
        en_passant,
        Move::new(square(36), square(43), MoveFlag::EN_PASSANT),
    );

    let mut promotion = Position::empty(Color::White);
    promotion.place_piece(square(48), Piece::new(Color::White, PieceKind::Pawn));
    assert_round_trip(
        promotion,
        Move::new(square(48), square(56), MoveFlag::QUEEN_PROMOTION),
    );

    let mut promotion_capture = Position::empty(Color::White);
    promotion_capture.place_piece(square(49), Piece::new(Color::White, PieceKind::Pawn));
    promotion_capture.place_piece(square(56), Piece::new(Color::Black, PieceKind::Rook));
    assert_round_trip(
        promotion_capture,
        Move::new(square(49), square(56), MoveFlag::KNIGHT_PROMOTION_CAPTURE),
    );
}

#[test]
fn orthodox_and_chess960_castling_round_trip_overlapping_destinations() {
    let mut orthodox = Position::empty(Color::White);
    orthodox.place_piece(square(4), Piece::new(Color::White, PieceKind::King));
    orthodox.place_piece(square(7), Piece::new(Color::White, PieceKind::Rook));
    orthodox.set_castling_rook(Color::White, CastlingSide::KingSide, Some(square(7)));
    assert_round_trip(
        orthodox,
        Move::new(square(4), square(7), MoveFlag::CASTLING),
    );

    let mut rook_lands_on_king_origin = Position::empty(Color::White);
    rook_lands_on_king_origin.place_piece(square(3), Piece::new(Color::White, PieceKind::King));
    rook_lands_on_king_origin.place_piece(square(1), Piece::new(Color::White, PieceKind::Rook));
    rook_lands_on_king_origin.set_castling_rook(
        Color::White,
        CastlingSide::QueenSide,
        Some(square(1)),
    );
    assert_round_trip(
        rook_lands_on_king_origin,
        Move::new(square(3), square(1), MoveFlag::CASTLING),
    );

    let mut king_already_on_destination = Position::empty(Color::White);
    king_already_on_destination.place_piece(square(6), Piece::new(Color::White, PieceKind::King));
    king_already_on_destination.place_piece(square(7), Piece::new(Color::White, PieceKind::Rook));
    king_already_on_destination.set_castling_rook(
        Color::White,
        CastlingSide::KingSide,
        Some(square(7)),
    );
    assert_round_trip(
        king_already_on_destination,
        Move::new(square(6), square(7), MoveFlag::CASTLING),
    );
}

#[test]
#[should_panic(expected = "castling side must match king and rook geometry")]
fn castling_rook_rejects_inconsistent_side_metadata() {
    let mut position = Position::empty(Color::White);
    position.place_piece(square(4), Piece::new(Color::White, PieceKind::King));
    position.place_piece(square(7), Piece::new(Color::White, PieceKind::Rook));

    position.set_castling_rook(Color::White, CastlingSide::QueenSide, Some(square(7)));
}

#[test]
fn castling_rights_follow_king_rook_moves_and_rook_captures() {
    let mut king_move = Position::empty(Color::White);
    king_move.place_piece(square(4), Piece::new(Color::White, PieceKind::King));
    king_move.place_piece(square(0), Piece::new(Color::White, PieceKind::Rook));
    king_move.place_piece(square(7), Piece::new(Color::White, PieceKind::Rook));
    king_move.set_castling_rook(Color::White, CastlingSide::QueenSide, Some(square(0)));
    king_move.set_castling_rook(Color::White, CastlingSide::KingSide, Some(square(7)));
    let undo = king_move.make_move(Move::new(square(4), square(12), MoveFlag::QUIET));
    assert_eq!(
        king_move.castling_rook(Color::White, CastlingSide::QueenSide),
        None
    );
    assert_eq!(
        king_move.castling_rook(Color::White, CastlingSide::KingSide),
        None
    );
    king_move.unmake_move(Move::new(square(4), square(12), MoveFlag::QUIET), undo);
    assert_eq!(
        king_move.castling_rook(Color::White, CastlingSide::QueenSide),
        Some(square(0))
    );
    assert_eq!(
        king_move.castling_rook(Color::White, CastlingSide::KingSide),
        Some(square(7))
    );

    let mut rook_capture = Position::empty(Color::White);
    rook_capture.place_piece(square(4), Piece::new(Color::White, PieceKind::King));
    rook_capture.place_piece(square(60), Piece::new(Color::Black, PieceKind::King));
    rook_capture.place_piece(square(0), Piece::new(Color::White, PieceKind::Rook));
    rook_capture.place_piece(square(56), Piece::new(Color::Black, PieceKind::Rook));
    rook_capture.set_castling_rook(Color::White, CastlingSide::QueenSide, Some(square(0)));
    rook_capture.set_castling_rook(Color::Black, CastlingSide::QueenSide, Some(square(56)));
    let mv = Move::new(square(0), square(56), MoveFlag::CAPTURE);
    let undo = rook_capture.make_move(mv);
    assert_eq!(
        rook_capture.castling_rook(Color::White, CastlingSide::QueenSide),
        None
    );
    assert_eq!(
        rook_capture.castling_rook(Color::Black, CastlingSide::QueenSide),
        None
    );
    rook_capture.unmake_move(mv, undo);
    assert_eq!(
        rook_capture.castling_rook(Color::White, CastlingSide::QueenSide),
        Some(square(0))
    );
    assert_eq!(
        rook_capture.castling_rook(Color::Black, CastlingSide::QueenSide),
        Some(square(56))
    );
}
