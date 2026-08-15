use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use mf_core::{Position, generate_legal_moves};
use mf_nnue::Network;
use mf_search::{
    Bound, EntryData, PoolError, SearchLimits, SearchOptions, SearchPool, TranspositionTable,
};

const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(10);

fn run_bounded<F>(scenario: F)
where
    F: FnOnce() + Send + 'static,
{
    let (outcome_tx, outcome_rx) = mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || {
        let _ = outcome_tx.send(catch_unwind(AssertUnwindSafe(scenario)));
    });

    match outcome_rx.recv_timeout(SCENARIO_TIMEOUT) {
        Ok(Ok(())) => handle.join().expect("bounded scenario thread should join"),
        Ok(Err(payload)) => {
            handle.join().expect("bounded scenario thread should join");
            resume_unwind(payload);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            std::mem::forget(handle);
            panic!("pool scenario exceeded {SCENARIO_TIMEOUT:?}");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            handle.join().expect("bounded scenario thread should join");
            panic!("bounded scenario channel disconnected");
        }
    }
}

fn depth_limits(depth: u32) -> SearchLimits {
    SearchLimits {
        depth: Some(depth),
        nodes: None,
        soft_time: None,
        hard_time: None,
        infinite: false,
        use_clock_management: false,
    }
}

fn infinite_limits() -> SearchLimits {
    SearchLimits {
        depth: None,
        nodes: None,
        soft_time: None,
        hard_time: None,
        infinite: true,
        use_clock_management: false,
    }
}

fn history(position: &Position) -> Vec<u64> {
    vec![position.repetition_key()]
}

fn local_network() -> Option<Arc<Network>> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
    if !path.is_file() {
        eprintln!("SKIPPED: NNUE SMP tests are missing {}", path.display());
        return None;
    }
    Some(Arc::new(Network::load(&path).unwrap_or_else(|error| {
        panic!("test NNUE network {}: {error}", path.display())
    })))
}

fn assert_legal_result(position: &Position, result: &mf_search::SearchResult) {
    let legal_moves = generate_legal_moves(position);
    let best_move = result.best_move.expect("search should return a move");
    assert!(legal_moves.contains(&best_move));
    assert_eq!(result.pv.first(), Some(&best_move));

    let mut replay = position.clone();
    for &mv in &result.pv {
        assert!(
            generate_legal_moves(&replay).contains(&mv),
            "PV move {mv:?} must be legal"
        );
        replay.make_move(mv);
    }
}

#[test]
fn pool_reports_its_persistent_worker_count() {
    run_bounded(|| {
        let pool = SearchPool::new(4).expect("worker pool should start");

        assert_eq!(pool.thread_count(), 4);
        assert!(SearchPool::new(0).is_err(), "zero workers must be rejected");
    });
}

#[test]
fn fixed_depth_public_path_uses_worker_zero_from_a_larger_pool() {
    run_bounded(|| {
        let Some(network) = local_network() else {
            return;
        };
        let pool = SearchPool::new(4).expect("worker pool should start");
        let baseline_pool = SearchPool::new(1).expect("baseline worker should start");
        let position = Position::startpos();
        let table = Arc::new(TranspositionTable::new(4).expect("test TT should allocate"));
        let baseline_table =
            Arc::new(TranspositionTable::new(4).expect("baseline TT should allocate"));
        let stop = Arc::new(AtomicBool::new(false));
        let mut callback_depths = Vec::new();

        let pooled = pool
            .search_fixed_depth_with_history_callback(
                &position,
                &history(&position),
                table,
                depth_limits(4),
                SearchOptions::default(),
                Arc::clone(&stop),
                Arc::clone(&network),
                None,
                None,
                |iteration| callback_depths.push(iteration.depth),
            )
            .expect("worker-zero search should complete");
        let baseline = baseline_pool
            .search_fixed_depth_with_history_callback(
                &position,
                &history(&position),
                baseline_table,
                depth_limits(4),
                SearchOptions::default(),
                Arc::new(AtomicBool::new(false)),
                Arc::clone(&network),
                None,
                None,
                |_| {},
            )
            .expect("baseline search should complete");

        assert_eq!(pooled.selected_worker, 0);
        assert!(
            !stop.load(Ordering::Relaxed),
            "worker-zero-only search must not publish a shared helper stop"
        );
        assert_eq!(pooled.result.best_move, baseline.result.best_move);
        assert_eq!(pooled.result.score, baseline.result.score);
        assert_eq!(pooled.result.depth, baseline.result.depth);
        assert_eq!(pooled.result.nodes, baseline.result.nodes);
        assert_eq!(pooled.result.pv, baseline.result.pv);
        assert_eq!(
            callback_depths,
            pooled
                .result
                .iterations
                .iter()
                .map(|iteration| iteration.depth)
                .collect::<Vec<_>>()
        );
        assert_legal_result(&position, &pooled.result);
    });
}

