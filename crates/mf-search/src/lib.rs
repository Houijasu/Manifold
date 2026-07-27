//! Alpha-beta search, transposition tables, and time management.

mod evaluation;
mod move_ordering;
mod search;
mod transposition_table;

pub use evaluation::{
    DEFAULT_PARAMETERS, EvaluationParameters, TaperedScore, evaluate, evaluate_with_parameters,
};
pub use search::{
    IterationInfo, MATE_SCORE, MAX_SEARCH_PLY, SearchLimits, SearchResult, UNEVALUATED_STATIC_EVAL,
    clamp_centipawn_score, is_mate_score, score_to_uci_mate, search,
};
pub use transposition_table::{
    AllocationError, Bound, CACHE_LINE_BYTES, CLUSTER_ALIGNMENT, CLUSTER_BYTES,
    ENTRIES_PER_CLUSTER, ENTRY_BYTES, EntryData, TranspositionTable,
};
