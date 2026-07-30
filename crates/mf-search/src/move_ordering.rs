use mf_core::{
    Color, Move, PieceKind, Position, generate_pseudo_legal_moves, material_value,
    static_exchange_evaluation,
};

use crate::evaluation::piece_square_value;
use crate::history::{SharedHistory, captured_kind};

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

#[derive(Clone, Copy)]
pub(crate) struct OrderingContext<'a> {
    pub(crate) history: &'a SharedHistory,
    pub(crate) pawn_key: u64,
    pub(crate) use_butterfly_history: bool,
    pub(crate) use_capture_history: bool,
    pub(crate) use_pawn_history: bool,
}

impl OrderingContext<'_> {
    /// Combined quiet history used to SORT moves.
    ///
    /// Pawn history contributes here but NOT to `quiet_history`: sorting only needs a
    /// relative ranking, so a coarse extra signal helps break ties, whereas LMR and
    /// history pruning compare the value against absolute thresholds tuned for the
    /// butterfly range. Feeding the summed value to those consumers made every quiet
    /// look better than it was and cost 1.6-4.6% of bench nodes depending on weight.
    pub(crate) fn ordering_history(&self, position: &Position, color: Color, mv: Move) -> i32 {
        let mut score = self.quiet_history(position, color, mv);
        if self.use_pawn_history
            && let Some(piece) = position.piece_at(mv.from())
        {
            score += PAWN_HISTORY_WEIGHT * self.history.pawn_score(self.pawn_key, piece, mv.to());
        }
        score
    }

    /// Quiet history on the butterfly scale: the value LMR scales its reduction by and
    /// that history pruning thresholds against.
    pub(crate) fn quiet_history(&self, _position: &Position, color: Color, mv: Move) -> i32 {
        if self.use_butterfly_history {
            2 * self.history.butterfly_score(color, mv)
        } else {
            0
        }
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
