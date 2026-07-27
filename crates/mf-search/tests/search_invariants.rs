use std::time::Duration;

use mf_core::{Position, generate_legal_moves};
use mf_search::{
    MATE_SCORE, MAX_SEARCH_PLY, SearchLimits, TranspositionTable, UNEVALUATED_STATIC_EVAL, search,
};

const MATE_CASES: [(&str, i32); 12] = [
    ("7k/8/6QK/8/8/8/8/8 w - - 0 1", 1),
    ("6k1/5ppp/8/8/8/8/5PPP/R5K1 w - - 0 1", 1),
    (
        "2bqkbn1/2pppp2/np2N3/r3P1p1/p2N2B1/5Q2/PPPPKPP1/RNB2r2 w - - 0 1",
        2,
    ),
    ("8/8/8/8/8/2k5/1q6/K7 b - - 0 1", 2),
    ("6k1/pp4p1/2p5/2bp4/8/P5Pb/1P3rrP/2BRRN1K b - - 0 1", 2),
    ("1k1r4/pp1b1R2/3q2pp/4p3/2B5/4Q3/PPP2B2/2K5 b - - 0 1", 3),
    ("r5rk/5p1p/5R2/4B3/8/8/7P/7K w - - 0 1", 3),
    (
        "r1b1kb1r/pppp1ppp/5q2/4n3/3KP3/2N3PN/PPP4P/R1BQ1B1R b - - 0 1",
        3,
    ),
    ("2r3k1/p4p2/3Rp2p/1p2P1pK/8/1P4P1/P3Q2P/1q6 b - - 0 1", 3),
    (
        "3q1rk1/p4pp1/2pb3p/3p4/6Pr/1PNQ4/P1PB1PP1/4RRK1 b - - 0 1",
        5,
    ),
    (
        "5rk1/1b3p1p/pp3p2/3n1N2/1P1qN2P/P5P1/1P1Q1P2/3R2K1 w - - 0 1",
        5,
    ),
    ("8/8/8/8/8/3k4/3p4/3K4 b - - 0 1", 9),
];

fn limits(depth: u32) -> SearchLimits {
    SearchLimits {
        depth: Some(depth),
        nodes: None,
        soft_time: None,
        hard_time: None,
        infinite: false,
    }
}

fn mate_moves(score: i32) -> Option<i32> {
    if score >= MATE_SCORE - MAX_SEARCH_PLY as i32 {
        Some((MATE_SCORE - score + 1) / 2)
    } else if score <= -MATE_SCORE + MAX_SEARCH_PLY as i32 {
        Some(-((MATE_SCORE + score) / 2))
    } else {
        None
    }
}

#[test]
fn pv_is_legal() {
    let mut position = Position::startpos();
    let table = TranspositionTable::new(16).expect("test TT should allocate");

    for sample in 0..32 {
        let before = position.clone();
        let result = search(&position, &table, limits(5));
        let mut replay = position.clone();
        for (ply, &mv) in result.pv.iter().enumerate() {
            assert!(
                generate_legal_moves(&replay).contains(&mv),
                "sample {sample} PV move {ply} is illegal"
            );
            replay.make_move(mv);
        }
        assert_eq!(position, before, "search must not mutate the root position");

        let moves = generate_legal_moves(&position);
        if moves.is_empty() {
            position = Position::startpos();
        } else {
            let index = (sample * 17 + 3) % moves.len();
            position.make_move(moves[index]);
        }
    }
}

#[test]
fn mate_in_n_found() {
    for (fen, expected) in MATE_CASES {
        let position = Position::from_fen(fen, false).expect("mate FEN should parse");
        let table = TranspositionTable::new(16).expect("test TT should allocate");
        let result = search(&position, &table, limits(24));

        assert_eq!(
            mate_moves(result.score),
            Some(expected),
            "wrong mate distance for {fen}: score {}, pv {:?}",
            result.score,
            result.pv
        );

        let mut replay = position.clone();
        for &mv in &result.pv {
            assert!(generate_legal_moves(&replay).contains(&mv));
            replay.make_move(mv);
        }
        assert!(
            generate_legal_moves(&replay).is_empty(),
            "mate PV must end at a terminal position for {fen}"
        );
    }
}

#[test]
fn score_bounds_sane() {
    let positions = [
        Position::startpos(),
        Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .unwrap(),
        Position::from_fen("7k/8/6QK/8/8/8/8/8 w - - 0 1", false).unwrap(),
    ];

    for position in positions {
        let table = TranspositionTable::new(4).expect("test TT should allocate");
        let result = search(&position, &table, limits(6));
        assert!(
            (-MATE_SCORE..=MATE_SCORE).contains(&result.score),
            "score escaped representable bounds: {}",
            result.score
        );
        assert!(result.seldepth >= result.depth);
        assert!(result.seldepth <= MAX_SEARCH_PLY as u32);
    }
}

#[test]
fn deterministic_single_thread() {
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .unwrap();
    let limits = SearchLimits {
        depth: None,
        nodes: Some(50_000),
        soft_time: None,
        hard_time: None,
        infinite: false,
    };

    let table = TranspositionTable::new(16).expect("test TT should allocate");
    let first = search(&position, &table, limits);
    let second = search(&position, &table, limits);

    assert_eq!(first.best_move, second.best_move);
    assert_eq!(first.nodes, 50_000);
    assert_eq!(second.nodes, first.nodes);
    assert_eq!(first.score, second.score);
    assert_eq!(first.pv, second.pv);
}

#[test]
fn m2_tt_marks_static_evaluation_as_unavailable() {
    let position = Position::startpos();
    let table = TranspositionTable::new(4).expect("test TT should allocate");

    search(&position, &table, limits(2));

    let entry = table
        .probe(position.zobrist().main())
        .expect("root search should store a TT entry");
    assert_eq!(entry.static_eval, UNEVALUATED_STATIC_EVAL);
}

#[test]
fn time_limits_are_observed_without_returning_immediately() {
    let position = Position::startpos();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search(
        &position,
        &table,
        SearchLimits {
            depth: None,
            nodes: None,
            soft_time: Some(Duration::from_millis(30)),
            hard_time: Some(Duration::from_millis(40)),
            infinite: false,
        },
    );

    assert!(result.elapsed >= Duration::from_millis(15));
    assert!(result.elapsed <= Duration::from_millis(200));
    assert!(result.best_move.is_some());
}
