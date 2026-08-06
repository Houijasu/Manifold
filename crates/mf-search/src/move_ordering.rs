use mf_core::{
    Color, Move, Piece, PieceKind, Position, Square, generate_pseudo_legal_moves, material_value,
    static_exchange_evaluation,
};

use crate::history::{CONTINUATION_PLIES, ContinuationKey, SharedHistory, captured_kind};

/// Middle-game piece-square values used purely to order quiet moves.
///
/// These outlived the hand-crafted evaluation they were born in. NNUE scores positions,
/// but the move picker still needs a cheap static hint for "does this square look better
/// than the one I came from", and it must not call the network to get it. The table is
/// therefore an ordering heuristic, not an evaluation term: changing it changes the node
/// counts the deterministic bench signature pins, but never the score of any position.
const PIECE_SQUARE_VALUES: [[i32; 64]; 6] = build_piece_square_values();

const fn build_piece_square_values() -> [[i32; 64]; 6] {
    let mut tables = [[0; 64]; 6];
    let mut kind = 0;
    while kind < PieceKind::ALL.len() {
        let mut square = 0;
        while square < 64 {
            let file = (square & 7) as i32;
            let rank = (square >> 3) as i32;
            let file_center = center_distance(file);
            let center = file_center + center_distance(rank);
            tables[kind][square] = match PieceKind::ALL[kind] {
                PieceKind::Pawn => rank * 8 + file_center * 2,
                PieceKind::Knight => center * 12,
                PieceKind::Bishop => center * 7 + rank * 2,
                PieceKind::Rook => rank * 2 + file_center,
                PieceKind::Queen => center * 3,
                PieceKind::King => -center * 8,
            };
            square += 1;
        }
        kind += 1;
    }
    tables
}

const fn center_distance(coordinate: i32) -> i32 {
    let from_three = abs(coordinate - 3);
    let from_four = abs(coordinate - 4);
    3 - min(from_three, from_four)
}

const fn abs(value: i32) -> i32 {
    if value < 0 { -value } else { value }
}

const fn min(left: i32, right: i32) -> i32 {
    if left < right { left } else { right }
}

/// Mirrors the square for black so both colours read the same white-relative table.
fn piece_square_value(kind: PieceKind, color: Color, square: Square) -> i32 {
    let index = match color {
        Color::White => square.index(),
        Color::Black => (7 - square.rank()) * 8 + square.file(),
    };
    PIECE_SQUARE_VALUES[kind.index()][index as usize]
}

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

/// `statScore` weights in 1024ths (reference: `2252*main + 1126*cont1 + 1093*cont2`).
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

/// Static exchange value a quiet check must promise before qsearch will search it.
///
/// This gate belongs to the quiet-check generator, NOT to `UseSEEPruning`. Its job is
/// to stop the first qsearch ply from expanding every spite check on the board, which
/// is a property of the widening rather than of the capture SEE gate. Wiring it to
/// `qsearch_see_threshold` would make `UseSEEPruning=false` generate every quiet check
/// including the ones that simply hang the checking piece, so an ablation of an
/// unrelated toggle would change this feature's node explosion instead of measuring
/// its own technique.
const QUIET_CHECK_SEE_THRESHOLD: i32 = 0;

/// Quiet moves that give check and survive the SEE gate, in ordering-history order.
///
/// Castling is excluded: `static_exchange_evaluation` refuses castling by assertion
/// (it is not an exchange), and a castling check is not the tactic a first-ply
/// widening exists to find.
fn quiet_checks(position: &Position, ordering: OrderingContext<'_>) -> Vec<Move> {
    let mut checks: Vec<_> = generate_pseudo_legal_moves(position)
        .iter()
        .copied()
        .filter(|mv| {
            !mv.flag().is_capture() && mv.flag().promotion().is_none() && !mv.flag().is_castling()
        })
        // `move_gives_check` before the SEE call on purpose: it rejects the large
        // majority of quiets for a handful of attack-table lookups, while SEE walks a
        // whole recapture sequence.
        .filter(|&mv| crate::search::move_gives_check(position, mv))
        .filter(|&mv| static_exchange_evaluation(position, mv) >= QUIET_CHECK_SEE_THRESHOLD)
        .collect();
    let color = position.side_to_move();
    checks.sort_by_cached_key(|&mv| {
        core::cmp::Reverse(ordering.ordering_history(position, color, mv))
    });
    checks
}

