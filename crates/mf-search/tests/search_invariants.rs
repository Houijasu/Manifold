use std::time::Duration;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use mf_core::{Position, format_uci_move, generate_legal_moves, parse_uci_move};
use mf_nnue::Network;
use mf_search::{
    MATE_SCORE, MAX_SEARCH_PLY, SEARCH_PARAMETERS, SearchLimits, SearchOptions, SearchParameters,
    TranspositionTable, UNEVALUATED_STATIC_EVAL, search, search_with_callback,
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
            // Capture LMR ships ENABLED, but only after a second measurement. It
            // measured -8.11 +/- 20.67 Elo when the verification re-search always paid
            // full depth, and +11.59 +/- 22.22 once `use_post_lmr_depth` below removed
            // that constraint. See the comment on `SearchOptions::default`.
            use_capture_lmr: true,
            // The verification-depth band is the M3-F4 answer to the constraint M3-F2
            // identified, and it ships ON. Its package-mate, the continuation bonus,
            // ships OFF: measured separately they move the fixed-depth tree in opposite
            // directions. See the comment on `SearchOptions::default` in `search.rs`.
            use_post_lmr_depth: true,
            use_post_lmr_conthist: false,
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
            // The best-move node-share time term also ships DISABLED: it measured
            // -17.39 +/- 18.99 Elo at 8+0.08 and -34.86 +/- 44.35 at 30+0.3, because
            // the share it measures is r = -0.348 correlated with the stability count
            // it multiplies. See the comment on `SearchOptions::default` in `search.rs`.
            use_time_effort: false,
            // The tunable margins default to the constants the search shipped with, so
            // an untouched engine is bit-identical to one with no tuning surface at
            // all. `search_parameter_defaults_match_the_shipped_constants` pins each
            // value individually.
            parameters: SearchParameters::default(),
        }
    );
}

/// Every advertised spin's default is the constant the shipped search uses.
///
/// This is the make-or-break invariant of the tuning surface: an SPSA tuner reads the
/// handshake and writes values back with `setoption`, so a default that disagreed with
/// the constant would change the engine the first time a GUI echoed back what it was
/// just told. The bounds are checked here too, because a tuner samples inside them
/// without knowing what any parameter means.
#[test]
fn every_search_parameter_advertises_its_shipped_value_inside_a_usable_range() {
    let defaults = SearchParameters::default();
    assert!(
        (20..=40).contains(&SEARCH_PARAMETERS.len()),
        "the tuning surface should stay pragmatic, got {}",
        SEARCH_PARAMETERS.len()
    );

    for spec in SEARCH_PARAMETERS {
        assert_eq!(
            spec.value(&defaults),
            spec.default,
            "{} must default to its shipped constant",
            spec.name
        );
        assert!(
            spec.min <= spec.default && spec.default <= spec.max,
            "{} default {} is outside [{}, {}]",
            spec.name,
            spec.default,
            spec.min,
            spec.max
        );
        assert!(
            spec.min < spec.max,
            "{} has an empty tuning range",
            spec.name
        );
        assert!(
            SEARCH_PARAMETERS
                .iter()
                .filter(|other| other.name.eq_ignore_ascii_case(spec.name))
                .count()
                == 1,
            "{} is advertised twice",
            spec.name
        );
        assert_eq!(
            mf_search::search_parameter(spec.name).map(|found| found.name),
            Some(spec.name)
        );
    }
}

/// A spin write lands on the field it names, and is clamped to the advertised range.
///
/// Clamping rather than rejecting: a tuner that steps past a bound must get the bound,
/// not a silently ignored write that leaves it tuning a value the engine never adopted.
#[test]
fn writing_a_search_parameter_updates_only_that_field_and_clamps_to_its_range() {
    for spec in SEARCH_PARAMETERS {
        let mut parameters = SearchParameters::default();
        spec.set(&mut parameters, spec.max);
        assert_eq!(spec.value(&parameters), spec.max, "{}", spec.name);

        spec.set(&mut parameters, spec.max.saturating_add(1_000_000));
        assert_eq!(spec.value(&parameters), spec.max, "{} clamps up", spec.name);
        spec.set(&mut parameters, spec.min.saturating_sub(1_000_000));
        assert_eq!(
            spec.value(&parameters),
            spec.min,
            "{} clamps down",
            spec.name
        );

        // Nothing else moved: restoring this one field must restore the whole struct.
        spec.set(&mut parameters, spec.default);
        assert_eq!(parameters, SearchParameters::default(), "{}", spec.name);
    }
}

