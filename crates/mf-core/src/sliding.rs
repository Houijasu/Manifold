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
    // SAFETY: this function is only compiled when the target guarantees BMI2. Debug builds also
    // compare every result with the software extraction below.
    let index = unsafe { pext_index(occupancy.bits(), entry.mask) };
    attack_at(attacks, entry.offset + index)
}

#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[target_feature(enable = "bmi2")]
unsafe fn pext_index(occupancy: u64, mask: u64) -> usize {
    let result = core::arch::x86_64::_pext_u64(occupancy, mask);

    #[cfg(debug_assertions)]
    debug_assert_eq!(
        result as usize,
        software_pext_index(occupancy, mask),
        "PEXT disagrees with software extraction"
    );

    result as usize
}

#[cfg(all(debug_assertions, target_arch = "x86_64", target_feature = "bmi2"))]
fn software_pext_index(value: u64, mut mask: u64) -> usize {
    let mut result = 0usize;
    let mut target = 1usize;
    while mask != 0 {
        let source = mask & mask.wrapping_neg();
        if value & source != 0 {
            result |= target;
        }
        mask &= mask - 1;
        target <<= 1;
    }
    result
}

#[inline]
fn entry_for_square(entries: &[MagicEntry; 64], square: Square) -> MagicEntry {
    entries[square.index() as usize]
}

#[inline]
fn attack_at(attacks: &[u64], index: usize) -> Bitboard {
    Bitboard::new(attacks[index])
}

#[cfg(all(test, target_arch = "x86_64", target_feature = "bmi2"))]
mod tests {
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    use std::process::Command;

    use super::pext_index;

    #[test]
    fn pext_matches_software_extraction() {
        let cases = [
            (0x0123_4567_89ab_cdef, 0x00ff_00ff_00ff_00ff),
            (0xfedc_ba98_7654_3210, 0x7e7e_7e7e_7e7e_7e7e),
            (0xa55a_3cc3_f00f_9669, 0x0001_0101_0101_017e),
            (u64::MAX, 0x0040_2010_0804_0200),
        ];

        for (occupancy, mask) in cases {
            let expected = software_pext(occupancy, mask);
            // SAFETY: this test is only compiled when BMI2 is enabled.
            assert_eq!(unsafe { pext_index(occupancy, mask) }, expected);
        }
    }

    #[test]
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    fn compiler_backend_avoids_llvm_21_pext_corruption() {
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let version = Command::new(rustc)
            .arg("-vV")
            .output()
            .expect("rustc should report its version");
        assert!(
            version.status.success(),
            "rustc -vV failed with status {}",
            version.status
        );
        let version = String::from_utf8(version.stdout).expect("rustc version should be UTF-8");
        let llvm_major = version
            .lines()
            .find_map(|line| line.strip_prefix("LLVM version: "))
            .and_then(|version| version.split('.').next())
            .and_then(|major| major.parse::<u32>().ok())
            .expect("rustc should report a numeric LLVM version");
        assert!(
            llvm_major >= 22,
            "LLVM 21 generated a release binary that intermittently corrupted Chess960 perft on \
             the validation CPU; use the repository's pinned Rust toolchain (rustc output:\n\
             {version})"
        );
    }

    fn software_pext(value: u64, mut mask: u64) -> usize {
        let mut result = 0usize;
        let mut target = 1usize;
        while mask != 0 {
            let source = mask & mask.wrapping_neg();
            if value & source != 0 {
                result |= target;
            }
            mask &= mask - 1;
            target <<= 1;
        }
        result
    }
}