pub(crate) fn quiescence_moves(
    position: &Position,
    tt_move: Option<Move>,
    see_threshold: i32,
    include_quiet_checks: bool,
    ordering: OrderingContext<'_>,
) -> Vec<Move> {
    let pseudo_legal = generate_pseudo_legal_moves(position);
    // The TT move is the one move qsearch has actual evidence about, so it is searched
    // first and is exempt from the SEE gate: the entry that named it was produced by a
    // search, which is strictly better evidence than a static exchange estimate.
    let tt_move = tt_move.filter(|mv| {
        pseudo_legal.contains(mv) && (mv.flag().is_capture() || mv.flag().promotion().is_some())
    });
    let mut moves: Vec<_> = pseudo_legal
        .iter()
        .copied()
        .filter(|mv| Some(*mv) != tt_move)
        .filter(|mv| mv.flag().is_capture() || mv.flag().promotion().is_some())
        // Promotions are exempt from the SEE gate. A promotion changes the material on
        // the board by more than the exchange it initiates, and dropping one because the
        // pawn is recaptured loses the tactic the qsearch exists to find.
        .filter(|&mv| {
            mv.flag().promotion().is_some()
                || static_exchange_evaluation(position, mv) >= see_threshold
        })
        .collect();
    // `sort_by_cached_key` for the same reason as the main loop above, and this site is
    // the more expensive of the two: `capture_score` opens with a full
    // `static_exchange_evaluation()` and closes with a capture-history table read, and
    // `sort_unstable_by_key` re-evaluates its key on EVERY comparison. That made both
    // the SEE and the table read run O(n log n) times per qsearch node instead of O(n),
    // and qsearch is the majority of nodes (mission AGENTS.md 4.54 trap 1).
    moves.sort_by_cached_key(|&mv| core::cmp::Reverse(capture_score(position, mv, ordering)));
    // Quiet checks are appended AFTER every capture rather than interleaved with them.
    // A capture resolves material immediately and a quiet check does not, so a check
    // must never displace a capture that could raise the standing pat first: qsearch
    // cuts off on the first move that reaches beta, and searching the cheap resolving
    // move first is what keeps the widening affordable.
    if include_quiet_checks {
        moves.extend(
            quiet_checks(position, ordering)
                .into_iter()
                .filter(|mv| Some(*mv) != tt_move),
        );
    }
    if let Some(tt_move) = tt_move {
        moves.insert(0, tt_move);
    }
    moves
}

