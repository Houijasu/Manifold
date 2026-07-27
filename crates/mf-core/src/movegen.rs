use core::ops::Deref;

use crate::attacks::{
    is_in_check, is_square_attacked_with_occupancy, king_attacks, knight_attacks, offset,
};
use crate::{
    Bitboard, CastlingSide, Color, Move, MoveFlag, Piece, PieceKind, Position, Square,
    bishop_attacks, rook_attacks,
};

const MAX_LEGAL_MOVES: usize = 256;
const A1: Square = match Square::new(0) {
    Some(square) => square,
    None => unreachable!(),
};
const EMPTY_MOVE: Move = Move::new(A1, A1, MoveFlag::QUIET);

/// Fixed-capacity legal move collection. Chess has at most 218 legal moves.
#[derive(Clone)]
pub struct MoveList {
    moves: [Move; MAX_LEGAL_MOVES],
    len: usize,
}

impl MoveList {
    #[inline]
    pub const fn new() -> Self {
        Self {
            moves: [EMPTY_MOVE; MAX_LEGAL_MOVES],
            len: 0,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn as_slice(&self) -> &[Move] {
        &self.moves[..self.len]
    }

    #[inline]
    fn push(&mut self, mv: Move) {
        assert!(self.len < MAX_LEGAL_MOVES, "legal move list overflow");
        self.moves[self.len] = mv;
        self.len += 1;
    }
}

impl Default for MoveList {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for MoveList {
    type Target = [Move];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = core::slice::Iter<'a, Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// Generates all legal moves for the side to move.
pub fn generate_legal_moves(position: &Position) -> MoveList {
    let mut pseudo = MoveList::new();
    generate_pseudo_legal_moves_into(position, &mut pseudo);

    let mover = position.side_to_move();
    let mut scratch = position.clone();
    let mut legal = MoveList::new();
    for &mv in &pseudo {
        let undo = scratch.make_move(mv);
        if !is_in_check(&scratch, mover) {
            legal.push(mv);
        }
        scratch.unmake_move(mv, undo);
    }
    legal
}

/// Generates moves that obey piece movement but may leave the moving king in check.
pub fn generate_pseudo_legal_moves(position: &Position) -> MoveList {
    let mut moves = MoveList::new();
    generate_pseudo_legal_moves_into(position, &mut moves);
    moves
}

fn generate_pseudo_legal_moves_into(position: &Position, moves: &mut MoveList) {
    let us = position.side_to_move();
    let them = !us;
    let friends = position.color_occupancy(us);
    let enemies = position.color_occupancy(them);
    let occupancy = friends | enemies;

    generate_pawns(position, moves, us, enemies);
    generate_piece_moves(
        moves,
        position.pieces(us, PieceKind::Knight),
        friends,
        enemies,
        knight_attacks,
    );
    generate_sliders(
        moves,
        position.pieces(us, PieceKind::Bishop),
        friends,
        enemies,
        occupancy,
        bishop_attacks,
    );
    generate_sliders(
        moves,
        position.pieces(us, PieceKind::Rook),
        friends,
        enemies,
        occupancy,
        rook_attacks,
    );
    generate_sliders(
        moves,
        position.pieces(us, PieceKind::Queen),
        friends,
        enemies,
        occupancy,
        |square, occupied| bishop_attacks(square, occupied) | rook_attacks(square, occupied),
    );
    generate_piece_moves(
        moves,
        position.pieces(us, PieceKind::King),
        friends,
        enemies,
        king_attacks,
    );
    generate_castling(position, moves);
}

fn generate_pawns(position: &Position, moves: &mut MoveList, color: Color, enemies: Bitboard) {
    let rank_step = match color {
        Color::White => 1,
        Color::Black => -1,
    };
    let start_rank = match color {
        Color::White => 1,
        Color::Black => 6,
    };
    let promotion_rank = match color {
        Color::White => 7,
        Color::Black => 0,
    };

    for from in position.pieces(color, PieceKind::Pawn) {
        if let Some(to) = offset(from, 0, rank_step)
            && position.piece_at(to).is_none()
        {
            if to.rank() == promotion_rank {
                push_promotions(moves, from, to, false);
            } else {
                moves.push(Move::new(from, to, MoveFlag::QUIET));
                if from.rank() == start_rank
                    && let Some(double_to) = offset(from, 0, rank_step * 2)
                    && position.piece_at(double_to).is_none()
                {
                    moves.push(Move::new(from, double_to, MoveFlag::DOUBLE_PAWN_PUSH));
                }
            }
        }

        for file_step in [-1, 1] {
            let Some(to) = offset(from, file_step, rank_step) else {
                continue;
            };
            if enemies.contains(to) {
                if to.rank() == promotion_rank {
                    push_promotions(moves, from, to, true);
                } else {
                    moves.push(Move::new(from, to, MoveFlag::CAPTURE));
                }
            } else if position.en_passant() == Some(to) {
                let captured = offset(to, 0, -rank_step);
                if captured.is_some_and(|square| {
                    position.piece_at(square) == Some(Piece::new(!color, PieceKind::Pawn))
                }) {
                    moves.push(Move::new(from, to, MoveFlag::EN_PASSANT));
                }
            }
        }
    }
}

fn push_promotions(moves: &mut MoveList, from: Square, to: Square, capture: bool) {
    let flags = if capture {
        [
            MoveFlag::KNIGHT_PROMOTION_CAPTURE,
            MoveFlag::BISHOP_PROMOTION_CAPTURE,
            MoveFlag::ROOK_PROMOTION_CAPTURE,
            MoveFlag::QUEEN_PROMOTION_CAPTURE,
        ]
    } else {
        [
            MoveFlag::KNIGHT_PROMOTION,
            MoveFlag::BISHOP_PROMOTION,
            MoveFlag::ROOK_PROMOTION,
            MoveFlag::QUEEN_PROMOTION,
        ]
    };
    for flag in flags {
        moves.push(Move::new(from, to, flag));
    }
}

fn generate_piece_moves(
    moves: &mut MoveList,
    pieces: Bitboard,
    friends: Bitboard,
    enemies: Bitboard,
    attacks: fn(Square) -> Bitboard,
) {
    for from in pieces {
        push_targets(moves, from, attacks(from) & !friends, enemies);
    }
}

fn generate_sliders(
    moves: &mut MoveList,
    pieces: Bitboard,
    friends: Bitboard,
    enemies: Bitboard,
    occupancy: Bitboard,
    attacks: fn(Square, Bitboard) -> Bitboard,
) {
    for from in pieces {
        push_targets(moves, from, attacks(from, occupancy) & !friends, enemies);
    }
}

fn push_targets(moves: &mut MoveList, from: Square, targets: Bitboard, enemies: Bitboard) {
    for to in targets {
        let flag = if enemies.contains(to) {
            MoveFlag::CAPTURE
        } else {
            MoveFlag::QUIET
        };
        moves.push(Move::new(from, to, flag));
    }
}

fn generate_castling(position: &Position, moves: &mut MoveList) {
    let color = position.side_to_move();
    let Some(king) = position.pieces(color, PieceKind::King).first() else {
        return;
    };
    if king.rank() != color.back_rank() || is_in_check(position, color) {
        return;
    }

    for side in CastlingSide::ALL {
        let Some(rook) = position.castling_rook(color, side) else {
            continue;
        };
        if position.piece_at(rook) != Some(Piece::new(color, PieceKind::Rook))
            || rook.rank() != color.back_rank()
        {
            continue;
        }

        let king_destination = side.king_destination(color);
        let rook_destination = side.rook_destination(color);
        if !path_is_clear(position, king, king_destination, rook)
            || !path_is_clear(position, rook, rook_destination, king)
            || king_path_is_attacked(position, king, king_destination, rook)
        {
            continue;
        }
        moves.push(Move::new(king, rook, MoveFlag::CASTLING));
    }
}

fn path_is_clear(
    position: &Position,
    from: Square,
    destination: Square,
    allowed_occupant: Square,
) -> bool {
    let step = (destination.file() as i8 - from.file() as i8).signum();
    let mut file = from.file() as i8 + step;
    while file != destination.file() as i8 + step {
        let square = Square::new(from.rank() * 8 + file as u8).unwrap();
        if square != allowed_occupant && position.piece_at(square).is_some() {
            return false;
        }
        file += step;
    }
    true
}

fn king_path_is_attacked(
    position: &Position,
    from: Square,
    destination: Square,
    rook: Square,
) -> bool {
    let color = position.side_to_move();
    let mut occupancy = position.occupancy();
    occupancy.clear(from);
    occupancy.clear(rook);
    let step = (destination.file() as i8 - from.file() as i8).signum();
    let mut file = from.file() as i8 + step;
    while file != destination.file() as i8 + step {
        let square = Square::new(from.rank() * 8 + file as u8).unwrap();
        if is_square_attacked_with_occupancy(position, square, !color, occupancy) {
            return true;
        }
        file += step;
    }
    false
}
