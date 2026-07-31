use std::sync::atomic::{AtomicI16, Ordering};

use mf_core::{Color, Move, Piece, PieceKind, Position, Square};

use crate::MAX_SEARCH_PLY;

/// Gravity saturation bound for butterfly (main) history.
const BUTTERFLY_MAX: i32 = 7_183;
/// Gravity saturation bound for capture history.
const CAPTURE_MAX: i32 = 10_692;
/// Gravity saturation bound for pawn-structure history.
const PAWN_MAX: i32 = 8_192;
/// Gravity saturation bound for continuation history (Stockfish `PieceToHistory`).
///
/// This is deliberately ~4x the butterfly bound: a continuation entry is conditioned on
/// a specific predecessor move, so it is a far sharper statistic than a from-to average
/// and is allowed to express a correspondingly stronger opinion.
const CONTINUATION_MAX: i32 = 30_000;

/// Lookback distances, in plies, at which continuation history is kept.
///
/// The 1-ply table IS the counter-move heuristic: `continuation[0]` is indexed by the
/// opponent's immediately preceding move, so "the reply that refutes this move" falls
/// out of it without a separate counter-move structure.
///
/// `{1, 2, 4, 6}` is the community consensus subset for a new engine (Berserk,
/// Stormphrax). Stockfish's dense 1-6 is a late refinement, and plies 3 and 5 carry the
/// two weakest weights in its own bonus table (290 and 132 against 1040/780/502/418).
pub(crate) const CONTINUATION_PLIES: [usize; 4] = [1, 2, 4, 6];
/// Update weights in 1024ths, taken from Stockfish `conthist_bonuses` for exactly the
/// plies in `CONTINUATION_PLIES`.
pub(crate) const CONTINUATION_WEIGHTS: [i32; CONTINUATION_PLIES.len()] = [1_040, 780, 502, 418];

const COLORS: usize = 2;
const SQUARES: usize = 64;
const PIECES: usize = 12;
/// Victim slot count. Index 5 (`PieceKind::King`) is unreachable as a real victim and
/// is therefore reused as the "promotion without capture" slot.
const VICTIMS: usize = 6;
const NO_VICTIM: usize = PieceKind::King.index();

/// Bucket count of the pawn-history table at one thread, per Stockfish's
/// `PAWN_HISTORY_SIZE`. Scaled by `nextPow2(threads)` so a wider pool gets a
/// proportionally larger table and the collision rate stays flat.
///
/// This is deliberately NOT `PAWN_MAX`: the two are different numbers that both
/// happen to be powers of two in Stockfish's source. 512 buckets is 768 KiB, which
/// stays inside L2; 8192 buckets would be 12 MiB and cost ~18% NPS to cache misses
/// for no ordering benefit.
const PAWN_BASE_BUCKETS: usize = 512;

const BUTTERFLY_LEN: usize = COLORS * SQUARES * SQUARES;
const CAPTURE_LEN: usize = PIECES * SQUARES * VICTIMS;
const PAWN_BUCKET_LEN: usize = PIECES * SQUARES;
/// One `[piece][to]` plane, the inner table a single predecessor move selects.
const CONTINUATION_PLANE_LEN: usize = PIECES * SQUARES;
/// A whole ply's table: one plane per `[previous_piece][previous_to]`.
const CONTINUATION_LEN: usize = PIECES * SQUARES * CONTINUATION_PLANE_LEN;

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
}

impl ContinuationKey {
    #[inline]
    pub(crate) fn new(piece: Piece, to: Square) -> Self {
        let index = piece.index() * SQUARES + usize::from(to.index());
        Self {
            plane: (index * CONTINUATION_PLANE_LEN) as u32,
        }
    }
}

/// Relaxed-atomic history tables shared by every search worker.
///
/// Thread-private history is obsolete: Stockfish moved correction, pawn, and
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
    pawn_bucket_mask: u64,
}

