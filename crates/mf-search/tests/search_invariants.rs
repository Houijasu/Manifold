use std::time::Duration;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use mf_core::{Position, format_uci_move, generate_legal_moves, parse_uci_move};
use mf_nnue::Network;
use mf_search::{
    MATE_SCORE, MAX_SEARCH_PLY, SearchLimits, SearchOptions, TranspositionTable,
    UNEVALUATED_STATIC_EVAL, search, search_with_callback,
};

/// The engine evaluates only with NNUE, so every search here needs a network.
///
/// Loaded once for the whole target: the file is ~106 MiB, and re-reading it per test
/// dominated the runtime. Returns `None` when the (gitignored) network is absent so a
/// fresh clone reports skips rather than 20 confusing failures.
fn network() -> Option<&'static Network> {
    static NETWORK: OnceLock<Option<Network>> = OnceLock::new();
    NETWORK
        .get_or_init(|| {
            let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
                || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
                PathBuf::from,
            );
            if !path.is_file() {
                eprintln!("SKIPPED: search invariants need {}", path.display());
                return None;
            }
            Some(Network::load(&path).unwrap_or_else(|error| {
                panic!("failed to load NNUE network {}: {error}", path.display())
            }))
        })
        .as_ref()
}

/// `search` with the default options, which is what most invariants below want.
fn search_default(
    position: &Position,
    table: &TranspositionTable,
    limits: SearchLimits,
    network: &Network,
) -> mf_search::SearchResult {
    search(position, table, limits, SearchOptions::default(), network)
}

/// `search` seeded with a game history, for repetition and fifty-move invariants.
fn search_history(
    position: &Position,
    history: &[u64],
    table: &TranspositionTable,
    limits: SearchLimits,
    network: &Network,
) -> mf_search::SearchResult {
    let stop = AtomicBool::new(false);
    search_with_callback(
        position,
        history,
        table,
        limits,
        SearchOptions::default(),
        network,
        &stop,
        |_| {},
    )
}

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
fn selectivity_options_default_to_enabled() {
    assert_eq!(
        SearchOptions::default(),
        SearchOptions {
            use_nmp: true,
            use_rfp: true,
            use_razoring: true,
            use_lmr: true,
            use_lmp: true,
            use_futility: true,
            use_see_pruning: true,
            use_qsearch_tt: true,
            use_qsearch_delta_pruning: true,
            // Quiet checks in quiescence ship DISABLED: measured at -12.75 +/- 23.01
            // Elo over 300 games and 0.12 plies SHALLOWER at equal time. See the
            // comment on `SearchOptions::default` in `search.rs`.
            use_qsearch_checks: false,
            // Capture LMR also ships DISABLED: it saves 21-33% of the tree at fixed
            // depth and converts that to only +0.12 plies at equal time, measuring
            // -8.11 +/- 20.67 Elo. See the comment on `SearchOptions::default`.
            use_capture_lmr: false,
            use_singular_ext: true,
            use_check_ext: true,
            use_multicut: true,
            use_iir: true,
            use_probcut: true,
            use_butterfly_history: true,
            use_capture_history: true,
            // Pawn history is the one selectivity option that ships DISABLED: it is a
            // measured node-count regression until continuation history exists. See
            // the comment on `SearchOptions::default` in `search.rs`.
            use_pawn_history: false,
            use_continuation_history: true,
            // History pruning also ships DISABLED: it saves nodes but loses ~237 Elo.
            // See the comment on `SearchOptions::default` in `search.rs`.
            use_history_pruning: false,
            use_correction_history: true,
            // Major-piece and material correction history ship DISABLED. Both were
            // added to the reference engine and later removed, and both saturate here: they key
            // on a piece set that barely varies within one search, so residuals from
            // unrelated positions pile into a handful of buckets. Enabling them is
            // 3.3x MORE nodes at depth 14 while looking cheaper on bench. See the
            // comment on `SearchOptions::default` in `search.rs`.
            use_correction_sources: [true, true, false, false, true],
        }
    );
}

