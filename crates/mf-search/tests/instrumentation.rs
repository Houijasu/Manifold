#![cfg(feature = "instrumentation")]

use std::path::PathBuf;

use mf_core::Position;
use mf_nnue::Network;
use mf_search::{
    SearchCounters, SearchLimits, SearchOptions, TranspositionTable, reset_search_counters, search,
    search_counters,
};

fn network() -> Option<Network> {
    let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
    let path = explicit_path.clone().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    if !path.is_file() {
        assert!(
            explicit_path.is_none(),
            "MF_NNUE_TEST_NET requires an existing network file: {}",
            path.display()
        );
        eprintln!("SKIPPED: search instrumentation needs {}", path.display());
        return None;
    }
    Some(
        Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}

#[test]
fn instrumentation_counts_a_search_and_resets_between_searches() {
    let Some(network) = network() else {
        return;
    };
    let position = Position::startpos();

    reset_search_counters();
    let table = TranspositionTable::new(16).expect("test TT should allocate");
    search(
        &position,
        &table,
        SearchLimits {
            depth: Some(7),
            ..SearchLimits::default()
        },
        SearchOptions::default(),
        &network,
    );
    let counters = search_counters();
    assert!(counters.interior_nodes > 0);
    assert!(counters.qsearch_nodes > 0);
    assert!(counters.interior_static_evals > 0);
    assert!(counters.lmr_reductions > 0);
    assert!(counters.checked_interior_nodes <= counters.interior_nodes);
    assert!(counters.razoring_cutoffs <= counters.razoring_attempts);

    reset_search_counters();
    assert_eq!(search_counters(), SearchCounters::default());

    let table = TranspositionTable::new(16).expect("test TT should allocate");
    search(
        &position,
        &table,
        SearchLimits {
            depth: Some(2),
            ..SearchLimits::default()
        },
        SearchOptions::default(),
        &network,
    );
    assert!(search_counters().interior_nodes > 0);
}
