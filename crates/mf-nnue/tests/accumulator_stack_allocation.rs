use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use mf_core::{Position, parse_uci_move};
use mf_nnue::{AccumulatorStack, Network};

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
            "SKIPPED: NNUE accumulator-stack allocation test is missing {}",
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
fn real_push_null_push_and_pop_do_not_allocate_after_creation_and_lut_warmup() {
    let Some(network) = local_network() else {
        return;
    };
    let parent = Position::startpos();
    let mut child = parent.clone();
    let mv = parse_uci_move(&parent, "e2e4", false).expect("e2e4 should be legal");
    let undo = child.make_move(mv);
    let mut stack = AccumulatorStack::new(&network, &parent);

    stack
        .push_real(&child, mv, &undo)
        .expect("warmup real push should fit");
    stack.pop().expect("warmup pop should return to root");
    stack.push_null().expect("warmup null push should fit");
    stack.pop().expect("warmup null pop should return to root");

    ALLOCATIONS.with(|allocations| allocations.set(0));
    TRACKING.with(|tracking| tracking.set(true));
    stack
        .push_real(&child, mv, &undo)
        .expect("tracked real push should fit");
    stack.pop().expect("tracked real pop should return to root");
    stack.push_null().expect("tracked null push should fit");
    stack.pop().expect("tracked null pop should return to root");
    TRACKING.with(|tracking| tracking.set(false));

    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}
