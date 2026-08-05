//! Thread-local NNUE incremental-update counters.
//!
//! Compiled only under the `instrumentation` feature so the production hot path keeps no
//! counter state at all. The counters are thread-local rather than atomic because the
//! accumulator stack is per-worker: sharing one atomic set across Lazy SMP workers would
//! add contention to the hottest loop in the engine and would blur per-thread behaviour
//! into an average that describes no worker.
//!
//! Cycle counts come from `rdtsc`, not `Instant::now`. A `QueryPerformanceCounter` pair
//! costs on the order of the region being measured here (a fused accumulator update is a
//! few hundred nanoseconds), so a wall-clock timer would mostly measure itself.

use std::cell::Cell;

/// Snapshot of one thread's NNUE update counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateCounters {
    /// Real (non-null) accumulator pushes.
    pub real_pushes: u64,
    /// Null-move pushes, which copy the parent state instead of updating it.
    pub null_pushes: u64,
    /// Forward passes evaluated from a stack state.
    pub forward_evaluations: u64,
    /// Perspective rebuilds forced by a king move.
    pub king_rebuilds: u64,
    /// Whole-state rebuilds forced by changed-threat buffer overflow.
    pub overflow_rebuilds: u64,
    /// Net physical FullThreats edge changes discovered across all real pushes.
    pub changed_threat_edges: u64,
    /// Slider candidates inspected during changed-threat discovery.
    pub sliders_scanned: u64,
    /// Reference cycles spent discovering changed threat edges.
    pub threat_discovery_cycles: u64,
    /// Reference cycles spent applying the accumulator update, including rebuilds.
    pub accumulator_update_cycles: u64,
    /// Reference cycles spent inside full rebuilds, a subset of `accumulator_update_cycles`.
    pub rebuild_cycles: u64,
    /// Reference cycles spent in the forward pass.
    pub forward_cycles: u64,
}

impl UpdateCounters {
    const ZERO: Self = Self {
        real_pushes: 0,
        null_pushes: 0,
        forward_evaluations: 0,
        king_rebuilds: 0,
        overflow_rebuilds: 0,
        changed_threat_edges: 0,
        sliders_scanned: 0,
        threat_discovery_cycles: 0,
        accumulator_update_cycles: 0,
        rebuild_cycles: 0,
        forward_cycles: 0,
    };
}

thread_local! {
    static COUNTERS: Cell<UpdateCounters> = const { Cell::new(UpdateCounters::ZERO) };
}

/// Clears the calling thread's counters.
pub fn reset_update_counters() {
    COUNTERS.with(|counters| counters.set(UpdateCounters::ZERO));
}

/// Reads the calling thread's counters.
#[must_use]
pub fn update_counters() -> UpdateCounters {
    COUNTERS.with(Cell::get)
}

#[inline]
pub(crate) fn record(update: impl FnOnce(&mut UpdateCounters)) {
    COUNTERS.with(|counters| {
        let mut current = counters.get();
        update(&mut current);
        counters.set(current);
    });
}

/// Reads a monotonically increasing cycle counter.
#[inline]
pub(crate) fn cycles() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `_rdtsc` is unconditionally available on x86_64 and reads a counter.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{record, reset_update_counters, update_counters};

    #[test]
    fn counters_start_at_zero_accumulate_and_reset() {
        reset_update_counters();
        assert_eq!(update_counters(), super::UpdateCounters::default());

        record(|counters| counters.real_pushes += 3);
        record(|counters| counters.king_rebuilds += 1);
        assert_eq!(update_counters().real_pushes, 3);
        assert_eq!(update_counters().king_rebuilds, 1);

        reset_update_counters();
        assert_eq!(update_counters(), super::UpdateCounters::default());
    }

    #[test]
    fn counters_are_thread_local() {
        reset_update_counters();
        record(|counters| counters.real_pushes += 7);

        let observed = std::thread::spawn(|| update_counters().real_pushes)
            .join()
            .expect("counter thread should not panic");

        assert_eq!(observed, 0);
        assert_eq!(update_counters().real_pushes, 7);
    }
}
