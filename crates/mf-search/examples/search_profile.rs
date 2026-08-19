//! Prints search, SEE, and NNUE event counters for the six built-in bench positions.
//!
//! Usage: `search_profile [depth]` (default 7, matching `manifold bench`).

use std::path::PathBuf;
use std::time::Instant;

use mf_core::{Position, reset_see_counters, see_counters};
use mf_nnue::{Network, reset_update_counters, update_counters};
use mf_search::{
    SearchLimits, SearchOptions, SharedHistory, TranspositionTable, reset_search_counters,
    search_counters, search_with_shared_history,
};

const DEFAULT_DEPTH: u32 = 7;
const HASH_MIB: usize = 16;

const BENCH_CASES: [&str; 6] = [
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
    "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
];

fn main() {
    let depth = std::env::args().nth(1).map_or(DEFAULT_DEPTH, |value| {
        value
            .parse::<u32>()
            .ok()
            .filter(|&depth| depth > 0)
            .expect("depth must be a positive integer")
    });
    assert!(
        std::env::args().nth(2).is_none(),
        "usage: search_profile [depth]"
    );

    let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    let network = Network::load(&path)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", path.display()));
    let table = TranspositionTable::new(HASH_MIB).expect("profile Hash should allocate");
    let history = SharedHistory::new();

    for (index, fen) in BENCH_CASES.iter().enumerate() {
        let position = Position::from_fen(fen, false).expect("profile FEN should parse");
        table.clear();
        history.clear();
        reset_search_counters();
        reset_see_counters();
        reset_update_counters();

        let started = Instant::now();
        let result = search_with_shared_history(
            &position,
            &table,
            SearchLimits {
                depth: Some(depth),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &history,
            &network,
            None,
            None,
        );
        let elapsed = started.elapsed();
        let nps = ((u128::from(result.nodes) * 1_000_000_000) / elapsed.as_nanos().max(1)) as u64;
        let search = search_counters();
        let see = see_counters();
        let update = update_counters();

        println!(
            "position=bench{} depth={depth} nodes={} \
             interior_nodes={} qsearch_nodes={} checked_interior_nodes={} \
             checked_node_static_evals={} checked_node_evals_skipped={} \
             interior_static_evals={} qsearch_static_evals={} tt_cutoffs={} \
             razoring_attempts={} razoring_cutoffs={} rfp_attempts={} rfp_cutoffs={} \
             nmp_attempts={} nmp_cutoffs={} probcut_attempts={} probcut_cutoffs={} \
             lmp_attempts={} lmp_cutoffs={} futility_attempts={} futility_cutoffs={} \
             history_pruning_attempts={} history_pruning_cutoffs={} \
             see_pruning_attempts={} see_pruning_cutoffs={} lmr_reductions={} \
             reduced_fail_highs={} full_depth_researches={} \
             see_calls={} see_cycles={} \
             update_real_pushes={} update_null_pushes={} update_forward_evaluations={} \
             update_king_rebuilds={} update_overflow_rebuilds={} \
             update_changed_threat_edges={} update_sliders_scanned={} \
             update_threat_discovery_cycles={} update_accumulator_update_cycles={} \
             update_rebuild_cycles={} update_finny_king_updates={} \
             update_finny_threat_rebuilds={} update_finny_cycles={} \
             update_finny_refreshes={} update_finny_delta_rows={} \
             update_forward_cycles={} update_deferred_pushes_skipped={} nps={nps}",
            index + 1,
            result.nodes,
            search.interior_nodes,
            search.qsearch_nodes,
            search.checked_interior_nodes,
            search.checked_node_static_evals,
            search.checked_node_evals_skipped,
            search.interior_static_evals,
            search.qsearch_static_evals,
            search.tt_cutoffs,
            search.razoring_attempts,
            search.razoring_cutoffs,
            search.rfp_attempts,
            search.rfp_cutoffs,
            search.nmp_attempts,
            search.nmp_cutoffs,
            search.probcut_attempts,
            search.probcut_cutoffs,
            search.lmp_attempts,
            search.lmp_cutoffs,
            search.futility_attempts,
            search.futility_cutoffs,
            search.history_pruning_attempts,
            search.history_pruning_cutoffs,
            search.see_pruning_attempts,
            search.see_pruning_cutoffs,
            search.lmr_reductions,
            search.reduced_fail_highs,
            search.full_depth_researches,
            see.calls,
            see.cycles,
            update.real_pushes,
            update.null_pushes,
            update.forward_evaluations,
            update.king_rebuilds,
            update.overflow_rebuilds,
            update.changed_threat_edges,
            update.sliders_scanned,
            update.threat_discovery_cycles,
            update.accumulator_update_cycles,
            update.rebuild_cycles,
            update.finny_king_updates,
            update.finny_threat_rebuilds,
            update.finny_cycles,
            update.finny_refreshes,
            update.finny_delta_rows,
            update.forward_cycles,
            update.deferred_pushes_skipped,
        );
    }
}
