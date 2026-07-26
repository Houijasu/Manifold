use crate::{Bitboard, Color, PieceKind, Position, Square, bishop_attacks, rook_attacks};

#[inline]
pub fn knight_attacks(square: Square) -> Bitboard {
    leaper_attacks(square, &KNIGHT_DELTAS)
}

#[inline]
pub fn king_attacks(square: Square) -> Bitboard {
    leaper_attacks(square, &KING_DELTAS)
}

#[inline]
pub fn pawn_attacks(square: Square, color: Color) -> Bitboard {
    let rank_delta = match color {
        Color::White => 1,
        Color::Black => -1,
    };
    let mut attacks = 0;
    for file_delta in [-1, 1] {
        if let Some(target) = offset(square, file_delta, rank_delta) {
            attacks |= target.bitboard().bits();
        }
    }
    Bitboard::new(attacks)
}

#[inline]
pub fn is_square_attacked(position: &Position, square: Square, by: Color) -> bool {
    is_square_attacked_with_occupancy(position, square, by, position.occupancy())
}

pub(crate) fn is_square_attacked_with_occupancy(
    position: &Position,
    square: Square,
    by: Color,
    occupancy: Bitboard,
) -> bool {
    if !(pawn_attackers(square, by) & position.pieces(by, PieceKind::Pawn)).is_empty()
        || !(knight_attacks(square) & position.pieces(by, PieceKind::Knight)).is_empty()
        || !(king_attacks(square) & position.pieces(by, PieceKind::King)).is_empty()
    {
        return true;
    }

    let bishops_and_queens =
        position.pieces(by, PieceKind::Bishop) | position.pieces(by, PieceKind::Queen);
    if !(bishop_attacks(square, occupancy) & bishops_and_queens).is_empty() {
        return true;
    }

    let rooks_and_queens =
        position.pieces(by, PieceKind::Rook) | position.pieces(by, PieceKind::Queen);
    !(rook_attacks(square, occupancy) & rooks_and_queens).is_empty()
}

#[inline]
pub fn is_in_check(position: &Position, color: Color) -> bool {
    let Some(king) = position.pieces(color, PieceKind::King).first() else {
        return false;
    };
    is_square_attacked(position, king, !color)
}

#[inline]
pub(crate) fn offset(square: Square, file_delta: i8, rank_delta: i8) -> Option<Square> {
    let file = square.file() as i8 + file_delta;
    let rank = square.rank() as i8 + rank_delta;
    if !(0..8).contains(&file) || !(0..8).contains(&rank) {
        return None;
    }
    Square::new((rank as u8) * 8 + file as u8)
}

#[inline]
fn pawn_attackers(square: Square, by: Color) -> Bitboard {
    pawn_attacks(square, !by)
}

#[inline]
fn leaper_attacks(square: Square, deltas: &[(i8, i8)]) -> Bitboard {
    let mut attacks = 0;
    for &(file_delta, rank_delta) in deltas {
        if let Some(target) = offset(square, file_delta, rank_delta) {
            attacks |= target.bitboard().bits();
        }
    }
    Bitboard::new(attacks)
}

const KNIGHT_DELTAS: [(i8, i8); 8] = [
    (-2, -1),
    (-2, 1),
    (-1, -2),
    (-1, 2),
    (1, -2),
    (1, 2),
    (2, -1),
    (2, 1),
];

const KING_DELTAS: [(i8, i8); 8] = [
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, -1),
    (0, 1),
    (1, -1),
    (1, 0),
    (1, 1),
];
