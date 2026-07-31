//! FullThreats feature indexing and active-feature enumeration.
//!
//! Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
//! Eonego's copyright and MIT license notice are reproduced there.

use std::sync::OnceLock;

use mf_core::{
    Bitboard, Color, Piece, PieceKind, Position, Square, bishop_attacks, king_attacks,
    knight_attacks, pawn_attacks, queen_attacks, rook_attacks,
};

/// Number of FullThreats input dimensions.
pub const DIMENSIONS: usize = 60_720;
/// Safe fixed capacity for active FullThreats features.
pub const MAX_ACTIVE: usize = 256;

const PIECE_NB: usize = 16;
const BOARD_SQUARES: usize = 64;
const INDEX_LUT1_LEN: usize = PIECE_NB * PIECE_NB * 2;
const INDEX_LUT2_LEN: usize = PIECE_NB * BOARD_SQUARES * BOARD_SQUARES;
const OFFSETS_LEN: usize = PIECE_NB * BOARD_SQUARES;
const ALL_PIECES: [u8; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];
const NUM_VALID_TARGETS: [usize; PIECE_NB] = [0, 6, 10, 8, 8, 10, 0, 0, 0, 6, 10, 8, 8, 10, 0, 0];
const THREAT_MAP: [[i8; 6]; 6] = [
    [0, 1, -1, 2, -1, -1],
    [0, 1, 2, 3, 4, -1],
    [0, 1, 2, 3, -1, -1],
    [0, 1, 2, 3, -1, -1],
    [0, 1, 2, 3, 4, -1],
    [-1, -1, -1, -1, -1, -1],
];
const ORIENT_TABLE: [u8; 64] = [
    0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7,
    0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7,
];
const FILE_A: u64 = 0x0101_0101_0101_0101;
const FILE_H: u64 = 0x8080_8080_8080_8080;

/// A piece in the FullThreats reference encoding: `(color << 3) + piece_type`.
///
/// Piece types are pawn=1 through king=6, matching the Eonego feature format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreatPiece(u8);

impl ThreatPiece {
    #[inline]
    pub const fn new(color: Color, kind: PieceKind) -> Self {
        Self(((color as u8) << 3) + kind as u8 + 1)
    }

    #[inline]
    const fn encoded(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug)]
struct ThreatLuts {
    index_lut1: [u32; INDEX_LUT1_LEN],
    index_lut2: [u8; INDEX_LUT2_LEN],
    offsets: [u16; OFFSETS_LEN],
}

static THREAT_LUTS: OnceLock<ThreatLuts> = OnceLock::new();

#[inline]
const fn piece_type(piece: usize) -> usize {
    piece & 7
}

#[inline]
const fn piece_color(piece: usize) -> usize {
    piece >> 3
}

#[inline]
fn square(index: usize) -> Square {
    Square::new(index as u8).expect("FullThreats square must be on board")
}

#[inline]
fn pawn_push_or_attacks(color: usize, from: usize) -> Bitboard {
    let color = if color == Color::White.index() {
        Color::White
    } else {
        Color::Black
    };
    let from = square(from);
    let push = match color {
        Color::White if from.index() < 56 => 1u64 << (from.index() + 8),
        Color::Black if from.index() >= 8 => 1u64 << (from.index() - 8),
        Color::White | Color::Black => 0,
    };
    Bitboard::new(pawn_attacks(from, color).bits() | push)
}

#[inline]
fn pseudo_attacks(kind: usize, from: usize) -> Bitboard {
    let from = square(from);
    match kind {
        2 => knight_attacks(from),
        3 => bishop_attacks(from, Bitboard::EMPTY),
        4 => rook_attacks(from, Bitboard::EMPTY),
        5 => queen_attacks(from, Bitboard::EMPTY),
        6 => king_attacks(from),
        _ => unreachable!("only non-pawn reference piece types have pseudo-attacks"),
    }
}

#[inline]
fn attack_set(piece: usize, from: usize) -> Bitboard {
    if piece_type(piece) == 1 {
        pawn_push_or_attacks(piece_color(piece), from)
    } else {
        pseudo_attacks(piece_type(piece), from)
    }
}