#[test]
fn null_move_pruning_is_inert_without_non_pawn_material() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen("7k/8/p1p5/1p5p/1P5P/2P5/P4K2/8 w - - 0 1", false)
        .expect("pawn-only FEN should parse");
    let enabled_table = TranspositionTable::new(4).expect("test TT should allocate");
    let disabled_table = TranspositionTable::new(4).expect("test TT should allocate");
    let enabled = search(
        &position,
        &enabled_table,
        limits(7),
        SearchOptions::default(),
        network,
    );
    let disabled = search(
        &position,
        &disabled_table,
        limits(7),
        SearchOptions {
            use_nmp: false,
            ..SearchOptions::default()
        },
        network,
    );

    assert_eq!(enabled.best_move, disabled.best_move);
    assert_eq!(enabled.score, disabled.score);
    assert_eq!(enabled.nodes, disabled.nodes);
}

#[test]
fn pv_is_legal() {
    let Some(network) = network() else {
        return;
    };
    let mut position = Position::startpos();
    let table = TranspositionTable::new(16).expect("test TT should allocate");

    for sample in 0..32 {
        let before = position.clone();
        let result = search_default(&position, &table, limits(5), network);
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
    let Some(network) = network() else {
        return;
    };
    for (fen, expected) in MATE_CASES {
        let position = Position::from_fen(fen, false).expect("mate FEN should parse");
        let table = TranspositionTable::new(16).expect("test TT should allocate");
        let result = search(
            &position,
            &table,
            limits(24),
            SearchOptions {
                use_nmp: false,
                use_rfp: false,
                use_razoring: false,
                use_lmr: false,
                use_lmp: false,
                use_futility: false,
                use_see_pruning: false,
                // The qsearch TT is a transposition cutoff, not a pruning heuristic, so
                // it stays on here exactly as the interior TT does.
                use_qsearch_tt: true,
                // Delta pruning DROPS captures, so it is off for the same reason every
                // other pruning toggle is: a mate search must be exact.
                use_qsearch_delta_pruning: false,
                // Quiet checks ADD moves to the qsearch and never drop one, so they
                // cannot hide a forced mate either way. Set to the SHIPPED default so
                // this test exercises the search that actually plays games -- finding
                // every mate here without the widening is the stronger statement.
                use_qsearch_checks: false,
                // Capture LMR is a reduction, so it is off here for the same reason
                // `use_lmr` is: a mate search must be exact.
                use_capture_lmr: false,
                use_singular_ext: false,
                use_check_ext: true,
                use_multicut: false,
                use_iir: false,
                use_probcut: false,
                // History affects only move ORDER, never which moves are legal, so a
                // forced mate must still be found with the tables on. Leaving them
                // enabled keeps this test exercising the shipped ordering path.
                use_butterfly_history: true,
                use_capture_history: true,
                use_pawn_history: false,
                use_continuation_history: true,
                // History pruning DROPS moves, so it must be off here for the same
                // reason every other pruning toggle is: a mate search must be exact.
                use_history_pruning: false,
                // Correction history adjusts the static EVAL, never the move list, and
                // `to_corrected_static_eval` clamps away from the decisive range so a
                // residual can never manufacture or mask a mate score. Leaving it on
                // keeps this test exercising the shipped eval path.
                use_correction_history: true,
                use_correction_sources: SearchOptions::default().use_correction_sources,
            },
            network,
        );

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
fn selectivity_preserves_shallow_forced_mates() {
    let Some(network) = network() else {
        return;
    };
    for (fen, expected) in &MATE_CASES[..9] {
        let position = Position::from_fen(fen, false).expect("mate FEN should parse");
        let table = TranspositionTable::new(16).expect("test TT should allocate");
        let result = search_default(&position, &table, limits(24), network);

        assert_eq!(
            mate_moves(result.score),
            Some(*expected),
            "selectivity lost a forced mate for {fen}: score {}, pv {:?}",
            result.score,
            result.pv
        );
    }
}

#[test]
fn score_bounds_sane() {
    let Some(network) = network() else {
        return;
    };
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
        let result = search_default(&position, &table, limits(6), network);
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
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .unwrap();
    let limits = SearchLimits {
        depth: None,
        nodes: Some(20_000),
        soft_time: None,
        hard_time: None,
        infinite: false,
    };

    let first_table = TranspositionTable::new(16).expect("test TT should allocate");
    let second_table = TranspositionTable::new(16).expect("test TT should allocate");
    let first = search_default(&position, &first_table, limits, network);
    let second = search_default(&position, &second_table, limits, network);

    assert_eq!(first.best_move, second.best_move);
    assert_eq!(first.nodes, 20_000);
    assert_eq!(second.nodes, first.nodes);
    assert_eq!(first.score, second.score);
    assert_eq!(first.pv, second.pv);

    let warm = search_default(&position, &first_table, limits, network);
    assert_eq!(warm.best_move, first.best_move);
    assert_eq!(warm.nodes, first.nodes);
}

#[test]
fn selective_fixed_depth_search_is_reproducible_with_fresh_and_warm_tt() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .unwrap();
    let mut expected = None;

    for _ in 0..5 {
        let table = TranspositionTable::new(16).expect("test TT should allocate");
        let result = search_default(&position, &table, limits(7), network);
        let signature = (
            result.best_move,
            result.score,
            result.nodes,
            result.pv.clone(),
        );
        assert_eq!(
            expected.get_or_insert_with(|| signature.clone()),
            &signature
        );

        // Re-searching a WARM table is deliberately not asserted to reproduce the cold
        // score. It never was an invariant: a warm table lets later iterations start
        // from deeper bounds, so the aspiration windows differ and a fail-soft search
        // returns a different value inside the same window. It only looked like one
        // because this FEN happened to be stable, and M4-F3 perturbed the search enough
        // to expose that. Measured on the shipped build with `UseCorrHistory=false`, so
        // with none of this feature's code active, three of five test positions still
        // drift their warm score and two drift their warm best move -- so tightening
        // this back up would be pinning a property the engine does not have.
        //
        // The real invariant is the one asserted above: independent FRESH tables must
        // reproduce the full signature bit-for-bit, which they do (66_739 nodes at
        // depth 7 here, identical across runs).
        let warm = search_default(&position, &table, limits(7), network);
        assert!(
            warm.best_move.is_some(),
            "a warm re-search must still return a legal move"
        );
    }
}

#[test]
fn campbell_ghi_position_is_stable_with_warm_and_cleared_tt() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen("5B2/1p3pk1/3p4/8/3p4/p7/8/7K b - - 78 30", false).unwrap();
    let table = TranspositionTable::new(16).expect("test TT should allocate");

    let drawing_line =
        Position::from_fen("5k2/1p3p2/3p4/8/3p4/p7/8/7K w - - 99 31", false).unwrap();
    let draw_first = search_default(&drawing_line, &table, limits(9), network);
    let warm = search_default(&position, &table, limits(10), network);
    table.clear();
    let after_new_game = search_default(&position, &table, limits(10), network);

    assert_eq!(draw_first.score, 0);
    for result in [&warm, &after_new_game] {
        assert_eq!(
            result
                .best_move
                .map(|mv| format_uci_move(&position, mv, false)),
            Some("g7f8".to_string())
        );
        assert!(
            result.score >= 500,
            "Campbell win score was {}",
            result.score
        );
    }
    assert_eq!(after_new_game.best_move, warm.best_move);
}

#[test]
fn m2_tt_marks_static_evaluation_as_unavailable() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::startpos();
    let table = TranspositionTable::new(4).expect("test TT should allocate");

    search_default(&position, &table, limits(2), network);

    let entry = table
        .probe(position.zobrist().main())
        .expect("root search should store a TT entry");
    assert_eq!(entry.static_eval, UNEVALUATED_STATIC_EVAL);
}