/// Material a capture stands to win, used by the qsearch delta-pruning margin.
///
/// Promotions add the piece they create minus the pawn they consume, so a promotion is
/// never pruned on the strength of the (frequently empty) square it lands on.
pub(crate) fn captured_material(position: &Position, mv: Move) -> i32 {
    let victim = if mv.flag().is_en_passant() {
        material_value(PieceKind::Pawn)
    } else {
        position
            .piece_at(mv.to())
            .map_or(0, |piece| material_value(piece.kind()))
    };
    let promotion = mv.flag().promotion().map_or(0, |kind| {
        material_value(kind) - material_value(PieceKind::Pawn)
    });
    victim + promotion
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

#[cfg(test)]
mod tests {
    use mf_core::{Position, generate_legal_moves, generate_pseudo_legal_moves, is_in_check};

    use super::*;
    use crate::history::{CONTINUATION_PLIES, SharedHistory};
    use crate::search::move_gives_check;

    /// Ordering context with every history table live but empty, so the generator
    /// tests exercise the shipped code path without a warmed table biasing the order.
    fn empty_ordering<'a>(history: &'a SharedHistory, position: &Position) -> OrderingContext<'a> {
        OrderingContext {
            history,
            pawn_key: position.zobrist().pawn(),
            continuation: [None; CONTINUATION_PLIES.len()],
            use_butterfly_history: true,
            use_capture_history: true,
            use_pawn_history: false,
            use_continuation_history: true,
        }
    }

    /// Positions carrying quiet checks of every kind the generator must handle:
    /// direct, discovered, knight, and a promotion-free pawn push, plus a random walk
    /// so the equivalence below is not tested only on hand-picked boards.
    fn generator_positions() -> Vec<Position> {
        let mut positions: Vec<_> = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "4k3/8/8/8/8/8/4B3/4R1K1 w - - 0 1",
            "r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5Q2/PPPP1PPP/RNB1K1NR w KQkq - 4 4",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
            "4k3/8/8/3N4/8/8/8/4K3 w - - 0 1",
        ]
        .into_iter()
        .map(|fen| Position::from_fen(fen, false).expect("test FEN should parse"))
        .collect();

        let mut walk = Position::startpos();
        for sample in 0..48 {
            positions.push(walk.clone());
            let moves = generate_legal_moves(&walk);
            if moves.is_empty() {
                walk = Position::startpos();
            } else {
                walk.make_move(moves[(sample * 13 + 5) % moves.len()]);
            }
        }
        positions
    }

    /// The generator must be exactly "filter every quiet through `move_gives_check`
    /// and the SEE gate" -- no move it misses, no move it invents.
    ///
    /// This is the assertion that keeps the generator honest if it is ever replaced by
    /// a targeted check-move generator instead of a filtered full quiet generation. A
    /// faster generator that silently loses discovered checks would pass every
    /// node-count anchor in the repo, because losing moves only makes the tree smaller.
    #[test]
    fn quiet_check_generation_matches_filtering_every_quiet_through_gives_check() {
        let history = SharedHistory::new();
        for position in generator_positions() {
            if is_in_check(&position, position.side_to_move()) {
                continue;
            }
            let ordering = empty_ordering(&history, &position);
            let generated = quiet_checks(&position, ordering);

            let expected: Vec<_> = generate_pseudo_legal_moves(&position)
                .iter()
                .copied()
                .filter(|mv| {
                    !mv.flag().is_capture()
                        && mv.flag().promotion().is_none()
                        && !mv.flag().is_castling()
                        && move_gives_check(&position, *mv)
                        && static_exchange_evaluation(&position, *mv) >= QUIET_CHECK_SEE_THRESHOLD
                })
                .collect();

            let mut generated: Vec<_> = generated.iter().map(|mv| mv.raw()).collect();
            let mut expected: Vec<_> = expected.iter().map(|mv| mv.raw()).collect();
            generated.sort_unstable();
            expected.sort_unstable();
            assert_eq!(generated, expected, "{position:?}");
        }
    }

    /// Every generated move must actually give check once played, and must not be a
    /// capture, a promotion, or castling.
    #[test]
    fn every_generated_quiet_check_gives_check_and_is_quiet() {
        let history = SharedHistory::new();
        let mut checked_any = false;
        for position in generator_positions() {
            if is_in_check(&position, position.side_to_move()) {
                continue;
            }
            let ordering = empty_ordering(&history, &position);
            for mv in quiet_checks(&position, ordering) {
                assert!(!mv.flag().is_capture(), "{position:?} {mv:?}");
                assert!(mv.flag().promotion().is_none(), "{position:?} {mv:?}");
                assert!(!mv.flag().is_castling(), "{position:?} {mv:?}");
                // Pseudo-legal moves that leave the mover in check are filtered by the
                // qsearch loop itself, so only the legal ones can be asserted here.
                let mover = position.side_to_move();
                let mut after = position.clone();
                let undo = after.make_move(mv);
                if is_in_check(&after, mover) {
                    after.unmake_move(mv, undo);
                    continue;
                }
                assert!(
                    is_in_check(&after, after.side_to_move()),
                    "generated move does not give check: {position:?} {mv:?}"
                );
                checked_any = true;
            }
        }
        assert!(
            checked_any,
            "the corpus must contain at least one legal quiet check"
        );
    }

    /// A quiet check that simply hangs the checking piece must not be generated.
    ///
    /// Without the SEE gate the first qsearch ply expands every spite check on the
    /// board, which is the node explosion this feature has to avoid paying for.
    #[test]
    fn a_quiet_check_that_hangs_the_checking_piece_is_gated_out_by_see() {
        // The white queen can check on e6, where the f7 pawn takes it for free.
        let position = Position::from_fen("4k3/5p2/8/8/8/1Q6/8/4K3 w - - 0 1", false)
            .expect("test FEN should parse");
        let history = SharedHistory::new();
        let hanging = generate_legal_moves(&position)
            .iter()
            .copied()
            .find(|mv| {
                !mv.flag().is_capture()
                    && move_gives_check(&position, *mv)
                    && static_exchange_evaluation(&position, *mv) < 0
            })
            .expect("test position should offer a losing quiet check");

        assert!(!quiet_checks(&position, empty_ordering(&history, &position)).contains(&hanging));
    }

    /// The widened list must be the unwidened list plus quiet checks, in that order.
    ///
    /// Captures first is the property under test: qsearch cuts off on the first move
    /// that reaches beta, and a quiet check resolves no material, so letting one
    /// displace a capture would make the widening cost nodes it does not have to.
    #[test]
    fn quiet_checks_are_appended_after_every_capture_and_change_nothing_else() {
        let history = SharedHistory::new();
        for position in generator_positions() {
            if is_in_check(&position, position.side_to_move()) {
                continue;
            }
            let ordering = empty_ordering(&history, &position);
            let captures = quiescence_moves(&position, None, 0, false, ordering);
            let widened = quiescence_moves(&position, None, 0, true, ordering);

            assert_eq!(
                widened[..captures.len()],
                captures[..],
                "widening must not reorder or drop a capture: {position:?}"
            );
            assert_eq!(
                widened[captures.len()..].to_vec(),
                quiet_checks(&position, ordering),
                "{position:?}"
            );
        }
    }

    /// No sort site in this module may re-evaluate its key per comparison.
    ///
    /// This is a SOURCE-level guard on purpose. The defect it catches is invisible to
    /// every behavioural test and every node-count anchor: on this workload
    /// `sort_unstable_by_key` and `sort_by_cached_key` produce the same move order, so
    /// the tree is bit-for-bit identical and only throughput moves (mission AGENTS.md
    /// 4.54 trap 1).
    ///
    /// It has now been introduced twice. M4-F1 hit it on the three main-loop sites for
    /// -12% NPS. M4-F2 fixed those three and missed `quiescence_moves`, whose key calls
    /// `static_exchange_evaluation()` AND `capture_history()`; qsearch is the majority
    /// of nodes and SEE is far dearer than a table read, so that one line cost 6.7% NPS
    /// on a capture-rich position (`experiments/M4-F3-defects/`). The defect appears at
    /// a call site nobody edited, whenever a term is added to a scoring function, which
    /// is exactly what a reviewer does not think to grep for.
    #[test]
    fn no_sort_site_re_evaluates_its_key_per_comparison() {
        // The needles are split so this test's own source does not match them.
        let uncached = ["sort_unstable_by", "_key"].concat();
        let by_key = ["sort_by", "_key("].concat();
        let source = include_str!("move_ordering.rs");
        let offenders: Vec<_> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                let code = line.split("//").next().unwrap_or(line);
                code.contains('.') && (code.contains(&uncached) || code.contains(&by_key))
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "these sort sites re-evaluate their key on every comparison; \
             use sort_by_cached_key instead: {offenders:?}"
        );

        let cached = ["sort_by", "_cached_key("].concat();
        assert_eq!(
            source.lines().filter(|line| line.contains(&cached)).count(),
            5,
            "the five sort sites are three in MovePicker::new, one in quiescence_moves, \
             and one in quiet_checks"
        );
    }
}
