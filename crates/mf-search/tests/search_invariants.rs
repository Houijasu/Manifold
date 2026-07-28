use std::time::Duration;

use mf_core::{Position, format_uci_move, generate_legal_moves, parse_uci_move};
use mf_search::{
    MATE_SCORE, MAX_SEARCH_PLY, SearchLimits, TranspositionTable, UNEVALUATED_STATIC_EVAL, search,
    search_with_history,
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

fn position_and_history(fen: &str, moves: &[&str]) -> (Position, Vec<u64>) {
    let mut position = Position::from_fen(fen, false).expect("test FEN should parse");
    let mut history = vec![position.repetition_key()];
    for notation in moves {
        let mv = parse_uci_move(&position, notation, false)
            .unwrap_or_else(|| panic!("test move {notation} should be legal"));
        position.make_move(mv);
        history.push(position.repetition_key());
    }
    (position, history)
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

#[test]
fn fifty_move_rule_draws_at_the_boundary_but_not_from_a_fresh_clock() {
    let near_draw = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 98 1", false).unwrap();
    let fresh = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 0 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");

    assert_eq!(search(&near_draw, &table, limits(6)).score, 0);

    let fresh_parent = Position::from_fen("8/8/4k3/8/8/8/8/K1Q5 b - - 0 1", false).unwrap();
    assert!(
        search(&fresh_parent, &table, limits(6)).score <= -400,
        "a parent must not reuse a draw cached for the same child board at clock 98"
    );
    assert!(
        search(&fresh, &table, limits(6)).score >= 400,
        "positions with different halfmove clocks must not share TT scores"
    );
}

#[test]
fn already_claimable_fifty_move_draw_is_scored_at_the_root() {
    let position = Position::from_fen("7k/8/8/8/8/8/P1q5/K7 w - - 100 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search(&position, &table, limits(6));

    assert_eq!(result.score, 0);
    assert!(result.best_move.is_some());
}

#[test]
fn supplied_history_detects_threefold_repetition_and_selects_the_drawing_move() {
    let fen = "1q5k/8/8/8/8/8/8/R5K1 w - - 0 1";
    let moves = [
        "a1a2", "b8b7", "a2a1", "b7b8", "a1a2", "b8b7", "a2a1", "b7b8", "a1a2", "b8b7",
    ];
    let (position, history) = position_and_history(fen, &moves);
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_with_history(&position, &history, &table, limits(6));

    assert_eq!(result.score, 0);
    assert_eq!(
        result
            .best_move
            .map(|mv| format_uci_move(&position, mv, false)),
        Some("a2a1".to_string())
    );
}

#[test]
fn winning_side_avoids_completing_a_threefold_repetition() {
    let fen = "8/8/8/4k3/8/8/8/K1Q5 w - - 0 1";
    let moves = ["c1c2", "e5e6", "c2c1", "e6e5", "c1c2", "e5e6"];
    let (position, history) = position_and_history(fen, &moves);
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_with_history(&position, &history, &table, limits(6));
    let best_move = result
        .best_move
        .map(|mv| format_uci_move(&position, mv, false));

    assert!(result.score >= 300, "winning score was {}", result.score);
    assert_ne!(best_move.as_deref(), Some("c2c1"));
}

#[test]
fn insufficient_material_is_scored_as_a_draw_without_masking_two_bishops() {
    for fen in [
        "8/8/8/4k3/8/8/8/4K3 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KB2 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KN2 w - - 0 1",
    ] {
        let position = Position::from_fen(fen, false).unwrap();
        let table = TranspositionTable::new(4).expect("test TT should allocate");
        assert_eq!(search(&position, &table, limits(4)).score, 0, "{fen}");
    }

    let position = Position::from_fen("8/8/8/4k3/8/8/8/2B1KB2 w - - 0 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    assert!(search(&position, &table, limits(4)).score >= 100);
}

#[test]
fn stalemate_resource_is_visible_through_quiescence() {
    let position = Position::from_fen("1r5k/7p/8/8/8/8/1r6/K6Q w - - 0 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search(&position, &table, limits(4));

    assert_eq!(result.score, 0);
    assert_eq!(
        result
            .best_move
            .map(|mv| format_uci_move(&position, mv, false)),
        Some("h1h7".to_string())
    );
}
