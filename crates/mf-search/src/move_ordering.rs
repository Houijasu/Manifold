use mf_core::{
    Color, Move, Piece, PieceKind, Position, Square, generate_pseudo_legal_moves, material_value,
    static_exchange_evaluation,
};

use crate::evaluation::piece_square_value;
use crate::history::{CONTINUATION_PLIES, ContinuationKey, SharedHistory, captured_kind};

/// Everything the move picker needs to score a move, bundled so the read sites stay
/// in one place.
///
/// Each `use_*` flag gates ONLY the read of the table it names. History MAINTENANCE is
/// unconditional (mission AGENTS.md 4.4), so turning a table off changes which
/// information the search consumes, never which information it collects.
/// Weight of pawn history relative to butterfly history in the combined quiet score.
///
/// Pawn history is a coarser signal than butterfly history: it is keyed on a hashed
/// pawn structure, so unrelated positions share buckets. Weighting it equally with
/// butterfly cost 1.6% of bench nodes.
const PAWN_HISTORY_WEIGHT: i32 = 2;

/// `statScore` weights in 1024ths (Stockfish: `2252*main + 1126*cont1 + 1093*cont2`).
///
/// The butterfly weight is pinned to **2048**, not the reference 2252, so that
/// `2048 * butterfly / 1024` is *exactly* the `2 * butterfly` this engine used before
/// continuation history existed. That keeps `UseContHistory=false` bit-for-bit
/// identical to the M4-F1 build: a toggle must gate only its own technique
/// (mission AGENTS.md 4.4), and adopting 2252 here would silently rescale the LMR
/// reduction in the arm that is supposed to be the control. Retuning it belongs with
/// the LMR retune M5-F6 already owns.
const STAT_SCORE_BUTTERFLY_WEIGHT: i32 = 2_048;
const STAT_SCORE_CONTINUATION_WEIGHTS: [i32; 2] = [1_126, 1_093];

#[derive(Clone, Copy)]
pub(crate) struct OrderingContext<'a> {
    pub(crate) history: &'a SharedHistory,
    pub(crate) pawn_key: u64,
    /// The predecessor plane at each lookback distance in `CONTINUATION_PLIES`, or
    /// `None` where the stack does not reach back that far or a null move broke the
    /// chain. Resolved once per node when the stack is read, never per scored move.
    pub(crate) continuation: [Option<ContinuationKey>; CONTINUATION_PLIES.len()],
    pub(crate) use_butterfly_history: bool,
    pub(crate) use_capture_history: bool,
    pub(crate) use_pawn_history: bool,
    pub(crate) use_continuation_history: bool,
}

