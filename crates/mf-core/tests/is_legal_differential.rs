use cozy_chess::{
    Board, Color as CozyColor, Move as CozyMove, Piece as CozyPiece, Rank, Square as CozySquare,
};
use mf_core::{
    CastlingSide, Color, Move, Piece, PieceKind, Position, Square, generate_legal_moves,
    generate_pseudo_legal_moves, is_legal,
};

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

fn cozy_legal_moves(board: &Board) -> Vec<CozyMove> {
    let mut moves = Vec::new();
    board.generate_moves(|piece_moves| {
        moves.extend(piece_moves);
        false
    });
    moves
}

fn random_reachable_position(board: &mut Board, rng: &mut SplitMix64) {
    let plies = 8 + (rng.next() % 72) as usize;
    for _ in 0..plies {
        let moves = cozy_legal_moves(board);
        if moves.is_empty() {
            break;
        }
        let mv = moves[rng.next() as usize % moves.len()];
        board.play_unchecked(mv);
    }
}

/// For one position, `is_legal` must equal legal-list membership for every pseudo-legal
/// move (so every pin, check, king-move, and en-passant branch is reached through real
/// moves) and for a handful of corrupted encodings standing in for damaged TT entries.
fn compare(board: &Board, rng: &mut SplitMix64) {
    let position = from_cozy(board);
    let legal: std::collections::HashSet<u16> = generate_legal_moves(&position)
        .iter()
        .map(|mv| mv.raw())
        .collect();

    for mv in generate_pseudo_legal_moves(&position).iter().copied() {
        assert_eq!(
            is_legal(&position, mv),
            legal.contains(&mv.raw()),
            "pseudo-legal {mv:?} disagrees with the legal list: {position:?}"
        );
    }

    for _ in 0..16 {
        let raw = rng.next() as u16;
        let Some(mv) = Move::from_raw(raw) else {
            continue;
        };
        assert_eq!(
            is_legal(&position, mv),
            legal.contains(&raw),
            "corrupted encoding {mv:?} disagrees with the legal list: {position:?}"
        );
    }
}

#[test]
fn random_reachable_positions_match_legal_list_membership() {
    let mut rng = SplitMix64(0x6973_5f6c_6567_616c);

    for index in 0..256 {
        let mut board = if index % 2 == 0 {
            Board::startpos()
        } else {
            Board::chess960_startpos((rng.next() % 960) as u32)
        };
        random_reachable_position(&mut board, &mut rng);
        compare(&board, &mut rng);
    }
}
