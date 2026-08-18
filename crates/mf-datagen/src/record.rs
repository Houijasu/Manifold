//! The `bulletformat::ChessBoard` 32-byte training record.
//!
//! The layout is reproduced here rather than depended on at runtime, for the same
//! reason `mf-core` hand-rolls move generation: `mf-datagen` must have zero runtime
//! dependencies. `bulletformat` itself is pulled in as a **dev-dependency only** and
//! used as a round-trip oracle in `tests/bulletformat_round_trip.rs`, exactly as
//! `cozy-chess` is used for perft. That test is what proves this encoder agrees with
//! the trainer that will actually consume the data.

use mf_core::{Color, PieceKind, Position, Square};

/// The on-disk size of one record, in bytes.
///
/// `bulletformat` asserts `size_of::<ChessBoard>() == 32` at compile time; the
/// validation contract checks that emitted files are an exact multiple of this.
pub const RECORD_BYTES: usize = 32;

/// The most pieces one record can hold.
///
/// The nibble array is 16 bytes, two pieces per byte. This is also the maximum legal
/// piece count in chess, so a well-formed record never approaches it from above.
pub const MAX_PIECES: usize = 32;

const MAX_ZOBRIST_MATERIAL_COUNT: u32 = 16;

/// The game result, from the perspective of the side to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Outcome {
    /// The side to move lost.
    Loss = 0,
    Draw = 1,
    /// The side to move won.
    Win = 2,
}

impl Outcome {
    /// Maps a white-relative game result onto the perspective of `side_to_move`.
    pub const fn from_white_relative(white_result: Self, side_to_move: Color) -> Self {
        match side_to_move {
            Color::White => white_result,
            Color::Black => match white_result {
                Self::Loss => Self::Win,
                Self::Draw => Self::Draw,
                Self::Win => Self::Loss,
            },
        }
    }

    const fn code(self) -> u8 {
        self as u8
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Loss),
            1 => Some(Self::Draw),
            2 => Some(Self::Win),
            _ => None,
        }
    }
}

/// One 32-byte `bulletformat::ChessBoard` training record.
///
/// Records are stored **side-to-move relative**: when black is to move the board is
/// vertically mirrored, colours are swapped, and the score and result are negated. The
/// format therefore has no side-to-move field — it is implied, and every record reads
/// as though white were to move. The format is also deliberately lossy: castling
/// rights, the en-passant square, and both clocks are discarded, because the network
/// does not see them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Record {
    occupancy: u64,
    pieces: [u8; 16],
    score: i16,
    result: u8,
    king_square: u8,
    opponent_king_square: u8,
    extra: [u8; 3],
}

const _RECORD_IS_32_BYTES: () = assert!(size_of::<Record>() == RECORD_BYTES);

/// A reason a position could not be encoded as a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// A side is missing its king, so the record's king-square fields are undefined.
    MissingKing(Color),
    /// More than 32 pieces are on the board; the nibble array holds exactly 32.
    TooManyPieces(u32),
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingKing(color) => write!(formatter, "position has no {color:?} king"),
            Self::TooManyPieces(count) => {
                write!(
                    formatter,
                    "position has {count} pieces, the format holds 32"
                )
            }
        }
    }
}

impl std::error::Error for EncodeError {}