/// A changed parameter must actually reach the search.
///
/// The wiring proof, and the reason it is a NODE COUNT rather than an assertion about a
/// formula: a spin that updates `SearchParameters` but is never read at the use site
/// would pass every unit test above and change nothing about the engine. Shrinking the
/// LMR coefficient reduces less, so a fixed-depth search must visit strictly more nodes.
///
/// **Measured over a SET of positions since M5-F5, not one.** The direction is a
/// property of the reduction table, but it is not a property of every individual tree:
/// a shallower reduction changes which moves fail high and can find a cutoff sooner in
/// a particular position. Measured on this set at the shipped 2,754 against a 20%
/// smaller 2,203 (`experiments/MSN-M5-F5-spsa/lmr_position_probe.ps1`), 2 of 5
/// positions invert at depth 8 and 1 of 5 at depth 9, while the TOTALS move the right
/// way by 31% and 50% respectively. The single-position form of this test passed for
/// four milestones and then failed on a re-based default without anything being wrong,
/// which is what a test measuring the wrong granularity looks like. The full-range
/// bench sweep in `lmr_monotonicity_probe.ps1` confirms the aggregate direction across
/// all thirteen sampled coefficients from 1,000 to 6,000.
#[test]
fn changing_the_lmr_coefficient_changes_fixed_depth_node_counts() {
    let Some(network) = network() else {
        return;
    };
    const POSITIONS: [&str; 5] = [
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
        "2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1",
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
    ];
    let limits = SearchLimits {
        depth: Some(8),
        ..SearchLimits::default()
    };

    let mut parameters = SearchParameters::default();
    parameters.lmr_coefficient = parameters.lmr_coefficient * 4 / 5;

    let mut total_shipped = 0u64;
    let mut total_softer = 0u64;
    for fen in POSITIONS {
        let position = Position::from_fen(fen, false).expect("test FEN should parse");

        let shipped_table = TranspositionTable::new(4).expect("test TT should allocate");
        total_shipped += search(
            &position,
            &shipped_table,
            limits,
            SearchOptions::default(),
            network,
        )
        .nodes;

        let softer_table = TranspositionTable::new(4).expect("test TT should allocate");
        total_softer += search(
            &position,
            &softer_table,
            limits,
            SearchOptions {
                parameters,
                ..SearchOptions::default()
            },
            network,
        )
        .nodes;
    }

    assert!(
        total_softer > total_shipped,
        "a 20% smaller LMR coefficient must reduce less and search more across the set: \
         {total_softer} vs {total_shipped}"
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
                // Inert here: with `use_lmr` off no scout is ever reduced, so neither
                // post-LMR site can be reached. Set to the SHIPPED defaults so this
                // test keeps exercising the search that actually plays games.
                use_post_lmr_depth: true,
                use_post_lmr_conthist: false,
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
                // Inert here either way: this is a fixed-depth search with no soft
                // limit to scale. Set to the SHIPPED default like every other toggle.
                use_time_effort: false,
                parameters: SearchOptions::default().parameters,
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
/// This is the ablation anchor for the technique. It was written while the feature
/// shipped OFF, to keep the rejection a measured trade rather than a forgotten one:
/// the saving was real and large and the moves were the same, so the reason for the
/// -8.11 +/- 20.67 Elo was that the tree could not SPEND the saving (+0.12 plies at
/// equal time), not that the reduction was unsound.
///
/// The feature ships ON now (M4-F1b, +11.59 +/- 22.22 after `use_post_lmr_depth`
/// removed the always-full-depth re-search), and the assertion is unchanged. Only the
/// arms swapped: the DEFAULT is now the reduced one, so the flipped option produces the
/// unreduced control. That is the point of writing it as a property rather than as
/// pinned numbers — a soundness anchor should survive its feature's default flipping.
/// If it ever fails, both `experiments/MSN-S2-capture-lmr/results.md` and
/// `experiments/MSN-S7-capture-lmr-v2/results.md` stop being the right explanation.
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
///
/// **Move agreement is asserted with a score fallback since M5-F5.** Under the tuned
/// margins the two arms split on the promotion position, `d7c8r` reduced against
/// `d7c8q` unreduced, and the reason is a near-tie rather than a lost tactic. Measured
/// at depth 9 on that FEN (`experiments/MSN-M5-F5-spsa/promotion_tiebreak_probe.ps1`):
///
///   tuned,   capture LMR on  -> d7c8r, +463cp, 33,557 nodes
///   tuned,   capture LMR off -> d7c8q, +475cp, 52,788 nodes
///   untuned, capture LMR on  -> d7c8q, +482cp, 52,435 nodes
///   untuned, capture LMR off -> d7c8q, +444cp, 82,345 nodes
///
/// Two winning promotions on the same square 12cp apart, and the untuned arms agreed on
/// the queen while disagreeing about its value by 38cp — more than the gap that made
/// the tuned arms pick different moves. Demanding exact move equality here would pin
/// which side of a coin-flip the search lands on. The fallback keeps the property the
/// test exists for: where the arms disagree, the reduced one must not have given
/// anything up, and a genuinely dropped tactic moves the score by far more than a pawn.
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

        // The technique ships ON, so the DEFAULT arm here is the reduced one.
        let enabled_table = TranspositionTable::new(16).expect("test TT should allocate");
        let enabled = search_default(&position, &enabled_table, limits(9), network);

        let disabled_table = TranspositionTable::new(16).expect("test TT should allocate");
        let disabled = search(
            &position,
            &disabled_table,
            limits(9),
            SearchOptions {
                use_capture_lmr: false,
                ..SearchOptions::default()
            },
            network,
        );

        let enabled_move = enabled
            .best_move
            .map(|mv| format_uci_move(&position, mv, false));
        let disabled_move = disabled
            .best_move
            .map(|mv| format_uci_move(&position, mv, false));
        // Agreeing on the MOVE is the strong form and is what four of the five
        // positions still give. Where the two arms disagree, the property that
        // actually matters is that the reduction did not throw anything away, so the
        // fallback is that the scores agree to within a pawn. See the block comment
        // above for the M5-F5 measurement that made this necessary.
        if enabled_move != disabled_move {
            assert!(
                (enabled.score - disabled.score).abs() <= 100,
                "capture LMR changed the move played on {fen} AND moved the score by \
                 more than a pawn: {enabled_move:?} at {} vs {disabled_move:?} at {}",
                enabled.score,
                disabled.score
            );
        }
        total_enabled += enabled.nodes;
        total_disabled += disabled.nodes;
    }

    assert!(
        total_enabled < total_disabled,
        "capture LMR must save nodes across the tactical set: {total_enabled} with, \
         {total_disabled} without"
    );
}

