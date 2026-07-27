//! Alpha-beta search, transposition tables, and time management.

mod transposition_table;

pub use transposition_table::{
    AllocationError, Bound, CACHE_LINE_BYTES, CLUSTER_ALIGNMENT, CLUSTER_BYTES,
    ENTRIES_PER_CLUSTER, ENTRY_BYTES, EntryData, TranspositionTable,
};
