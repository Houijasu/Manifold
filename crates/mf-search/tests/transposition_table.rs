use std::sync::{Arc, Barrier};
use std::thread;

use mf_core::{Move, MoveFlag, Square};
use mf_search::{
    AllocationError, Bound, CACHE_LINE_BYTES, CLUSTER_ALIGNMENT, CLUSTER_BYTES,
    ENTRIES_PER_CLUSTER, ENTRY_BYTES, EntryData, TranspositionTable, max_hash_for_installed_memory,
    max_hash_mebibytes,
};

fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn random_move(state: &mut u64) -> Move {
    let from = (next_random(state) & 63) as u8;
    let mut to = (next_random(state) & 63) as u8;
    if to == from {
        to = (to + 1) & 63;
    }
    let flags = [
        MoveFlag::QUIET,
        MoveFlag::DOUBLE_PAWN_PUSH,
        MoveFlag::CASTLING,
        MoveFlag::EN_PASSANT,
        MoveFlag::CAPTURE,
        MoveFlag::KNIGHT_PROMOTION,
        MoveFlag::BISHOP_PROMOTION,
        MoveFlag::ROOK_PROMOTION,
        MoveFlag::QUEEN_PROMOTION,
        MoveFlag::KNIGHT_PROMOTION_CAPTURE,
        MoveFlag::BISHOP_PROMOTION_CAPTURE,
        MoveFlag::ROOK_PROMOTION_CAPTURE,
        MoveFlag::QUEEN_PROMOTION_CAPTURE,
    ];
    let flag = flags[next_random(state) as usize % flags.len()];
    Move::new(
        Square::new(from).expect("generated source square should be valid"),
        Square::new(to).expect("generated destination square should be valid"),
        flag,
    )
}

#[test]
fn table_layout_is_cache_line_aligned_without_straddling() {
    assert_eq!(CACHE_LINE_BYTES, 64);
    assert_eq!(ENTRY_BYTES, 16);
    assert_eq!(
        CACHE_LINE_BYTES % (ENTRY_BYTES * ENTRIES_PER_CLUSTER),
        0,
        "entry stride times cluster size must divide a cache line exactly"
    );
    assert_eq!(ENTRIES_PER_CLUSTER, 4);
    assert_eq!(CLUSTER_BYTES, CACHE_LINE_BYTES);
    assert_eq!(CLUSTER_ALIGNMENT, CACHE_LINE_BYTES);

    let table = TranspositionTable::new(1).expect("one MiB table should allocate");
    assert_eq!(table.base_address() % CACHE_LINE_BYTES, 0);
}

#[test]
fn requested_hash_size_allocates_approximately_that_many_mebibytes() {
    for mebibytes in [1, 3, 16] {
        let table = TranspositionTable::new(mebibytes).expect("small table should allocate");
        let requested = mebibytes * 1024 * 1024;
        assert!(
            table.allocated_bytes() >= requested,
            "allocation must not undershoot the requested Hash size"
        );
        assert!(
            table.allocated_bytes() < requested + CACHE_LINE_BYTES,
            "allocation should round only to the next cache line"
        );
    }
}

/// The advertised ceiling must describe this machine, not a constant chosen once.
///
/// The engine used to advertise `max 1048576` while refusing anything over 4096 MB, so a
/// GUI that honoured the advertised range got a diagnostic and the previous table. The
/// ceiling is now derived from installed memory, which is the only number that makes
/// "advertised" and "allocatable" the same claim.
#[test]
fn the_offered_maximum_is_half_of_installed_memory_rounded_down_to_a_power_of_two() {
    const GIB: u64 = 1024 * 1024 * 1024;
    for (installed_gib, expected_mib) in [(8u64, 4096), (16, 8192), (32, 16384), (64, 32768)] {
        assert_eq!(
            max_hash_for_installed_memory(Some(installed_gib * GIB)),
            expected_mib,
            "a {installed_gib} GB machine should be offered {expected_mib} MB"
        );
    }
    // This machine reports 33,957,138,432 bytes -- 31.6 GiB, not a clean 32. Half of that
    // is 15.8 GiB, which rounds DOWN to 8192 MB. Rounding up to 16384 would offer more
    // than half the machine, so the answer must be the smaller one.
    assert_eq!(max_hash_for_installed_memory(Some(33_957_138_432)), 8192);
}

