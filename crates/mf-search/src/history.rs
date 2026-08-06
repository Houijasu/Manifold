use std::sync::atomic::{AtomicI16, Ordering};

use mf_core::{Color, Move, Piece, PieceKind, Position, Square};

use crate::MAX_SEARCH_PLY;

/// Gravity saturation bound for butterfly (main) history.
const BUTTERFLY_MAX: i32 = 7_183;
/// Gravity saturation bound for capture history.
const CAPTURE_MAX: i32 = 10_692;
/// Gravity saturation bound for pawn-structure history.
const PAWN_MAX: i32 = 8_192;
/// Gravity saturation bound for continuation history (the piece-to history table).
///
/// This is deliberately ~4x the butterfly bound: a continuation entry is conditioned on
/// a specific predecessor move, so it is a far sharper statistic than a from-to average
/// and is allowed to express a correspondingly stronger opinion.
pub(crate) const CONTINUATION_MAX: i32 = 30_000;

/// Lookback distances, in plies, at which continuation history is kept.
///
/// The 1-ply table IS the counter-move heuristic: `continuation[0]` is indexed by the
/// opponent's immediately preceding move, so "the reply that refutes this move" falls
/// out of it without a separate counter-move structure.
///
/// `{1, 2, 4, 6}` is the community consensus subset for a new engine (Berserk,
/// Stormphrax). The dense 1-6 set is a late refinement, and plies 3 and 5 carry the
/// two weakest weights in the reference bonus table (290 and 132 against 1040/780/502/418).
pub(crate) const CONTINUATION_PLIES: [usize; 4] = [1, 2, 4, 6];
/// Update weights in 1024ths, from the reference continuation-history bonus table for
/// exactly the plies in `CONTINUATION_PLIES`.
pub(crate) const CONTINUATION_WEIGHTS: [i32; CONTINUATION_PLIES.len()] = [1_040, 780, 502, 418];

/// Gravity saturation bound for every correction-history table.
///
/// This is the reference `CORRECTION_HISTORY_LIMIT` and it is deliberately far smaller
/// than the ordering bounds above: a corrhist entry is a *mean residual in eval units*
/// that gets divided by [`CORRECTION_SCALE`] before it touches the static evaluation,
/// not a relative ordering score. The two families are not comparable and must not be
/// given each other's constants.
pub(crate) const CORRECTION_MAX: i32 = 1_024;

/// Bucket count of each hash-keyed correction table.
///
/// Sized by the BUCKET COUNT, never by the gravity bound (mission AGENTS.md 4.54 trap
/// 2 -- sizing pawn history from the reference's gravity divisor instead of its bucket
/// count built a 12 MiB L2-thrashing table and cost 18% NPS). At 16,384 buckets x 2
/// colors x `i16` each table is 64 KiB, so all four together are 256 KiB and stay
/// inside L2. The reference uses 65,536, but it packs all four variants into ONE
/// `CorrectionBundle` table sharing a single size mask; four separate 256 KiB tables
/// would be 1 MiB of independently-strided lookups on the eval path. A search tree
/// visits far fewer distinct pawn structures than 16,384 per node region, so the
/// collision rate a larger table would hold flat is not the binding constraint here.
///
/// **Thread-count invariant. Do not reintroduce a `* nextPow2(threads)` factor.** See
/// [`SharedHistory::new`].
const CORRECTION_BUCKETS: usize = 16_384;
const CORRECTION_BUCKET_MASK: u64 = CORRECTION_BUCKETS as u64 - 1;

/// Blend slots for the hash-keyed correction sources.
///
/// The five Zobrist keys M1-F3 built exist for exactly this: `pawn` is the pawn
/// structure, `minor` and `major` are placement hashes of {N,B} and {R,Q}, and
/// `non_pawn_material` is a per-color piece-COUNT hash, which is precisely Caissa's
/// original material-configuration corrhist.
pub const CORRECTION_PAWN: usize = 0;
pub const CORRECTION_MINOR: usize = 1;
pub const CORRECTION_MAJOR: usize = 2;
pub const CORRECTION_MATERIAL: usize = 3;
pub const CORRECTION_SOURCES: usize = 4;

/// Blend weights, applied before the `/ CORRECTION_SCALE` division.
///
/// Reference: `15341*pawn + 10569*minor + 12906*(nonPawnWhite + nonPawnBlack) + cont`.
/// Manifold's `major` and `material` keys both stand in for a non-pawn characterisation
/// of the position, so both take the reference's non-pawn weight.
pub(crate) const CORRECTION_WEIGHTS: [i32; CORRECTION_SOURCES] = [15_341, 10_569, 12_906, 12_906];
/// The reference's `8761 *` multiplier on the summed continuation-corrhist entries.
pub(crate) const CORRECTION_CONTINUATION_WEIGHT: i32 = 8_761;
/// The reference `to_corrected_static_eval`: `v + cv / 131072`.
pub(crate) const CORRECTION_SCALE: i32 = 131_072;

/// Per-source update weights in 128ths (reference: pawn `bonus`, minor `150/128`,
/// non-pawn `186/128`).
pub(crate) const CORRECTION_UPDATE_WEIGHTS: [i32; CORRECTION_SOURCES] = [128, 150, 186, 186];

/// Lookback distances, in plies, at which continuation correction history is kept.
///
/// **2 and 4, not 1 and 2.** Unlike continuation *ordering* history, which asks "what
/// refutes the opponent's last move", continuation corrhist looks back at *our own*
/// previous moves, so it steps two plies at a time. Getting this wrong silently indexes
/// the residual on the opponent's move instead.
pub(crate) const CORRECTION_CONTINUATION_PLIES: [usize; 2] = [2, 4];
/// The reference's `130/128` and `70/128` update weights for plies 2 and 4.
pub(crate) const CORRECTION_CONTINUATION_UPDATE_WEIGHTS: [i32; CORRECTION_CONTINUATION_PLIES
    .len()] = [130, 70];