impl Record {
    /// Encodes `position` with a **side-to-move-relative** `score` in centipawns and a
    /// **side-to-move-relative** `outcome`.
    ///
    /// Taking both inputs already relative to the side to move, rather than
    /// white-relative, keeps the negation in exactly one place. `bulletformat`'s own
    /// `from_raw` takes them white-relative and negates internally; expressing the
    /// conversion once, at the call site that knows the perspective, avoids a
    /// double-negation bug that would typecheck and silently invert half the corpus.
    pub fn encode(position: &Position, score: i16, outcome: Outcome) -> Result<Self, EncodeError> {
        let side_to_move = position.side_to_move();
        let piece_count = position.occupancy().count();
        if piece_count > MAX_PIECES as u32 {
            return Err(EncodeError::TooManyPieces(piece_count));
        }

        let mut occupancy = 0u64;
        let mut pieces = [0u8; 16];
        let mut index = 0usize;

        // Squares are visited in ascending index order of the *relative* board so that
        // the nibble at position `i` corresponds to the `i`-th set bit of `occupancy`,
        // which is the invariant bullet's `BoardIter` relies on.
        for square_index in 0..64u8 {
            let relative = relative_square(square_index, side_to_move);
            let absolute = Square::new(relative).expect("relative square stays in 0..64");
            let Some(piece) = position.piece_at(absolute) else {
                continue;
            };
            occupancy |= 1u64 << square_index;

            // Colour bit 3 is set for the side NOT to move: records read as though the
            // side to move were white.
            let is_opponent = piece.color() != side_to_move;
            let code = (u8::from(is_opponent) << 3) | piece.kind().index() as u8;
            pieces[index / 2] |= code << (4 * (index & 1));
            index += 1;
        }

        let king_square =
            king_square_of(position, side_to_move).ok_or(EncodeError::MissingKing(side_to_move))?;
        let opponent_king_square = king_square_of(position, !side_to_move)
            .ok_or(EncodeError::MissingKing(!side_to_move))?;

        Ok(Self {
            occupancy,
            pieces,
            score,
            result: outcome.code(),
            king_square: relative_square(king_square.index(), side_to_move),
            // bullet stores the opponent king already flipped into its own perspective.
            opponent_king_square: relative_square(opponent_king_square.index(), side_to_move) ^ 56,
            extra: [0; 3],
        })
    }

    /// Reinterprets 32 bytes as a record.
    ///
    /// Every field is a plain integer with no invalid bit patterns, so any 32 bytes
    /// decode; semantic validity is what [`Self::structural_errors`] checks.
    pub fn from_bytes(bytes: [u8; RECORD_BYTES]) -> Self {
        Self {
            occupancy: u64::from_le_bytes(bytes[0..8].try_into().expect("8 bytes")),
            pieces: bytes[8..24].try_into().expect("16 bytes"),
            score: i16::from_le_bytes(bytes[24..26].try_into().expect("2 bytes")),
            result: bytes[26],
            king_square: bytes[27],
            opponent_king_square: bytes[28],
            extra: bytes[29..32].try_into().expect("3 bytes"),
        }
    }

    /// Serializes the record to its 32-byte on-disk form.
    ///
    /// Written field by field in explicit little-endian rather than by transmuting the
    /// `#[repr(C)]` struct, so the file format is defined by this function and not by
    /// the host's endianness or the compiler's padding choices.
    pub fn to_bytes(self) -> [u8; RECORD_BYTES] {
        let mut bytes = [0u8; RECORD_BYTES];
        bytes[0..8].copy_from_slice(&self.occupancy.to_le_bytes());
        bytes[8..24].copy_from_slice(&self.pieces);
        bytes[24..26].copy_from_slice(&self.score.to_le_bytes());
        bytes[26] = self.result;
        bytes[27] = self.king_square;
        bytes[28] = self.opponent_king_square;
        bytes[29..32].copy_from_slice(&self.extra);
        bytes
    }

    /// The side-to-move-relative score in centipawns.
    pub const fn score(self) -> i16 {
        self.score
    }

    /// The side-to-move-relative game result, or `None` if the stored code is invalid.
    pub const fn outcome(self) -> Option<Outcome> {
        Outcome::from_code(self.result)
    }

    /// The raw result byte, including values [`Outcome`] does not accept.
    pub const fn result_code(self) -> u8 {
        self.result
    }

    /// The occupancy bitboard, in the record's side-to-move-relative orientation.
    pub const fn occupancy(self) -> u64 {
        self.occupancy
    }

    /// The side-to-move king square.
    pub const fn king_square(self) -> u8 {
        self.king_square
    }

    /// The opponent king square, stored pre-flipped (`XOR 56`).
    pub const fn opponent_king_square(self) -> u8 {
        self.opponent_king_square
    }

