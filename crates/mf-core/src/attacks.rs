use crate::{Bitboard, Color, PieceKind, Position, Square, bishop_attacks, rook_attacks};

#[inline]
pub fn knight_attacks(square: Square) -> Bitboard {
    KNIGHT_ATTACKS[square.index() as usize & 63]
}

#[inline]
pub fn king_attacks(square: Square) -> Bitboard {
    KING_ATTACKS[square.index() as usize & 63]
}

#[inline]
pub fn pawn_attacks(square: Square, color: Color) -> Bitboard {
    PAWN_ATTACKS[color.index()][square.index() as usize & 63]
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

/// Builds one leaper's per-square attack table at compile time.
///
/// This is the `offset` walk the runtime path used to repeat on every call, evaluated
/// once by the const evaluator instead. `attacks.rs`'s tests assert the tables are
/// bit-identical to that loop for every square.
const fn leaper_table<const N: usize>(deltas: &[(i8, i8); N]) -> [Bitboard; 64] {
    let mut table = [Bitboard::EMPTY; 64];
    let mut index = 0;
    while index < 64 {
        let file = (index & 7) as i8;
        let rank = (index >> 3) as i8;
        let mut attacks = 0u64;
        let mut delta = 0;
        while delta < N {
            let (file_delta, rank_delta) = deltas[delta];
            let target_file = file + file_delta;
            let target_rank = rank + rank_delta;
            if target_file >= 0 && target_file < 8 && target_rank >= 0 && target_rank < 8 {
                attacks |= 1u64 << (target_rank * 8 + target_file);
            }
            delta += 1;
        }
        table[index] = Bitboard::new(attacks);
        index += 1;
    }
    table
}

const KNIGHT_ATTACKS: [Bitboard; 64] = leaper_table(&KNIGHT_DELTAS);

const KING_ATTACKS: [Bitboard; 64] = leaper_table(&KING_DELTAS);

/// Indexed by `Color::index()`: white pawns attack up the board, black pawns down.
const PAWN_ATTACKS: [[Bitboard; 64]; 2] = [
    leaper_table(&[(-1, 1), (1, 1)]),
    leaper_table(&[(-1, -1), (1, -1)]),
];

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The delta-loop the lookup tables replace, kept here as the reference oracle.
    fn delta_loop_attacks(square: Square, deltas: &[(i8, i8)]) -> Bitboard {
        let mut attacks = 0;
        for &(file_delta, rank_delta) in deltas {
            if let Some(target) = offset(square, file_delta, rank_delta) {
                attacks |= target.bitboard().bits();
            }
        }
        Bitboard::new(attacks)
    }

    fn delta_loop_pawn_attacks(square: Square, color: Color) -> Bitboard {
        let rank_delta = match color {
            Color::White => 1,
            Color::Black => -1,
        };
        delta_loop_attacks(square, &[(-1, rank_delta), (1, rank_delta)])
    }

    #[test]
    fn leaper_tables_match_the_delta_loop_on_every_square() {
        for index in 0..64u8 {
            let square = Square::new(index).expect("square index is in range");
            assert_eq!(
                KNIGHT_ATTACKS[index as usize],
                delta_loop_attacks(square, &KNIGHT_DELTAS),
                "knight table disagrees on {square:?}"
            );
            assert_eq!(
                KING_ATTACKS[index as usize],
                delta_loop_attacks(square, &KING_DELTAS),
                "king table disagrees on {square:?}"
            );
            for color in Color::ALL {
                assert_eq!(
                    PAWN_ATTACKS[color.index()][index as usize],
                    delta_loop_pawn_attacks(square, color),
                    "{color:?} pawn table disagrees on {square:?}"
                );
            }
        }
    }

    #[test]
    fn leaper_lookups_serve_the_same_bits_the_delta_loop_produced() {
        for index in 0..64u8 {
            let square = Square::new(index).expect("square index is in range");
            assert_eq!(
                knight_attacks(square),
                delta_loop_attacks(square, &KNIGHT_DELTAS)
            );
            assert_eq!(
                king_attacks(square),
                delta_loop_attacks(square, &KING_DELTAS)
            );
            for color in Color::ALL {
                assert_eq!(
                    pawn_attacks(square, color),
                    delta_loop_pawn_attacks(square, color)
                );
            }
        }
    }
}