const COLORS: usize = 2;
const SQUARES: usize = 64;
const PIECES: usize = 12;
/// Victim slot count. Index 5 (`PieceKind::King`) is unreachable as a real victim and
/// is therefore reused as the "promotion without capture" slot.
const VICTIMS: usize = 6;
const NO_VICTIM: usize = PieceKind::King.index();

/// Bucket count of the pawn-history table, per the reference's `PAWN_HISTORY_SIZE`.
///
/// This is deliberately NOT `PAWN_MAX`: the two are different numbers that both
/// happen to be powers of two in the reference source. 512 buckets is 768 KiB, which
/// stays inside L2; 8192 buckets would be 12 MiB and cost ~18% NPS to cache misses
/// for no ordering benefit.
///
/// **Thread-count invariant. Do not reintroduce a `* nextPow2(threads)` factor.** See
/// [`SharedHistory::new`].
const PAWN_BUCKETS: usize = 512;
const PAWN_BUCKET_MASK: u64 = PAWN_BUCKETS as u64 - 1;

const BUTTERFLY_LEN: usize = COLORS * SQUARES * SQUARES;
const CAPTURE_LEN: usize = PIECES * SQUARES * VICTIMS;
const PAWN_BUCKET_LEN: usize = PIECES * SQUARES;
/// One `[piece][to]` plane, the inner table a single predecessor move selects.
const CONTINUATION_PLANE_LEN: usize = PIECES * SQUARES;
/// A whole ply's table: one plane per `[previous_piece][previous_to]`.
const CONTINUATION_LEN: usize = PIECES * SQUARES * CONTINUATION_PLANE_LEN;
/// One continuation-corrhist table: a `[piece][to]` residual per predecessor plane.
///
/// This is the same shape as the ordering continuation table but is a SEPARATE
/// allocation storing a different quantity: an eval residual bounded by
/// [`CORRECTION_MAX`], not an ordering score bounded by `CONTINUATION_MAX`.
const CORRECTION_CONTINUATION_LEN: usize = PIECES * SQUARES * CONTINUATION_PLANE_LEN;

/// Forces each table onto its own cache line so concurrent updates to two different
/// tables never contend for the same line.
#[repr(align(64))]
struct CacheAligned<T>(T);

/// Identifies the `[piece][to]` plane a predecessor move selects inside one
/// continuation table.
///
/// Resolved once when the move is pushed onto the search stack rather than on every
/// lookup, so a node that scores forty quiets performs four plane resolutions, not one
/// hundred and sixty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ContinuationKey {
    plane: u32,
    /// The `(piece, to)` pair the plane was derived from, retained so continuation
    /// CORRECTION history can use a past move as its *entry* index while a different,
    /// further-back move selects the plane. Ordering continuation history only ever
    /// needs the plane, but corrhist indexes `[plane at ply-N][piece,to at ply-1]`.
    piece: u8,
    to: u8,
}

impl ContinuationKey {
    #[inline]
    pub(crate) fn new(piece: Piece, to: Square) -> Self {
        let index = piece.index() * SQUARES + usize::from(to.index());
        Self {
            plane: (index * CONTINUATION_PLANE_LEN) as u32,
            piece: piece.index() as u8,
            to: to.index(),
        }
    }

    /// The `[piece][to]` offset this move contributes when used as an ENTRY index
    /// rather than as a plane selector.
    #[inline]
    pub(crate) fn entry_offset(self) -> usize {
        usize::from(self.piece) * SQUARES + usize::from(self.to)
    }
}

/// Relaxed-atomic history tables shared by every search worker.
///
/// Thread-private history is obsolete: the reference moved correction, pawn, and
/// continuation history to shared relaxed-atomic tables between Dec 2025 and Jun 2026,
/// SPRT-verified at 1, 8, 16, and 64 threads. Sharing lets a worker reuse ordering
/// knowledge another worker already paid for. Updates race exactly like the
/// transposition table does: each `apply` performs one load and one store, so a torn
/// interleaving can lose an update but can never store a value outside `[-D, D]`.
pub struct SharedHistory {
    butterfly: CacheAligned<Box<[AtomicI16]>>,
    capture: CacheAligned<Box<[AtomicI16]>>,
    pawn: CacheAligned<Box<[AtomicI16]>>,
    /// One table per entry in `CONTINUATION_PLIES`, each on its own cache-line-aligned
    /// allocation so an update at ply 1 cannot false-share with one at ply 6.
    continuation: [CacheAligned<Box<[AtomicI16]>>; CONTINUATION_PLIES.len()],
    /// One table per entry in [`CORRECTION_SOURCES`], each `[bucket][color]`.
    ///
    /// Kept as four separate allocations rather than the reference's packed
    /// `CorrectionBundle` because Manifold's four keys are genuinely independent
    /// hashes: bundling them would make every read of one variant pull the other three
    /// into cache on a line they will never be used from, since a single position
    /// hashes to four *different* buckets.
    correction: [CacheAligned<Box<[AtomicI16]>>; CORRECTION_SOURCES],
    /// Continuation correction history at plies 2 and 4.
    correction_continuation: [CacheAligned<Box<[AtomicI16]>>; CORRECTION_CONTINUATION_PLIES.len()],
}

