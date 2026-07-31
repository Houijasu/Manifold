use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use mf_core::Position;
use mf_nnue::{AccumulatorState, ForwardMode, L1, Network, SimdBackend};

struct CountingAllocator;

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

fn record_allocation() {
    TRACKING.with(|tracking| {
        if tracking.get() {
            ALLOCATIONS.with(|allocations| allocations.set(allocations.get() + 1));
        }
    });
}

fn local_network() -> Option<Network> {
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
        eprintln!(
            "SKIPPED: NNUE evaluation allocation test is missing {}",
            path.display()
        );
        return None;
    }
    Some(
        Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}

#[test]
fn scalar_evaluation_and_dump_do_not_allocate_after_network_load() {
    let Some(network) = local_network() else {
        return;
    };
    let position = Position::startpos();
    let mut transformed = [0; L1];

    std::hint::black_box(network.evaluate(&position));
    std::hint::black_box(network.dump_features(&position, &mut transformed));

    ALLOCATIONS.with(|allocations| allocations.set(0));
    TRACKING.with(|tracking| tracking.set(true));
    std::hint::black_box(network.evaluate(&position));
    std::hint::black_box(network.dump_features(&position, &mut transformed));
    TRACKING.with(|tracking| tracking.set(false));

    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn supplied_state_forward_modes_do_not_allocate_after_network_load() {
    let Some(network) = local_network() else {
        return;
    };
    let position = Position::startpos();
    let state = AccumulatorState::from_position(&network, &position);
    let mut transformed = [0; L1];

    for backend in [
        SimdBackend::Scalar,
        SimdBackend::Avx2,
        SimdBackend::Avx2Vnni,
    ] {
        if !backend.is_supported() {
            continue;
        }
        let sparse_choices: &[bool] = if backend == SimdBackend::Scalar {
            &[false]
        } else {
            &[false, true]
        };
        for &sparse_fc0 in sparse_choices {
            let mode = ForwardMode::new(backend, sparse_fc0).expect("backend is supported");
            std::hint::black_box(network.evaluate_from_state_with_mode(&position, &state, mode));
            std::hint::black_box(network.dump_features_from_state_with_mode(
                &position,
                &state,
                &mut transformed,
                mode,
            ));

            ALLOCATIONS.with(|allocations| allocations.set(0));
            TRACKING.with(|tracking| tracking.set(true));
            std::hint::black_box(network.evaluate_from_state_with_mode(&position, &state, mode));
            std::hint::black_box(network.dump_features_from_state_with_mode(
                &position,
                &state,
                &mut transformed,
                mode,
            ));
            TRACKING.with(|tracking| tracking.set(false));

            assert_eq!(
                ALLOCATIONS.with(Cell::get),
                0,
                "{mode:?} supplied-state forward allocated"
            );
        }
    }
}
