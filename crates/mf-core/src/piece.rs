use core::ops::Not;

/// A chess side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Color {
    White,
    Black,
}

impl Color {
    pub const ALL: [Self; 2] = [Self::White, Self::Black];

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn back_rank(self) -> u8 {
        match self {
            Self::White => 0,
            Self::Black => 7,
        }
    }
}

impl Not for Color {
    type Output = Self;

    #[inline]
    fn not(self) -> Self::Output {
        match self {
            Self::White => Self::Black,
            Self::Black => Self::White,
        }
    }
}

/// A chess piece kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PieceKind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceKind {
    pub const ALL: [Self; 6] = [
        Self::Pawn,
        Self::Knight,
        Self::Bishop,
        Self::Rook,
        Self::Queen,
        Self::King,
    ];
    pub const NON_PAWN_MATERIAL: [Self; 4] = [Self::Knight, Self::Bishop, Self::Rook, Self::Queen];

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn is_minor(self) -> bool {
        matches!(self, Self::Knight | Self::Bishop)
    }

    #[inline]
    pub const fn is_major(self) -> bool {
        matches!(self, Self::Rook | Self::Queen)
    }

    #[inline]
    pub const fn is_non_pawn_material(self) -> bool {
        matches!(self, Self::Knight | Self::Bishop | Self::Rook | Self::Queen)
    }
}

/// A colored chess piece.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    color: Color,
    kind: PieceKind,
}

impl Piece {
    #[inline]
    pub const fn new(color: Color, kind: PieceKind) -> Self {
        Self { color, kind }
    }

    #[inline]
    pub const fn color(self) -> Color {
        self.color
    }

    #[inline]
    pub const fn kind(self) -> PieceKind {
        self.kind
    }

    #[inline]
    pub const fn index(self) -> usize {
        self.color.index() * PieceKind::ALL.len() + self.kind.index()
    }
}
