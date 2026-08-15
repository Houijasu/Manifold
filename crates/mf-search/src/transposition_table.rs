use core::fmt;
use core::mem::{align_of, size_of};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::TryReserveError;
use std::sync::OnceLock;

use mf_core::Move;

pub const CACHE_LINE_BYTES: usize = 64;
pub const ENTRIES_PER_CLUSTER: usize = 4;
const AGE_MASK: u8 = 31;
const MEBIBYTE: usize = 1024 * 1024;
/// Share of installed memory the largest offered table may claim, as a divisor.
///
/// The table is written eagerly, so every mebibyte offered is a mebibyte resident. Half
/// the machine leaves room for the ~106 MiB network, the search stacks, the operating
/// system, and whatever else the user is running while the engine analyses.
const MEMORY_SHARE_DIVISOR: u64 = 2;
/// The maximum offered when installed memory cannot be determined.
///
/// This is the size the engine allocated successfully for its whole history before the
/// limit became machine-derived, so an unknown machine is offered exactly what every
/// machine used to be offered.
const FALLBACK_MAX_HASH_MIB: usize = 4096;
/// Floor on the offered maximum, so the advertised range is never degenerate.
const MIN_MAX_HASH_MIB: usize = 16;

/// The largest Hash size, in MiB, this machine will actually allocate.
///
/// This is the number the UCI handshake advertises. Advertising a size the engine then
/// refuses is worse than advertising a small one: a GUI that honours the advertised range
/// gets a diagnostic buried in `info string` output and keeps searching with whatever
/// table it had, which is usually the 16 MB default and saturates within the first
/// second of every search.
///
/// The result is a power of two, both because it is the shape a GUI's spin control
/// expects and because rounding down is the conservative direction.
pub fn max_hash_mebibytes() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| max_hash_for_installed_memory(installed_memory_bytes()))
}

/// The offered maximum for a machine with `installed_bytes` of memory.
///
/// Separated from [`max_hash_mebibytes`] so the policy can be tested at machine sizes
/// this machine does not have, which is the only way to test it without asserting the
/// answer against itself.
pub fn max_hash_for_installed_memory(installed_bytes: Option<u64>) -> usize {
    let Some(installed_bytes) = installed_bytes else {
        return FALLBACK_MAX_HASH_MIB;
    };
    let offerable_mib = (installed_bytes / MEMORY_SHARE_DIVISOR / MEBIBYTE as u64) as usize;
    previous_power_of_two(offerable_mib).max(MIN_MAX_HASH_MIB)
}

/// The largest power of two not exceeding `value`, or 0 for 0.
fn previous_power_of_two(value: usize) -> usize {
    if value == 0 {
        return 0;
    }
    1 << (usize::BITS - 1 - value.leading_zeros())
}

#[cfg(windows)]
fn installed_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page_file: u64,
        available_page_file: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended_virtual: u64,
    }

    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    let mut status = MemoryStatusEx {
        length: size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_physical: 0,
        available_physical: 0,
        total_page_file: 0,
        available_page_file: 0,
        total_virtual: 0,
        available_virtual: 0,
        available_extended_virtual: 0,
    };
    // SAFETY: `status` is a live, correctly sized `MEMORYSTATUSEX` whose `length` field
    // is set to its own size, which is the whole contract of this call.
    let succeeded = unsafe { GlobalMemoryStatusEx(&mut status) } != 0;
    (succeeded && status.total_physical > 0).then_some(status.total_physical)
}

#[cfg(not(windows))]
fn installed_memory_bytes() -> Option<u64> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Bound {
    Upper = 1,
    Lower = 2,
    Exact = 3,
}

impl Bound {
    #[inline]
    const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Upper),
            2 => Some(Self::Lower),
            3 => Some(Self::Exact),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryData {
    pub best_move: Option<Move>,
    pub score: i16,
    pub static_eval: i16,
    pub depth: u8,
    pub bound: Bound,
    pub age: u8,
    pub pv: bool,
}

#[derive(Debug)]
pub enum AllocationError {
    ZeroSize,
    SizeOverflow,
    RequestTooLarge {
        requested_mib: usize,
        limit_mib: usize,
    },
    Reserve(TryReserveError),
}