#[test]
fn time_limits_are_observed_without_returning_immediately() {
    let Some(network) = network() else {
        return;
    };
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
        SearchOptions::default(),
        network,
    );

    assert!(result.elapsed >= Duration::from_millis(15));
    assert!(result.elapsed <= Duration::from_millis(200));
    assert!(result.best_move.is_some());
}

#[test]
fn fifty_move_rule_draws_at_the_boundary_but_not_from_a_fresh_clock() {
    let Some(network) = network() else {
        return;
    };
    let near_draw = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 98 1", false).unwrap();
    let fresh = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 0 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");

    assert_eq!(
        search_default(&near_draw, &table, limits(6), network).score,
        0
    );

    let fresh_parent = Position::from_fen("8/8/4k3/8/8/8/8/K1Q5 b - - 0 1", false).unwrap();
    assert!(
        search_default(&fresh_parent, &table, limits(6), network).score <= -400,
        "a parent must not reuse a draw cached for the same child board at clock 98"
    );
    assert!(
        search_default(&fresh, &table, limits(6), network).score >= 400,
        "positions with different halfmove clocks must not share TT scores"
    );

    let draw_last_table = TranspositionTable::new(4).expect("test TT should allocate");
    assert!(search_default(&fresh, &draw_last_table, limits(6), network).score >= 400);
    assert_eq!(
        search_default(&near_draw, &draw_last_table, limits(6), network).score,
        0,
        "a decisive value stored first must not poison the later draw path"
    );
}

