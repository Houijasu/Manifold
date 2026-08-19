#![cfg(feature = "instrumentation")]

use std::path::PathBuf;

use mf_core::Position;
use mf_nnue::Network;
use mf_search::{
    SearchCounters, SearchLimits, SearchOptions, TranspositionTable, reset_search_counters, search,
    search_counters,
};

fn network() -> Network {
    let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
    let path = explicit_path.clone().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    if !path.is_file() {
        if explicit_path.is_some() {
            panic!(
                "MF_NNUE_TEST_NET requires an existing network file: {}",
                path.display()
            );
        }
        panic!(
            "search instrumentation requires an NNUE fixture; set MF_NNUE_TEST_NET or provision {}",
            path.display()
        );
    }
    Network::load(&path)
        .unwrap_or_else(|error| panic!("failed to load NNUE network {}: {error}", path.display()))
}

#[test]
fn instrumentation_counts_a_search_and_resets_between_searches() {
    let network = network();
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

/// Kiwipete is the tactical standard position: a depth-7 search from it visits plenty
/// of checked interior nodes, so the checked-node counters have something to count.
const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";

fn search_kiwipete(options: SearchOptions, network: &Network) -> SearchCounters {
    let position = Position::from_fen(KIWIPETE, false).expect("kiwipete should parse");
    reset_search_counters();
    let table = TranspositionTable::new(16).expect("test TT should allocate");
    search(
        &position,
        &table,
        SearchLimits {
            depth: Some(7),
            ..SearchLimits::default()
        },
        options,
        network,
    );
    search_counters()
}

/// With `use_checked_node_eval` OFF every checked interior node must skip evaluation
/// entirely: no fresh checked forwards, and exactly one skip per checked node.
#[test]
fn checked_node_eval_off_skips_every_checked_node_forward() {
    let network = network();
    let counters = search_kiwipete(
        SearchOptions {
            use_checked_node_eval: false,
            ..SearchOptions::default()
        },
        &network,
    );

    assert!(
        counters.checked_interior_nodes > 0,
        "kiwipete at depth 7 must visit checked interior nodes"
    );
    assert_eq!(
        counters.checked_node_static_evals, 0,
        "no checked node may perform a fresh forward with the toggle off"
    );
    assert_eq!(
        counters.checked_node_evals_skipped, counters.checked_interior_nodes,
        "with the toggle off every checked node skips evaluation"
    );
}

/// With the toggle ON the fresh checked forwards are counted and the skip counter
/// stays at zero. The gap between `checked_interior_nodes` and
/// `checked_node_static_evals` is checked nodes that reused a TT static eval.
#[test]
fn checked_node_eval_on_counts_fresh_checked_forwards() {
    let network = network();
    let counters = search_kiwipete(SearchOptions::default(), &network);

    assert!(
        counters.checked_interior_nodes > 0,
        "kiwipete at depth 7 must visit checked interior nodes"
    );
    assert_eq!(counters.checked_node_evals_skipped, 0);
    assert!(
        counters.checked_node_static_evals > 0,
        "a fresh-TT tactical search must evaluate some checked nodes"
    );
    assert!(
        counters.checked_node_static_evals <= counters.checked_interior_nodes,
        "checked forwards are a subset of checked nodes"
    );
}
