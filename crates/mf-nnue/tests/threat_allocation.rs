use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mf_core::{Color, Position};
use mf_nnue::threats::{self, MAX_ACTIVE};

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

#[test]
fn first_active_threat_enumeration_does_not_allocate() {
    let position = Position::startpos();
    let mut buffer = [0; MAX_ACTIVE];

    ALLOCATIONS.with(|allocations| allocations.set(0));
    TRACKING.with(|tracking| tracking.set(true));
    let count = threats::append_active_threats(Color::White, &position, &mut buffer);
    TRACKING.with(|tracking| tracking.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);

    assert!(count <= 128);
    assert_eq!(allocations, 0);
}