impl SharedHistory {
    /// Builds the shared tables. **Every table is sized independently of the thread
    /// count, and must stay that way.**
    ///
    /// Pawn history and corrhist used to be sized `BASE * nextPow2(threads)`, on the
    /// reference's argument that a wider pool holds more distinct positions in flight
    /// so the table should grow to keep the collision rate flat. That made the bucket
    /// MASK a function of `Threads`, so a hash collision that happens at 512 buckets
    /// and not at 4,096 changed the corrhist residual applied to a static eval, and
    /// therefore changed the tree — deterministically, with every helper parked. Fixed
    /// depth from a single worker then produced different node counts at `Threads=1`
    /// and `Threads=8` (kiwipete depth 10 on the M2 baseline binary), which is a
    /// reproducibility defect that turns any tree-enlarging search change into an
    /// apparent regression.
    ///
    /// This does not regress SMP. The tables stay SHARED across workers and every
    /// access is the same single relaxed load / relaxed store it always was, so
    /// cross-thread contention is unchanged in kind. The scaling only ever reduced
    /// *collisions*, never contention: workers contend on the entries they both touch,
    /// and a larger table does not stop two workers from updating the same bucket for
    /// the same position — which is the sharing the reference explicitly wants. What it
    /// did buy was collision headroom at high thread counts, paid for with cache: at 8
    /// threads the four corrhist tables went from 256 KiB to 2 MiB and pawn history
    /// from 768 KiB to 6 MiB, well past L2 on the target machine (AGENTS.md 4.54 trap
    /// 2, the same trap that cost 18% NPS). Determinism is worth more than headroom the
    /// cache cannot hold.
    pub fn new() -> Self {
        Self {
            butterfly: CacheAligned(zeroed(BUTTERFLY_LEN)),
            capture: CacheAligned(zeroed(CAPTURE_LEN)),
            pawn: CacheAligned(zeroed(PAWN_BUCKETS * PAWN_BUCKET_LEN)),
            // Continuation history is keyed on an exact predecessor move rather than on
            // a hashed structure, so it has no collision rate at all. Each ply is 12*64
            // planes of 12*64 `i16` = 1.125 MiB, 4.5 MiB in total; multiplying a table
            // under no collision pressure would only thrash cache (AGENTS.md 4.54 trap).
            continuation: core::array::from_fn(|_| CacheAligned(zeroed(CONTINUATION_LEN))),
            correction: core::array::from_fn(|_| CacheAligned(zeroed(CORRECTION_BUCKETS * COLORS))),
            correction_continuation: core::array::from_fn(|_| {
                CacheAligned(zeroed(CORRECTION_CONTINUATION_LEN))
            }),
        }
    }

    /// Resets every table. Called on `ucinewgame` so a new game does not inherit the
    /// previous game's ordering statistics, and by `bench` between positions so one
    /// allocation serves all six without polluting the timed region.
    pub fn clear(&self) {
        for entry in self
            .butterfly
            .0
            .iter()
            .chain(self.capture.0.iter())
            .chain(self.pawn.0.iter())
            .chain(self.continuation.iter().flat_map(|table| table.0.iter()))
            .chain(self.correction.iter().flat_map(|table| table.0.iter()))
            .chain(
                self.correction_continuation
                    .iter()
                    .flat_map(|table| table.0.iter()),
            )
        {
            entry.store(0, Ordering::Relaxed);
        }
    }

    pub(crate) fn butterfly_score(&self, color: Color, mv: Move) -> i32 {
        load(&self.butterfly.0[butterfly_index(color, mv)])
    }

    pub(crate) fn update_butterfly(&self, color: Color, mv: Move, bonus: i32) {
        apply(
            &self.butterfly.0[butterfly_index(color, mv)],
            bonus,
            BUTTERFLY_MAX,
        );
    }

    pub(crate) fn capture_score(&self, piece: Piece, to: Square, victim: Option<PieceKind>) -> i32 {
        load(&self.capture.0[capture_index(piece, to, victim)])
    }

    pub(crate) fn update_capture(
        &self,
        piece: Piece,
        to: Square,
        victim: Option<PieceKind>,
        bonus: i32,
    ) {
        apply(
            &self.capture.0[capture_index(piece, to, victim)],
            bonus,
            CAPTURE_MAX,
        );
    }

    pub(crate) fn pawn_score(&self, pawn_key: u64, piece: Piece, to: Square) -> i32 {
        load(&self.pawn.0[self.pawn_index(pawn_key, piece, to)])
    }

    /// Pawn history uses an asymmetric update: penalties land at roughly 42% of the
    /// strength of bonuses (reference `bonus * (bonus > -4 ? 1104 : 459) / 1024`).
    pub(crate) fn update_pawn(&self, pawn_key: u64, piece: Piece, to: Square, bonus: i32) {
        let scaled = bonus * if bonus > -4 { 1_104 } else { 459 } / 1_024;
        apply(
            &self.pawn.0[self.pawn_index(pawn_key, piece, to)],
            scaled,
            PAWN_MAX,
        );
    }

    /// Continuation score at one lookback distance.
    ///
    /// `slot` indexes `CONTINUATION_PLIES`, not the ply distance itself.
    #[inline]
    pub(crate) fn continuation_score_at(
        &self,
        slot: usize,
        previous: ContinuationKey,
        piece: Piece,
        to: Square,
    ) -> i32 {
        load(&self.continuation[slot].0[continuation_index(previous, piece, to)])
    }

    #[inline]
    pub(crate) fn update_continuation_at(
        &self,
        slot: usize,
        previous: ContinuationKey,
        piece: Piece,
        to: Square,
        bonus: i32,
    ) {
        apply(
            &self.continuation[slot].0[continuation_index(previous, piece, to)],
            bonus,
            CONTINUATION_MAX,
        );
    }

    /// Correction residual for one hash-keyed source.
    ///
    /// `source` indexes [`CORRECTION_WEIGHTS`], `key` is the matching Zobrist key.
    #[inline]
    pub(crate) fn correction_score(&self, source: usize, key: u64, color: Color) -> i32 {
        load(&self.correction[source].0[self.correction_index(key, color)])
    }

    #[inline]
    pub(crate) fn update_correction(&self, source: usize, key: u64, color: Color, bonus: i32) {
        apply(
            &self.correction[source].0[self.correction_index(key, color)],
            scale_correction_bonus(bonus, CORRECTION_UPDATE_WEIGHTS[source]),
            CORRECTION_MAX,
        );
    }

    /// Continuation correction residual at one lookback distance.
    ///
    /// `slot` indexes [`CORRECTION_CONTINUATION_PLIES`], not the ply distance itself.
    /// `plane` is the move at that distance; `entry` is the IMMEDIATELY preceding move,
    /// matching the reference's
    /// `(*(ss-2)->continuationCorrectionHistory)[piece_on(m.to)][m.to]` where
    /// `m = (ss-1)->currentMove`.
    #[inline]
    pub(crate) fn correction_continuation_score(
        &self,
        slot: usize,
        plane: ContinuationKey,
        entry: ContinuationKey,
    ) -> i32 {
        load(&self.correction_continuation[slot].0[plane.plane as usize + entry.entry_offset()])
    }