#[test]
fn already_claimable_fifty_move_draw_is_scored_at_the_root() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen("7k/8/8/8/8/8/P1q5/K7 w - - 100 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_default(&position, &table, limits(6), network);

    assert_eq!(result.score, 0);
    assert!(result.best_move.is_some());
}

#[test]
fn supplied_history_detects_threefold_repetition_and_selects_the_drawing_move() {
    let Some(network) = network() else {
        return;
    };
    let fen = "1q5k/8/8/8/8/8/8/R5K1 w - - 0 1";
    let moves = [
        "a1a2", "b8b7", "a2a1", "b7b8", "a1a2", "b8b7", "a2a1", "b7b8", "a1a2", "b8b7",
    ];
    let (position, history) = position_and_history(fen, &moves);
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_history(&position, &history, &table, limits(6), network);

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
    let Some(network) = network() else {
        return;
    };
    let fen = "8/8/8/4k3/8/8/8/K1Q5 w - - 0 1";
    let moves = ["c1c2", "e5e6", "c2c1", "e6e5", "c1c2", "e5e6"];
    let (position, history) = position_and_history(fen, &moves);
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_history(&position, &history, &table, limits(6), network);
    let best_move = result
        .best_move
        .map(|mv| format_uci_move(&position, mv, false));

    assert!(result.score >= 300, "winning score was {}", result.score);
    assert_ne!(best_move.as_deref(), Some("c2c1"));
}

#[test]
fn insufficient_material_is_scored_as_a_draw_without_masking_two_bishops() {
    let Some(network) = network() else {
        return;
    };
    for fen in [
        "8/8/8/4k3/8/8/8/4K3 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KB2 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KN2 w - - 0 1",
    ] {
        let position = Position::from_fen(fen, false).unwrap();
        let table = TranspositionTable::new(4).expect("test TT should allocate");
        assert_eq!(
            search_default(&position, &table, limits(4), network).score,
            0,
            "{fen}"
        );
    }

    let position = Position::from_fen("8/8/8/4k3/8/8/8/2B1KB2 w - - 0 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    assert!(search_default(&position, &table, limits(4), network).score >= 100);
}

/// Iterative deepening must stop at `MAX_SEARCH_PLY`, even when nothing else stops it.
///
/// A user ran `go infinite` on a forced mate and watched the engine iterate to depth
/// 3546. Nothing above `MAX_SEARCH_PLY` can produce a new line -- `pvs` returns the
/// static evaluation at that ply -- so those iterations were pure waste, and the info
/// lines they emitted were what made the search look unstoppable.
#[test]
fn an_infinite_search_stops_iterating_at_the_ply_ceiling() {
    let Some(network) = network() else {
        return;
    };
    // The user's scenario: a forced mate under `infinite`, where the mate-score early
    // exit is deliberately suppressed. Every iteration past the first is cheap, so an
    // uncapped loop runs away in seconds -- the observed depth was 322013 in six.
    let position = Position::from_fen("7k/6Q1/6K1/8/8/8/8/8 w - - 0 1", false)
        .expect("mate-in-one FEN should parse");
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let stop = AtomicBool::new(false);
    let result = search_with_callback(
        &position,
        &[position.repetition_key()],
        &table,
        SearchLimits {
            depth: None,
            nodes: None,
            soft_time: None,
            hard_time: None,
            infinite: true,
        },
        SearchOptions::default(),
        network,
        &stop,
        |_| {},
    );

    assert!(
        result.depth <= MAX_SEARCH_PLY as u32,
        "infinite search reported depth {}, above the {MAX_SEARCH_PLY}-ply ceiling",
        result.depth
    );
    assert!(
        result.best_move.is_some(),
        "a capped infinite search must still name a move"
    );
}

/// `go depth N` above the ceiling searches to the ceiling rather than to `N`.
#[test]
fn a_requested_depth_above_the_ceiling_is_clamped_to_it() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen("7k/6Q1/6K1/8/8/8/8/8 w - - 0 1", false)
        .expect("mate-in-one FEN should parse");
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_default(&position, &table, limits(4_000), network);

    assert!(
        result.depth <= MAX_SEARCH_PLY as u32,
        "go depth 4000 reported depth {}, above the {MAX_SEARCH_PLY}-ply ceiling",
        result.depth
    );
}

