//! Thread-local search event counters.
//!
//! This module is compiled only under the `instrumentation` feature so ordinary engine
//! builds contain neither the counter state nor updates in search hot paths.

use std::cell::Cell;

/// Snapshot of one thread's search counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchCounters {
    pub interior_nodes: u64,
    pub qsearch_nodes: u64,
    pub checked_interior_nodes: u64,
    pub interior_static_evals: u64,
    pub qsearch_static_evals: u64,
    pub tt_cutoffs: u64,
    pub razoring_attempts: u64,
    pub razoring_cutoffs: u64,
    pub rfp_attempts: u64,
    pub rfp_cutoffs: u64,
    pub nmp_attempts: u64,
    pub nmp_cutoffs: u64,
    pub probcut_attempts: u64,
    pub probcut_cutoffs: u64,
    pub lmp_attempts: u64,
    pub lmp_cutoffs: u64,
    pub futility_attempts: u64,
    pub futility_cutoffs: u64,
    pub history_pruning_attempts: u64,
    pub history_pruning_cutoffs: u64,
    pub see_pruning_attempts: u64,
    pub see_pruning_cutoffs: u64,
    pub lmr_reductions: u64,
    pub reduced_fail_highs: u64,
    pub full_depth_researches: u64,
}

thread_local! {
    static COUNTERS: Cell<SearchCounters> = const { Cell::new(SearchCounters {
        interior_nodes: 0,
        qsearch_nodes: 0,
        checked_interior_nodes: 0,
        interior_static_evals: 0,
        qsearch_static_evals: 0,
        tt_cutoffs: 0,
        razoring_attempts: 0,
        razoring_cutoffs: 0,
        rfp_attempts: 0,
        rfp_cutoffs: 0,
        nmp_attempts: 0,
        nmp_cutoffs: 0,
        probcut_attempts: 0,
        probcut_cutoffs: 0,
        lmp_attempts: 0,
        lmp_cutoffs: 0,
        futility_attempts: 0,
        futility_cutoffs: 0,
        history_pruning_attempts: 0,
        history_pruning_cutoffs: 0,
        see_pruning_attempts: 0,
        see_pruning_cutoffs: 0,
        lmr_reductions: 0,
        reduced_fail_highs: 0,
        full_depth_researches: 0,
    }) };
}

/// Clears the calling thread's search counters.
pub fn reset_search_counters() {
    COUNTERS.with(|counters| counters.set(SearchCounters::default()));
}

/// Reads the calling thread's search counters.
#[must_use]
pub fn search_counters() -> SearchCounters {
    COUNTERS.with(Cell::get)
}

#[inline]
pub(crate) fn record(update: impl FnOnce(&mut SearchCounters)) {
    COUNTERS.with(|counters| {
        let mut current = counters.get();
        update(&mut current);
        counters.set(current);
    });
}

#[cfg(test)]
mod tests {
    use super::{SearchCounters, record, reset_search_counters, search_counters};

    #[test]
    fn search_counters_start_at_zero_accumulate_and_reset() {
        reset_search_counters();
        assert_eq!(search_counters(), SearchCounters::default());

        record(|counters| {
            counters.interior_nodes += 2;
            counters.lmr_reductions += 1;
        });
        record(|counters| counters.interior_nodes += 3);
        assert_eq!(search_counters().interior_nodes, 5);
        assert_eq!(search_counters().lmr_reductions, 1);

        reset_search_counters();
        assert_eq!(search_counters(), SearchCounters::default());
    }

    #[test]
    fn search_counters_are_thread_local() {
        reset_search_counters();
        record(|counters| counters.interior_nodes += 1);

        let observed = std::thread::spawn(|| search_counters().interior_nodes)
            .join()
            .expect("counter thread should not panic");

        assert_eq!(observed, 0);
        assert_eq!(search_counters().interior_nodes, 1);
    }
}