    #[inline]
    pub(crate) fn update_correction_continuation(
        &self,
        slot: usize,
        plane: ContinuationKey,
        entry: ContinuationKey,
        bonus: i32,
    ) {
        apply(
            &self.correction_continuation[slot].0[plane.plane as usize + entry.entry_offset()],
            scale_correction_bonus(bonus, CORRECTION_CONTINUATION_UPDATE_WEIGHTS[slot]),
            CORRECTION_MAX,
        );
    }

    #[inline]
    fn correction_index(&self, key: u64, color: Color) -> usize {
        (key & CORRECTION_BUCKET_MASK) as usize * COLORS + color.index()
    }

    #[cfg(test)]
    pub(crate) fn correction_bucket_count(&self) -> usize {
        self.correction[0].0.len() / COLORS
    }

    #[inline]
    fn pawn_index(&self, pawn_key: u64, piece: Piece, to: Square) -> usize {
        let bucket = (pawn_key & PAWN_BUCKET_MASK) as usize;
        bucket * PAWN_BUCKET_LEN + piece.index() * SQUARES + usize::from(to.index())
    }

    #[cfg(test)]
    pub(crate) fn pawn_bucket_count(&self) -> usize {
        self.pawn.0.len() / PAWN_BUCKET_LEN
    }

    #[cfg(test)]
    pub(crate) fn continuation_score(
        &self,
        previous: ContinuationKey,
        piece: Piece,
        to: Square,
    ) -> i32 {
        self.continuation_score_at(0, previous, piece, to)
    }

    #[cfg(test)]
    pub(crate) fn update_continuation(
        &self,
        previous: ContinuationKey,
        piece: Piece,
        to: Square,
        bonus: i32,
    ) {
        self.update_continuation_at(0, previous, piece, to, bonus);
    }

    #[cfg(test)]
    pub(crate) fn continuation_len(&self) -> usize {
        self.continuation[0].0.len()
    }

    #[cfg(test)]
    pub(crate) fn continuation_base_address(&self) -> usize {
        self.continuation[0].0.as_ptr() as usize
    }
}

impl Default for SharedHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// Applies a per-source update weight in 128ths without overflowing.
///
/// `apply` clamps to `+/-max` anyway, so clamping the bonus first is behaviour-neutral
/// for every in-range value and only removes the overflow on the `i32::MAX` sentinel
/// that saturation tests and a degenerate depth could both produce.
#[inline]
fn scale_correction_bonus(bonus: i32, weight: i32) -> i32 {
    bonus.clamp(-CORRECTION_MAX, CORRECTION_MAX) * weight / 128
}

#[inline]
fn continuation_index(previous: ContinuationKey, piece: Piece, to: Square) -> usize {
    previous.plane as usize + piece.index() * SQUARES + usize::from(to.index())
}

/// Allocates a zeroed table in ONE bulk request rather than element by element.
///
/// `(0..len).map(|_| AtomicI16::new(0)).collect()` writes every entry individually. At
/// the 4.5 MiB the continuation tables occupy that is millions of stores per
/// construction, which is invisible to any node-count anchor and cost ~15% of the
/// measured bench NPS, since `bench` builds a fresh table per position inside its own
/// timed region. `vec![0i16; len]` routes through `alloc_zeroed`, which hands back
/// OS-zeroed pages.
fn zeroed(len: usize) -> Box<[AtomicI16]> {
    let zeros = vec![0i16; len].into_boxed_slice();
    // SAFETY: `AtomicI16` is `#[repr(transparent)]` over `i16`, so the two have
    // identical size, alignment, and layout, and every `i16` bit pattern is a valid
    // `AtomicI16`. The pointer comes from `Box::into_raw`, is not aliased, and the
    // slice length is carried through the fat pointer unchanged, so the reconstructed
    // `Box` owns exactly the allocation it was handed. The debug assertion below pins
    // the layout equality this relies on.
    debug_assert_eq!(size_of::<AtomicI16>(), size_of::<i16>());
    debug_assert_eq!(align_of::<AtomicI16>(), align_of::<i16>());
    unsafe { Box::from_raw(Box::into_raw(zeros) as *mut [AtomicI16]) }
}

#[inline]
fn butterfly_index(color: Color, mv: Move) -> usize {
    color.index() * SQUARES * SQUARES
        + usize::from(mv.from().index()) * SQUARES
        + usize::from(mv.to().index())
}

#[inline]
fn capture_index(piece: Piece, to: Square, victim: Option<PieceKind>) -> usize {
    let victim = victim.map_or(NO_VICTIM, PieceKind::index);
    piece.index() * SQUARES * VICTIMS + usize::from(to.index()) * VICTIMS + victim
}

#[inline]
fn load(entry: &AtomicI16) -> i32 {
    i32::from(entry.load(Ordering::Relaxed))
}

/// The universal "gravity" update. The `- value * |bonus| / max` term pulls entries
/// toward zero in proportion to how extreme they already are, so the table
/// self-normalises and saturates smoothly at `+/-max` without periodic rescaling.
#[inline]
fn apply(entry: &AtomicI16, bonus: i32, max: i32) {
    let bonus = bonus.clamp(-max, max);
    let value = load(entry);
    let updated = value + bonus - value * bonus.abs() / max;
    entry.store(updated.clamp(-max, max) as i16, Ordering::Relaxed);
}

/// Returns the victim kind a capture removes, or `None` for a non-capturing promotion.
#[inline]
pub(crate) fn captured_kind(position: &Position, mv: Move) -> Option<PieceKind> {
    if mv.flag().is_en_passant() {
        return Some(PieceKind::Pawn);
    }
    position.piece_at(mv.to()).map(|piece| piece.kind())
}

