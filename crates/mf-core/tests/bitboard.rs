use mf_core::{Bitboard, Square};

#[test]
fn square_round_trips_valid_indices() {
    for index in 0..64 {
        let square = Square::new(index).expect("indices below 64 are valid");
        assert_eq!(square.index(), index);
        assert_eq!(square.bitboard(), Bitboard::new(1u64 << index));
    }

    assert_eq!(Square::new(64), None);
    assert_eq!(Square::new(u8::MAX), None);
}

#[test]
fn bitboard_bitwise_operations_match_u64() {
    let left = Bitboard::new(0x00ff_00ff_1234_5678);
    let right = Bitboard::new(0x0f0f_f0f0_8765_4321);

    assert_eq!((left & right).bits(), left.bits() & right.bits());
    assert_eq!((left | right).bits(), left.bits() | right.bits());
    assert_eq!((left ^ right).bits(), left.bits() ^ right.bits());
    assert_eq!((!left).bits(), !left.bits());
    assert_eq!((left << 7).bits(), left.bits() << 7);
    assert_eq!((right >> 9).bits(), right.bits() >> 9);
}

#[test]
fn bitboard_mutation_and_queries_track_membership() {
    let a1 = Square::new(0).unwrap();
    let d4 = Square::new(27).unwrap();
    let h8 = Square::new(63).unwrap();
    let mut board = Bitboard::EMPTY;

    assert!(board.is_empty());
    board.set(a1);
    board.set(d4);
    board.set(h8);

    assert_eq!(board.count(), 3);
    assert!(board.contains(d4));
    assert_eq!(board.first(), Some(a1));

    board.clear(d4);
    assert!(!board.contains(d4));
    assert_eq!(board.count(), 2);
}

#[test]
fn bitboard_iteration_consumes_squares_low_to_high() {
    let expected = [1, 8, 17, 42, 63];
    let board = expected
        .into_iter()
        .map(|index| Square::new(index).unwrap().bitboard())
        .fold(Bitboard::EMPTY, |all, square| all | square);

    let actual: Vec<u8> = board.into_iter().map(Square::index).collect();
    assert_eq!(actual, expected);
}
