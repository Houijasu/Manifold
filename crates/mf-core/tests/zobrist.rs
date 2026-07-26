use cozy_chess::{
    Board, Color as CozyColor, Move as CozyMove, Piece as CozyPiece, Rank, Square as CozySquare,
};
use mf_core::{Bitboard, CastlingSide, Color, Move, MoveFlag, Piece, PieceKind, Position, Square};

const RANDOM_PLIES: usize = 200_000;

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

fn square(index: u8) -> Square {
    Square::new(index).unwrap()
}

fn color(color: CozyColor) -> Color {
    match color {
        CozyColor::White => Color::White,
        CozyColor::Black => Color::Black,
    }
}

fn piece_kind(piece: CozyPiece) -> PieceKind {
    match piece {
        CozyPiece::Pawn => PieceKind::Pawn,
        CozyPiece::Knight => PieceKind::Knight,
        CozyPiece::Bishop => PieceKind::Bishop,
        CozyPiece::Rook => PieceKind::Rook,
        CozyPiece::Queen => PieceKind::Queen,
        CozyPiece::King => PieceKind::King,
    }
}

fn cozy_piece(kind: PieceKind) -> CozyPiece {
    match kind {
        PieceKind::Pawn => CozyPiece::Pawn,
        PieceKind::Knight => CozyPiece::Knight,
        PieceKind::Bishop => CozyPiece::Bishop,
        PieceKind::Rook => CozyPiece::Rook,
        PieceKind::Queen => CozyPiece::Queen,
        PieceKind::King => CozyPiece::King,
    }
}

fn from_cozy(board: &Board) -> Position {
    let mut position = Position::empty(color(board.side_to_move()));

    for index in 0..64 {
        let cozy_square = CozySquare::index(index);
        if let (Some(kind), Some(side)) = (board.piece_on(cozy_square), board.color_on(cozy_square))
        {
            position.place_piece(
                square(index as u8),
                Piece::new(color(side), piece_kind(kind)),
            );
        }
    }

    for side in [CozyColor::White, CozyColor::Black] {
        let back_rank = Rank::First.relative_to(side);
        let rights = board.castle_rights(side);
        position.set_castling_rook(
            color(side),
            CastlingSide::KingSide,
            rights
                .short
                .map(|file| square(CozySquare::new(file, back_rank) as u8)),
        );
        position.set_castling_rook(
            color(side),
            CastlingSide::QueenSide,
            rights
                .long
                .map(|file| square(CozySquare::new(file, back_rank) as u8)),
        );
    }

    let en_passant = board.en_passant().map(|file| {
        let rank = match board.side_to_move() {
            CozyColor::White => Rank::Sixth,
            CozyColor::Black => Rank::Third,
        };
        square(CozySquare::new(file, rank) as u8)
    });
    position.set_en_passant(en_passant);
    position.set_halfmove_clock(u16::from(board.halfmove_clock()));
    position.set_fullmove_number(board.fullmove_number());
    position
}

fn move_from_cozy(board: &Board, mv: CozyMove) -> Move {
    let from = square(mv.from as u8);
    let to = square(mv.to as u8);
    let moved = board.piece_on(mv.from).unwrap();
    let castle = moved == CozyPiece::King && board.color_on(mv.to) == Some(board.side_to_move());
    let capture = !castle && board.piece_on(mv.to).is_some();
    let en_passant_target = board
        .en_passant()
        .map(|file| CozySquare::new(file, Rank::Sixth.relative_to(board.side_to_move())));
    let en_passant = moved == CozyPiece::Pawn && !capture && en_passant_target == Some(mv.to);
    let double_push =
        moved == CozyPiece::Pawn && (i16::from(mv.from as u8) - i16::from(mv.to as u8)).abs() == 16;

    let flag = if castle {
        MoveFlag::CASTLING
    } else if en_passant {
        MoveFlag::EN_PASSANT
    } else if let Some(promotion) = mv.promotion {
        match (promotion, capture) {
            (CozyPiece::Knight, false) => MoveFlag::KNIGHT_PROMOTION,
            (CozyPiece::Bishop, false) => MoveFlag::BISHOP_PROMOTION,
            (CozyPiece::Rook, false) => MoveFlag::ROOK_PROMOTION,
            (CozyPiece::Queen, false) => MoveFlag::QUEEN_PROMOTION,
            (CozyPiece::Knight, true) => MoveFlag::KNIGHT_PROMOTION_CAPTURE,
            (CozyPiece::Bishop, true) => MoveFlag::BISHOP_PROMOTION_CAPTURE,
            (CozyPiece::Rook, true) => MoveFlag::ROOK_PROMOTION_CAPTURE,
            (CozyPiece::Queen, true) => MoveFlag::QUEEN_PROMOTION_CAPTURE,
            (CozyPiece::Pawn | CozyPiece::King, _) => unreachable!(),
        }
    } else if double_push {
        MoveFlag::DOUBLE_PAWN_PUSH
    } else if capture {
        MoveFlag::CAPTURE
    } else {
        MoveFlag::QUIET
    };

    Move::new(from, to, flag)
}

fn legal_moves(board: &Board) -> Vec<CozyMove> {
    let mut moves = Vec::new();
    board.generate_moves(|piece_moves| {
        moves.extend(piece_moves);
        false
    });
    moves
}