/// Per-worker ordering state. Killers are ply-indexed scratch that is meaningless
/// across workers searching different subtrees, so they stay thread-private while the
/// statistical tables are shared.
pub(crate) struct KillerTable {
    killers: [[Option<Move>; 2]; MAX_SEARCH_PLY],
}

impl KillerTable {
    pub(crate) fn new() -> Self {
        Self {
            killers: [[None; 2]; MAX_SEARCH_PLY],
        }
    }

    pub(crate) fn killers(&self, ply: usize) -> [Option<Move>; 2] {
        self.killers[ply]
    }

    pub(crate) fn record_killer(&mut self, ply: usize, mv: Move) {
        if ply >= MAX_SEARCH_PLY || self.killers[ply][0] == Some(mv) {
            return;
        }
        self.killers[ply][1] = self.killers[ply][0];
        self.killers[ply][0] = Some(mv);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use mf_core::{Color, Move, MoveFlag, Piece, PieceKind, Position, Square};

    use super::{
        BUTTERFLY_MAX, CAPTURE_MAX, CONTINUATION_MAX, CONTINUATION_PLIES, CONTINUATION_WEIGHTS,
        CORRECTION_BUCKETS, CORRECTION_CONTINUATION_PLIES, CORRECTION_CONTINUATION_UPDATE_WEIGHTS,
        CORRECTION_MAJOR, CORRECTION_MATERIAL, CORRECTION_MAX, CORRECTION_MINOR, CORRECTION_PAWN,
        CORRECTION_SOURCES, CORRECTION_UPDATE_WEIGHTS, CORRECTION_WEIGHTS, ContinuationKey,
        KillerTable, PAWN_BUCKETS, PAWN_MAX, SharedHistory, captured_kind,
    };

    fn square(index: u8) -> Square {
        Square::new(index).expect("valid square")
    }

    fn first_move() -> Move {
        Move::new(square(8), square(16), MoveFlag::QUIET)
    }

    fn second_move() -> Move {
        Move::new(square(9), square(17), MoveFlag::QUIET)
    }

    #[test]
    fn killers_rotate_without_duplicates() {
        let mut killers = KillerTable::new();
        killers.record_killer(3, first_move());
        killers.record_killer(3, second_move());
        killers.record_killer(3, second_move());

        assert_eq!(
            killers.killers(3),
            [Some(second_move()), Some(first_move())]
        );
    }

    #[test]
    fn butterfly_history_is_color_and_square_specific() {
        let history = SharedHistory::new();
        history.update_butterfly(Color::White, first_move(), BUTTERFLY_MAX);

        assert_eq!(
            history.butterfly_score(Color::White, first_move()),
            BUTTERFLY_MAX
        );
        assert_eq!(history.butterfly_score(Color::Black, first_move()), 0);
        assert_eq!(history.butterfly_score(Color::White, second_move()), 0);
    }

    #[test]
    fn butterfly_history_uses_bounded_gravity_updates() {
        let history = SharedHistory::new();
        history.update_butterfly(Color::White, first_move(), BUTTERFLY_MAX / 2);
        history.update_butterfly(Color::White, first_move(), BUTTERFLY_MAX / 2);
        // v + b - v*|b|/D with v = D/2, b = D/2 gives 3D/4 (integer division exact here).
        assert_eq!(
            history.butterfly_score(Color::White, first_move()),
            3 * BUTTERFLY_MAX / 4
        );

        history.update_butterfly(Color::White, first_move(), i32::MAX);
        assert_eq!(
            history.butterfly_score(Color::White, first_move()),
            BUTTERFLY_MAX
        );
        history.update_butterfly(Color::White, first_move(), i32::MIN);
        assert_eq!(
            history.butterfly_score(Color::White, first_move()),
            -BUTTERFLY_MAX
        );
    }

    #[test]
    fn capture_history_separates_victims_and_saturates() {
        let history = SharedHistory::new();
        let piece = Piece::new(Color::White, PieceKind::Knight);
        history.update_capture(piece, square(20), Some(PieceKind::Rook), CAPTURE_MAX);

        assert_eq!(
            history.capture_score(piece, square(20), Some(PieceKind::Rook)),
            CAPTURE_MAX
        );
        assert_eq!(
            history.capture_score(piece, square(20), Some(PieceKind::Queen)),
            0
        );
        assert_eq!(history.capture_score(piece, square(20), None), 0);
        assert_eq!(
            history.capture_score(
                Piece::new(Color::Black, PieceKind::Knight),
                square(20),
                Some(PieceKind::Rook)
            ),
            0
        );
    }

    #[test]
    fn pawn_history_penalties_are_weaker_than_bonuses() {
        let history = SharedHistory::new();
        let piece = Piece::new(Color::White, PieceKind::Knight);

        history.update_pawn(0x1234, piece, square(20), 1_000);
        let after_bonus = history.pawn_score(0x1234, piece, square(20));
        assert_eq!(after_bonus, 1_000 * 1_104 / 1_024);

        let symmetric = SharedHistory::new();
        symmetric.update_pawn(0x1234, piece, square(20), -1_000);
        let after_malus = symmetric.pawn_score(0x1234, piece, square(20));
        assert_eq!(after_malus, -(1_000 * 459 / 1_024));
        assert!(
            after_malus.abs() < after_bonus.abs(),
            "a malus of equal magnitude must move the entry less than a bonus"
        );
    }

    #[test]
    fn pawn_history_is_keyed_on_the_pawn_structure() {
        let history = SharedHistory::new();
        let piece = Piece::new(Color::White, PieceKind::Knight);
        history.update_pawn(0x1111, piece, square(20), PAWN_MAX);

        assert!(history.pawn_score(0x1111, piece, square(20)) > 0);
        assert_eq!(history.pawn_score(0x2222, piece, square(20)), 0);
    }

    #[test]
    fn every_hash_keyed_table_indexes_a_key_the_same_way_regardless_of_pool_width() {
        // The bucket MASK is what makes the search reproducible. Both tables used to be
        // sized `BASE * nextPow2(threads)`, so the mask changed with `Threads`, and a
        // key that collided at the narrow size but not the wide one changed the corrhist
        // residual, the static eval, and therefore the tree -- with every helper parked.
        // Sizing is now a compile-time constant, so the only thing that can reintroduce
        // the coupling is a deliberate edit, and this test is what catches it.
        assert_eq!(SharedHistory::new().pawn_bucket_count(), PAWN_BUCKETS);
        assert_eq!(
            SharedHistory::new().correction_bucket_count(),
            CORRECTION_BUCKETS
        );
        // Powers of two so indexing stays a single mask rather than a modulo.
        assert!(PAWN_BUCKETS.is_power_of_two());
        assert!(CORRECTION_BUCKETS.is_power_of_two());
        assert_eq!(super::PAWN_BUCKET_MASK, PAWN_BUCKETS as u64 - 1);
        assert_eq!(super::CORRECTION_BUCKET_MASK, CORRECTION_BUCKETS as u64 - 1);

        // Two tables built by two independently-sized pools must land the same key in
        // the same bucket. `SharedHistory::new` takes no thread count at all now, so
        // this is the behavioural statement of that fact.
        let piece = Piece::new(Color::White, PieceKind::Knight);
        let narrow = SharedHistory::new();
        let wide = SharedHistory::new();
        // A key far above both bucket counts, so any surviving mask difference shows.
        let key = 0xDEAD_BEEF_1234_5678u64;
        narrow.update_pawn(key, piece, square(20), PAWN_MAX);
        wide.update_pawn(key, piece, square(20), PAWN_MAX);
        narrow.update_correction(CORRECTION_PAWN, key, Color::White, CORRECTION_MAX);
        wide.update_correction(CORRECTION_PAWN, key, Color::White, CORRECTION_MAX);
        for other in [
            key ^ (PAWN_BUCKETS as u64),
            key ^ (CORRECTION_BUCKETS as u64),
        ] {
            assert_eq!(
                narrow.pawn_score(other, piece, square(20)),
                wide.pawn_score(other, piece, square(20)),
                "a key that aliases at one size must alias identically at every pool width"
            );
            assert_eq!(
                narrow.correction_score(CORRECTION_PAWN, other, Color::White),
                wide.correction_score(CORRECTION_PAWN, other, Color::White)
            );
        }
    }

    #[test]
    fn clear_resets_every_table() {
        let history = SharedHistory::new();
        let piece = Piece::new(Color::White, PieceKind::Knight);
        history.update_butterfly(Color::White, first_move(), BUTTERFLY_MAX);
        history.update_capture(piece, square(20), Some(PieceKind::Rook), CAPTURE_MAX);
        history.update_pawn(0x1111, piece, square(20), PAWN_MAX);

        history.clear();

        assert_eq!(history.butterfly_score(Color::White, first_move()), 0);
        assert_eq!(
            history.capture_score(piece, square(20), Some(PieceKind::Rook)),
            0
        );
        assert_eq!(history.pawn_score(0x1111, piece, square(20)), 0);
    }

    #[test]
    fn concurrent_updates_stay_inside_the_saturation_bound() {
        let history = Arc::new(SharedHistory::new());
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let history = Arc::clone(&history);
                thread::spawn(move || {
                    let piece = Piece::new(Color::White, PieceKind::Knight);
                    for _ in 0..2_000 {
                        let sign = if worker % 2 == 0 { 1 } else { -1 };
                        history.update_butterfly(Color::White, first_move(), sign * 4_000);
                        history.update_capture(
                            piece,
                            square(20),
                            Some(PieceKind::Rook),
                            sign * 4_000,
                        );
                        history.update_pawn(0x1111, piece, square(20), sign * 4_000);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("history worker should not panic");
        }

        let piece = Piece::new(Color::White, PieceKind::Knight);
        assert!(history.butterfly_score(Color::White, first_move()).abs() <= BUTTERFLY_MAX);
        assert!(
            history
                .capture_score(piece, square(20), Some(PieceKind::Rook))
                .abs()
                <= CAPTURE_MAX
        );
        assert!(history.pawn_score(0x1111, piece, square(20)).abs() <= PAWN_MAX);
    }

    #[test]
    fn continuation_history_uses_the_reference_ply_set_and_weights() {
        // {1, 2, 4, 6} is the community consensus subset for a new engine (Berserk,
        // Stormphrax). The dense 1-6 set is a late refinement; plies 3 and 5 carry
        // the two weakest weights in the reference table and are deliberately not built here.
        assert_eq!(CONTINUATION_PLIES, [1, 2, 4, 6]);
        // Reference continuation-history bonus entries for exactly those plies, in 1024ths.
        assert_eq!(CONTINUATION_WEIGHTS, [1_040, 780, 502, 418]);
        assert!(
            CONTINUATION_WEIGHTS
                .windows(2)
                .all(|pair| pair[0] > pair[1]),
            "over the {{1,2,4,6}} subset the reference weights decrease with distance"
        );
    }

    #[test]
    fn continuation_history_is_keyed_on_the_previous_move() {
        let history = SharedHistory::new();
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let rook = Piece::new(Color::Black, PieceKind::Rook);
        let previous = ContinuationKey::new(rook, square(20));
        let other_piece = ContinuationKey::new(knight, square(20));
        let other_square = ContinuationKey::new(rook, square(21));

        history.update_continuation(previous, knight, square(30), CONTINUATION_MAX);

        assert_eq!(
            history.continuation_score(previous, knight, square(30)),
            CONTINUATION_MAX
        );
        // A different previous piece, previous square, current piece, or current square
        // must all be independent entries.
        assert_eq!(
            history.continuation_score(other_piece, knight, square(30)),
            0
        );
        assert_eq!(
            history.continuation_score(other_square, knight, square(30)),
            0
        );
        assert_eq!(history.continuation_score(previous, rook, square(30)), 0);
        assert_eq!(history.continuation_score(previous, knight, square(31)), 0);
    }

    #[test]
    fn continuation_history_saturates_at_its_own_bound() {
        let history = SharedHistory::new();
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let previous = ContinuationKey::new(knight, square(20));

        history.update_continuation(previous, knight, square(30), i32::MAX);
        assert_eq!(
            history.continuation_score(previous, knight, square(30)),
            CONTINUATION_MAX
        );
        history.update_continuation(previous, knight, square(30), i32::MIN);
        assert_eq!(
            history.continuation_score(previous, knight, square(30)),
            -CONTINUATION_MAX
        );
        // The bound must remain representable in the i16 the table stores.
        assert!(CONTINUATION_MAX <= i32::from(i16::MAX));
    }

    #[test]
    fn continuation_history_is_fixed_size_and_cache_line_aligned() {
        // Continuation history does NOT scale with thread count -- it is keyed on the
        // previous move, not on a hashed structure, so there is no collision rate to
        // hold flat. Sizing it from a gravity bound instead of its real dimensions is
        // the mission AGENTS.md 4.54 trap that cost 18% NPS on the pawn table.
        let one = SharedHistory::new();
        let eight = SharedHistory::new();
        assert_eq!(one.continuation_len(), eight.continuation_len());
        assert_eq!(one.continuation_len(), 12 * 64 * 12 * 64);

        assert_eq!(
            one.continuation_base_address() % 64,
            0,
            "the table base must sit on a cache line"
        );
        // Each previous-move plane is 12*64 i16 = 1,536 bytes = exactly 24 cache lines,
        // so every plane starts on a line boundary without per-entry padding (which
        // would multiply the table by 32x for no ordering benefit).
        assert_eq!((12 * 64 * size_of::<i16>()) % 64, 0);
    }

    #[test]
    fn continuation_history_clears_with_the_other_tables() {
        let history = SharedHistory::new();
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let previous = ContinuationKey::new(knight, square(20));
        history.update_continuation(previous, knight, square(30), CONTINUATION_MAX);

        history.clear();

        assert_eq!(history.continuation_score(previous, knight, square(30)), 0);
    }

    #[test]
    fn concurrent_continuation_updates_stay_inside_the_saturation_bound() {
        let history = Arc::new(SharedHistory::new());
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let previous = ContinuationKey::new(knight, square(20));
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let history = Arc::clone(&history);
                thread::spawn(move || {
                    for _ in 0..2_000 {
                        let sign = if worker % 2 == 0 { 1 } else { -1 };
                        history.update_continuation(previous, knight, square(30), sign * 12_000);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("history worker should not panic");
        }

        assert!(
            history
                .continuation_score(previous, knight, square(30))
                .abs()
                <= CONTINUATION_MAX
        );
    }

    #[test]
    fn captured_kind_reports_the_real_victim() {
        let position = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .expect("valid FEN");
        // e5xg6 captures a pawn.
        let capture = Move::new(square(36), square(46), MoveFlag::CAPTURE);
        assert_eq!(captured_kind(&position, capture), Some(PieceKind::Pawn));

        // e5-g4 is a genuine quiet knight move: g4 is empty in this position.
        let quiet = Move::new(square(36), square(30), MoveFlag::QUIET);
        assert_eq!(captured_kind(&position, quiet), None);
    }

    #[test]
    fn correction_history_is_keyed_on_the_position_hash_and_the_side_to_move() {
        let history = SharedHistory::new();
        history.update_correction(CORRECTION_PAWN, 0xAAAA, Color::White, 400);

        assert!(history.correction_score(CORRECTION_PAWN, 0xAAAA, Color::White) > 0);
        // A different key, a different color, and a different SOURCE must all be
        // independent entries. The source independence is the one that matters most:
        // the four variants are four separate tables consulted with four different
        // Zobrist keys, and folding them would make every position teach every variant.
        assert_eq!(
            history.correction_score(CORRECTION_PAWN, 0xBBBB, Color::White),
            0
        );
        assert_eq!(
            history.correction_score(CORRECTION_PAWN, 0xAAAA, Color::Black),
            0
        );
        for source in [CORRECTION_MINOR, CORRECTION_MAJOR, CORRECTION_MATERIAL] {
            assert_eq!(history.correction_score(source, 0xAAAA, Color::White), 0);
        }
    }

    #[test]
    fn correction_history_applies_the_reference_per_source_update_weights() {
        // Reference: pawn takes `bonus`, minor `bonus * 150/128`, non-pawn
        // `bonus * 186/128`. Manifold's major and material keys both characterise the
        // non-pawn position, so both take the non-pawn weight.
        assert_eq!(CORRECTION_UPDATE_WEIGHTS, [128, 150, 186, 186]);
        for (source, weight) in CORRECTION_UPDATE_WEIGHTS.iter().enumerate() {
            let history = SharedHistory::new();
            history.update_correction(source, 0x1234, Color::White, 128);
            assert_eq!(
                history.correction_score(source, 0x1234, Color::White),
                *weight,
                "source {source} must scale its bonus by {weight}/128"
            );
        }
    }

    #[test]
    fn correction_history_saturates_at_the_reference_limit() {
        let history = SharedHistory::new();
        history.update_correction(CORRECTION_PAWN, 0x1234, Color::White, i32::MAX);
        assert_eq!(
            history.correction_score(CORRECTION_PAWN, 0x1234, Color::White),
            CORRECTION_MAX
        );
        history.update_correction(CORRECTION_PAWN, 0x1234, Color::White, i32::MIN);
        assert_eq!(
            history.correction_score(CORRECTION_PAWN, 0x1234, Color::White),
            -CORRECTION_MAX
        );
        // 1024 is the reference CORRECTION_HISTORY_LIMIT and is deliberately three orders
        // of magnitude below the ordering bounds: a corrhist entry is a mean residual
        // in eval units that gets divided by 131,072, not an ordering score. Giving the
        // two families each other's constants is how this feature goes silently wrong.
        assert_eq!(CORRECTION_MAX, 1_024);
        const { assert!(CORRECTION_MAX < BUTTERFLY_MAX) };
    }

    #[test]
    fn correction_history_uses_the_reference_blend_weights() {
        // Reference: 15341*pawn + 10569*minor + 12906*(nonPawnWhite + nonPawnBlack),
        // then `8761 *` the summed continuation entries, all divided by 131,072.
        assert_eq!(CORRECTION_WEIGHTS, [15_341, 10_569, 12_906, 12_906]);
        assert_eq!(super::CORRECTION_CONTINUATION_WEIGHT, 8_761);
        assert_eq!(super::CORRECTION_SCALE, 131_072);
        // A single source saturated in one direction must not by itself be able to move
        // the eval more than a few centipawns: 15341 * 1024 / 131072 = 119. Corrhist is
        // a nudge, not an override.
        let widest = CORRECTION_WEIGHTS.into_iter().max().expect("nonempty");
        assert!(
            widest * CORRECTION_MAX / super::CORRECTION_SCALE < 200,
            "one saturated source must not dominate the static eval"
        );
    }

    #[test]
    fn continuation_correction_history_looks_back_two_and_four_plies() {
        // NOT 1 and 2. Ordering continuation history asks "what refutes the opponent's
        // last move" and steps one ply; correction continuation history looks at OUR
        // OWN previous moves and steps two. Getting this wrong indexes the residual on
        // the opponent's move and is invisible in every behavioural test.
        assert_eq!(CORRECTION_CONTINUATION_PLIES, [2, 4]);
        assert_eq!(CORRECTION_CONTINUATION_UPDATE_WEIGHTS, [130, 70]);
        assert!(
            CORRECTION_CONTINUATION_UPDATE_WEIGHTS[0] > CORRECTION_CONTINUATION_UPDATE_WEIGHTS[1],
            "the nearer ply must carry the stronger update"
        );
    }

    #[test]
    fn continuation_correction_history_is_indexed_by_plane_and_entry_independently() {
        let history = SharedHistory::new();
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let rook = Piece::new(Color::Black, PieceKind::Rook);
        let plane = ContinuationKey::new(rook, square(20));
        let entry = ContinuationKey::new(knight, square(30));
        let other_plane = ContinuationKey::new(rook, square(21));
        let other_entry = ContinuationKey::new(knight, square(31));

        history.update_correction_continuation(0, plane, entry, 512);
        assert!(history.correction_continuation_score(0, plane, entry) > 0);
        // The plane comes from the move 2 (or 4) plies back and the entry from the move
        // 1 ply back. They are different moves, so swapping either must land elsewhere.
        assert_eq!(
            history.correction_continuation_score(0, other_plane, entry),
            0
        );
        assert_eq!(
            history.correction_continuation_score(0, plane, other_entry),
            0
        );
        // Ply 2 and ply 4 are separate tables.
        assert_eq!(history.correction_continuation_score(1, plane, entry), 0);
    }

    #[test]
    fn continuation_correction_history_is_a_separate_table_from_ordering_continuation() {
        // Same shape, different quantity: an eval residual bounded by CORRECTION_MAX
        // versus an ordering score bounded by CONTINUATION_MAX. Aliasing them would let
        // a 30,000-magnitude ordering score be read as a 1,024-magnitude eval residual.
        let history = SharedHistory::new();
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let plane = ContinuationKey::new(knight, square(20));
        let entry = ContinuationKey::new(knight, square(30));

        history.update_continuation_at(0, plane, knight, square(30), CONTINUATION_MAX);
        assert_eq!(history.correction_continuation_score(0, plane, entry), 0);

        history.update_correction_continuation(0, plane, entry, i32::MAX);
        assert_eq!(
            history.correction_continuation_score(0, plane, entry),
            CORRECTION_MAX
        );
        assert_eq!(
            history.continuation_score_at(0, plane, knight, square(30)),
            CONTINUATION_MAX
        );
    }

    #[test]
    fn correction_history_is_sized_by_its_bucket_count_and_stays_inside_l2() {
        // Corrhist is SHARED, which is the point: one worker consumes correction values
        // another paid to search for. Sharing is what the atomics buy; the table SIZE is
        // a separate question, and it is fixed so the mask cannot depend on `Threads`.
        // Sized by the BUCKET COUNT, never by the gravity bound (AGENTS.md 4.54 trap 2).
        // 16,384 buckets x 2 colors x i16 = 64 KiB per table, 256 KiB for all four --
        // the whole point of not letting it scale to 2 MiB at eight threads.
        assert_eq!(CORRECTION_BUCKETS * 2 * size_of::<i16>(), 64 * 1024);
        assert_ne!(CORRECTION_BUCKETS, CORRECTION_MAX as usize);
    }

    #[test]
    fn correction_history_clears_with_the_other_tables() {
        let history = SharedHistory::new();
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let plane = ContinuationKey::new(knight, square(20));
        let entry = ContinuationKey::new(knight, square(30));
        for source in 0..CORRECTION_SOURCES {
            history.update_correction(source, 0x1234, Color::White, CORRECTION_MAX);
        }
        history.update_correction_continuation(0, plane, entry, CORRECTION_MAX);

        history.clear();

        for source in 0..CORRECTION_SOURCES {
            assert_eq!(history.correction_score(source, 0x1234, Color::White), 0);
        }
        assert_eq!(history.correction_continuation_score(0, plane, entry), 0);
    }

    #[test]
    fn concurrent_correction_updates_stay_inside_the_saturation_bound() {
        let history = Arc::new(SharedHistory::new());
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let history = Arc::clone(&history);
                thread::spawn(move || {
                    for _ in 0..2_000 {
                        let sign = if worker % 2 == 0 { 1 } else { -1 };
                        for source in 0..CORRECTION_SOURCES {
                            history.update_correction(source, 0x1234, Color::White, sign * 500);
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("history worker should not panic");
        }

        for source in 0..CORRECTION_SOURCES {
            assert!(history.correction_score(source, 0x1234, Color::White).abs() <= CORRECTION_MAX);
        }
    }
}