impl fmt::Display for AllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSize => write!(formatter, "Hash size must be at least 1 MB"),
            Self::SizeOverflow => write!(formatter, "Hash size overflows the address space"),
            Self::RequestTooLarge {
                requested_mib,
                limit_mib,
            } => write!(
                formatter,
                "{requested_mib} MB exceeds the maximum of {limit_mib} MB"
            ),
            Self::Reserve(error) => write!(formatter, "allocator rejected the request: {error}"),
        }
    }
}

impl std::error::Error for AllocationError {}

#[repr(C)]
struct RawEntry {
    verification: AtomicU64,
    data: AtomicU64,
}

impl RawEntry {
    const fn empty() -> Self {
        Self {
            verification: AtomicU64::new(0),
            data: AtomicU64::new(0),
        }
    }

    #[inline]
    fn load(&self) -> Option<PackedEntry> {
        let data = self.data.load(Ordering::Relaxed);
        if data == 0 {
            return None;
        }
        let verification = self.verification.load(Ordering::Relaxed);
        Some(PackedEntry {
            key: verification ^ data,
            data,
        })
    }

    #[inline]
    fn store(&self, packed: PackedEntry) {
        // Publish the XOR verifier before the payload. A reader that overlaps
        // either relaxed store reconstructs a different key and rejects the
        // snapshot instead of accepting fields torn across two writes.
        self.verification
            .store(packed.key ^ packed.data, Ordering::Relaxed);
        self.data.store(packed.data, Ordering::Relaxed);
    }

    #[inline]
    fn clear(&self) {
        self.data.store(0, Ordering::Relaxed);
    }
}

#[repr(C, align(64))]
struct Cluster {
    entries: [RawEntry; ENTRIES_PER_CLUSTER],
}

impl Cluster {
    const fn empty() -> Self {
        Self {
            entries: [
                RawEntry::empty(),
                RawEntry::empty(),
                RawEntry::empty(),
                RawEntry::empty(),
            ],
        }
    }
}

pub const ENTRY_BYTES: usize = size_of::<RawEntry>();
pub const CLUSTER_BYTES: usize = size_of::<Cluster>();
pub const CLUSTER_ALIGNMENT: usize = align_of::<Cluster>();

const _: () = assert!(size_of::<RawEntry>() * ENTRIES_PER_CLUSTER == CACHE_LINE_BYTES);
const _: () = assert!(
    CACHE_LINE_BYTES.is_multiple_of(size_of::<RawEntry>() * ENTRIES_PER_CLUSTER),
    "TT entries must never straddle a cache line"
);
const _: () = assert!(size_of::<Cluster>() == CACHE_LINE_BYTES);
const _: () = assert!(align_of::<Cluster>() == CACHE_LINE_BYTES);

/// The table's backing memory: either an ordinary heap allocation or a Windows
/// large-page region.
///
/// Random probes over a multi-hundred-megabyte table touch a new 4 KiB page almost
/// every time, so the TLB misses along with the cache. 2 MiB pages cut the page walk
/// out of most probes. The large-page path needs `SeLockMemoryPrivilege`, which most
/// accounts do not hold, so failure of any kind falls back silently to the heap path
/// -- the cluster count is decided BEFORE the allocation strategy, so both paths index
/// identically and the choice can never move a node count.
enum ClusterStorage {
    Heap(Box<[Cluster]>),
    #[cfg(windows)]
    LargePages(large_pages::LargePageAllocation),
}

impl core::ops::Deref for ClusterStorage {
    type Target = [Cluster];

    #[inline]
    fn deref(&self) -> &[Cluster] {
        match self {
            Self::Heap(clusters) => clusters,
            #[cfg(windows)]
            Self::LargePages(allocation) => allocation.clusters(),
        }
    }
}

#[cfg(windows)]
mod large_pages {
    //! Minimal Win32 surface for a `MEM_LARGE_PAGES` allocation, declared by hand
    //! because the workspace deliberately carries no Windows bindings crate.

