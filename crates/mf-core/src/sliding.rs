use crate::{Bitboard, Square};

type AttackFunction = fn(Square, Bitboard) -> Bitboard;

#[derive(Clone, Copy)]
struct MagicEntry {
    mask: u64,
    magic: u64,
    shift: u32,
    offset: usize,
}

include!(concat!(env!("OUT_DIR"), "/sliding_tables.rs"));

/// The table-indexing backend selected for sliding-piece attacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlidingAttackBackend {
    Pext,
    Magic,
}

/// Sliding-piece attack functions with the backend selected once at construction.
#[derive(Clone, Copy)]
pub struct SlidingAttacks {
    backend: SlidingAttackBackend,
    rook: AttackFunction,
    bishop: AttackFunction,
}

impl SlidingAttacks {
    /// Selects the shipping backend at compile-time.
    ///
    /// Manifold's native build enables BMI2 and therefore chooses PEXT. Portable
    /// builds, and builds with `force-magic`, choose the generated black-magics.
    pub const fn new() -> Self {
        #[cfg(feature = "force-magic")]
        {
            Self::magic()
        }

        #[cfg(all(
            not(feature = "force-magic"),
            target_arch = "x86_64",
            target_feature = "bmi2"
        ))]
        {
            Self::pext()
        }

        #[cfg(all(
            not(feature = "force-magic"),
            not(all(target_arch = "x86_64", target_feature = "bmi2"))
        ))]
        {
            Self::magic()
        }
    }

    /// Creates a portable black-magic backend.
    pub const fn magic() -> Self {
        Self {
            backend: SlidingAttackBackend::Magic,
            rook: rook_attacks_magic,
            bishop: bishop_attacks_magic,
        }
    }

    /// Creates the BMI2 PEXT backend.
    #[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
    pub const fn pext() -> Self {
        Self {
            backend: SlidingAttackBackend::Pext,
            rook: rook_attacks_pext,
            bishop: bishop_attacks_pext,
        }
    }

    #[inline]
    pub const fn backend(self) -> SlidingAttackBackend {
        self.backend
    }

    #[inline]
    pub fn rook_attacks(self, square: Square, occupancy: Bitboard) -> Bitboard {
        (self.rook)(square, occupancy)
    }

    #[inline]
    pub fn bishop_attacks(self, square: Square, occupancy: Bitboard) -> Bitboard {
        (self.bishop)(square, occupancy)
    }

    #[inline]
    pub fn queen_attacks(self, square: Square, occupancy: Bitboard) -> Bitboard {
        self.rook_attacks(square, occupancy) | self.bishop_attacks(square, occupancy)
    }
}

impl Default for SlidingAttacks {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide attack backend. Backend selection happens once when compiled.
static SLIDING_ATTACKS: SlidingAttacks = SlidingAttacks::new();

#[inline]
pub fn rook_attacks(square: Square, occupancy: Bitboard) -> Bitboard {
    SLIDING_ATTACKS.rook_attacks(square, occupancy)
}

#[inline]
pub fn bishop_attacks(square: Square, occupancy: Bitboard) -> Bitboard {
    SLIDING_ATTACKS.bishop_attacks(square, occupancy)
}

#[inline]
pub fn queen_attacks(square: Square, occupancy: Bitboard) -> Bitboard {
    SLIDING_ATTACKS.queen_attacks(square, occupancy)
}

#[inline]
fn rook_attacks_magic(square: Square, occupancy: Bitboard) -> Bitboard {
    magic_attacks(square, occupancy, &ROOK_ENTRIES, &ROOK_MAGIC_ATTACKS)
}

#[inline]
fn bishop_attacks_magic(square: Square, occupancy: Bitboard) -> Bitboard {
    magic_attacks(square, occupancy, &BISHOP_ENTRIES, &BISHOP_MAGIC_ATTACKS)
}

#[inline]
fn magic_attacks(
    square: Square,
    occupancy: Bitboard,
    entries: &[MagicEntry; 64],
    attacks: &[u64],
) -> Bitboard {
    let entry = entry_for_square(entries, square);
    let black_occupancy = occupancy.bits() | !entry.mask;
    let index = black_occupancy.wrapping_mul(entry.magic) >> entry.shift;
    attack_at(attacks, entry.offset + index as usize)
}

#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[inline]
fn rook_attacks_pext(square: Square, occupancy: Bitboard) -> Bitboard {
    pext_attacks(square, occupancy, &ROOK_ENTRIES, &ROOK_PEXT_ATTACKS)
}

#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[inline]
fn bishop_attacks_pext(square: Square, occupancy: Bitboard) -> Bitboard {
    pext_attacks(square, occupancy, &BISHOP_ENTRIES, &BISHOP_PEXT_ATTACKS)
}

#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[inline]
fn pext_attacks(
    square: Square,
    occupancy: Bitboard,
    entries: &[MagicEntry; 64],
    attacks: &[u64],
) -> Bitboard {
    let entry = entry_for_square(entries, square);
    // SAFETY: This function is only compiled when the target guarantees BMI2.
    let index = unsafe { pext_index(occupancy.bits(), entry.mask) };
    attack_at(attacks, entry.offset + index)
}

#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[target_feature(enable = "bmi2")]
unsafe fn pext_index(occupancy: u64, mask: u64) -> usize {
    core::arch::x86_64::_pext_u64(occupancy, mask) as usize
}

#[inline]
fn entry_for_square(entries: &[MagicEntry; 64], square: Square) -> MagicEntry {
    let index = square.index() as usize;
    debug_assert!(index < entries.len());
    // SAFETY: Square can only contain an index in 0..64, matching the entry array.
    unsafe { *entries.get_unchecked(index) }
}

#[inline]
fn attack_at(attacks: &[u64], index: usize) -> Bitboard {
    debug_assert!(index < attacks.len());
    // SAFETY: build.rs allocates 1 << relevant_bits entries per square. PEXT
    // extracts at most relevant_bits, and magic shifts to exactly that width.
    unsafe { Bitboard::new(*attacks.get_unchecked(index)) }
}