fn build_luts() -> ThreatLuts {
    let mut luts = ThreatLuts {
        index_lut1: [DIMENSIONS as u32; INDEX_LUT1_LEN],
        index_lut2: [0; INDEX_LUT2_LEN],
        offsets: [0; OFFSETS_LEN],
    };

    for &piece in &ALL_PIECES {
        let piece = usize::from(piece);
        for from in 0..BOARD_SQUARES {
            let attacks = attack_set(piece, from).bits();
            for to in 0..BOARD_SQUARES {
                let below_target = if to == 0 { 0 } else { (1u64 << to) - 1 };
                luts.index_lut2[(piece * BOARD_SQUARES + from) * BOARD_SQUARES + to] =
                    (attacks & below_target).count_ones() as u8;
            }
        }
    }

    let mut cumulative_piece = [0usize; PIECE_NB];
    let mut cumulative_offsets = [0usize; PIECE_NB];
    let mut cumulative_offset = 0;
    for &piece in &ALL_PIECES {
        let piece = usize::from(piece);
        let mut piece_offset = 0;
        let is_pawn = piece_type(piece) == 1;
        for from in 0..BOARD_SQUARES {
            luts.offsets[piece * BOARD_SQUARES + from] = piece_offset as u16;
            if !is_pawn {
                piece_offset += pseudo_attacks(piece_type(piece), from).count() as usize;
            } else if (8..=55).contains(&from) {
                piece_offset += pawn_push_or_attacks(piece_color(piece), from).count() as usize;
            }
        }
        cumulative_piece[piece] = piece_offset;
        cumulative_offsets[piece] = cumulative_offset;
        cumulative_offset += NUM_VALID_TARGETS[piece] * piece_offset;
    }
    debug_assert_eq!(cumulative_offset, DIMENSIONS);

    for &attacker in &ALL_PIECES {
        let attacker = usize::from(attacker);
        for &attacked in &ALL_PIECES {
            let attacked = usize::from(attacked);
            let enemy = attacker ^ attacked == 8;
            let attacker_type = piece_type(attacker);
            let attacked_type = piece_type(attacked);
            let mapped = THREAT_MAP[attacker_type - 1][attacked_type - 1];
            let semi_excluded = attacker_type == attacked_type && (enemy || attacker_type != 1);
            let table_index = (attacker * PIECE_NB + attacked) * 2;

            if mapped >= 0 {
                let feature = cumulative_offsets[attacker]
                    + (piece_color(attacked) * (NUM_VALID_TARGETS[attacker] / 2) + mapped as usize)
                        * cumulative_piece[attacker];
                luts.index_lut1[table_index] = feature as u32;
                if !semi_excluded {
                    luts.index_lut1[table_index + 1] = feature as u32;
                }
            }
        }
    }

    luts
}

#[inline]
fn luts() -> &'static ThreatLuts {
    THREAT_LUTS.get_or_init(build_luts)
}

#[inline]
fn make_index_oriented(
    orientation: u8,
    swap: usize,
    attacker: ThreatPiece,
    from: Square,
    to: Square,
    attacked: ThreatPiece,
) -> usize {
    let luts = luts();
    let from = usize::from(from.index() ^ orientation);
    let to = usize::from(to.index() ^ orientation);
    let attacker = attacker.encoded() ^ swap;
    let attacked = attacked.encoded() ^ swap;

    luts.index_lut1[(attacker * PIECE_NB + attacked) * 2 + usize::from(from < to)] as usize
        + usize::from(luts.offsets[attacker * BOARD_SQUARES + from])
        + usize::from(luts.index_lut2[(attacker * BOARD_SQUARES + from) * BOARD_SQUARES + to])
}

/// Returns the FullThreats feature index for one physical threat.
///
/// Excluded threats return an index greater than or equal to [`DIMENSIONS`].
#[inline]
pub fn make_index(
    perspective: Color,
    attacker: ThreatPiece,
    from: Square,
    to: Square,
    attacked: ThreatPiece,
    king_square: Square,
) -> usize {
    let orientation = ORIENT_TABLE[usize::from(king_square.index())] ^ (56 * perspective as u8);
    make_index_oriented(
        orientation,
        8 * perspective.index(),
        attacker,
        from,
        to,
        attacked,
    )
}

