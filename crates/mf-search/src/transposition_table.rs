use core::fmt;
use core::mem::{align_of, size_of};
use core::sync::atomic::{AtomicU64, Ordering};
use std::collections::TryReserveError;

use mf_core::Move;

pub const CACHE_LINE_BYTES: usize = 64;
pub const ENTRIES_PER_CLUSTER: usize = 4;
const AGE_MASK: u8 = 31;
const MAX_EAGER_ALLOCATION_MIB: usize = 4096;
const MEBIBYTE: usize = 1024 * 1024;

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
                "{requested_mib} MB exceeds the safe eager-allocation limit of {limit_mib} MB"
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

pub struct TranspositionTable {
    clusters: Box<[Cluster]>,
}

impl TranspositionTable {
    pub fn new(mebibytes: usize) -> Result<Self, AllocationError> {
        if mebibytes == 0 {
            return Err(AllocationError::ZeroSize);
        }
        if mebibytes > MAX_EAGER_ALLOCATION_MIB {
            return Err(AllocationError::RequestTooLarge {
                requested_mib: mebibytes,
                limit_mib: MAX_EAGER_ALLOCATION_MIB,
            });
        }

        let requested_bytes = mebibytes
            .checked_mul(MEBIBYTE)
            .ok_or(AllocationError::SizeOverflow)?;
        let cluster_count = requested_bytes
            .checked_add(CACHE_LINE_BYTES - 1)
            .ok_or(AllocationError::SizeOverflow)?
            / CACHE_LINE_BYTES;

        let mut clusters = Vec::new();
        clusters
            .try_reserve_exact(cluster_count)
            .map_err(AllocationError::Reserve)?;
        clusters.resize_with(cluster_count, Cluster::empty);
        Ok(Self {
            clusters: clusters.into_boxed_slice(),
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
        ((occupied * 1_000) / sampled_entries) as u16
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

        if let Some(entry) = cluster
            .entries
            .iter()
            .find(|entry| entry.load().is_some_and(|stored| stored.key == key))
        {
            entry.store(packed);
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
