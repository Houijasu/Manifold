use crate::{Color, Square};

/// The wing used for a castling right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CastlingSide {
    KingSide,
    QueenSide,
}

impl CastlingSide {
    pub const ALL: [Self; 2] = [Self::KingSide, Self::QueenSide];

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn from_rook_origin(king: Square, rook: Square) -> Self {
        if rook.file() > king.file() {
            Self::KingSide
        } else {
            Self::QueenSide
        }
    }

    #[inline]
    pub const fn king_destination(self, color: Color) -> Square {
        destination(
            color,
            match self {
                Self::KingSide => 6,
                Self::QueenSide => 2,
            },
        )
    }

    #[inline]
    pub const fn rook_destination(self, color: Color) -> Square {
        destination(
            color,
            match self {
                Self::KingSide => 5,
                Self::QueenSide => 3,
            },
        )
    }
}

/// Castling rights represented by the originating rook square for each wing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CastlingRights {
    rooks: [[Option<Square>; 2]; 2],
}

impl CastlingRights {
    #[inline]
    pub const fn rook(self, color: Color, side: CastlingSide) -> Option<Square> {
        self.rooks[color.index()][side.index()]
    }

    #[inline]
    pub(crate) fn set_rook(&mut self, color: Color, side: CastlingSide, rook: Option<Square>) {
        self.rooks[color.index()][side.index()] = rook;
    }
}

#[inline]
const fn destination(color: Color, file: u8) -> Square {
    let index = color.back_rank() * 8 + file;
    match Square::new(index) {
        Some(square) => square,
        None => unreachable!(),
    }
}