    use core::ffi::c_void;
    use core::ptr::{self, NonNull};
    use std::sync::OnceLock;

    use super::{CLUSTER_BYTES, Cluster};

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_LARGE_PAGES: u32 = 0x2000_0000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;
    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
    const ERROR_SUCCESS: u32 = 0;

    #[repr(C)]
    struct Luid {
        low_part: u32,
        high_part: i32,
    }

    #[repr(C)]
    struct LuidAndAttributes {
        luid: Luid,
        attributes: u32,
    }

    #[repr(C)]
    struct TokenPrivileges {
        privilege_count: u32,
        privileges: [LuidAndAttributes; 1],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
        fn GetLargePageMinimum() -> usize;
        fn GetCurrentProcess() -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn GetLastError() -> u32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: isize, desired_access: u32, token: *mut isize) -> i32;
        fn LookupPrivilegeValueW(system_name: *const u16, name: *const u16, luid: *mut Luid)
        -> i32;
        fn AdjustTokenPrivileges(
            token: isize,
            disable_all_privileges: i32,
            new_state: *const TokenPrivileges,
            buffer_length: u32,
            previous_state: *mut TokenPrivileges,
            return_length: *mut u32,
        ) -> i32;
    }

    /// A committed large-page region holding exactly `cluster_count` clusters.
    ///
    /// The region itself is rounded up to the large-page minimum, but only the first
    /// `cluster_count` clusters are ever exposed: the count is part of the table's
    /// indexing function and must not depend on how the memory behind it was obtained.
    pub(super) struct LargePageAllocation {
        pointer: NonNull<Cluster>,
        cluster_count: usize,
    }

    // SAFETY: the region is plain memory accessed through `&Cluster`, whose fields are
    // atomics; the allocation is freed only on drop, which requires exclusive access.
    unsafe impl Send for LargePageAllocation {}
    unsafe impl Sync for LargePageAllocation {}

    impl LargePageAllocation {
        #[inline]
        pub(super) fn clusters(&self) -> &[Cluster] {
            // SAFETY: `allocate` committed at least `cluster_count * CLUSTER_BYTES`
            // zeroed bytes at `pointer`, and zeroed bytes are valid (empty) clusters.
            unsafe { core::slice::from_raw_parts(self.pointer.as_ptr(), self.cluster_count) }
        }
    }

    impl Drop for LargePageAllocation {
        fn drop(&mut self) {
            // SAFETY: the pointer came from `VirtualAlloc` with `MEM_RESERVE`, and
            // `MEM_RELEASE` requires a zero size.
            unsafe { VirtualFree(self.pointer.as_ptr().cast(), 0, MEM_RELEASE) };
        }
    }

    /// Attempts a large-page allocation for `cluster_count` clusters.
    ///
    /// Any failure -- the privilege cannot be enabled, large pages are unsupported,
    /// or the allocation itself is refused (large pages need physically contiguous
    /// memory, which a fragmented machine may not have) -- returns `None` and the
    /// caller falls back to the heap.
    pub(super) fn allocate(cluster_count: usize) -> Option<LargePageAllocation> {
        static LOCK_MEMORY_PRIVILEGE: OnceLock<bool> = OnceLock::new();
        if !*LOCK_MEMORY_PRIVILEGE.get_or_init(enable_lock_memory_privilege) {
            return None;
        }
        // SAFETY: no arguments; returns 0 when the processor lacks large pages.
        let page_bytes = unsafe { GetLargePageMinimum() };
        if page_bytes == 0 {
            return None;
        }
        let bytes = cluster_count
            .checked_mul(CLUSTER_BYTES)?
            .checked_add(page_bytes - 1)?
            / page_bytes
            * page_bytes;
        // SAFETY: a fresh reserve-and-commit of `bytes`; the result is checked below.
        // `MEM_COMMIT` memory is zero-initialized, which is the table's empty state.
        let pointer = unsafe {
            VirtualAlloc(
                ptr::null_mut(),
                bytes,
                MEM_COMMIT | MEM_RESERVE | MEM_LARGE_PAGES,
                PAGE_READWRITE,
            )
        };
        NonNull::new(pointer.cast::<Cluster>()).map(|pointer| LargePageAllocation {
            pointer,
            cluster_count,
        })
    }

    /// Enables `SeLockMemoryPrivilege` on the process token, once per process.
    ///
    /// `AdjustTokenPrivileges` succeeds even when it assigns nothing, so the call is
    /// only trusted when `GetLastError` confirms every requested privilege was
    /// enabled (otherwise it reports `ERROR_NOT_ALL_ASSIGNED`).
    fn enable_lock_memory_privilege() -> bool {
        let mut name: Vec<u16> = "SeLockMemoryPrivilege".encode_utf16().collect();
        name.push(0);
        let mut token = 0isize;
        // SAFETY: standard token-privilege sequence over a token opened here; every
        // out-parameter points at a live local, and the token is closed on all paths.
        unsafe {
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            ) == 0
            {
                return false;
            }
            let mut luid = Luid {
                low_part: 0,
                high_part: 0,
            };
            let enabled = LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &mut luid) != 0 && {
                let privileges = TokenPrivileges {
                    privilege_count: 1,
                    privileges: [LuidAndAttributes {
                        luid,
                        attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                AdjustTokenPrivileges(token, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut())
                    != 0
                    && GetLastError() == ERROR_SUCCESS
            };
            CloseHandle(token);
            enabled
        }
    }
}