    /// Iterates `(piece_code, square)` pairs in ascending square order.
    ///
    /// `piece_code` is `kind | (is_opponent << 3)`, matching bullet's `BoardIter`.
    ///
    /// Yields at most [`MAX_PIECES`] pairs. The nibble array holds exactly 32 entries,
    /// so an occupancy with more than 32 bits set — which only a corrupt or truncated
    /// file produces — would index past it. bullet's own `BoardIter` panics in that
    /// case; this iterator stops instead, because `datagen --validate` must be able to
    /// *report* a corrupt file rather than abort on it. The excess is not silently
    /// ignored: [`Self::structural_errors`] counts the occupancy bits directly and
    /// raises [`StructuralError::TooManyPieces`].
    pub fn iter_pieces(self) -> impl Iterator<Item = (u8, u8)> {
        let mut occupancy = self.occupancy;
        let pieces = self.pieces;
        let mut index = 0usize;
        core::iter::from_fn(move || {
            if occupancy == 0 || index >= MAX_PIECES {
                return None;
            }
            let square = occupancy.trailing_zeros() as u8;
            let code = (pieces[index / 2] >> (4 * (index & 1))) & 0b1111;
            occupancy &= occupancy - 1;
            index += 1;
            Some((code, square))
        })
    }

    fn material_count_overflow(self) -> Option<StructuralError> {
        let mut counts = [[0u32; 4]; 2];
        for (code, _) in self.iter_pieces() {
            let Some(&kind) = PieceKind::ALL.get(usize::from(code & 0b0111)) else {
                continue;
            };
            let kind_index = match kind {
                PieceKind::Knight => 0,
                PieceKind::Bishop => 1,
                PieceKind::Rook => 2,
                PieceKind::Queen => 3,
                PieceKind::Pawn | PieceKind::King => continue,
            };
            let opponent = code >> 3 != 0;
            let count = &mut counts[usize::from(opponent)][kind_index];
            *count += 1;
            if *count > MAX_ZOBRIST_MATERIAL_COUNT {
                return Some(StructuralError::MaterialCountOverflow {
                    opponent,
                    kind,
                    found: *count,
                    max: MAX_ZOBRIST_MATERIAL_COUNT,
                });
            }
        }
        None
    }

    /// Returns the structural defects in this record, using bullet's own validation
    /// rules (`bullet-utils validate`), plus a check that piece codes are in range.
    ///
    /// This exists so that `datagen --validate` can report the same verdict a
    /// third-party bullet load would, without shelling out to bullet.
    pub fn structural_errors(self) -> Vec<StructuralError> {
        let mut errors = Vec::new();
        let mut kings = [0u32; 2];
        // Counted from the occupancy bitboard rather than from `iter_pieces`, which
        // caps at MAX_PIECES so that a corrupt record cannot index past the nibble
        // array. Counting bits here is what makes the overflow observable.
        let total = self.occupancy.count_ones();

        for (code, square) in self.iter_pieces() {
            let kind = code & 0b0111;
            let is_opponent = usize::from(code >> 3);
            if kind > PieceKind::King.index() as u8 {
                errors.push(StructuralError::InvalidPieceCode(code));
                continue;
            }
            if kind == PieceKind::King.index() as u8 {
                kings[is_opponent] += 1;
                let expected = if is_opponent == 0 {
                    self.king_square
                } else {
                    self.opponent_king_square ^ 56
                };
                if expected != square {
                    errors.push(StructuralError::KingSquareMismatch);
                }
            } else if kind == PieceKind::Pawn.index() as u8 && matches!(square / 8, 0 | 7) {
                errors.push(StructuralError::PawnOnBackRank(square));
            }
        }

        if kings[0] != 1 {
            errors.push(StructuralError::WrongKingCount {
                opponent: false,
                found: kings[0],
            });
        }
        if kings[1] != 1 {
            errors.push(StructuralError::WrongKingCount {
                opponent: true,
                found: kings[1],
            });
        }
        if total <= 2 {
            errors.push(StructuralError::NoNonKingPieces);
        }
        if total > MAX_PIECES as u32 {
            errors.push(StructuralError::TooManyPieces(total));
        }
        if self.outcome().is_none() {
            errors.push(StructuralError::InvalidResult(self.result));
        }
        if let Some(error) = self.material_count_overflow() {
            errors.push(error);
        }
        errors
    }