/// Middlegames with plenty of late quiets, which is where LMR fail-highs live.
const POST_LMR_POSITIONS: [&str; 3] = [
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
    "2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1",
];

/// Both post-LMR toggles must reach the tree independently of each other.
///
/// This is the ablation anchor that makes the M3-F4 split real. The two sub-mechanisms
/// hang off the SAME event -- a reduced scout that beat alpha -- and were specified as
/// one package; they ship on different defaults because measured separately they move
/// the fixed-depth tree in opposite directions. That decision is only meaningful if
/// each toggle independently reaches a real search, which is what this pins.
#[test]
fn each_post_lmr_toggle_independently_reaches_the_tree() {
    let Some(network) = network() else {
        return;
    };
    for fen in POST_LMR_POSITIONS {
        let position = Position::from_fen(fen, false).expect("test FEN should parse");
        let shipped_table = TranspositionTable::new(16).expect("test TT should allocate");
        let shipped = search_default(&position, &shipped_table, limits(9), network);

        for flipped in [
            SearchOptions {
                use_post_lmr_depth: false,
                ..SearchOptions::default()
            },
            SearchOptions {
                use_post_lmr_conthist: true,
                ..SearchOptions::default()
            },
        ] {
            let table = TranspositionTable::new(16).expect("test TT should allocate");
            let result = search(&position, &table, limits(9), flipped, network);
            assert_ne!(
                shipped.nodes, result.nodes,
                "each post-LMR toggle must reach the search on {fen}"
            );
        }
    }
}