pub struct TranspositionTable {
    clusters: ClusterStorage,
    /// Whether anything has been stored since the last clear.
    ///
    /// `hashfull` samples only the first 1000 clusters, so a large table holding a small
    /// search is very likely to have every stored entry outside the sample and read as
    /// empty. This flag makes "empty" exact without widening the sample. It is only ever
    /// set to `true` on the store path, so after the first store it is a read-only load
    /// from a shared cache line.
    has_stored: AtomicBool,
}

impl TranspositionTable {
    pub fn new(mebibytes: usize) -> Result<Self, AllocationError> {
        if mebibytes == 0 {
            return Err(AllocationError::ZeroSize);
        }
        let limit_mib = max_hash_mebibytes();
        if mebibytes > limit_mib {
            return Err(AllocationError::RequestTooLarge {
                requested_mib: mebibytes,
                limit_mib,
            });
        }

        let requested_bytes = mebibytes
            .checked_mul(MEBIBYTE)
            .ok_or(AllocationError::SizeOverflow)?;
        let cluster_count = requested_bytes
            .checked_add(CACHE_LINE_BYTES - 1)
            .ok_or(AllocationError::SizeOverflow)?
            / CACHE_LINE_BYTES;

        #[cfg(windows)]
        if let Some(allocation) = large_pages::allocate(cluster_count) {
            return Ok(Self {
                clusters: ClusterStorage::LargePages(allocation),
                has_stored: AtomicBool::new(false),
            });
        }

        let mut clusters = Vec::new();
        clusters
            .try_reserve_exact(cluster_count)
            .map_err(AllocationError::Reserve)?;
        clusters.resize_with(cluster_count, Cluster::empty);
        Ok(Self {
            clusters: ClusterStorage::Heap(clusters.into_boxed_slice()),
            has_stored: AtomicBool::new(false),
        })
    }

    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.clusters.len() * CACHE_LINE_BYTES
    }

    #[doc(hidden)]
    #[inline]
    pub fn base_address(&self) -> usize {
        self.clusters.as_ptr() as usize
    }

    pub fn clear(&self) {
        self.clear_cluster_range(0, self.clusters.len());
        self.has_stored.store(false, Ordering::Relaxed);
    }

    pub(crate) fn cluster_count(&self) -> usize {
        self.clusters.len()
    }

    pub(crate) fn clear_cluster_range(&self, start_cluster: usize, end_cluster: usize) {
        debug_assert!(start_cluster <= end_cluster);
        debug_assert!(end_cluster <= self.clusters.len());
        for cluster in &self.clusters[start_cluster..end_cluster] {
            for entry in &cluster.entries {
                entry.clear();
            }
        }
    }

    /// Returns sampled table occupancy in per-mille, matching the UCI `hashfull` unit.
    pub fn hashfull_per_mille(&self) -> u16 {
        const SAMPLE_CLUSTERS: usize = 1_000;

        let sampled_clusters = self.clusters.len().min(SAMPLE_CLUSTERS);
        let sampled_entries = sampled_clusters * ENTRIES_PER_CLUSTER;
        let occupied = self.clusters[..sampled_clusters]
            .iter()
            .flat_map(|cluster| &cluster.entries)
            .filter(|entry| entry.data.load(Ordering::Relaxed) != 0)
            .count();
        let per_mille = ((occupied * 1_000) / sampled_entries) as u16;
        // A table that has been written to never reports 0, so that `hashfull 0` means
        // "empty" and nothing else. Two things otherwise floor a live table to zero: the
        // integer division above, and the sample itself -- only the first 1000 clusters
        // are examined, so a 4 GiB table holding a short search will usually have every
        // stored entry outside the sample. Both read as empty, and at least one GUI
        // renders `hashfull 0` as 100% used rather than 0%. The reported value is never
        // off by as much as one per-mille, which is finer than the field can express.
        if per_mille == 0 && self.has_stored.load(Ordering::Relaxed) {
            return 1;
        }
        per_mille
    }

    #[inline]
    pub fn probe(&self, key: u64) -> Option<EntryData> {
        self.cluster(key)
            .entries
            .iter()
            .filter_map(RawEntry::load)
            .find(|entry| entry.key == key)
            .and_then(PackedEntry::unpack)
    }

    pub fn store(&self, key: u64, data: EntryData) {
        assert!(
            data.age <= AGE_MASK,
            "TT age must fit in five bits (0..={AGE_MASK})"
        );
        let packed = PackedEntry::pack(key, data);
        let cluster = self.cluster(key);
        // Check-before-store: an unconditional store keeps this shared cache line in
        // exclusive state on every storing thread at node frequency, while the load
        // leaves it shared once the flag is set.
        if !self.has_stored.load(Ordering::Relaxed) {
            self.has_stored.store(true, Ordering::Relaxed);
        }

        if let Some((entry, stored)) = cluster.entries.iter().find_map(|entry| {
            entry
                .load()
                .filter(|stored| stored.key == key)
                .map(|stored| (entry, stored))
        }) {
            // A same-key hit is refreshed rather than blindly overwritten. Qsearch
            // stores its nodes under negative depth domains, so without this guard a
            // horizon node would evict the deep interior entry for the very same
            // position every time the search re-reached it through a capture -- the
            // deep result would be recomputed from scratch on each visit, which is a
            // node-count catastrophe rather than a correctness bug.
            //
            // Depth is the arbiter, with a small slack so a re-search one or two plies
            // shallower can still refresh the age and the move. An exact bound is NOT
            // an unconditional override: a qsearch node returns an exact score for the
            // position *below the horizon*, which says nothing about the deep result
            // already stored for it, and letting it replace on bound alone measured 6x
            // the nodes to depth 12 in the endgame position. An exact bound only wins
            // ties, where it is genuinely the stronger of two equally deep results.
            let stored_is_current = stored.age() == data.age;
            let new_depth = i16::from(data.depth);
            let stored_depth = i16::from(stored.depth());
            let replace = new_depth + 4 > stored_depth
                || (data.bound == Bound::Exact && new_depth >= stored_depth)
                || !stored_is_current;
            if replace {
                // A store that names no move (a qsearch stand-pat fail-high, say) must
                // not erase a move an earlier search proved good for this position.
                let preserved = if data.best_move.is_none() {
                    stored.best_move()
                } else {
                    data.best_move
                };
                entry.store(PackedEntry::pack(
                    key,
                    EntryData {
                        best_move: preserved,
                        ..data
                    },
                ));
            }
            return;
        }

        if let Some(entry) = cluster
            .entries
            .iter()
            .find(|entry| entry.data.load(Ordering::Relaxed) == 0)
        {
            entry.store(packed);
            return;
        }

        let replacement = cluster
            .entries
            .iter()
            .min_by_key(|entry| {
                entry.load().map_or(i16::MIN, |stored| {
                    let relative_age = data.age.wrapping_sub(stored.age()) & AGE_MASK;
                    i16::from(stored.depth()) - 8 * i16::from(relative_age)
                })
            })
            .expect("a cluster always contains at least one entry");
        replacement.store(packed);
    }

    #[inline]
    pub fn prefetch(&self, key: u64) {
        let address = self.cluster(key) as *const Cluster as *const i8;
        prefetch_read(address);
    }

    #[inline]
    fn cluster(&self, key: u64) -> &Cluster {
        &self.clusters[cluster_index(key, self.clusters.len())]
    }
}

