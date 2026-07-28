//! Chess rules, board representation, move generation, and position state.

mod attacks;
mod bitboard;
mod castling;
mod chess_move;
mod fen;
mod movegen;
mod notation;
mod perft;
mod piece;
mod position;
mod see;
mod sliding;
mod zobrist;

pub use attacks::{is_in_check, is_square_attacked, king_attacks, knight_attacks, pawn_attacks};
pub use bitboard::{Bitboard, BitboardIter, Square};
pub use castling::{CastlingRights, CastlingSide};
pub use chess_move::{Move, MoveFlag};
pub use fen::FenError;
pub use movegen::{MoveList, generate_legal_moves, generate_pseudo_legal_moves, has_legal_move};
pub use notation::{format_uci_move, parse_uci_move};
pub use perft::{perft, perft_divide};
pub use piece::{Color, Piece, PieceKind, material_value};
pub use position::{Position, Undo};
pub use see::static_exchange_evaluation;
pub use sliding::{
    SlidingAttackBackend, SlidingAttacks, bishop_attacks, queen_attacks, rook_attacks,
};
pub use zobrist::ZobristKeys;