/// A mate delivered by a QUIET move must be visible from the first quiescence ply.
///
/// White to move has only two legal moves (both pawn pushes); after either, Black mates
/// with `Rd1#`, which captures nothing and promotes nothing. A capture-only quiescence
/// therefore returns the standing pat and reports a merely losing position, which is
/// exactly the class of tactic this feature exists to recover: the search returns a
/// score for a position it has not actually resolved.
///
/// Both arms below are the same build at the same depth with only `UseQSearchChecks`
/// moved, so the mate is attributable to the widening and to nothing else.
///
/// The technique ships OFF (it lost 0.12 plies of depth for -12.75 +/- 23.01 Elo), so
/// the DEFAULT arm here is the blind one. This test is what keeps the disabled feature
/// honest: it records exactly what the shipped search gives up, in executable form, so
/// that "quiet checks are off" stays a measured trade rather than a forgotten one.
#[test]
fn a_quiet_mating_check_is_found_only_when_the_qsearch_widening_is_enabled() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen("3r3k/8/8/8/8/6q1/P7/7K w - - 0 1", false)
        .expect("quiet-mate FEN should parse");
    assert_eq!(
        generate_legal_moves(&position).len(),
        2,
        "the test position must force White into a pawn push"
    );

    let with_checks_table = TranspositionTable::new(4).expect("test TT should allocate");
    let with_checks = search(
        &position,
        &with_checks_table,
        limits(1),
        SearchOptions {
            use_qsearch_checks: true,
            ..SearchOptions::default()
        },
        network,
    );

    let without_checks_table = TranspositionTable::new(4).expect("test TT should allocate");
    let without_checks = search_default(&position, &without_checks_table, limits(1), network);

    assert!(
        mate_moves(with_checks.score).is_some_and(|distance| distance < 0),
        "quiet checks must expose the forced mate at depth 1: score {}",
        with_checks.score
    );
    assert!(
        mate_moves(without_checks.score).is_none(),
        "a capture-only quiescence cannot see a quiet mate; if it can, this test no \
         longer isolates the widening: score {}",
        without_checks.score
    );
}

/// The toggle must reach the search and change the tree it builds.
///
/// Node counts, not scores: a widening that quietly failed to widen would still return
/// the same score on most positions, so the score is not the observable that proves the
/// option is wired.
#[test]
fn the_qsearch_checks_toggle_changes_the_searched_tree() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .expect("test FEN should parse");

    let enabled_table = TranspositionTable::new(16).expect("test TT should allocate");
    let enabled = search(
        &position,
        &enabled_table,
        limits(6),
        SearchOptions {
            use_qsearch_checks: true,
            ..SearchOptions::default()
        },
        network,
    );

    let disabled_table = TranspositionTable::new(16).expect("test TT should allocate");
    let disabled = search_default(&position, &disabled_table, limits(6), network);

    assert_ne!(
        enabled.nodes, disabled.nodes,
        "UseQSearchChecks must reach the quiescence move list"
    );
}

