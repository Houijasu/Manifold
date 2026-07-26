use core::fmt;

use crate::{PieceKind, Square};

/// The four-bit semantic tag stored in a [`Move`].
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MoveFlag(u8);

impl MoveFlag {
    pub const QUIET: Self = Self(0);
    pub const DOUBLE_PAWN_PUSH: Self = Self(1);
    pub const CASTLING: Self = Self(2);
    pub const EN_PASSANT: Self = Self(3);
    pub const CAPTURE: Self = Self(4);
    pub const KNIGHT_PROMOTION: Self = Self(8);
    pub const BISHOP_PROMOTION: Self = Self(9);
    pub const ROOK_PROMOTION: Self = Self(10);
    pub const QUEEN_PROMOTION: Self = Self(11);
    pub const KNIGHT_PROMOTION_CAPTURE: Self = Self(12);
    pub const BISHOP_PROMOTION_CAPTURE: Self = Self(13);
    pub const ROOK_PROMOTION_CAPTURE: Self = Self(14);
    pub const QUEEN_PROMOTION_CAPTURE: Self = Self(15);

    #[inline]
    pub const fn new(value: u8) -> Option<Self> {
        match value {
            0..=4 | 8..=15 => Some(Self(value)),
            _ => None,
        }
    }

    #[inline]
    pub const fn raw(self) -> u8 {
        self.0
    }

    #[inline]
    pub const fn is_capture(self) -> bool {
        matches!(self.0, 3 | 4 | 12..=15)
    }

    #[inline]
    pub const fn is_castling(self) -> bool {
        self.0 == Self::CASTLING.0
    }

    #[inline]
    pub const fn is_en_passant(self) -> bool {
        self.0 == Self::EN_PASSANT.0
    }

    #[inline]
    pub const fn is_double_pawn_push(self) -> bool {
        self.0 == Self::DOUBLE_PAWN_PUSH.0
    }

    #[inline]
    pub const fn promotion(self) -> Option<PieceKind> {
        match self.0 & 0b1011 {
            8 => Some(PieceKind::Knight),
            9 => Some(PieceKind::Bishop),
            10 => Some(PieceKind::Rook),
            11 => Some(PieceKind::Queen),
            _ => None,
        }
    }
}

impl fmt::Debug for MoveFlag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MoveFlag({})", self.0)
    }
}

/// A compact move: source square (6 bits), destination square (6 bits), flags (4 bits).
///
/// Castling follows the Chess960 convention used by strong engines: `from` is the
/// king origin and `to` is the castling rook origin.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Move(u16);

impl Move {
    #[inline]
    pub const fn new(from: Square, to: Square, flag: MoveFlag) -> Self {
        Self(from.index() as u16 | ((to.index() as u16) << 6) | ((flag.raw() as u16) << 12))
    }

    #[inline]
    pub const fn from_raw(raw: u16) -> Option<Self> {
        match MoveFlag::new((raw >> 12) as u8) {
            Some(_) => Some(Self(raw)),
            None => None,
        }
    }

    #[inline]
    pub const fn raw(self) -> u16 {
        self.0
    }

    #[inline]
    pub const fn from(self) -> Square {
        // The mask guarantees an index in 0..64.
        match Square::new((self.0 & 0x3f) as u8) {
            Some(square) => square,
            None => unreachable!(),
        }
    }

    #[inline]
    pub const fn to(self) -> Square {
        // The mask guarantees an index in 0..64.
        match Square::new(((self.0 >> 6) & 0x3f) as u8) {
            Some(square) => square,
            None => unreachable!(),
        }
    }

    #[inline]
    pub const fn flag(self) -> MoveFlag {
        MoveFlag(((self.0 >> 12) & 0x0f) as u8)
    }
}

impl fmt::Debug for Move {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Move")
            .field("from", &self.from())
            .field("to", &self.to())
            .field("flag", &self.flag())
            .finish()
    }
}