#[test]
fn explicit_fixed_depth_smp_path_returns_a_legal_result() {
    run_bounded(|| {
        let Some(network) = local_network() else {
            return;
        };
        let pool = SearchPool::new(4).expect("worker pool should start");
        let position = Position::from_fen(KIWIPETE, false).expect("test FEN should parse");
        let table = Arc::new(TranspositionTable::new(8).expect("test TT should allocate"));
        let stop = Arc::new(AtomicBool::new(false));

        let pooled = pool
            .search_fixed_depth_smp_with_history_callback(
                &position,
                &history(&position),
                table,
                depth_limits(4),
                SearchOptions::default(),
                Arc::clone(&stop),
                Arc::clone(&network),
                None,
                None,
                |_| {},
            )
            .expect("SMP search should complete");

        assert!(pooled.selected_worker < 4);
        assert!(stop.load(Ordering::Relaxed));
        assert_legal_result(&position, &pooled.result);
    });
}

#[test]
fn nnue_fixed_depth_is_identical_across_pool_sizes() {
    run_bounded(|| {
        let one = SearchPool::new(1).expect("single worker pool should start");
        let eight = SearchPool::new(8).expect("eight worker pool should start");
        let position = Position::from_fen(KIWIPETE, false).expect("test FEN should parse");
        let Some(network) = local_network() else {
            return;
        };

        let one_result = one
            .search_fixed_depth_with_history_callback(
                &position,
                &history(&position),
                Arc::new(TranspositionTable::new(8).expect("test TT should allocate")),
                depth_limits(4),
                SearchOptions::default(),
                Arc::new(AtomicBool::new(false)),
                Arc::clone(&network),
                None,
                None,
                |_| {},
            )
            .expect("single worker NNUE search should complete");
        let eight_result = eight
            .search_fixed_depth_with_history_callback(
                &position,
                &history(&position),
                Arc::new(TranspositionTable::new(8).expect("test TT should allocate")),
                depth_limits(4),
                SearchOptions::default(),
                Arc::new(AtomicBool::new(false)),
                network,
                None,
                None,
                |_| {},
            )
            .expect("eight worker NNUE search should complete");

        assert_eq!(one_result.selected_worker, 0);
        assert_eq!(eight_result.selected_worker, 0);
        assert_eq!(one_result.result.score, eight_result.result.score);
        assert_eq!(one_result.result.nodes, eight_result.result.nodes);
        assert_eq!(one_result.result.pv, eight_result.result.pv);
        assert_legal_result(&position, &eight_result.result);
    });
}

#[test]
fn nnue_fixed_depth_smp_uses_a_network_on_every_worker() {
    run_bounded(|| {
        let Some(network) = local_network() else {
            return;
        };
        let pool = SearchPool::new(4).expect("worker pool should start");
        let position = Position::from_fen(KIWIPETE, false).expect("test FEN should parse");
        let pooled = pool
            .search_fixed_depth_smp_with_history_callback(
                &position,
                &history(&position),
                Arc::new(TranspositionTable::new(8).expect("test TT should allocate")),
                depth_limits(3),
                SearchOptions::default(),
                Arc::new(AtomicBool::new(false)),
                network,
                None,
                None,
                |_| {},
            )
            .expect("SMP NNUE search should complete");

        assert!(pooled.selected_worker < 4);
        assert_legal_result(&position, &pooled.result);
    });
}