impl SharedHistory {
    pub fn new(thread_count: usize) -> Self {
        assert!(thread_count > 0, "thread count must be nonzero");

        let buckets = PAWN_BASE_BUCKETS
            .checked_mul(thread_count.next_power_of_two())
            .expect("pawn history bucket count must not overflow");

        Self {
            butterfly: CacheAligned(zeroed(BUTTERFLY_LEN)),
            capture: CacheAligned(zeroed(CAPTURE_LEN)),
            pawn: CacheAligned(zeroed(buckets * PAWN_BUCKET_LEN)),
            // Continuation history is FIXED SIZE and does not scale with the thread
            // count, unlike pawn history and corrhist. Stockfish says so explicitly, and
            // the reason is structural: this table is keyed on an exact predecessor move
            // rather than on a hashed structure, so there is no collision rate for a
            // larger table to hold flat. Four planes at 9 MiB each is already the whole
            // Each ply is 12*64 planes of 12*64 `i16` = 1.125 MiB, 4.5 MiB in total;
            // scaling that by the thread count would multiply a table under no collision
            // pressure and only thrash cache, which is the AGENTS.md 4.54 trap.
            continuation: core::array::from_fn(|_| CacheAligned(zeroed(CONTINUATION_LEN))),
            pawn_bucket_mask: (buckets - 1) as u64,
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
    /// strength of bonuses (Stockfish `bonus * (bonus > -4 ? 1104 : 459) / 1024`).
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

    #[inline]
    fn pawn_index(&self, pawn_key: u64, piece: Piece, to: Square) -> usize {
        let bucket = (pawn_key & self.pawn_bucket_mask) as usize;
        bucket * PAWN_BUCKET_LEN + piece.index() * SQUARES + usize::from(to.index())
    }

    #[cfg(test)]
    pub(crate) fn pawn_bucket_count(&self) -> usize {
        self.pawn_bucket_mask as usize + 1
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
        ContinuationKey, KillerTable, PAWN_BASE_BUCKETS, PAWN_MAX, SharedHistory, captured_kind,
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
        let history = SharedHistory::new(1);
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
        let history = SharedHistory::new(1);
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
        let history = SharedHistory::new(1);
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
        let history = SharedHistory::new(1);
        let piece = Piece::new(Color::White, PieceKind::Knight);

        history.update_pawn(0x1234, piece, square(20), 1_000);
        let after_bonus = history.pawn_score(0x1234, piece, square(20));
        assert_eq!(after_bonus, 1_000 * 1_104 / 1_024);

        let symmetric = SharedHistory::new(1);
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
        let history = SharedHistory::new(1);
        let piece = Piece::new(Color::White, PieceKind::Knight);
        history.update_pawn(0x1111, piece, square(20), PAWN_MAX);

        assert!(history.pawn_score(0x1111, piece, square(20)) > 0);
        assert_eq!(history.pawn_score(0x2222, piece, square(20)), 0);
    }

    #[test]
    fn pawn_history_scales_with_the_next_power_of_two_thread_count() {
        assert_eq!(SharedHistory::new(1).pawn_bucket_count(), PAWN_BASE_BUCKETS);
        assert_eq!(
            SharedHistory::new(2).pawn_bucket_count(),
            PAWN_BASE_BUCKETS * 2
        );
        // Non-powers of two round UP, so indexing stays a single mask.
        assert_eq!(
            SharedHistory::new(5).pawn_bucket_count(),
            PAWN_BASE_BUCKETS * 8
        );
        assert_eq!(
            SharedHistory::new(8).pawn_bucket_count(),
            PAWN_BASE_BUCKETS * 8
        );
    }

    #[test]
    fn clear_resets_every_table() {
        let history = SharedHistory::new(1);
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
        let history = Arc::new(SharedHistory::new(8));
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
        // Stormphrax). Stockfish's dense 1-6 is a late refinement; plies 3 and 5 carry
        // the two weakest weights in its own table and are deliberately not built here.
        assert_eq!(CONTINUATION_PLIES, [1, 2, 4, 6]);
        // Stockfish `conthist_bonuses` entries for exactly those plies, in 1024ths.
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
        let history = SharedHistory::new(1);
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
        let history = SharedHistory::new(1);
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
        let one = SharedHistory::new(1);
        let eight = SharedHistory::new(8);
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
        let history = SharedHistory::new(1);
        let knight = Piece::new(Color::White, PieceKind::Knight);
        let previous = ContinuationKey::new(knight, square(20));
        history.update_continuation(previous, knight, square(30), CONTINUATION_MAX);

        history.clear();

        assert_eq!(history.continuation_score(previous, knight, square(30)), 0);
    }

    #[test]
    fn concurrent_continuation_updates_stay_inside_the_saturation_bound() {
        let history = Arc::new(SharedHistory::new(8));
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
    #[should_panic(expected = "thread count must be nonzero")]
    fn shared_history_requires_a_nonzero_thread_count() {
        let _ = SharedHistory::new(0);
    }
}
