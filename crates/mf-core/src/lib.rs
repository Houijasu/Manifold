//! Chess rules, board representation, move generation, and position state.

mod bitboard;
mod castling;
mod chess_move;
mod piece;
mod position;
mod sliding;
mod zobrist;

pub use bitboard::{Bitboard, BitboardIter, Square};
pub use castling::{CastlingRights, CastlingSide};
pub use chess_move::{Move, MoveFlag};
pub use piece::{Color, Piece, PieceKind};
pub use position::{Position, Undo};
pub use sliding::{
    SlidingAttackBackend, SlidingAttacks, bishop_attacks, queen_attacks, rook_attacks,
};
pub use zobrist::ZobristKeys;