/// A machine whose memory cannot be read is offered the size the engine allocated
/// successfully for its whole history, rather than nothing or a guess.
#[test]
fn an_unknown_machine_falls_back_to_the_historically_safe_maximum() {
    assert_eq!(max_hash_for_installed_memory(None), 4096);
}

/// The offered maximum is never degenerate, even on a very small machine.
#[test]
fn a_tiny_machine_is_still_offered_a_usable_table() {
    let maximum = max_hash_for_installed_memory(Some(64 * 1024 * 1024));
    assert_eq!(maximum, 32);
    assert_eq!(max_hash_for_installed_memory(Some(1024 * 1024)), 16);
}

/// The live ceiling -- real memory detection plus the cached answer -- obeys the
/// policy's invariants on every machine, from a small laptop to a workstation.
/// Machine-size claims belong to the parameterized policy test above: a 16 GB CI
/// runner is legitimately offered 4096 MB, so asserting more here would hard-code one
/// developer's RAM into the suite.
#[test]
fn this_machine_is_offered_a_power_of_two_at_or_above_the_floor() {
    let maximum = max_hash_mebibytes();
    assert!(
        maximum.is_power_of_two(),
        "the advertised maximum should be a round number a GUI can offer: got {maximum}"
    );
    assert!(
        maximum >= 16,
        "even the smallest machine must be offered a usable table: got {maximum}"
    );
}

#[test]
fn a_request_past_the_machine_maximum_reports_the_real_limit() {
    let maximum = max_hash_mebibytes();
    let rejection = TranspositionTable::new(maximum + 1)
        .err()
        .expect("a request past the machine maximum should be rejected");
    match rejection {
        AllocationError::RequestTooLarge {
            requested_mib,
            limit_mib,
        } => {
            assert_eq!(requested_mib, maximum + 1);
            assert_eq!(
                limit_mib, maximum,
                "the reported limit must be the advertised maximum, not a hard-coded constant"
            );
        }
        other => panic!("expected RequestTooLarge, got {other:?}"),
    }
}

#[test]
fn pack_and_probe_round_trip_losslessly_under_fuzzing() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    let mut random = 0x6d61_6e69_666f_6c64;

    for iteration in 0..1_000_000 {
        let key = next_random(&mut random);
        let best_move = (iteration % 5 != 0).then(|| random_move(&mut random));
        let data = EntryData {
            best_move,
            score: next_random(&mut random) as i16,
            static_eval: next_random(&mut random) as i16,
            depth: next_random(&mut random) as u8,
            bound: match next_random(&mut random) % 3 {
                0 => Bound::Upper,
                1 => Bound::Lower,
                _ => Bound::Exact,
            },
            age: next_random(&mut random) as u8 & 31,
            pv: next_random(&mut random) & 1 != 0,
        };

        table.store(key, data);
        assert_eq!(
            table.probe(key),
            Some(data),
            "round-trip mismatch at fuzz iteration {iteration}"
        );
    }
}

#[test]
fn boundary_values_round_trip_without_losing_bits() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    let maximum_legal_encoding = Move::new(
        Square::new(62).expect("g8 should be valid"),
        Square::new(63).expect("h8 should be valid"),
        MoveFlag::QUEEN_PROMOTION_CAPTURE,
    );
    let cases = [
        EntryData {
            best_move: None,
            score: i16::MIN,
            static_eval: i16::MAX,
            depth: 0,
            bound: Bound::Upper,
            age: 0,
            pv: false,
        },
        EntryData {
            best_move: Some(maximum_legal_encoding),
            score: i16::MAX,
            static_eval: i16::MIN,
            depth: u8::MAX,
            bound: Bound::Exact,
            age: 31,
            pv: true,
        },
    ];

    for (index, data) in cases.into_iter().enumerate() {
        let key = (0xABCDu64 << 48) | (0x1234 + index as u64);
        table.store(key, data);
        assert_eq!(table.probe(key), Some(data));
    }
}