/// Capture LMR must SAVE nodes on tactical middlegames without changing the move played.
///
/// This is the ablation anchor for the technique, and it is what makes the decision to
/// ship it OFF a measured trade rather than a forgotten one. The saving is real and
/// large, and the moves are the same — the feature was rejected because it could not
/// SPEND the saving (+0.12 plies at equal time), not because the reduction is unsound.
/// If that ever stops being true, this test fails and the write-up in
/// `experiments/MSN-S2-capture-lmr/results.md` stops being the right explanation.
///
/// Tactical middlegames are the hostile case on purpose: they are where a wrongly
/// reduced capture would change the answer, so agreeing on the best move here is the
/// property worth pinning. Nodes are compared at a FIXED DEPTH, which is the only
/// comparison a reduction can be judged by — at fixed time a reduction that saves nodes
/// simply searches deeper and the node count says nothing.
///
/// Node counts are asserted as a strict drop rather than against pinned values, because
/// `bench_cli.rs` already pins the exact enabled tree. What this test adds is that the
/// saving is not concentrated in the one position bench happens to contain.
#[test]
fn capture_lmr_saves_nodes_on_tactical_middlegames_without_changing_the_move() {
    let Some(network) = network() else {
        return;
    };
    // Kiwipete, the Fine endgame study, a sharp Sicilian tabiya, and two promotion-race
    // positions -- all with several captures available at the root.
    const TACTICAL: [&str; 5] = [
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        "2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1",
    ];

    let mut total_enabled = 0u64;
    let mut total_disabled = 0u64;
    for fen in TACTICAL {
        let position = Position::from_fen(fen, false).expect("tactical FEN should parse");

        let enabled_table = TranspositionTable::new(16).expect("test TT should allocate");
        let enabled = search(
            &position,
            &enabled_table,
            limits(9),
            SearchOptions {
                use_capture_lmr: true,
                ..SearchOptions::default()
            },
            network,
        );

        // The technique ships OFF, so the DEFAULT arm here is the unreduced one.
        let disabled_table = TranspositionTable::new(16).expect("test TT should allocate");
        let disabled = search_default(&position, &disabled_table, limits(9), network);

        assert_eq!(
            enabled
                .best_move
                .map(|mv| format_uci_move(&position, mv, false)),
            disabled
                .best_move
                .map(|mv| format_uci_move(&position, mv, false)),
            "capture LMR changed the move played on {fen}"
        );
        total_enabled += enabled.nodes;
        total_disabled += disabled.nodes;
    }

    assert!(
        total_enabled < total_disabled,
        "capture LMR must save nodes across the tactical set: {total_enabled} with, \
         {total_disabled} without"
    );
}

/// `UseLMR=false` must silence capture LMR too.
///
/// The LMR ablation arm is the control every other selectivity delta is read against
/// (mission AGENTS.md 4.4). If capture LMR survived `UseLMR=false`, that arm would stop
/// being "no late-move reduction" and every historical LMR anchor would quietly change
/// meaning.
#[test]
fn disabling_lmr_disables_capture_lmr_as_well() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .expect("test FEN should parse");

    let with_capture_table = TranspositionTable::new(16).expect("test TT should allocate");
    let with_capture = search(
        &position,
        &with_capture_table,
        limits(8),
        SearchOptions {
            use_lmr: false,
            use_capture_lmr: true,
            ..SearchOptions::default()
        },
        network,
    );

    let without_capture_table = TranspositionTable::new(16).expect("test TT should allocate");
    let without_capture = search(
        &position,
        &without_capture_table,
        limits(8),
        SearchOptions {
            use_lmr: false,
            use_capture_lmr: false,
            ..SearchOptions::default()
        },
        network,
    );

    assert_eq!(
        with_capture.nodes, without_capture.nodes,
        "UseCaptureLMR must be inert while UseLMR is off"
    );
}

#[test]
fn stalemate_resource_is_visible_through_quiescence() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::from_fen("1r5k/7p/8/8/8/8/1r6/K6Q w - - 0 1", false).unwrap();
    let table = TranspositionTable::new(4).expect("test TT should allocate");
    let result = search_default(&position, &table, limits(4), network);

    assert_eq!(result.score, 0);
    assert_eq!(
        result
            .best_move
            .map(|mv| format_uci_move(&position, mv, false)),
        Some("h1h7".to_string())
    );
}
