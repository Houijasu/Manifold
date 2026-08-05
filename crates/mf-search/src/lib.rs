//! Alpha-beta search, transposition tables, and time management.

mod history;
mod move_ordering;
mod repetition;
mod search;
mod thread_pool;
mod transposition_table;
mod vote;

pub use history::{
    CORRECTION_MAJOR, CORRECTION_MATERIAL, CORRECTION_MINOR, CORRECTION_PAWN, CORRECTION_SOURCES,
    SharedHistory,
};
pub use search::{
    IterationInfo, MATE_SCORE, MAX_SEARCH_PLY, RootMoveInfo, SearchLimits, SearchOptions,
    SearchResult, UNEVALUATED_STATIC_EVAL, clamp_centipawn_score, is_mate_score, score_to_uci_mate,
    search, search_with_callback, search_with_shared_history,
};
pub use thread_pool::{PoolError, PoolSearchResult, SearchPool};
pub use transposition_table::{
    AllocationError, Bound, CACHE_LINE_BYTES, CLUSTER_ALIGNMENT, CLUSTER_BYTES,
    ENTRIES_PER_CLUSTER, ENTRY_BYTES, EntryData, TranspositionTable, max_hash_for_installed_memory,
    max_hash_mebibytes,
};
