//! Thread-local search event counters.
//!
//! This module is compiled only under the `instrumentation` feature so ordinary engine
//! builds contain neither the counter state nor updates in search hot paths.

use std::cell::Cell;

/// Snapshot of one thread's search counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchCounters {
    /// Calls entering principal-variation search, including stopped calls.
    pub interior_nodes: u64,
    /// Calls entering quiescence, including its uncounted transition calls.
    pub qsearch_nodes: u64,
    /// Interior nodes that reached and observed the existing in-check test.
    pub checked_interior_nodes: u64,
    /// Fresh NNUE forwards performed AT checked interior nodes
    /// (`UseCheckedNodeEval` on). Checked nodes that reused a TT static eval are
    /// counted in neither this nor `checked_node_evals_skipped`, so
    /// `checked_node_static_evals + checked_node_evals_skipped <=
    /// checked_interior_nodes`.
    pub checked_node_static_evals: u64,
    /// Checked interior nodes that skipped evaluation (`UseCheckedNodeEval` off).
    /// Equals `checked_interior_nodes` in that mode.
    pub checked_node_evals_skipped: u64,
    /// NNUE evaluations actually requested by principal-variation search.
    pub interior_static_evals: u64,
    /// NNUE evaluations actually requested by quiescence.
    pub qsearch_static_evals: u64,
    /// Returns driven by a usable transposition-table bound.
    pub tt_cutoffs: u64,
    /// Razoring margin decisions evaluated after their eligibility gates.
    pub razoring_attempts: u64,
    /// Razoring decisions that returned into quiescence.
    pub razoring_cutoffs: u64,
    /// Reverse-futility margin decisions evaluated after their eligibility gates.
    pub rfp_attempts: u64,
    /// Reverse-futility decisions that returned immediately.
    pub rfp_cutoffs: u64,
    /// Null-move eval preconditions evaluated after their eligibility gates.
    pub nmp_attempts: u64,
    /// Null-move searches that produced an accepted cutoff.
    pub nmp_cutoffs: u64,
    /// Eligible ProbCut nodes that entered the existing attempt path.
    pub probcut_attempts: u64,
    /// ProbCut searches that returned a cutoff value.
    pub probcut_cutoffs: u64,
    /// Late-move-pruning thresholds actually evaluated.
    pub lmp_attempts: u64,
    /// Late moves skipped by the LMP decision.
    pub lmp_cutoffs: u64,
    /// Frontier-futility values actually evaluated.
    pub futility_attempts: u64,
    /// Moves skipped by the frontier-futility decision.
    pub futility_cutoffs: u64,
    /// History-pruning scores actually evaluated.
    pub history_pruning_attempts: u64,
    /// Moves skipped by the history-pruning decision.
    pub history_pruning_cutoffs: u64,
    /// Main-search SEE pruning comparisons actually evaluated.
    pub see_pruning_attempts: u64,
    /// Moves skipped by the main-search SEE decision.
    pub see_pruning_cutoffs: u64,
    /// Scout searches launched below their original child depth.
    pub lmr_reductions: u64,
    /// Reduced scout results that exceeded alpha.
    pub reduced_fail_highs: u64,
    /// Searches of an LMR-reduced move launched at its original child depth.
    pub full_depth_researches: u64,
    /// SEE calls issued from `load_captures` capture staging: one per generated
    /// capture in the eager variants (full and ProbCut), and one per capture in the
    /// lazy qsearch variant only when the gate threshold cannot classify the
    /// good/bad split by itself (a below-zero threshold, i.e. the
    /// `UseSEEPruning=false` ablation).
    pub see_calls_load_captures: u64,
    /// SEE calls issued from TT-move validation of a capture-family move.
    pub see_calls_tt_validation: u64,
    /// SEE calls issued from the interior SEE-pruning gate's quiets fallback, where no
    /// memoized capture SEE exists and the exchange is walked fresh.
    pub see_calls_interior_quiets_fallback: u64,
    /// SEE calls issued from the qsearch quiet-checks gate.
    pub see_calls_quiet_checks: u64,
    /// SEE calls issued from the lazy qsearch picker's yield-time capture gate, one
    /// per candidate the picker actually reaches before the stage exhausts or the
    /// search cuts off.
    pub see_calls_qsearch_yield_gate: u64,
}

thread_local! {
    static COUNTERS: Cell<SearchCounters> = const { Cell::new(SearchCounters {
        interior_nodes: 0,
        qsearch_nodes: 0,
        checked_interior_nodes: 0,
        checked_node_static_evals: 0,
        checked_node_evals_skipped: 0,
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
        see_calls_load_captures: 0,
        see_calls_tt_validation: 0,
        see_calls_interior_quiets_fallback: 0,
        see_calls_quiet_checks: 0,
        see_calls_qsearch_yield_gate: 0,
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

#[inline]
pub(crate) fn record_full_depth_research(reduced_depth: i32, search_depth: i32, child_depth: i32) {
    if reduced_depth < child_depth && search_depth == child_depth {
        record(|counters| counters.full_depth_researches += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SearchCounters, record, record_full_depth_research, reset_search_counters, search_counters,
    };

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

    #[test]
    fn full_depth_research_counts_only_reduced_moves_searched_at_child_depth() {
        reset_search_counters();

        record_full_depth_research(6, 7, 8);
        record_full_depth_research(6, 9, 8);
        record_full_depth_research(8, 8, 8);
        assert_eq!(search_counters().full_depth_researches, 0);

        record_full_depth_research(6, 8, 8);
        assert_eq!(search_counters().full_depth_researches, 1);
    }
}