/// Neither post-LMR toggle may do anything while `UseLMR` is off.
///
/// Both sites sit behind `reduced_depth < child_depth`, which is unreachable when
/// nothing is reduced. This matters more than it looks: the continuation bonus writes
/// to a SHARED table that move ordering, the LMR statScore, and pruning history all
/// read, so a write leaking outside the LMR path would make `UseLMR=false` stop meaning
/// "no late-move reduction of any kind" -- the control every other selectivity delta in
/// `bench_cli.rs` is read against (mission AGENTS.md 4.4).
#[test]
fn disabling_lmr_disables_both_post_lmr_mechanisms() {
    let Some(network) = network() else {
        return;
    };
    for fen in POST_LMR_POSITIONS {
        let position = Position::from_fen(fen, false).expect("test FEN should parse");
        let lmr_off = SearchOptions {
            use_lmr: false,
            ..SearchOptions::default()
        };
        let baseline_table = TranspositionTable::new(16).expect("test TT should allocate");
        let baseline = search(&position, &baseline_table, limits(8), lmr_off, network);

        for flipped in [
            SearchOptions {
                use_post_lmr_depth: false,
                ..lmr_off
            },
            SearchOptions {
                use_post_lmr_conthist: true,
                ..lmr_off
            },
        ] {
            let table = TranspositionTable::new(16).expect("test TT should allocate");
            let result = search(&position, &table, limits(8), flipped, network);
            assert_eq!(
                baseline.nodes, result.nodes,
                "post-LMR handling must be inert while UseLMR is off on {fen}"
            );
        }
    }
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

/// The effort term must be invisible to every search that is not on a clock.
///
/// It scales the SOFT TIME LIMIT, so a `go depth`, a `go nodes`, and `bench` have
/// nothing for it to act on. That claim is worth an executable test rather than a
/// comment, because the term is computed unconditionally at the end of every iteration
/// -- it is only its CONSUMER that is time-gated. If that computation ever grew a side
/// effect on the tree, the shipped bench signature would move and the two consecutive
/// M3 features that ship OFF would stop being comparable to anything.
///
/// Trees are compared node-for-node rather than by best move: a bestmove match is
/// satisfiable by two different searches that happen to agree, while an identical node
/// count at several depths is not.
#[test]
fn the_time_effort_term_cannot_reach_a_fixed_depth_or_fixed_node_search() {
    let Some(network) = network() else {
        return;
    };
    const POSITIONS: [&str; 3] = [
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    ];

    for fen in POSITIONS {
        let position = Position::from_fen(fen, false).expect("test FEN should parse");
        for limits in [
            SearchLimits {
                depth: Some(8),
                ..SearchLimits::default()
            },
            SearchLimits {
                nodes: Some(60_000),
                ..SearchLimits::default()
            },
        ] {
            let enabled_table = TranspositionTable::new(16).expect("test TT should allocate");
            let enabled = search(
                &position,
                &enabled_table,
                limits,
                SearchOptions {
                    use_time_effort: true,
                    ..SearchOptions::default()
                },
                network,
            );

            let disabled_table = TranspositionTable::new(16).expect("test TT should allocate");
            let disabled = search(
                &position,
                &disabled_table,
                limits,
                SearchOptions {
                    use_time_effort: false,
                    ..SearchOptions::default()
                },
                network,
            );

            assert_eq!(
                enabled.nodes, disabled.nodes,
                "the effort term changed an untimed search on {fen} ({limits:?})"
            );
            assert_eq!(
                enabled.depth, disabled.depth,
                "the effort term changed the depth reached on {fen} ({limits:?})"
            );
            assert_eq!(
                enabled
                    .best_move
                    .map(|mv| format_uci_move(&position, mv, false)),
                disabled
                    .best_move
                    .map(|mv| format_uci_move(&position, mv, false)),
                "the effort term changed the move played on {fen} ({limits:?})"
            );
        }
    }
}

/// The effort term may move the soft limit; it may never move the HARD one.
///
/// The hard limit is the promise the engine does not forfeit on, and the whole reason
/// the soft/hard split exists (M7-F2 lost three games on time before it). A second
/// multiplicative factor on the soft side is exactly the change that could quietly
/// stretch the real bound, so the property is asserted against a live timed search
/// rather than argued from the composition rule alone.
#[test]
fn the_effort_term_leaves_the_hard_limit_of_a_timed_search_intact() {
    let Some(network) = network() else {
        return;
    };
    // Black has just captured on d4 and White must recapture; every other move hangs a
    // queen, so the rivals are refuted on the first null-window scout.
    let position = Position::from_fen(
        "r1bqkb1r/pppp1ppp/2n2n2/4p3/3PP3/5N2/PPP2PPP/RNBQKB1R w KQkq - 0 4",
        false,
    )
    .expect("test FEN should parse");
    let timed = SearchLimits {
        soft_time: Some(std::time::Duration::from_millis(400)),
        hard_time: Some(std::time::Duration::from_millis(4_000)),
        ..SearchLimits::default()
    };

    let enabled_table = TranspositionTable::new(32).expect("test TT should allocate");
    let enabled = search(
        &position,
        &enabled_table,
        timed,
        SearchOptions {
            use_time_effort: true,
            ..SearchOptions::default()
        },
        network,
    );
    let disabled_table = TranspositionTable::new(32).expect("test TT should allocate");
    let disabled = search(
        &position,
        &disabled_table,
        timed,
        SearchOptions {
            use_time_effort: false,
            ..SearchOptions::default()
        },
        network,
    );

    // Both must respect the hard limit, which is the promise that matters.
    for result in [&enabled, &disabled] {
        assert!(
            result.elapsed <= std::time::Duration::from_millis(5_000),
            "a timed search overran its hard limit: {:?}",
            result.elapsed
        );
        assert!(result.best_move.is_some(), "a timed search must answer");
    }
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
