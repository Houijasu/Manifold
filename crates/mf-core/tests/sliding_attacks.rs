use cozy_chess::{
    BitBoard as CozyBitboard, Square as CozySquare, get_bishop_moves, get_rook_moves,
};
use mf_core::{
    Bitboard, SlidingAttackBackend, SlidingAttacks, Square, bishop_attacks, queen_attacks,
    rook_attacks,
};

fn for_each_subset(mask: Bitboard, mut visit: impl FnMut(Bitboard)) {
    let mut subset = Bitboard::EMPTY;
    loop {
        visit(subset);
        subset = Bitboard::new(subset.bits().wrapping_sub(mask.bits()) & mask.bits());
        if subset.is_empty() {
            break;
        }
    }
}

fn relevant_mask(square: Square, oracle: impl Fn(Square, Bitboard) -> Bitboard) -> Bitboard {
    let empty_attacks = oracle(square, Bitboard::EMPTY);
    let mut mask = Bitboard::EMPTY;

    for index in 0..64 {
        let blocker = Square::new(index).unwrap().bitboard();
        if oracle(square, blocker) != empty_attacks {
            mask |= blocker;
        }
    }

    mask
}

fn expected_rook(square: Square, occupancy: Bitboard) -> Bitboard {
    Bitboard::new(
        get_rook_moves(
            CozySquare::index(square.index() as usize),
            CozyBitboard(occupancy.bits()),
        )
        .0,
    )
}

fn expected_bishop(square: Square, occupancy: Bitboard) -> Bitboard {
    Bitboard::new(
        get_bishop_moves(
            CozySquare::index(square.index() as usize),
            CozyBitboard(occupancy.bits()),
        )
        .0,
    )
}

fn verify_backend(backend: SlidingAttacks) {
    for index in 0..64 {
        let square = Square::new(index).unwrap();
        let rook_mask = relevant_mask(square, expected_rook);
        let bishop_mask = relevant_mask(square, expected_bishop);

        for_each_subset(rook_mask, |relevant| {
            let occupancies = [
                relevant,
                relevant | !rook_mask,
                relevant | Bitboard::new(0xa55a_3cc3_f00f_9669) & !rook_mask,
            ];
            for occupancy in occupancies {
                let expected = expected_rook(square, occupancy);
                assert_eq!(
                    backend.rook_attacks(square, occupancy),
                    expected,
                    "{:?} rook mismatch on square {index}, occupancy {:#018x}",
                    backend.backend(),
                    occupancy.bits()
                );
            }
        });

        for_each_subset(bishop_mask, |relevant| {
            let occupancies = [
                relevant,
                relevant | !bishop_mask,
                relevant | Bitboard::new(0x5aa5_c33c_0ff0_6996) & !bishop_mask,
            ];
            for occupancy in occupancies {
                let expected = expected_bishop(square, occupancy);
                assert_eq!(
                    backend.bishop_attacks(square, occupancy),
                    expected,
                    "{:?} bishop mismatch on square {index}, occupancy {:#018x}",
                    backend.backend(),
                    occupancy.bits()
                );
                assert_eq!(
                    backend.queen_attacks(square, occupancy),
                    expected | expected_rook(square, occupancy),
                    "{:?} queen mismatch on square {index}, occupancy {:#018x}",
                    backend.backend(),
                    occupancy.bits()
                );
            }
        });
    }
}

#[test]
fn magic_backend_matches_independent_oracle_exhaustively() {
    let backend = SlidingAttacks::magic();
    assert_eq!(backend.backend(), SlidingAttackBackend::Magic);
    verify_backend(backend);
}

#[test]
fn selected_backend_matches_independent_oracle_exhaustively() {
    let backend = SlidingAttacks::new();

    #[cfg(feature = "force-magic")]
    assert_eq!(backend.backend(), SlidingAttackBackend::Magic);

    #[cfg(all(
        not(feature = "force-magic"),
        target_arch = "x86_64",
        target_feature = "bmi2"
    ))]
    assert_eq!(backend.backend(), SlidingAttackBackend::Pext);

    verify_backend(backend);
}

#[test]
fn process_wide_attack_functions_use_selected_backend() {
    let square = Square::new(27).unwrap();
    let occupancy = Bitboard::new(0x8100_2418_1824_0081);
    let selected = SlidingAttacks::new();

    assert_eq!(
        rook_attacks(square, occupancy),
        selected.rook_attacks(square, occupancy)
    );
    assert_eq!(
        bishop_attacks(square, occupancy),
        selected.bishop_attacks(square, occupancy)
    );
    assert_eq!(
        queen_attacks(square, occupancy),
        selected.queen_attacks(square, occupancy)
    );
}

#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
#[test]
fn pext_backend_matches_independent_oracle_exhaustively() {
    let backend = SlidingAttacks::pext();
    assert_eq!(backend.backend(), SlidingAttackBackend::Pext);
    verify_backend(backend);
}
