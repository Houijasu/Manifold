//! Alpha-beta search, transposition tables, and time management.

mod evaluation;
mod move_ordering;
mod repetition;
mod search;
mod transposition_table;

pub use evaluation::{
    DEFAULT_PARAMETERS, EvaluationParameters, TaperedScore, evaluate, evaluate_with_parameters,
};
pub use search::{
    IterationInfo, MATE_SCORE, MAX_SEARCH_PLY, SearchLimits, SearchOptions, SearchResult,
    UNEVALUATED_STATIC_EVAL, clamp_centipawn_score, is_mate_score, score_to_uci_mate, search,
    search_with_callback, search_with_callback_options, search_with_history,
    search_with_history_callback, search_with_history_callback_options,
    search_with_history_options, search_with_options,
};
pub use transposition_table::{
    AllocationError, Bound, CACHE_LINE_BYTES, CLUSTER_ALIGNMENT, CLUSTER_BYTES,
    ENTRIES_PER_CLUSTER, ENTRY_BYTES, EntryData, TranspositionTable,
};