    /// Reconstructs the position implied by this record, as a white-to-move board.
    ///
    /// The reconstruction is necessarily partial: the format discards castling rights,
    /// the en-passant square, and both clocks, so those come back as defaults. It is
    /// enough to re-derive occupancy and material, which is what `--check-filters`
    /// needs to re-check in-check status against the emitted data rather than trusting
    /// the generator's own bookkeeping.
    pub fn to_position(self) -> Option<Position> {
        if self.material_count_overflow().is_some() {
            return None;
        }
        let mut position = Position::empty(Color::White);
        for (code, square) in self.iter_pieces() {
            let kind = *PieceKind::ALL.get(usize::from(code & 0b0111))?;
            let color = if code >> 3 == 0 {
                Color::White
            } else {
                Color::Black
            };
            position.place_piece(Square::new(square)?, mf_core::Piece::new(color, kind));
        }
        Some(position)
    }
}

/// A structural defect found in a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StructuralError {
    WrongKingCount {
        opponent: bool,
        found: u32,
    },
    NoNonKingPieces,
    TooManyPieces(u32),
    MaterialCountOverflow {
        opponent: bool,
        kind: PieceKind,
        found: u32,
        max: u32,
    },
    KingSquareMismatch,
    PawnOnBackRank(u8),
    InvalidPieceCode(u8),
    InvalidResult(u8),
}

impl core::fmt::Display for StructuralError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WrongKingCount { opponent, found } => {
                let side = if *opponent { "nstm" } else { "stm" };
                write!(formatter, "invalid number of {side} kings ({found})")
            }
            Self::NoNonKingPieces => write!(formatter, "no non-king pieces on the board"),
            Self::TooManyPieces(count) => write!(formatter, "too many pieces ({count})"),
            Self::MaterialCountOverflow {
                opponent,
                kind,
                found,
                max,
            } => {
                let side = if *opponent { "nstm" } else { "stm" };
                write!(
                    formatter,
                    "too many {side} {kind:?} pieces ({found}, maximum {max})"
                )
            }
            Self::KingSquareMismatch => {
                write!(formatter, "king square does not match occupancy")
            }
            Self::PawnOnBackRank(square) => write!(formatter, "pawn on 1st/8th rank ({square})"),
            Self::InvalidPieceCode(code) => write!(formatter, "invalid piece code ({code})"),
            Self::InvalidResult(code) => write!(formatter, "invalid result code ({code})"),
        }
    }
}

/// Maps an absolute square onto the record's side-to-move-relative orientation.
///
/// Black-to-move records are vertically mirrored so every record reads as
/// white-to-move; `XOR 56` flips the rank while preserving the file.
const fn relative_square(square: u8, side_to_move: Color) -> u8 {
    match side_to_move {
        Color::White => square,
        Color::Black => square ^ 56,
    }
}

fn king_square_of(position: &Position, color: Color) -> Option<Square> {
    position.pieces(color, PieceKind::King).first()
}

#[cfg(test)]
mod tests {
    use super::{Outcome, Record, StructuralError};
    use mf_core::{Color, Position};

    fn position(fen: &str) -> Position {
        Position::from_fen(fen, false).expect("test FEN parses")
    }