#[test]
#[should_panic(expected = "TT age must fit in five bits")]
fn out_of_range_age_is_rejected_instead_of_silently_truncated() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    table.store(
        0x1111_2222_3333_4444,
        EntryData {
            best_move: None,
            score: 0,
            static_eval: 0,
            depth: 1,
            bound: Bound::Upper,
            age: 32,
            pv: false,
        },
    );
}

#[test]
fn indexing_and_key_verification_use_independent_key_bits() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    let first = EntryData {
        best_move: None,
        score: 101,
        static_eval: 102,
        depth: 10,
        bound: Bound::Upper,
        age: 1,
        pv: false,
    };
    let second = EntryData {
        best_move: None,
        score: 201,
        static_eval: 202,
        depth: 20,
        bound: Bound::Lower,
        age: 2,
        pv: true,
    };

    // Both tiny keys map to cluster zero in a one-MiB table. Their low bits,
    // however, are independent verification fragments and must remain distinct.
    table.store(1, first);
    table.store(2, second);

    assert_eq!(table.probe(1), Some(first));
    assert_eq!(table.probe(2), Some(second));
}

#[test]
fn concurrent_probe_never_accepts_a_torn_entry() {
    let table = Arc::new(TranspositionTable::new(1).expect("test table should allocate"));
    let key = (0xCDEFu64 << 48) | 0x5678;
    let first = EntryData {
        best_move: Some(Move::new(
            Square::new(1).expect("b1 should be valid"),
            Square::new(18).expect("c3 should be valid"),
            MoveFlag::QUIET,
        )),
        score: 1111,
        static_eval: -2222,
        depth: 33,
        bound: Bound::Upper,
        age: 3,
        pv: false,
    };
    let second = EntryData {
        best_move: Some(Move::new(
            Square::new(6).expect("g1 should be valid"),
            Square::new(21).expect("f3 should be valid"),
            MoveFlag::CAPTURE,
        )),
        score: -3333,
        static_eval: 4444,
        depth: 77,
        bound: Bound::Exact,
        age: 19,
        pv: true,
    };
    table.store(key, first);

    let start = Arc::new(Barrier::new(2));
    let writer_table = Arc::clone(&table);
    let writer_start = Arc::clone(&start);
    let writer = thread::spawn(move || {
        writer_start.wait();
        for iteration in 0..2_000_000 {
            writer_table.store(key, if iteration & 1 == 0 { second } else { first });
        }
    });

    start.wait();
    let mut unexpected = None;
    for _ in 0..2_000_000 {
        if let Some(observed) = table.probe(key)
            && observed != first
            && observed != second
        {
            unexpected = Some(observed);
            break;
        }
    }
    writer.join().expect("writer thread should complete");

    assert_eq!(
        unexpected, None,
        "a probe accepted fields combined from different complete stores"
    );
}

#[test]
fn full_cluster_replaces_the_shallowest_current_entry() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    let depths = [4, 12, 20, 28];

    for (offset, depth) in depths.into_iter().enumerate() {
        table.store(
            1 + offset as u64,
            EntryData {
                best_move: None,
                score: depth.into(),
                static_eval: 0,
                depth,
                bound: Bound::Lower,
                age: 7,
                pv: false,
            },
        );
    }

    let replacement = EntryData {
        best_move: None,
        score: 99,
        static_eval: 0,
        depth: 16,
        bound: Bound::Exact,
        age: 7,
        pv: true,
    };
    table.store(5, replacement);

    assert_eq!(table.probe(1), None);
    for key in 2..=4 {
        assert!(table.probe(key).is_some(), "deeper key {key} was evicted");
    }
    assert_eq!(table.probe(5), Some(replacement));
}