#[test]
fn external_stop_ends_an_infinite_smp_search() {
    let Some(network) = local_network() else {
        return;
    };
    let pool = Arc::new(SearchPool::new(4).expect("worker pool should start"));
    let position = Position::startpos();
    let table = Arc::new(TranspositionTable::new(8).expect("test TT should allocate"));
    let stop = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (result_tx, result_rx) = mpsc::sync_channel(1);

    let search_pool = Arc::clone(&pool);
    let search_stop = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let result = search_pool.search_with_history_callback(
            &position,
            &history(&position),
            table,
            infinite_limits(),
            SearchOptions::default(),
            search_stop,
            Arc::clone(&network),
            None,
            None,
            |_| {
                let _ = started_tx.try_send(());
            },
        );
        let _ = result_tx.send(result);
    });

    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("infinite search should emit progress");
    stop.store(true, Ordering::Relaxed);
    let pooled = result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("external stop should finish every worker")
        .expect("stopped search should still return a result");
    handle.join().expect("search thread should not panic");

    assert_legal_result(&Position::startpos(), &pooled.result);
}

#[test]
fn callback_panic_drains_workers_before_pool_reuse_and_drop() {
    run_bounded(|| {
        let Some(network) = local_network() else {
            return;
        };
        let pool = SearchPool::new(4).expect("worker pool should start");
        let position = Position::startpos();
        let table = Arc::new(TranspositionTable::new(8).expect("test TT should allocate"));
        let stop = Arc::new(AtomicBool::new(false));
        let mut callback_count = 0;

        let callback_panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = pool.search_with_history_callback(
                &position,
                &history(&position),
                Arc::clone(&table),
                infinite_limits(),
                SearchOptions::default(),
                stop,
                Arc::clone(&network),
                None,
                None,
                |_| {
                    callback_count += 1;
                    panic!("callback failure");
                },
            );
        }));
        assert!(callback_panic.is_err());
        assert_eq!(callback_count, 1, "later callbacks must be suppressed");

        pool.clear(Arc::clone(&table))
            .expect("pool should be reusable after callback panic");
        let pooled = pool
            .search_fixed_depth_with_history_callback(
                &position,
                &history(&position),
                table,
                depth_limits(2),
                SearchOptions::default(),
                Arc::new(AtomicBool::new(false)),
                Arc::clone(&network),
                None,
                None,
                |_| {},
            )
            .expect("search after callback panic should complete");
        assert_legal_result(&position, &pooled.result);
        drop(pool);
    });
}

#[test]
fn infinite_search_ignores_embedded_limits_until_external_stop() {
    let Some(network) = local_network() else {
        return;
    };
    let pool = Arc::new(SearchPool::new(4).expect("worker pool should start"));
    let position = Position::startpos();
    let table = Arc::new(TranspositionTable::new(8).expect("test TT should allocate"));
    let stop = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (result_tx, result_rx) = mpsc::sync_channel(1);

    let search_pool = Arc::clone(&pool);
    let search_stop = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let result = search_pool.search_with_history_callback(
            &position,
            &history(&position),
            table,
            SearchLimits {
                depth: Some(1),
                nodes: Some(1),
                soft_time: Some(Duration::from_millis(1)),
                hard_time: Some(Duration::from_millis(1)),
                infinite: true,
                use_clock_management: false,
            },
            SearchOptions::default(),
            search_stop,
            Arc::clone(&network),
            None,
            None,
            |_| {
                let _ = started_tx.try_send(());
            },
        );
        let _ = result_tx.send(result);
    });

    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("infinite search should complete an iteration");
    match result_rx.recv_timeout(Duration::from_millis(250)) {
        Ok(_) => {
            handle.join().expect("search thread should not panic");
            panic!("depth, node, and clock limits must not end an infinite search");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            handle.join().expect("search thread should not panic");
            panic!("search result channel disconnected");
        }
    }

    stop.store(true, Ordering::Relaxed);
    result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("external stop should finish the infinite search")
        .expect("stopped search should return normally");
    handle.join().expect("search thread should not panic");
}