impl OrderingContext<'_> {
    /// Combined quiet history used to SORT moves.
    ///
    /// Sorting only needs a relative ranking, so every table contributes here. The two
    /// consumers below deliberately read narrower, differently-scaled quantities,
    /// because they compare against absolute thresholds rather than against each other.
    pub(crate) fn ordering_history(&self, position: &Position, color: Color, mv: Move) -> i32 {
        let mut score = if self.use_butterfly_history {
            2 * self.history.butterfly_score(color, mv)
        } else {
            0
        };
        let Some(piece) = position.piece_at(mv.from()) else {
            return score;
        };
        if self.use_pawn_history {
            score += PAWN_HISTORY_WEIGHT * self.history.pawn_score(self.pawn_key, piece, mv.to());
        }
        score + self.continuation_sum(piece, mv.to())
    }

    /// LMR's `statScore`: the value the reduction formula scales itself by.
    ///
    /// Only the 1- and 2-ply continuation tables appear, matching the reference. The 4-
    /// and 6-ply tables are ordering and update signals only.
    pub(crate) fn quiet_history(&self, position: &Position, color: Color, mv: Move) -> i32 {
        let mut score = if self.use_butterfly_history {
            STAT_SCORE_BUTTERFLY_WEIGHT * self.history.butterfly_score(color, mv)
        } else {
            0
        };
        if self.use_continuation_history
            && let Some(piece) = position.piece_at(mv.from())
        {
            for (slot, weight) in STAT_SCORE_CONTINUATION_WEIGHTS.into_iter().enumerate() {
                if let Some(previous) = self.continuation[slot] {
                    score += weight
                        * self
                            .history
                            .continuation_score_at(slot, previous, piece, mv.to());
                }
            }
        }
        score / 1_024
    }

    /// The signal history pruning thresholds against: the SUM of the 1- and 2-ply
    /// continuation tables and pawn history, exactly as in the reference.
    ///
    /// This is deliberately NOT `quiet_history`. M4-F1 measured history pruning at
    /// **-103.68 +/- 46.31 Elo** against **+133.61 +/- 44.43** with the toggle off --
    /// a ~237 Elo swing -- while thresholding a single butterfly statistic, and while
    /// *saving* 3.95% of bench nodes (mission AGENTS.md 4.53). Butterfly history
    /// averages a from-to pair over every context it ever occurred in, which is far too
    /// noisy to drop a move on; a continuation entry is conditioned on an exact
    /// predecessor move and is the signal the reference threshold was built for.
    pub(crate) fn pruning_history(&self, position: &Position, mv: Move) -> i32 {
        let Some(piece) = position.piece_at(mv.from()) else {
            return 0;
        };
        let mut score = if self.use_pawn_history {
            self.history.pawn_score(self.pawn_key, piece, mv.to())
        } else {
            0
        };
        if self.use_continuation_history {
            for slot in 0..STAT_SCORE_CONTINUATION_WEIGHTS.len() {
                if let Some(previous) = self.continuation[slot] {
                    score += self
                        .history
                        .continuation_score_at(slot, previous, piece, mv.to());
                }
            }
        }
        score
    }

    /// Unweighted sum across every kept lookback distance, used for move ordering.
    #[inline]
    fn continuation_sum(&self, piece: Piece, to: Square) -> i32 {
        if !self.use_continuation_history {
            return 0;
        }
        let mut score = 0;
        for (slot, previous) in self.continuation.iter().enumerate() {
            if let Some(previous) = *previous {
                score += self
                    .history
                    .continuation_score_at(slot, previous, piece, to);
            }
        }
        score
    }

    /// Capture history for a move, or zero when the table is disabled.
    pub(crate) fn capture_history(&self, position: &Position, mv: Move) -> i32 {
        if !self.use_capture_history {
            return 0;
        }
        let Some(piece) = position.piece_at(mv.from()) else {
            return 0;
        };
        self.history
            .capture_score(piece, mv.to(), captured_kind(position, mv))
    }
}

pub(crate) struct MovePicker {
    tt_move: Option<Move>,
    good_captures: Vec<Move>,
    quiets: Vec<Move>,
    bad_captures: Vec<Move>,
    stage: Stage,
    index: usize,
}

#[derive(Clone, Copy)]
enum Stage {
    Tt,
    GoodCaptures,
    Quiets,
    BadCaptures,
    Done,
}