#[test]
fn storing_an_existing_key_updates_it_without_evicting_cluster_peers() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    let original = EntryData {
        best_move: None,
        score: 10,
        static_eval: 20,
        depth: 5,
        bound: Bound::Upper,
        age: 4,
        pv: false,
    };
    for key in 1..=4 {
        table.store(key, original);
    }

    let updated = EntryData {
        best_move: None,
        score: 30,
        static_eval: 40,
        depth: 25,
        bound: Bound::Exact,
        age: 5,
        pv: true,
    };
    table.store(3, updated);

    for key in [1, 2, 4] {
        assert_eq!(table.probe(key), Some(original));
    }
    assert_eq!(table.probe(3), Some(updated));
}

#[test]
fn replacement_age_wraps_across_the_five_bit_generation_boundary() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    for (key, depth, age) in [(1, 20, 31), (2, 15, 0), (3, 30, 0), (4, 40, 0)] {
        table.store(
            key,
            EntryData {
                best_move: None,
                score: 0,
                static_eval: 0,
                depth,
                bound: Bound::Lower,
                age,
                pv: false,
            },
        );
    }

    let replacement = EntryData {
        best_move: None,
        score: 9,
        static_eval: 8,
        depth: 10,
        bound: Bound::Exact,
        age: 0,
        pv: true,
    };
    table.store(5, replacement);

    assert_eq!(table.probe(1), None);
    assert!(table.probe(2).is_some());
    assert_eq!(table.probe(5), Some(replacement));
}

#[test]
fn clear_makes_previously_stored_probe_miss() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    let data = EntryData {
        best_move: Some(Move::new(
            Square::new(12).expect("e2 should be valid"),
            Square::new(28).expect("e4 should be valid"),
            MoveFlag::DOUBLE_PAWN_PUSH,
        )),
        score: 37,
        static_eval: 29,
        depth: 18,
        bound: Bound::Exact,
        age: 7,
        pv: true,
    };

    table.store(0x1234_5678_9abc_def0, data);
    assert_eq!(table.probe(0x1234_5678_9abc_def0), Some(data));

    table.clear();

    assert_eq!(table.probe(0x1234_5678_9abc_def0), None);
}

#[test]
fn hashfull_reports_sampled_occupancy_in_per_mille() {
    let table = TranspositionTable::new(1).expect("test TT should allocate");
    assert_eq!(table.hashfull_per_mille(), 0);

    for index in 0..128u64 {
        table.store(
            index.wrapping_mul(0x9e37_79b9_7f4a_7c15),
            EntryData {
                best_move: None,
                score: index as i16,
                static_eval: 0,
                depth: 1,
                bound: Bound::Exact,
                age: 0,
                pv: false,
            },
        );
    }

    let hashfull = table.hashfull_per_mille();
    assert!((1..=1_000).contains(&hashfull));
    table.clear();
    assert_eq!(table.hashfull_per_mille(), 0);
}

/// `hashfull 0` must mean "empty", never "occupied but rounded down".
///
/// A large table holding a small search floors to zero under integer division, which is
/// accurate but collides with the empty-table reading. GUIs distinguish the two, and at
/// least one renders `hashfull 0` as 100% usage.
#[test]
fn a_table_holding_entries_never_reports_zero_occupancy() {
    let table = TranspositionTable::new(256).expect("test TT should allocate");
    assert_eq!(
        table.hashfull_per_mille(),
        0,
        "an untouched table is the only thing allowed to report 0"
    );

    // One entry in a 256 MiB table is ~0.0000002 per-mille: as close to zero as the
    // field can get without being empty.
    table.store(
        0x9e37_79b9_7f4a_7c15,
        EntryData {
            best_move: None,
            score: 1,
            static_eval: 0,
            depth: 1,
            bound: Bound::Exact,
            age: 0,
            pv: false,
        },
    );

    assert!(
        table.hashfull_per_mille() >= 1,
        "a table with a stored entry must not be reported as empty"
    );
}

#[test]
fn prefetch_is_safe_for_arbitrary_keys() {
    let table = TranspositionTable::new(1).expect("test table should allocate");
    for key in [0, 1, u64::MAX, 0x0123_4567_89ab_cdef] {
        table.prefetch(key);
    }
}
