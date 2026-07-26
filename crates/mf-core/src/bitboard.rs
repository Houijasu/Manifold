use core::fmt;
use core::iter::FusedIterator;
use core::ops::{
    BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not, Shl, ShlAssign, Shr,
    ShrAssign,
};

/// A square indexed in rank-major order from `a1 = 0` to `h8 = 63`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Square(u8);

impl Square {
    /// Creates a square when `index` is in `0..64`.
    #[inline]
    pub const fn new(index: u8) -> Option<Self> {
        if index < 64 { Some(Self(index)) } else { None }
    }

    /// Returns this square's `0..63` index.
    #[inline]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Returns a bitboard containing only this square.
    #[inline]
    pub const fn bitboard(self) -> Bitboard {
        Bitboard(1u64 << self.0)
    }

    /// Returns this square's zero-based file.
    #[inline]
    pub const fn file(self) -> u8 {
        self.0 & 7
    }

    /// Returns this square's zero-based rank.
    #[inline]
    pub const fn rank(self) -> u8 {
        self.0 >> 3
    }
}

impl fmt::Debug for Square {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = char::from(b'a' + self.file());
        let rank = char::from(b'1' + self.rank());
        write!(formatter, "{file}{rank}")
    }
}

/// A set of chessboard squares backed by a `u64`.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bitboard(u64);

impl Bitboard {
    pub const EMPTY: Self = Self(0);
    pub const FULL: Self = Self(u64::MAX);

    /// Wraps raw bitboard bits.
    #[inline]
    pub const fn new(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the raw bitboard bits.
    #[inline]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }

    #[inline]
    pub const fn contains(self, square: Square) -> bool {
        self.0 & square.bitboard().0 != 0
    }

    #[inline]
    pub fn set(&mut self, square: Square) {
        self.0 |= square.bitboard().0;
    }

    #[inline]
    pub fn clear(&mut self, square: Square) {
        self.0 &= !square.bitboard().0;
    }

    #[inline]
    pub const fn first(self) -> Option<Square> {
        if self.is_empty() {
            None
        } else {
            Square::new(self.0.trailing_zeros() as u8)
        }
    }

    /// Removes and returns the least-significant square.
    #[inline]
    pub fn pop_first(&mut self) -> Option<Square> {
        let square = self.first()?;
        self.0 &= self.0 - 1;
        Some(square)
    }
}

impl fmt::Debug for Bitboard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Bitboard({:#018x})", self.0)
    }
}

impl From<u64> for Bitboard {
    #[inline]
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Bitboard> for u64 {
    #[inline]
    fn from(value: Bitboard) -> Self {
        value.0
    }
}

macro_rules! impl_bitboard_binary_op {
    ($trait:ident, $method:ident, $assign_trait:ident, $assign_method:ident, $operator:tt) => {
        impl $trait for Bitboard {
            type Output = Self;

            #[inline]
            fn $method(self, rhs: Self) -> Self::Output {
                Self(self.0 $operator rhs.0)
            }
        }

        impl $assign_trait for Bitboard {
            #[inline]
            fn $assign_method(&mut self, rhs: Self) {
                self.0 = self.0 $operator rhs.0;
            }
        }
    };
}

impl_bitboard_binary_op!(BitAnd, bitand, BitAndAssign, bitand_assign, &);
impl_bitboard_binary_op!(BitOr, bitor, BitOrAssign, bitor_assign, |);
impl_bitboard_binary_op!(BitXor, bitxor, BitXorAssign, bitxor_assign, ^);

impl Not for Bitboard {
    type Output = Self;

    #[inline]
    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

macro_rules! impl_bitboard_shift {
    ($trait:ident, $method:ident, $assign_trait:ident, $assign_method:ident, $operator:tt) => {
        impl $trait<u32> for Bitboard {
            type Output = Self;

            #[inline]
            fn $method(self, rhs: u32) -> Self::Output {
                Self(self.0 $operator rhs)
            }
        }

        impl $assign_trait<u32> for Bitboard {
            #[inline]
            fn $assign_method(&mut self, rhs: u32) {
                self.0 = self.0 $operator rhs;
            }
        }
    };
}

impl_bitboard_shift!(Shl, shl, ShlAssign, shl_assign, <<);
impl_bitboard_shift!(Shr, shr, ShrAssign, shr_assign, >>);

pub struct BitboardIter(Bitboard);

impl Iterator for BitboardIter {
    type Item = Square;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.pop_first()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.0.count() as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for BitboardIter {}
impl FusedIterator for BitboardIter {}

impl IntoIterator for Bitboard {
    type Item = Square;
    type IntoIter = BitboardIter;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        BitboardIter(self)
    }
}