#[test]
fn concurrent_pool_calls_are_rejected_as_busy() {
    run_bounded(|| {
        let Some(network) = local_network() else {
            return;
        };
        let pool = Arc::new(SearchPool::new(4).expect("worker pool should start"));
        let position = Position::startpos();
        let history = history(&position);
        let table = Arc::new(TranspositionTable::new(8).expect("test TT should allocate"));
        let stop = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);

        let search_pool = Arc::clone(&pool);
        let search_position = position.clone();
        let search_history = history.clone();
        let search_table = Arc::clone(&table);
        let search_stop = Arc::clone(&stop);
        let search_network = Arc::clone(&network);
        let handle = std::thread::spawn(move || {
            let result = search_pool.search_with_history_callback(
                &search_position,
                &search_history,
                search_table,
                infinite_limits(),
                SearchOptions::default(),
                search_stop,
                search_network,
                None,
                None,
                |_| {
                    let _ = started_tx.try_send(());
                },
            );
            let _ = result_tx.send(result);
        });

        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first search should become active");
        assert!(matches!(
            pool.clear(Arc::clone(&table)),
            Err(PoolError::Busy)
        ));
        let second = pool.search_fixed_depth_with_history_callback(
            &position,
            &history,
            table,
            depth_limits(1),
            SearchOptions::default(),
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&network),
            None,
            None,
            |_| {},
        );
        assert!(matches!(second, Err(PoolError::Busy)));

        stop.store(true, Ordering::Relaxed);
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first search should stop")
            .expect("first search should return normally");
        handle.join().expect("search thread should not panic");
    });
}

#[test]
fn parallel_clear_removes_shared_table_entries() {
    run_bounded(|| {
        let pool = SearchPool::new(4).expect("worker pool should start");
        let table = Arc::new(TranspositionTable::new(4).expect("test TT should allocate"));
        let keys = [0x0123_4567_89ab_cdef, 0x5555_aaaa_ffff_0000, u64::MAX];
        for (index, &key) in keys.iter().enumerate() {
            table.store(
                key,
                EntryData {
                    best_move: None,
                    score: index as i16,
                    static_eval: 0,
                    depth: 6,
                    bound: Bound::Exact,
                    age: 3,
                    pv: false,
                },
            );
            assert!(table.probe(key).is_some());
        }

        pool.clear(Arc::clone(&table))
            .expect("parallel clear should complete");

        for key in keys {
            assert!(table.probe(key).is_none());
        }
    });
}

#[test]
fn eight_thread_pool_survives_representative_position_stress() {
    run_bounded(|| {
        let Some(network) = local_network() else {
            return;
        };
        let pool = SearchPool::new(8).expect("worker pool should start");
        let table = Arc::new(TranspositionTable::new(16).expect("test TT should allocate"));
        let positions = [
            Position::startpos(),
            Position::from_fen(KIWIPETE, false).expect("test FEN should parse"),
            Position::from_fen("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", false)
                .expect("test FEN should parse"),
            Position::from_fen(
                "rnbq1k1r/pp1Pbppp/2p2n2/8/2B5/8/PPP1NPPP/RNBQK2R b KQ - 1 8",
                false,
            )
            .expect("test FEN should parse"),
        ];

        for _ in 0..2 {
            for position in &positions {
                let pooled = pool
                    .search_fixed_depth_smp_with_history_callback(
                        position,
                        &history(position),
                        Arc::clone(&table),
                        depth_limits(3),
                        SearchOptions::default(),
                        Arc::new(AtomicBool::new(false)),
                        Arc::clone(&network),
                        None,
                        None,
                        |_| {},
                    )
                    .expect("stress search should complete");
                assert_legal_result(position, &pooled.result);
            }
        }
    });
}
