//! HalfKAv2_hm feature indexing.
//!
//! Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
//! Eonego's copyright and MIT license notice are reproduced there.

use mf_core::{Color, Piece, Square};

const PS_WHITE: [usize; 12] = [0, 128, 256, 384, 512, 640, 64, 192, 320, 448, 576, 640];
const PS_BLACK: [usize; 12] = [64, 192, 320, 448, 576, 640, 0, 128, 256, 384, 512, 640];

const WHITE_KING_BUCKETS: [usize; 64] = [
    19712, 20416, 21120, 21824, 21824, 21120, 20416, 19712, 16896, 17600, 18304, 19008, 19008,
    18304, 17600, 16896, 14080, 14784, 15488, 16192, 16192, 15488, 14784, 14080, 11264, 11968,
    12672, 13376, 13376, 12672, 11968, 11264, 8448, 9152, 9856, 10560, 10560, 9856, 9152, 8448,
    5632, 6336, 7040, 7744, 7744, 7040, 6336, 5632, 2816, 3520, 4224, 4928, 4928, 4224, 3520, 2816,
    0, 704, 1408, 2112, 2112, 1408, 704, 0,
];

/// Returns the HalfKAv2_hm feature index for a piece from one king's perspective.
#[inline]
pub fn make_index(perspective: Color, piece: Piece, square: Square, king_square: Square) -> usize {
    let orient = if king_square.file() < 4 { 7 } else { 0 }
        ^ if perspective == Color::Black { 56 } else { 0 };
    let bucket_square = if perspective == Color::White {
        king_square.index()
    } else {
        king_square.index() ^ 56
    };
    let piece_offsets = if perspective == Color::White {
        &PS_WHITE
    } else {
        &PS_BLACK
    };

    usize::from(square.index() ^ orient)
        + piece_offsets[piece.index()]
        + WHITE_KING_BUCKETS[usize::from(bucket_square)]
}