fn assert_matches_oracle(position: &Position, board: &Board) {
    assert_eq!(position.side_to_move(), color(board.side_to_move()));
    assert_eq!(position.halfmove_clock(), u16::from(board.halfmove_clock()));
    assert_eq!(position.fullmove_number(), board.fullmove_number());

    for index in 0..64 {
        let cozy_square = CozySquare::index(index);
        let expected = match (board.piece_on(cozy_square), board.color_on(cozy_square)) {
            (Some(kind), Some(side)) => Some(Piece::new(color(side), piece_kind(kind))),
            _ => None,
        };
        assert_eq!(position.piece_at(square(index as u8)), expected);
    }

    for side in [CozyColor::White, CozyColor::Black] {
        let expected_color = Bitboard::new(board.colors(side).0);
        assert_eq!(position.color_occupancy(color(side)), expected_color);
        for kind in PieceKind::ALL {
            let expected = Bitboard::new(board.colored_pieces(side, cozy_piece(kind)).0);
            assert_eq!(position.pieces(color(side), kind), expected);
        }

        let back_rank = Rank::First.relative_to(side);
        let rights = board.castle_rights(side);
        let expected_short = rights
            .short
            .map(|file| square(CozySquare::new(file, back_rank) as u8));
        let expected_long = rights
            .long
            .map(|file| square(CozySquare::new(file, back_rank) as u8));
        assert_eq!(
            position.castling_rook(color(side), CastlingSide::KingSide),
            expected_short
        );
        assert_eq!(
            position.castling_rook(color(side), CastlingSide::QueenSide),
            expected_long
        );
    }

    let expected_en_passant = board.en_passant().map(|file| {
        let rank = match board.side_to_move() {
            CozyColor::White => Rank::Sixth,
            CozyColor::Black => Rank::Third,
        };
        square(CozySquare::new(file, rank) as u8)
    });
    assert_eq!(position.en_passant(), expected_en_passant);
    assert_eq!(position.occupancy(), Bitboard::new(board.occupied().0));
}

#[test]
fn specialized_keys_track_only_their_intended_features() {
    let mut position = Position::empty(Color::White);
    let empty = position.zobrist();

    position.place_piece(square(8), Piece::new(Color::White, PieceKind::Pawn));
    let pawn = position.zobrist();
    assert_ne!(pawn.main(), empty.main());
    assert_ne!(pawn.pawn(), empty.pawn());
    assert_eq!(pawn.minor(), empty.minor());
    assert_eq!(pawn.major(), empty.major());
    assert_eq!(pawn.non_pawn_material(), empty.non_pawn_material());

    position.place_piece(square(1), Piece::new(Color::White, PieceKind::Knight));
    let knight = position.zobrist();
    assert_ne!(knight.main(), pawn.main());
    assert_eq!(knight.pawn(), pawn.pawn());
    assert_ne!(knight.minor(), pawn.minor());
    assert_eq!(knight.major(), pawn.major());
    assert_ne!(knight.non_pawn_material(), pawn.non_pawn_material());

    let undo = position.make_move(Move::new(square(1), square(18), MoveFlag::QUIET));
    let moved_knight = position.zobrist();
    assert_ne!(moved_knight.main(), knight.main());
    assert_ne!(moved_knight.minor(), knight.minor());
    assert_eq!(moved_knight.non_pawn_material(), knight.non_pawn_material());
    position.unmake_move(Move::new(square(1), square(18), MoveFlag::QUIET), undo);

    position.place_piece(square(0), Piece::new(Color::White, PieceKind::Rook));
    let rook = position.zobrist();
    assert_ne!(rook.major(), knight.major());
    assert_ne!(rook.non_pawn_material(), knight.non_pawn_material());
}

#[test]
fn main_key_includes_side_en_passant_and_chess960_rook_origins() {
    let white = Position::empty(Color::White);
    let black = Position::empty(Color::Black);
    assert_ne!(white.zobrist().main(), black.zobrist().main());

    let mut en_passant = white.clone();
    en_passant.set_en_passant(Some(square(20)));
    assert_ne!(white.zobrist().main(), en_passant.zobrist().main());

    let mut a_file_right = white.clone();
    a_file_right.set_castling_rook(Color::White, CastlingSide::QueenSide, Some(square(0)));
    let mut b_file_right = white;
    b_file_right.set_castling_rook(Color::White, CastlingSide::QueenSide, Some(square(1)));
    assert_ne!(a_file_right.zobrist().main(), b_file_right.zobrist().main());
}

#[test]
fn incremental_zobrist_matches_recomputation_over_200k_random_legal_plies() {
    let mut rng = SplitMix64(0x4d31_2d46_332d_7a6f);
    let mut plies = 0;
    let mut sequence = 0;

    while plies < RANDOM_PLIES {
        let mut board = if sequence % 4 == 0 {
            Board::chess960_startpos((rng.next() % 960) as u32)
        } else {
            Board::startpos()
        };
        let mut position = from_cozy(&board);
        let initial = position.clone();
        let mut history = Vec::new();

        for _ in 0..128 {
            let moves = legal_moves(&board);
            if moves.is_empty() || plies == RANDOM_PLIES {
                break;
            }

            let cozy_move = moves[rng.next() as usize % moves.len()];
            let mv = move_from_cozy(&board, cozy_move);
            let before = position.clone();
            let undo = position.make_move(mv);
            board.play_unchecked(cozy_move);

            plies += 1;
            assert_eq!(position.zobrist(), position.recompute_zobrist());
            assert_matches_oracle(&position, &board);
            history.push((mv, undo, before));
        }

        while let Some((mv, undo, before)) = history.pop() {
            position.unmake_move(mv, undo);
            assert_eq!(position, before);
            assert_eq!(position.zobrist(), position.recompute_zobrist());
        }
        assert_eq!(position, initial);
        sequence += 1;
    }

    assert_eq!(plies, RANDOM_PLIES);
}
