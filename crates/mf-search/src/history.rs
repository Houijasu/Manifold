use std::sync::atomic::{AtomicI16, Ordering};

use mf_core::{Color, Move, Piece, PieceKind, Position, Square};

use crate::MAX_SEARCH_PLY;

/// Gravity saturation bound for butterfly (main) history.
const BUTTERFLY_MAX: i32 = 7_183;
/// Gravity saturation bound for capture history.
const CAPTURE_MAX: i32 = 10_692;
/// Gravity saturation bound for pawn-structure history.
const PAWN_MAX: i32 = 8_192;

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

/// Forces each table onto its own cache line so concurrent updates to two different
/// tables never contend for the same line.
#[repr(align(64))]
struct CacheAligned<T>(T);

/// Relaxed-atomic history tables shared by every search worker.
///
/// Thread-private history is obsolete: Stockfish moved correction, pawn, and
/// continuation history to shared relaxed-atomic tables between Dec 2025 and Jun 2026,
/// SPRT-verified at 1, 8, 16, and 64 threads. Sharing lets a worker reuse ordering
/// knowledge another worker already paid for. Updates race exactly like the
/// transposition table does: each `apply` performs one load and one store, so a torn
/// interleaving can lose an update but can never store a value outside `[-D, D]`.
pub(crate) struct SharedHistory {
    butterfly: CacheAligned<Box<[AtomicI16]>>,
    capture: CacheAligned<Box<[AtomicI16]>>,
    pawn: CacheAligned<Box<[AtomicI16]>>,
    pawn_bucket_mask: u64,
}

impl SharedHistory {
    pub(crate) fn new(thread_count: usize) -> Self {
        assert!(thread_count > 0, "thread count must be nonzero");

        let buckets = PAWN_BASE_BUCKETS
            .checked_mul(thread_count.next_power_of_two())
            .expect("pawn history bucket count must not overflow");

        Self {
            butterfly: CacheAligned(zeroed(BUTTERFLY_LEN)),
            capture: CacheAligned(zeroed(CAPTURE_LEN)),
            pawn: CacheAligned(zeroed(buckets * PAWN_BUCKET_LEN)),
            pawn_bucket_mask: (buckets - 1) as u64,
        }
    }

    /// Resets every table. Called on `ucinewgame` so a new game does not inherit the
    /// previous game's ordering statistics.
    pub(crate) fn clear(&self) {
        for entry in self
            .butterfly
            .0
            .iter()
            .chain(self.capture.0.iter())
            .chain(self.pawn.0.iter())
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

    #[inline]
    fn pawn_index(&self, pawn_key: u64, piece: Piece, to: Square) -> usize {
        let bucket = (pawn_key & self.pawn_bucket_mask) as usize;
        bucket * PAWN_BUCKET_LEN + piece.index() * SQUARES + usize::from(to.index())
    }

    #[cfg(test)]
    pub(crate) fn pawn_bucket_count(&self) -> usize {
        self.pawn_bucket_mask as usize + 1
    }
}

fn zeroed(len: usize) -> Box<[AtomicI16]> {
    (0..len).map(|_| AtomicI16::new(0)).collect()
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
        BUTTERFLY_MAX, CAPTURE_MAX, KillerTable, PAWN_BASE_BUCKETS, PAWN_MAX, SharedHistory,
        captured_kind,
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
