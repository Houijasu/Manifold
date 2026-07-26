//! Chess rules, board representation, move generation, and position state.

mod bitboard;
mod sliding;

pub use bitboard::{Bitboard, BitboardIter, Square};
pub use sliding::{
    SlidingAttackBackend, SlidingAttacks, bishop_attacks, queen_attacks, rook_attacks,
};