#[derive(Clone, Copy)]
struct PackedEntry {
    key: u64,
    data: u64,
}

impl PackedEntry {
    const SCORE_SHIFT: u32 = 16;
    const EVAL_SHIFT: u32 = 32;
    const DEPTH_SHIFT: u32 = 48;
    const FLAGS_SHIFT: u32 = 56;
    const BOUND_MASK: u8 = 0b11;
    const PV_SHIFT: u32 = 2;
    const AGE_SHIFT: u32 = 3;

    #[inline]
    fn pack(key: u64, data: EntryData) -> Self {
        let flags = (data.age << Self::AGE_SHIFT)
            | (u8::from(data.pv) << Self::PV_SHIFT)
            | data.bound as u8;
        let move_raw = data.best_move.map_or(0, Move::raw);
        assert!(
            data.best_move.is_none() || move_raw != 0,
            "the all-zero move encoding is reserved for no TT move"
        );
        let packed_data = u64::from(move_raw)
            | (u64::from(data.score as u16) << Self::SCORE_SHIFT)
            | (u64::from(data.static_eval as u16) << Self::EVAL_SHIFT)
            | (u64::from(data.depth) << Self::DEPTH_SHIFT)
            | (u64::from(flags) << Self::FLAGS_SHIFT);
        Self {
            key,
            data: packed_data,
        }
    }

