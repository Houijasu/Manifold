//! Thread-local SEE call counters.
//!
//! Compiled only under the `instrumentation` feature so the production hot path keeps no
//! counter state at all. The counters are thread-local rather than atomic because the
//! exchange evaluation runs on per-worker search stacks: sharing one atomic set across Lazy
//! SMP workers would add contention to a hot helper and would blur per-thread behaviour into
//! an average that describes no worker.
//!
//! Cycle counts come from `rdtsc`, not `Instant::now`. A `QueryPerformanceCounter` pair
//! costs on the order of the region being measured here (one SEE call is tens of
//! nanoseconds), so a wall-clock timer would mostly measure itself.

use std::cell::Cell;

/// Snapshot of one thread's SEE counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeeCounters {
    /// Completed `static_exchange_evaluation` calls.
    pub calls: u64,
    /// Reference cycles spent inside those calls.
    pub cycles: u64,
}

impl SeeCounters {
    const ZERO: Self = Self {
        calls: 0,
        cycles: 0,
    };
}

thread_local! {
    static COUNTERS: Cell<SeeCounters> = const { Cell::new(SeeCounters::ZERO) };
}

/// Clears the calling thread's counters.
pub fn reset_see_counters() {
    COUNTERS.with(|counters| counters.set(SeeCounters::ZERO));
}

/// Reads the calling thread's counters.
#[must_use]
pub fn see_counters() -> SeeCounters {
    COUNTERS.with(Cell::get)
}

#[inline]
pub(crate) fn record_see(cycles: u64) {
    COUNTERS.with(|counters| {
        let mut current = counters.get();
        current.calls += 1;
        current.cycles += cycles;
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
    use super::{record_see, reset_see_counters, see_counters};

    #[test]
    fn see_counters_start_at_zero_accumulate_and_reset() {
        reset_see_counters();
        assert_eq!(see_counters(), super::SeeCounters::default());

        record_see(11);
        record_see(13);
        assert_eq!(see_counters().calls, 2);
        assert_eq!(see_counters().cycles, 24);

        reset_see_counters();
        assert_eq!(see_counters(), super::SeeCounters::default());
    }

    #[test]
    fn see_counters_are_thread_local() {
        reset_see_counters();
        record_see(7);

        let observed = std::thread::spawn(|| see_counters().calls)
            .join()
            .expect("counter thread should not panic");

        assert_eq!(observed, 0);
        assert_eq!(see_counters().calls, 1);
    }
}
