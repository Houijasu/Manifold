mod common;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use mf_core::{Position, perft};

static EXCLUSIVE_PERFT: Mutex<()> = Mutex::new(());

fn exclusive_perft() -> MutexGuard<'static, ()> {
    EXCLUSIVE_PERFT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn startpos_anchor_is_exact() {
    let _guard = exclusive_perft();
    let (depth, expected) = if cfg!(debug_assertions) {
        (4, 197_281)
    } else {
        (6, 119_060_324)
    };
    assert_eq!(perft(&mut Position::startpos(), depth), expected);
}

#[test]
fn kiwipete_anchor_is_exact() {
    let _guard = exclusive_perft();
    let mut position = common::position(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    );
    let (depth, expected) = if cfg!(debug_assertions) {
        (3, 97_862)
    } else {
        (5, 193_690_690)
    };
    assert_eq!(perft(&mut position, depth), expected);
}

#[test]
fn en_passant_pin_edges_are_exact() {
    let _guard = exclusive_perft();
    let mut horizontal = common::position("8/8/8/8/k1pP3R/8/8/4K3 b - d3 0 1", false);
    assert_eq!(perft(&mut horizontal, 1), 6);

    let mut before_double_push = common::position("8/8/8/8/k1p4R/8/3P4/4K3 w - - 0 1", false);
    assert_eq!(perft(&mut before_double_push, 3), 1_715);

    let mut legal = common::position("8/8/8/1k6/3Pp3/8/8/4KQ2 b - d3 0 1", false);
    assert_eq!(perft(&mut legal, 1), 6);
}

#[test]
fn promotions_are_exact() {
    let _guard = exclusive_perft();
    let fen = "8/PPPk4/8/8/8/8/4Kppp/8 w - - 0 1";
    for (depth, expected) in [(1, 18), (2, 270), (3, 4_699)] {
        assert_eq!(perft(&mut common::position(fen, false), depth), expected);
    }
}

#[test]
fn orthodox_castling_is_exact() {
    let _guard = exclusive_perft();
    let fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
    assert_eq!(perft(&mut common::position(fen, false), 1), 26);
    assert_eq!(perft(&mut common::position(fen, false), 4), 314_346);
}

#[test]
fn chess960_rook_file_castling_is_exact() {
    let _guard = exclusive_perft();
    let fen = "rk6/8/8/8/8/8/8/RK6 w Aa - 0 1";
    assert_eq!(perft(&mut common::position(fen, true), 1), 11);
}

#[test]
fn chess960_castling_through_attacked_rook_origin_is_illegal() {
    let _guard = exclusive_perft();
    let fen = "qn1rkrbb/pp1p1ppp/2p1p3/3n4/4P2P/2NP4/PPP2PP1/Q1NRKRBB w FDfd - 1 9";
    assert_eq!(perft(&mut common::position(fen, true), 3), 14_769);
}

#[test]
fn cpw_suite_is_exact() {
    let _guard = exclusive_perft();
    let depth = if cfg!(debug_assertions) { 3 } else { 6 };
    common::suite(
        &Path::new(common::TESTDATA).join("cpw_perft.epd"),
        depth,
        false,
    );
}