impl MovePicker {
    pub(crate) fn new(
        position: &Position,
        tt_move: Option<Move>,
        killers: [Option<Move>; 2],
        ordering: OrderingContext<'_>,
    ) -> Self {
        let pseudo_legal = generate_pseudo_legal_moves(position);
        let tt_move = tt_move.filter(|mv| pseudo_legal.contains(mv));
        let mut good_captures = Vec::new();
        let mut quiets = Vec::new();
        let mut bad_captures = Vec::new();

        for &mv in &pseudo_legal {
            if Some(mv) == tt_move {
                continue;
            }
            if mv.flag().is_capture() || mv.flag().promotion().is_some() {
                if static_exchange_evaluation(position, mv) >= 0 {
                    good_captures.push(mv);
                } else {
                    bad_captures.push(mv);
                }
            } else {
                quiets.push(mv);
            }
        }

        // `sort_by_cached_key` scores each move ONCE. `sort_unstable_by_key` would
        // re-evaluate the key on every comparison, which turned each history read into
        // O(n log n) dependent loads and cost ~12% NPS.
        let color = position.side_to_move();
        good_captures
            .sort_by_cached_key(|&mv| core::cmp::Reverse(capture_score(position, mv, ordering)));
        bad_captures
            .sort_by_cached_key(|&mv| core::cmp::Reverse(capture_score(position, mv, ordering)));
        quiets.sort_by_cached_key(|&mv| {
            (
                core::cmp::Reverse(quiet_score(position, mv, killers, color, ordering)),
                mv.raw(),
            )
        });

        Self {
            tt_move,
            good_captures,
            quiets,
            bad_captures,
            stage: Stage::Tt,
            index: 0,
        }
    }
}

impl Iterator for MovePicker {
    type Item = Move;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.stage {
                Stage::Tt => {
                    self.stage = Stage::GoodCaptures;
                    if self.tt_move.is_some() {
                        return self.tt_move;
                    }
                }
                Stage::GoodCaptures => {
                    if let Some(mv) = self.good_captures.get(self.index).copied() {
                        self.index += 1;
                        return Some(mv);
                    }
                    self.stage = Stage::Quiets;
                    self.index = 0;
                }
                Stage::Quiets => {
                    if let Some(mv) = self.quiets.get(self.index).copied() {
                        self.index += 1;
                        return Some(mv);
                    }
                    self.stage = Stage::BadCaptures;
                    self.index = 0;
                }
                Stage::BadCaptures => {
                    if let Some(mv) = self.bad_captures.get(self.index).copied() {
                        self.index += 1;
                        return Some(mv);
                    }
                    self.stage = Stage::Done;
                    self.index = 0;
                }
                Stage::Done => return None,
            }
        }
    }
}

pub(crate) fn quiescence_moves(
    position: &Position,
    see_threshold: i32,
    ordering: OrderingContext<'_>,
) -> Vec<Move> {
    let mut moves: Vec<_> = generate_pseudo_legal_moves(position)
        .iter()
        .copied()
        .filter(|mv| mv.flag().is_capture() || mv.flag().promotion().is_some())
        .filter(|&mv| static_exchange_evaluation(position, mv) >= see_threshold)
        .collect();
    moves.sort_unstable_by_key(|&mv| core::cmp::Reverse(capture_score(position, mv, ordering)));
    moves
}

fn capture_score(position: &Position, mv: Move, ordering: OrderingContext<'_>) -> i32 {
    let victim = if mv.flag().is_en_passant() {
        PieceKind::Pawn
    } else {
        position
            .piece_at(mv.to())
            .map_or(PieceKind::Pawn, |piece| piece.kind())
    };
    let attacker = position
        .piece_at(mv.from())
        .expect("ordered move must have an attacker")
        .kind();
    let promotion = mv.flag().promotion().map_or(0, |kind| {
        material_value(kind) - material_value(PieceKind::Pawn)
    });
    static_exchange_evaluation(position, mv) * 32 + material_value(victim) * 16
        - material_value(attacker)
        + promotion
        + ordering.capture_history(position, mv)
}

fn quiet_score(
    position: &Position,
    mv: Move,
    killers: [Option<Move>; 2],
    color: Color,
    ordering: OrderingContext<'_>,
) -> i32 {
    if killers[0] == Some(mv) {
        return 20_000;
    }
    if killers[1] == Some(mv) {
        return 19_000;
    }
    let piece = position
        .piece_at(mv.from())
        .expect("ordered move must have a moving piece");
    if mv.flag().is_castling() {
        return 1_000;
    }
    ordering.ordering_history(position, color, mv)
        + piece_square_value(piece.kind(), piece.color(), mv.to())
        - piece_square_value(piece.kind(), piece.color(), mv.from())
}