    #[inline]
    fn unpack(self) -> Option<EntryData> {
        let flags = (self.data >> Self::FLAGS_SHIFT) as u8;
        let bound = Bound::from_raw(flags & Self::BOUND_MASK)?;
        let move_raw = self.data as u16;
        let best_move = if move_raw == 0 {
            None
        } else {
            Move::from_raw(move_raw)
        };
        Some(EntryData {
            best_move,
            score: (self.data >> Self::SCORE_SHIFT) as u16 as i16,
            static_eval: (self.data >> Self::EVAL_SHIFT) as u16 as i16,
            depth: self.depth(),
            bound,
            age: self.age(),
            pv: flags & (1 << Self::PV_SHIFT) != 0,
        })
    }

    #[inline]
    const fn depth(self) -> u8 {
        (self.data >> Self::DEPTH_SHIFT) as u8
    }

    #[inline]
    fn best_move(self) -> Option<Move> {
        let move_raw = self.data as u16;
        if move_raw == 0 {
            None
        } else {
            Move::from_raw(move_raw)
        }
    }

    #[inline]
    const fn age(self) -> u8 {
        ((self.data >> Self::FLAGS_SHIFT) as u8 >> Self::AGE_SHIFT) & AGE_MASK
    }
}

#[inline]
fn cluster_index(key: u64, cluster_count: usize) -> usize {
    (((key as u128) * (cluster_count as u128)) >> 64) as usize
}

#[cfg(target_arch = "x86")]
#[inline]
fn prefetch_read(address: *const i8) {
    use core::arch::x86::{_MM_HINT_T0, _mm_prefetch};

    // SAFETY: `_mm_prefetch` only issues a hardware hint. The pointer is derived
    // from a live table allocation and is not dereferenced by Rust.
    unsafe { _mm_prefetch::<_MM_HINT_T0>(address) };
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn prefetch_read(address: *const i8) {
    use core::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};

    // SAFETY: `_mm_prefetch` only issues a hardware hint. The pointer is derived
    // from a live table allocation and is not dereferenced by Rust.
    unsafe { _mm_prefetch::<_MM_HINT_T0>(address) };
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
#[inline]
fn prefetch_read(_address: *const i8) {}