    fn record_with_seventeen_stm_knights() -> Record {
        let mut bytes = Record::encode(&Position::startpos(), 0, Outcome::Draw)
            .expect("encodes")
            .to_bytes();
        bytes[0..8].copy_from_slice(&((1u64 << 19) - 1).to_le_bytes());
        bytes[8..24].fill(0);

        let codes = [5, 13, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        for (index, code) in codes.into_iter().enumerate() {
            bytes[8 + index / 2] |= code << (4 * (index & 1));
        }
        bytes[27] = 0;
        bytes[28] = 1 ^ 56;
        Record::from_bytes(bytes)
    }

    #[test]
    fn a_record_serializes_to_exactly_thirty_two_bytes() {
        let record = Record::encode(&Position::startpos(), 21, Outcome::Draw).expect("encodes");
        assert_eq!(record.to_bytes().len(), super::RECORD_BYTES);
    }

    #[test]
    fn bytes_round_trip_through_the_record() {
        let record = Record::encode(&Position::startpos(), -137, Outcome::Win).expect("encodes");
        assert_eq!(Record::from_bytes(record.to_bytes()), record);
    }

    #[test]
    fn a_white_to_move_record_keeps_absolute_squares() {
        let record = Record::encode(&Position::startpos(), 0, Outcome::Draw).expect("encodes");
        // e1 = 4, e8 = 60; the opponent king is stored pre-flipped, so 60 ^ 56 = 4.
        assert_eq!(record.king_square(), 4);
        assert_eq!(record.opponent_king_square(), 4);
        assert_eq!(record.occupancy(), 0xffff_0000_0000_ffff);
    }

    #[test]
    fn a_black_to_move_record_is_vertically_mirrored_so_it_reads_as_white_to_move() {
        let white = Record::encode(&Position::startpos(), 30, Outcome::Win).expect("encodes");
        let black = Record::encode(
            &position("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR b KQkq - 0 1"),
            30,
            Outcome::Win,
        )
        .expect("encodes");

        // The start position is vertically symmetric, so mirroring it and swapping
        // colours must yield a byte-identical board. Scores and results are supplied
        // already side-to-move relative, so they are untouched by the mirroring.
        assert_eq!(black.occupancy(), white.occupancy());
        assert_eq!(black.king_square(), white.king_square());
        assert_eq!(black.to_bytes(), white.to_bytes());
    }

    #[test]
    fn the_side_to_move_is_always_encoded_as_the_non_opponent_colour() {
        let record = Record::encode(
            &position("4k3/8/8/8/8/8/4P3/4K3 b - - 0 1"),
            0,
            Outcome::Draw,
        )
        .expect("encodes");
        // Black is to move, so the black king must carry colour bit 0 and the white
        // pawn must carry colour bit 1 (`is_opponent`).
        let codes: Vec<u8> = record.iter_pieces().map(|(code, _)| code).collect();
        assert!(codes.contains(&5), "stm king present with colour bit clear");
        assert!(
            codes.contains(&8),
            "opponent pawn present with colour bit set"
        );
    }

    #[test]
    fn outcomes_map_onto_the_side_to_move() {
        assert_eq!(
            Outcome::from_white_relative(Outcome::Win, Color::White),
            Outcome::Win
        );
        assert_eq!(
            Outcome::from_white_relative(Outcome::Win, Color::Black),
            Outcome::Loss
        );
        assert_eq!(
            Outcome::from_white_relative(Outcome::Draw, Color::Black),
            Outcome::Draw
        );
    }

    #[test]
    fn a_well_formed_record_reports_no_structural_errors() {
        let record = Record::encode(&Position::startpos(), 0, Outcome::Draw).expect("encodes");
        assert_eq!(record.structural_errors(), Vec::new());
    }

    #[test]
    fn structural_validation_catches_the_defects_bullet_checks_for() {
        // A bare-kings record has no non-king pieces, which bullet rejects.
        let bare = Record::encode(&position("4k3/8/8/8/8/8/8/4K3 w - - 0 1"), 0, Outcome::Draw)
            .expect("encodes");
        assert!(
            bare.structural_errors()
                .contains(&StructuralError::NoNonKingPieces)
        );

        // An out-of-range result code is not a valid WDL label.
        let mut bytes = Record::encode(&Position::startpos(), 0, Outcome::Draw)
            .expect("encodes")
            .to_bytes();
        bytes[26] = 9;
        assert!(
            Record::from_bytes(bytes)
                .structural_errors()
                .contains(&StructuralError::InvalidResult(9))
        );
    }

    #[test]
    fn structural_validation_reports_material_count_overflow() {
        let record = record_with_seventeen_stm_knights();
        assert!(
            record
                .structural_errors()
                .contains(&StructuralError::MaterialCountOverflow {
                    opponent: false,
                    kind: mf_core::PieceKind::Knight,
                    found: 17,
                    max: 16,
                })
        );
    }

    #[test]
    fn malformed_material_is_rejected_before_position_reconstruction() {
        let record = record_with_seventeen_stm_knights();
        assert_eq!(record.to_position(), None);
    }

    #[test]
    fn a_record_reconstructs_the_material_of_its_position() {
        let original =
            position("r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4");
        let record = Record::encode(&original, 12, Outcome::Draw).expect("encodes");
        let rebuilt = record.to_position().expect("record decodes");
        assert_eq!(rebuilt.occupancy(), original.occupancy());
        for color in Color::ALL {
            for kind in mf_core::PieceKind::ALL {
                assert_eq!(
                    rebuilt.pieces(color, kind),
                    original.pieces(color, kind),
                    "{color:?} {kind:?} must survive the round trip"
                );
            }
        }
    }
}