/// Appends all active FullThreats features for one perspective into a fixed buffer.
///
/// Own-piece contacts are included as defences. Kings are targets but never attackers.
#[inline]
pub fn append_active_threats(
    perspective: Color,
    position: &Position,
    buffer: &mut [usize; MAX_ACTIVE],
) -> usize {
    let king_square = position.king_square(perspective);
    let orientation = ORIENT_TABLE[usize::from(king_square.index())] ^ (56 * perspective as u8);
    let swap = 8 * perspective.index();
    let occupied = position.occupancy();
    let occupied_bits = occupied.bits();
    let all_pawns = position.pieces(Color::White, PieceKind::Pawn).bits()
        | position.pieces(Color::Black, PieceKind::Pawn).bits();
    let mut count = 0;

    for color in [perspective, !perspective] {
        let attacker = ThreatPiece::new(color, PieceKind::Pawn);
        let color_pawns = position.pieces(color, PieceKind::Pawn).bits();

        match color {
            Color::White => {
                let mut northeast = ((color_pawns & !FILE_H) << 9) & occupied_bits;
                while northeast != 0 {
                    let to = northeast.trailing_zeros() as u8;
                    northeast &= northeast - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to - 9,
                        to,
                    );
                }

                let mut northwest = ((color_pawns & !FILE_A) << 7) & occupied_bits;
                while northwest != 0 {
                    let to = northwest.trailing_zeros() as u8;
                    northwest &= northwest - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to - 7,
                        to,
                    );
                }

                let mut blocked = ((all_pawns >> 8) & color_pawns) << 8;
                while blocked != 0 {
                    let to = blocked.trailing_zeros() as u8;
                    blocked &= blocked - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to - 8,
                        to,
                    );
                }
            }
            Color::Black => {
                let mut southwest = ((color_pawns & !FILE_A) >> 9) & occupied_bits;
                while southwest != 0 {
                    let to = southwest.trailing_zeros() as u8;
                    southwest &= southwest - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to + 9,
                        to,
                    );
                }

                let mut southeast = ((color_pawns & !FILE_H) >> 7) & occupied_bits;
                while southeast != 0 {
                    let to = southeast.trailing_zeros() as u8;
                    southeast &= southeast - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to + 7,
                        to,
                    );
                }

                let mut blocked = ((all_pawns << 8) & color_pawns) >> 8;
                while blocked != 0 {
                    let to = blocked.trailing_zeros() as u8;
                    blocked &= blocked - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to + 8,
                        to,
                    );
                }
            }
        }

        for kind in [
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Rook,
            PieceKind::Queen,
        ] {
            let attacker = ThreatPiece::new(color, kind);
            let mut pieces = position.pieces(color, kind);
            while let Some(from) = pieces.pop_first() {
                let mut attacks = match kind {
                    PieceKind::Knight => knight_attacks(from),
                    PieceKind::Bishop => bishop_attacks(from, occupied),
                    PieceKind::Rook => rook_attacks(from, occupied),
                    PieceKind::Queen => queen_attacks(from, occupied),
                    PieceKind::Pawn | PieceKind::King => unreachable!(),
                } & occupied;
                while let Some(to) = attacks.pop_first() {
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        from.index(),
                        to.index(),
                    );
                }
            }
        }
    }

    count
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn emit(
    position: &Position,
    buffer: &mut [usize; MAX_ACTIVE],
    count: &mut usize,
    orientation: u8,
    swap: usize,
    attacker: ThreatPiece,
    from: u8,
    to: u8,
) {
    let to_square = square(usize::from(to));
    let attacked = ThreatPiece::from(
        position
            .piece_at(to_square)
            .expect("FullThreats targets must be occupied"),
    );
    let index = make_index_oriented(
        orientation,
        swap,
        attacker,
        square(usize::from(from)),
        to_square,
        attacked,
    );
    if index < DIMENSIONS && *count < MAX_ACTIVE {
        buffer[*count] = index;
        *count += 1;
    }
}

impl From<Piece> for ThreatPiece {
    #[inline]
    fn from(piece: Piece) -> Self {
        Self::new(piece.color(), piece.kind())
    }
}
