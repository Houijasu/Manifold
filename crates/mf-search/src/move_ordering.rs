use mf_core::{
    Color, Move, MoveList, Piece, PieceKind, Position, Square, generate_pseudo_legal_captures,
    generate_pseudo_legal_quiets, is_pseudo_legal, material_value, see_ge,
    static_exchange_evaluation,
};

use crate::history::{
    CONTINUATION_PLIES, ContinuationKey, LOW_PLY_HISTORY_PLIES, LowPlyHistory, SharedHistory,
    captured_kind,
};

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

/// Weight of a low-ply entry in the combined quiet score, before the `1 + ply` decay.
const LOW_PLY_WEIGHT: i32 = 8;

#[derive(Clone, Copy)]
pub(crate) struct OrderingContext<'a> {
    pub(crate) history: &'a SharedHistory,
    /// This worker's low-ply table; consulted only while `ply` is below
    /// [`LOW_PLY_HISTORY_PLIES`].
    pub(crate) low_ply_history: &'a LowPlyHistory,
    /// Distance from the root of the node being ordered.
    pub(crate) ply: usize,
    pub(crate) pawn_key: u64,
    /// The predecessor plane at each lookback distance in `CONTINUATION_PLIES`, or
    /// `None` where the stack does not reach back that far or a null move broke the
    /// chain. Resolved once per node when the stack is read, never per scored move.
    pub(crate) continuation: [Option<ContinuationKey>; CONTINUATION_PLIES.len()],
    pub(crate) use_butterfly_history: bool,
    pub(crate) use_capture_history: bool,
    pub(crate) use_pawn_history: bool,
    pub(crate) use_continuation_history: bool,
    pub(crate) use_low_ply_history: bool,
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
        // Ordering read ONLY: low-ply history feeds neither `pruning_history` nor the
        // LMR `quiet_history` stat-score. The `1 + ply` decay fades the near-root
        // signal out before the table's ply ceiling cuts it off.
        if self.use_low_ply_history && self.ply < LOW_PLY_HISTORY_PLIES {
            score +=
                LOW_PLY_WEIGHT * self.low_ply_history.score(self.ply, mv) / (1 + self.ply as i32);
        }
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

const MAX_MOVES: usize = 256;

/// A 256-slot bitmask replacing a `[bool; 256]` in the picker's hot storage: same
/// answers, one eighth the stack footprint per array.
#[derive(Clone, Copy, Default)]
struct MoveMask([u64; 4]);

impl MoveMask {
    #[inline]
    fn get(&self, index: usize) -> bool {
        self.0[index >> 6] & (1u64 << (index & 63)) != 0
    }

    #[inline]
    fn set(&mut self, index: usize) {
        self.0[index >> 6] |= 1u64 << (index & 63);
    }
}

/// Score storage for one move family. Left uninitialized for the same reason
/// `MoveList` is: one of these lives in every picker, so zeroing it on construction
/// is a per-node memset. Only slots below the family list's length are ever written
/// (at scoring time) or read (at selection time).
type ScoreBuffer = [core::mem::MaybeUninit<i32>; MAX_MOVES];

#[inline]
const fn uninit_scores() -> ScoreBuffer {
    [core::mem::MaybeUninit::uninit(); MAX_MOVES]
}

#[inline]
fn read_score(scores: &ScoreBuffer, index: usize) -> i32 {
    // SAFETY: every caller passes an index below the scored list's length, and each
    // such slot was written when its list entry was scored.
    unsafe { scores[index].assume_init() }
}

/// Lazily staged move picker with stack storage.
///
/// Generation and scoring are DEFERRED, Stockfish-style: the TT move is yielded with
/// no generation at all, captures are generated and scored only if the TT move did not
/// already cut off, and quiets are generated and scored only when the good captures
/// are exhausted. A node that cuts off on its TT move or an early capture — the
/// common case at cut nodes — never pays for quiet generation or scoring at all.
///
/// This deliberately reads the history tables WARM. The search updates them mid-loop
/// (beta-cutoff bonuses in subtrees, the post-LMR continuation bonus), so a quiet
/// scored at stage four can see different history than it would have seen at node
/// entry. An earlier eager design froze every score at construction to keep the bench
/// signature bit-exact; lazy staging traded that away on purpose for per-node
/// throughput (the signature moved from `34_516` to `35_886` with it), matching the
/// reference engine, which also scores each stage at the moment it is reached.
///
/// Because generation is deferred, the picker holds no position borrow: `next` takes
/// the position each call. That is sound at every call site because `make_move`/
/// `unmake_move` restore the position bit-for-bit before the loop asks for the next
/// move (the make/unmake tests pin this), so each `next` call sees the node's own
/// position.
pub(crate) struct MovePicker<'a> {
    tt_move: Option<Move>,
    killers: [Option<Move>; 2],
    ordering: OrderingContext<'a>,
    captures_only: bool,
    stage: Stage,
    captures: MoveList,
    capture_scores: ScoreBuffer,
    capture_sees: ScoreBuffer,
    capture_good: MoveMask,
    capture_yielded: MoveMask,
    quiets: MoveList,
    quiet_scores: ScoreBuffer,
    quiet_yielded: MoveMask,
    /// SEE gate for the qsearch variant: `Some(threshold)` gates every non-promotion
    /// capture at YIELD time with `see_ge`, dropping below-threshold exchanges exactly
    /// as the eager load-time gate did; `None` in the full and ProbCut variants, which
    /// score eagerly and gate nothing. A threshold below zero admits losing captures
    /// that must still yield after the winning ones, so those thresholds fall back to
    /// a load-time threshold-0 predicate for the good/bad split.
    qsearch_see_threshold: Option<i32>,
    /// Whether the qsearch variant appends its quiet-checks stage after the bad
    /// captures. Only `MovePicker::qsearch` ever sets this.
    quiet_checks_after_captures: bool,
    quiet_check_moves: MoveList,
    quiet_check_index: usize,
    /// Exact SEE of the most recently yielded move when the eager variants yielded it
    /// from the captures family (captures and promotions), `None` after a quiet. The
    /// lazy qsearch variant walks no exchange for its captures, so there its
    /// capture-family yields expose `None` too — only the TT capture, validated
    /// before any generation, keeps its exact value. `load_captures` already walks the
    /// full exchange once per capture in the eager variants; exposing the value lets
    /// search call sites compare it against their own thresholds instead of
    /// re-walking it.
    current_capture_see: Option<i32>,
}

#[derive(Clone, Copy)]
enum Stage {
    Tt,
    GenerateCaptures,
    GoodCaptures,
    GenerateQuiets,
    Quiets,
    BadCaptures,
    GenerateQuietChecks,
    QuietChecks,
    Done,
}

impl<'a> MovePicker<'a> {
    pub(crate) fn new(
        tt_move: Option<Move>,
        killers: [Option<Move>; 2],
        ordering: OrderingContext<'a>,
    ) -> Self {
        Self::staged(tt_move, killers, ordering, false)
    }

    /// Captures-and-promotions iteration for ProbCut: the quiet stages are skipped
    /// entirely, so quiets are never generated or scored. Bad captures ARE yielded,
    /// after the good ones, preserving the eager picker's captures-only sequence.
    pub(crate) fn captures_only(tt_move: Option<Move>, ordering: OrderingContext<'a>) -> Self {
        Self::staged(tt_move, [None, None], ordering, true)
    }

    /// Staged iteration for the non-check quiescence loop, replacing the old eager
    /// generate-score-sort-gate qsearch path. The TT move — when it is a capture or
    /// promotion — yields with no generation at all; the capture stages then load
    /// WITHOUT any exchange walk (captures rank by MVV-LVA plus capture history) and
    /// the SEE gate runs at yield time, so a node that cuts off early never pays for
    /// the captures it never reaches; the quiet-check widening generates only if the
    /// captures did not satisfy the loop.
    ///
    /// Gate contract, carried over from the eager path this replaces:
    /// - the TT move is searched first and is exempt from the gate: the entry that
    ///   named it was produced by a search, which is strictly better evidence than a
    ///   static exchange estimate;
    /// - captures below `see_threshold` are dropped, not deferred, except promotions,
    ///   which are always kept (dropping one because the pawn is recaptured loses the
    ///   tactic the qsearch exists to find);
    /// - quiet checks, when enabled, yield after every capture — never interleaved.
    ///
    /// Two thresholds get special handling: zero (the shipped gate) admits exactly
    /// the winning captures, so it doubles as the good/bad split and no load-time
    /// classification is needed; `i32::MIN` (the `UseSEEPruning=false` ablation)
    /// admits every capture, so it can never reject one and the gate is skipped —
    /// but the good-before-bad staging survives through a load-time threshold-0
    /// predicate per capture.
    pub(crate) fn qsearch(
        tt_move: Option<Move>,
        see_threshold: i32,
        include_quiet_checks: bool,
        ordering: OrderingContext<'a>,
    ) -> Self {
        let mut picker = Self::staged(tt_move, [None, None], ordering, true);
        picker.qsearch_see_threshold = Some(see_threshold);
        picker.quiet_checks_after_captures = include_quiet_checks;
        picker
    }

    fn staged(
        tt_move: Option<Move>,
        killers: [Option<Move>; 2],
        ordering: OrderingContext<'a>,
        captures_only: bool,
    ) -> Self {
        Self {
            tt_move,
            killers,
            ordering,
            captures_only,
            stage: Stage::Tt,
            captures: MoveList::new(),
            capture_scores: uninit_scores(),
            capture_sees: uninit_scores(),
            capture_good: MoveMask::default(),
            capture_yielded: MoveMask::default(),
            quiets: MoveList::new(),
            quiet_scores: uninit_scores(),
            quiet_yielded: MoveMask::default(),
            qsearch_see_threshold: None,
            quiet_checks_after_captures: false,
            quiet_check_moves: MoveList::new(),
            quiet_check_index: 0,
            current_capture_see: None,
        }
    }

    /// Yields the next move, or `None` when every stage is exhausted.
    ///
    /// `position` must be the same position the picker was constructed for; the search
    /// guarantees this by unmaking every child move before asking for the next one.
    pub(crate) fn next(&mut self, position: &Position) -> Option<Move> {
        loop {
            match self.stage {
                Stage::Tt => {
                    self.stage = Stage::GenerateCaptures;
                    if let Some(mv) = self.validate_tt_move(position) {
                        return Some(mv);
                    }
                }
                Stage::GenerateCaptures => {
                    self.load_captures(position);
                    self.stage = Stage::GoodCaptures;
                }
                Stage::GoodCaptures => {
                    if let Some(mv) = self.next_good_capture(position) {
                        return Some(mv);
                    }
                    self.stage = if self.captures_only {
                        Stage::BadCaptures
                    } else {
                        Stage::GenerateQuiets
                    };
                }
                Stage::GenerateQuiets => {
                    self.load_quiets(position);
                    self.stage = Stage::Quiets;
                }
                Stage::Quiets => {
                    if let Some(mv) = self.next_quiet() {
                        return Some(mv);
                    }
                    self.stage = Stage::BadCaptures;
                }
                Stage::BadCaptures => {
                    if let Some(mv) = self.next_bad_capture(position) {
                        return Some(mv);
                    }
                    self.stage = if self.quiet_checks_after_captures {
                        Stage::GenerateQuietChecks
                    } else {
                        Stage::Done
                    };
                }
                Stage::GenerateQuietChecks => {
                    // The widening generates only here, so a node whose loop ends
                    // inside the captures never pays for it. The list arrives already
                    // gated and ordered by `quiet_checks`.
                    self.quiet_check_moves = quiet_checks(position, self.ordering);
                    self.quiet_check_index = 0;
                    self.stage = Stage::QuietChecks;
                }
                Stage::QuietChecks => {
                    if self.quiet_check_index < self.quiet_check_moves.len() {
                        let mv = self.quiet_check_moves[self.quiet_check_index];
                        self.quiet_check_index += 1;
                        self.current_capture_see = None;
                        return Some(mv);
                    }
                    self.stage = Stage::Done;
                }
                Stage::Done => return None,
            }
        }
    }

    fn load_captures(&mut self, position: &Position) {
        self.captures = generate_pseudo_legal_captures(position);
        for (index, &mv) in self.captures.iter().enumerate() {
            // The TT move was already yielded at the TT stage (with its own SEE), so
            // its slot is masked out and never scored.
            if Some(mv) == self.tt_move {
                self.capture_yielded.set(index);
                continue;
            }
            match self.qsearch_see_threshold {
                // Lazy qsearch variant: no exchange walk here. Captures are ranked by
                // MVV-LVA plus capture history, and the SEE gate runs at yield time
                // (see `pick_best_capture`), so a qsearch node that cuts off early
                // never pays for the captures it never reaches. The good/bad split
                // survives only when the gate cannot supply it itself — a
                // below-threshold-zero gate admits losing captures that must still
                // yield after the winning ones — and then one threshold-0 predicate
                // per capture classifies it, exactly as the eager `see >= 0` did.
                Some(threshold) => {
                    self.capture_scores[index].write(mvv_lva_capture_score(
                        position,
                        mv,
                        self.ordering,
                    ));
                    if threshold < 0 {
                        #[cfg(feature = "instrumentation")]
                        crate::instrumentation::record(|counters| {
                            counters.see_calls_load_captures += 1;
                        });
                        if see_ge(position, mv, 0) {
                            self.capture_good.set(index);
                        }
                    }
                }
                // Full and ProbCut variants: one exact SEE per capture — the score,
                // the good/bad split, and the value the search reads back through
                // `current_capture_see` all share it.
                None => {
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| {
                        counters.see_calls_load_captures += 1;
                    });
                    let see = static_exchange_evaluation(position, mv);
                    self.capture_scores[index].write(capture_score_with_see(
                        position,
                        mv,
                        see,
                        self.ordering,
                    ));
                    self.capture_sees[index].write(see);
                    if see >= 0 {
                        self.capture_good.set(index);
                    }
                }
            }
        }
    }

    fn load_quiets(&mut self, position: &Position) {
        self.quiets = generate_pseudo_legal_quiets(position);
        let color = position.side_to_move();
        for (index, &mv) in self.quiets.iter().enumerate() {
            if Some(mv) == self.tt_move {
                self.quiet_yielded.set(index);
                continue;
            }
            self.quiet_scores[index].write(quiet_score(
                position,
                mv,
                self.killers,
                color,
                self.ordering,
            ));
        }
    }

    /// Validates the TT move WITHOUT generating anything: `is_pseudo_legal` is pinned
    /// (exhaustively, in mf-core) to agree with generated-list containment, which is
    /// the same check the eager picker ran against its generated lists. A validated TT
    /// move stays in `self.tt_move` so the later generation stages mask it out; an
    /// invalid one is cleared so nothing masks a real move by accident.
    fn validate_tt_move(&mut self, position: &Position) -> Option<Move> {
        let tt_move = self.tt_move?;
        let capture_family = tt_move.flag().is_capture() || tt_move.flag().promotion().is_some();
        if (self.captures_only && !capture_family) || !is_pseudo_legal(position, tt_move) {
            self.tt_move = None;
            return None;
        }
        self.current_capture_see = if capture_family {
            // Exempt from generation, not from the SEE contract: ProbCut thresholds
            // every capture-family yield through `current_capture_see`.
            #[cfg(feature = "instrumentation")]
            crate::instrumentation::record(|counters| counters.see_calls_tt_validation += 1);
            Some(static_exchange_evaluation(position, tt_move))
        } else {
            None
        };
        Some(tt_move)
    }

    /// Yields the highest-scored un-yielded good capture. First-max-wins on ties, which
    /// reproduces the eager stable sort's generation-order tie-break exactly.
    fn next_good_capture(&mut self, position: &Position) -> Option<Move> {
        self.pick_best_capture(true, position)
    }

    fn next_bad_capture(&mut self, position: &Position) -> Option<Move> {
        self.pick_best_capture(false, position)
    }

    /// Yields the highest-scored un-yielded capture in the `good` half of the split
    /// (first-max-wins on ties, reproducing the eager stable sort's generation-order
    /// tie-break exactly).
    ///
    /// In the lazy qsearch variant this is also where the SEE gate runs: a picked
    /// candidate must prove `see_ge(threshold)` before it is yielded, and a failure is
    /// masked out exactly as the old load-time gate dropped it — never deferred, never
    /// re-yielded. Promotions are exempt (dropping one because the pawn is recaptured
    /// loses the tactic the qsearch exists to find), and so is the ablation's
    /// admit-everything threshold, where the answer cannot be `false`.
    fn pick_best_capture(&mut self, good: bool, position: &Position) -> Option<Move> {
        loop {
            let mut best: Option<usize> = None;
            for index in 0..self.captures.len() {
                if self.capture_yielded.get(index) || self.capture_good.get(index) != good {
                    continue;
                }
                if best.is_none_or(|chosen| {
                    read_score(&self.capture_scores, index)
                        > read_score(&self.capture_scores, chosen)
                }) {
                    best = Some(index);
                }
            }
            let index = best?;
            let threshold = self.qsearch_see_threshold;
            if threshold.is_some_and(|threshold| {
                threshold != i32::MIN && self.captures[index].flag().promotion().is_none() && {
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| {
                        counters.see_calls_qsearch_yield_gate += 1;
                    });
                    !see_ge(position, self.captures[index], threshold)
                }
            }) {
                // Dropped, not deferred: the load-time gate this replaces never gave
                // the move a later stage either.
                self.capture_yielded.set(index);
                continue;
            }
            self.capture_yielded.set(index);
            // Only the eager variants memoize an exact SEE per capture; the lazy
            // qsearch variant computes none, so its capture-family yields (other than
            // the TT capture, which validates its own) expose `None`.
            self.current_capture_see = if self.qsearch_see_threshold.is_none() {
                Some(read_score(&self.capture_sees, index))
            } else {
                None
            };
            return Some(self.captures[index]);
        }
    }

    /// Yields the quiet with the highest score, breaking ties on the lower raw move
    /// encoding, reproducing the eager sort's `(Reverse(score), raw)` key.
    fn next_quiet(&mut self) -> Option<Move> {
        let mut best: Option<usize> = None;
        for index in 0..self.quiets.len() {
            if self.quiet_yielded.get(index) {
                continue;
            }
            if best.is_none_or(|chosen| {
                read_score(&self.quiet_scores, index) > read_score(&self.quiet_scores, chosen)
                    || (read_score(&self.quiet_scores, index)
                        == read_score(&self.quiet_scores, chosen)
                        && self.quiets[index].raw() < self.quiets[chosen].raw())
            }) {
                best = Some(index);
            }
        }
        let index = best?;
        self.quiet_yielded.set(index);
        self.current_capture_see = None;
        Some(self.quiets[index])
    }

    /// Exact SEE of the move most recently yielded by `next`, when the eager variants
    /// yielded it from the captures family; `None` after a quiet, and `None` for
    /// every non-TT capture the lazy qsearch variant yields (it computes no exchange
    /// for them). Where a value is exposed, it is the one `load_captures` (or the
    /// TT validation) computed — identical to calling
    /// `static_exchange_evaluation(position, mv)` at the node's root position.
    #[inline]
    pub(crate) fn current_capture_see(&self) -> Option<i32> {
        self.current_capture_see
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
fn quiet_checks(position: &Position, ordering: OrderingContext<'_>) -> MoveList {
    // The Quiets family is documented to preserve full-generation relative order and
    // contains no captures or promotions, so filtering it here yields exactly the
    // sequence the old full generation produced after its capture/promotion filter.
    let color = position.side_to_move();
    let mut checks = MoveList::new();
    let mut scores = uninit_scores();
    let check_info = crate::search::CheckInfo::new(position);
    for &mv in &generate_pseudo_legal_quiets(position) {
        if mv.flag().is_castling() {
            continue;
        }
        // The check test before the SEE call on purpose: it rejects the large
        // majority of quiets for a couple of bitboard tests, while SEE walks a
        // whole recapture sequence.
        if !check_info.gives_check(position, mv) {
            continue;
        }
        #[cfg(feature = "instrumentation")]
        crate::instrumentation::record(|counters| counters.see_calls_quiet_checks += 1);
        if !see_ge(position, mv, QUIET_CHECK_SEE_THRESHOLD) {
            continue;
        }
        scores[checks.len()].write(ordering.ordering_history(position, color, mv));
        checks.push(mv);
    }
    // First-max selection with strict `>` reproduces the previous stable sort exactly:
    // descending score, generation order on ties, and each key evaluated once.
    sorted_by_score_descending(&checks, &scores)
}

/// Yields `moves` in descending `scores` order, first-in-generation-order on ties,
/// which is bit-for-bit the order of a stable sort on `Reverse(score)`.
fn sorted_by_score_descending(moves: &MoveList, scores: &ScoreBuffer) -> MoveList {
    let mut sorted = MoveList::new();
    let mut yielded = [false; MAX_MOVES];
    for _ in 0..moves.len() {
        let mut best: Option<usize> = None;
        for (index, &done) in yielded.iter().enumerate().take(moves.len()) {
            if done {
                continue;
            }
            if best.is_none_or(|chosen| read_score(scores, index) > read_score(scores, chosen)) {
                best = Some(index);
            }
        }
        let index = best.expect("selection must find an un-yielded move");
        yielded[index] = true;
        sorted.push(moves[index]);
    }
    sorted
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

#[cfg(test)]
fn capture_score(position: &Position, mv: Move, ordering: OrderingContext<'_>) -> i32 {
    capture_score_with_see(
        position,
        mv,
        static_exchange_evaluation(position, mv),
        ordering,
    )
}

/// `capture_score` with the SEE value supplied by the caller, so a site that already
/// paid for the exchange walk (the good/bad split, the qsearch gate) does not pay
/// twice.
fn capture_score_with_see(
    position: &Position,
    mv: Move,
    see: i32,
    ordering: OrderingContext<'_>,
) -> i32 {
    let (victim, attacker, promotion) = capture_score_terms(position, mv);
    see * 32 + material_value(victim) * 16 - material_value(attacker)
        + promotion
        + ordering.capture_history(position, mv)
}

/// Capture score for the lazy qsearch variant: the eager formula with the SEE term
/// dropped, so ranking captures needs no exchange walk at all. The yield-time gate
/// supplies the losing-capture filtering the SEE term used to contribute.
fn mvv_lva_capture_score(position: &Position, mv: Move, ordering: OrderingContext<'_>) -> i32 {
    let (victim, attacker, promotion) = capture_score_terms(position, mv);
    material_value(victim) * 16 - material_value(attacker)
        + promotion
        + ordering.capture_history(position, mv)
}

/// (victim kind, attacker kind, promotion material bonus) shared by both capture
/// scoring formulas. En-passant captures name a pawn as the victim; promotions add
/// the created piece minus the consumed pawn.
fn capture_score_terms(position: &Position, mv: Move) -> (PieceKind, PieceKind, i32) {
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
    (victim, attacker, promotion)
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
    use crate::history::{CONTINUATION_PLIES, LowPlyHistory, SharedHistory};
    use crate::search::move_gives_check;

    /// A shared zeroed low-ply table for ordering tests. Never written to, so sharing
    /// it across tests cannot bias an order.
    fn empty_low_ply_history() -> &'static LowPlyHistory {
        static TABLE: std::sync::OnceLock<LowPlyHistory> = std::sync::OnceLock::new();
        TABLE.get_or_init(LowPlyHistory::new)
    }

    /// Ordering context with every history table live but empty, so the generator
    /// tests exercise the shipped code path without a warmed table biasing the order.
    fn empty_ordering<'a>(history: &'a SharedHistory, position: &Position) -> OrderingContext<'a> {
        OrderingContext {
            history,
            low_ply_history: empty_low_ply_history(),
            ply: 0,
            pawn_key: position.zobrist().pawn(),
            continuation: [None; CONTINUATION_PLIES.len()],
            use_butterfly_history: true,
            use_capture_history: true,
            use_pawn_history: false,
            use_continuation_history: true,
            use_low_ply_history: true,
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
            for &mv in &quiet_checks(&position, ordering) {
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
            let drain_qsearch = |include_quiet_checks: bool| {
                let mut picker = MovePicker::qsearch(None, 0, include_quiet_checks, ordering);
                let mut moves = Vec::new();
                while let Some(mv) = picker.next(&position) {
                    moves.push(mv);
                }
                moves
            };
            let captures = drain_qsearch(false);
            let widened = drain_qsearch(true);

            assert_eq!(
                widened[..captures.len()],
                captures[..],
                "widening must not reorder or drop a capture: {position:?}"
            );
            assert_eq!(
                widened[captures.len()..],
                quiet_checks(&position, ordering)
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()[..],
                "{position:?}"
            );
        }
    }

    /// The qsearch picker's gate contract, in order of authority:
    ///
    /// 1. SET equality: the yielded moves are exactly the valid TT capture (when one
    ///    was supplied), every capture surviving the SEE gate, and — only when the
    ///    widening is enabled — the gated quiet checks.
    /// 2. TT FIRST: a capture-family TT move is yielded before anything else and is
    ///    exempt from the gate, so it is present even when its own SEE sits below the
    ///    threshold.
    /// 3. STAGE order: when the threshold classifies the split (any below-zero
    ///    threshold, including the `UseSEEPruning=false` ablation's admit-everything
    ///    `i32::MIN`), good captures (SEE >= 0) still precede bad ones. At the
    ///    shipped zero threshold the gate IS the split — it drops exactly the losing
    ///    non-promotions — so captures yield in a single pass with no split.
    ///    Either way, every capture precedes every quiet check, no interleaving.
    /// 4. WITHIN-pass order: captures descend by `mvv_lva_capture_score` (the eager
    ///    formula without the SEE term — the lazy variant ranks no exchange).
    /// 5. GATE: no non-promotion capture below the threshold is ever yielded, while
    ///    every promotion is yielded regardless of its SEE.
    /// 6. SEE contract: only the TT capture (validated before any generation)
    ///    exposes its exact SEE through `current_capture_see`; every other yield
    ///    exposes `None`, because the lazy variant walks no exchange for it.
    #[test]
    fn qsearch_picker_honors_the_gate_exemptions_and_stage_order() {
        let history = SharedHistory::new();
        for position in generator_positions() {
            if is_in_check(&position, position.side_to_move()) {
                continue;
            }
            let ordering = empty_ordering(&history, &position);
            for threshold in [i32::MIN, -50, 0, 50] {
                for include_quiet_checks in [false, true] {
                    for tt_move in tt_scenarios(&position) {
                        let mut picker =
                            MovePicker::qsearch(tt_move, threshold, include_quiet_checks, ordering);
                        let yielded = drain(&mut picker, &position);

                        let pseudo_captures = generate_pseudo_legal_captures(&position);
                        let tt_capture = tt_move
                            .filter(|mv| is_capture_family(*mv) && pseudo_captures.contains(mv));
                        let mut expected: Vec<_> = tt_capture
                            .into_iter()
                            .chain(pseudo_captures.iter().copied().filter(|mv| {
                                Some(*mv) != tt_capture
                                    && (mv.flag().promotion().is_some()
                                        || static_exchange_evaluation(&position, *mv) >= threshold)
                            }))
                            .collect();
                        if include_quiet_checks {
                            expected.extend(quiet_checks(&position, ordering).iter().copied());
                        }

                        // 1. Set equality, each surviving move exactly once.
                        let mut yielded_raw: Vec<_> =
                            yielded.iter().map(|(mv, _)| mv.raw()).collect();
                        let mut expected_raw: Vec<_> = expected.iter().map(|mv| mv.raw()).collect();
                        yielded_raw.sort_unstable();
                        expected_raw.sort_unstable();
                        assert_eq!(
                            yielded_raw, expected_raw,
                            "{position:?} tt={tt_move:?} t={threshold} q={include_quiet_checks}"
                        );

                        // 2. The TT capture is yielded first — and, the lazy point of
                        // the staging, without generating anything: a qsearch node
                        // that cuts off on its TT capture must not pay for the
                        // capture stage at all.
                        if let Some(tt) = tt_capture {
                            let mut picker = MovePicker::qsearch(
                                tt_move,
                                threshold,
                                include_quiet_checks,
                                ordering,
                            );
                            assert_eq!(
                                picker.next(&position),
                                Some(tt),
                                "{position:?}: TT capture must be yielded first"
                            );
                            assert!(
                                picker.captures.is_empty() && picker.quiet_check_moves.is_empty(),
                                "{position:?}: yielding the TT capture must not generate any list"
                            );
                            assert_eq!(yielded[0].0, tt, "{position:?}");
                        }

                        // 3. Stage order. A below-zero threshold classifies the
                        // good/bad split at load, so good captures must precede bad
                        // ones; the zero and positive thresholds let the gate supply
                        // the split itself, so captures yield in one pass. Either
                        // way every capture precedes every quiet check.
                        let stages: Vec<_> = yielded
                            .iter()
                            .map(|(mv, _)| {
                                if is_capture_family(*mv) {
                                    if static_exchange_evaluation(&position, *mv) >= 0 {
                                        0
                                    } else {
                                        1
                                    }
                                } else {
                                    2
                                }
                            })
                            .collect();
                        if threshold < 0 {
                            assert!(
                                stages.windows(2).all(|pair| pair[0] <= pair[1]),
                                "stages interleave: {position:?} tt={tt_move:?} \
                                 t={threshold} q={include_quiet_checks} stages={stages:?}"
                            );
                        } else {
                            let last_capture =
                                yielded.iter().rposition(|(mv, _)| is_capture_family(*mv));
                            let first_check =
                                yielded.iter().position(|(mv, _)| !is_capture_family(*mv));
                            if let (Some(last_capture), Some(first_check)) =
                                (last_capture, first_check)
                            {
                                assert!(
                                    last_capture < first_check,
                                    "quiet check interleaved with captures: {position:?} \
                                     tt={tt_move:?} t={threshold} q={include_quiet_checks}"
                                );
                            }
                        }

                        // 4. Within-pass order descends by the lazy variant's score
                        // (the eager formula minus the SEE term). The TT capture is
                        // exempt: it is yielded first on search evidence, not on
                        // score, so the pair straddling it is skipped just as the
                        // full-picker invariant test slices it off. Below-zero
                        // thresholds keep the good/bad split, so only same-half
                        // pairs are comparable; at zero and above the pass is
                        // unified and every adjacent capture pair is.
                        for pair in yielded.windows(2) {
                            let (a, _) = pair[0];
                            let (b, _) = pair[1];
                            if Some(a) == tt_capture || Some(b) == tt_capture {
                                continue;
                            }
                            if is_capture_family(a) && is_capture_family(b) {
                                let comparable = threshold >= 0
                                    || (static_exchange_evaluation(&position, a) >= 0)
                                        == (static_exchange_evaluation(&position, b) >= 0);
                                if comparable {
                                    assert!(
                                        mvv_lva_capture_score(&position, a, ordering)
                                            >= mvv_lva_capture_score(&position, b, ordering),
                                        "capture order violated: {position:?} {a:?} {b:?}"
                                    );
                                }
                            }
                        }

                        // 5. The gate itself: nothing below threshold except
                        // promotions and the TT capture; every promotion kept.
                        for &(mv, _) in &yielded {
                            if Some(mv) == tt_capture || mv.flag().promotion().is_some() {
                                continue;
                            }
                            if is_capture_family(mv) {
                                assert!(
                                    static_exchange_evaluation(&position, mv) >= threshold,
                                    "below-threshold capture yielded: {position:?} {mv:?}"
                                );
                            }
                        }
                        for &mv in pseudo_captures.iter() {
                            if mv.flag().promotion().is_some() && Some(mv) != tt_capture {
                                assert!(
                                    yielded.iter().any(|(seen, _)| *seen == mv),
                                    "promotion dropped by the gate: {position:?} {mv:?}"
                                );
                            }
                        }

                        // 6. The SEE contract on every yield: the TT capture —
                        // validated before any generation — still exposes its exact
                        // SEE; every other yield exposes `None`, because the lazy
                        // variant walks no exchange for the moves it yields.
                        for &(mv, see) in &yielded {
                            let expected_see = (Some(mv) == tt_capture && is_capture_family(mv))
                                .then(|| static_exchange_evaluation(&position, mv));
                            assert_eq!(see, expected_see, "{position:?} {mv:?}");
                        }
                    }
                }
            }
        }
    }

    /// Drains the staged picker, recording each yielded move together with the
    /// `current_capture_see` the search would read after it.
    fn drain(picker: &mut MovePicker<'_>, position: &Position) -> Vec<(Move, Option<i32>)> {
        let mut yielded = Vec::new();
        while let Some(mv) = picker.next(position) {
            yielded.push((mv, picker.current_capture_see()));
        }
        yielded
    }

    fn is_capture_family(mv: Move) -> bool {
        mv.flag().is_capture() || mv.flag().promotion().is_some()
    }

    /// Classifies one yielded move into the stage it must have come from.
    fn stage_of(position: &Position, mv: Move) -> u8 {
        if is_capture_family(mv) {
            if static_exchange_evaluation(position, mv) >= 0 {
                0 // good capture
            } else {
                2 // bad capture
            }
        } else {
            1 // quiet
        }
    }

    /// The staged invariants every full drain must satisfy:
    ///
    /// 1. SET equality: the yielded moves are exactly the pseudo-legal moves, each
    ///    once (the TT move deduplicated, a corrupt TT entry dropped).
    /// 2. STAGE order: a validated TT move first, then good captures (SEE >= 0), then
    ///    quiets, then bad captures, with no interleaving.
    /// 3. WITHIN-stage order: captures descend by `capture_score`; quiets descend by
    ///    `quiet_score` with the raw-encoding ascending tie-break.
    /// 4. SEE contract: every capture-family yield exposes its exact SEE through
    ///    `current_capture_see`, every quiet exposes `None`.
    ///
    /// Deliberately NOT asserted: identity with an eager reference sequence. The lazy
    /// picker scores each stage when it is reached, and in the search the history
    /// tables are warm by then, so order-identity with node-entry scores is no longer
    /// the contract.
    fn assert_staged_invariants(
        position: &Position,
        tt_move: Option<Move>,
        killers: [Option<Move>; 2],
        ordering: OrderingContext<'_>,
        yielded: &[(Move, Option<i32>)],
    ) {
        let pseudo_legal = generate_pseudo_legal_moves(position);
        let tt_move = tt_move.filter(|mv| pseudo_legal.contains(mv));

        // 1. Set equality, each move exactly once.
        let mut yielded_raw: Vec<_> = yielded.iter().map(|(mv, _)| mv.raw()).collect();
        let mut expected_raw: Vec<_> = pseudo_legal.iter().map(|mv| mv.raw()).collect();
        yielded_raw.sort_unstable();
        expected_raw.sort_unstable();
        assert_eq!(yielded_raw, expected_raw, "{position:?} tt={tt_move:?}");

        // 2. Stage order: TT first, then stages 0 -> 1 -> 2 monotonically.
        let mut rest = yielded;
        if let Some(tt) = tt_move {
            assert_eq!(
                yielded[0].0, tt,
                "{position:?}: TT move must be yielded first"
            );
            rest = &yielded[1..];
        }
        let stages: Vec<_> = rest.iter().map(|(mv, _)| stage_of(position, *mv)).collect();
        assert!(
            stages.windows(2).all(|pair| pair[0] <= pair[1]),
            "stages interleave: {position:?} tt={tt_move:?} stages={stages:?}"
        );

        // 3. Within-stage order.
        let color = position.side_to_move();
        for pair in rest.windows(2) {
            let (a, _) = pair[0];
            let (b, _) = pair[1];
            let (stage_a, stage_b) = (stage_of(position, a), stage_of(position, b));
            if stage_a != stage_b {
                continue;
            }
            if stage_a == 1 {
                let (score_a, score_b) = (
                    quiet_score(position, a, killers, color, ordering),
                    quiet_score(position, b, killers, color, ordering),
                );
                assert!(
                    score_a > score_b || (score_a == score_b && a.raw() < b.raw()),
                    "quiet order violated: {position:?} {a:?} {b:?}"
                );
            } else {
                assert!(
                    capture_score(position, a, ordering) >= capture_score(position, b, ordering),
                    "capture order violated: {position:?} {a:?} {b:?}"
                );
            }
        }

        // 4. The SEE contract on every yield, including the TT move.
        for &(mv, see) in yielded {
            let expected = is_capture_family(mv).then(|| static_exchange_evaluation(position, mv));
            assert_eq!(see, expected, "{position:?} {mv:?}");
        }
    }

    fn tt_scenarios(position: &Position) -> Vec<Option<Move>> {
        let pseudo_legal = generate_pseudo_legal_moves(position);
        let first_capture = pseudo_legal
            .iter()
            .copied()
            .find(|mv| mv.flag().is_capture() || mv.flag().promotion().is_some());
        let first_quiet = pseudo_legal
            .iter()
            .copied()
            .find(|mv| !mv.flag().is_capture() && mv.flag().promotion().is_none());
        let garbage = Move::new(
            Square::new(0).unwrap(),
            Square::new(63).unwrap(),
            mf_core::MoveFlag::QUIET,
        );
        vec![None, first_capture, first_quiet, Some(garbage)]
    }

    /// A full drain of the staged picker must satisfy the staged invariants for every
    /// TT scenario, including a corrupt entry that must be dropped without generating
    /// a phantom move.
    #[test]
    fn staged_picker_satisfies_the_staged_invariants() {
        let history = SharedHistory::new();
        for position in generator_positions() {
            let ordering = empty_ordering(&history, &position);
            let first_quiet = generate_pseudo_legal_moves(&position)
                .iter()
                .copied()
                .find(|mv| !mv.flag().is_capture() && mv.flag().promotion().is_none());
            for tt_move in tt_scenarios(&position) {
                for killers in [[None, None], [first_quiet, None]] {
                    let mut picker = MovePicker::new(tt_move, killers, ordering);
                    let yielded = drain(&mut picker, &position);
                    assert_staged_invariants(&position, tt_move, killers, ordering, &yielded);
                }
            }
        }
    }

    /// ProbCut's capture-only iteration must yield exactly the pseudo-legal captures
    /// and promotions — good ones (SEE >= 0) before bad ones, each stage descending by
    /// `capture_score`, every yield exposing its exact SEE — and must not be derailed
    /// by a quiet TT entry, which it neither yields nor lets suppress a real move.
    #[test]
    fn captures_only_picker_yields_exactly_the_capture_family_in_stage_order() {
        let history = SharedHistory::new();
        for position in generator_positions() {
            let ordering = empty_ordering(&history, &position);
            for tt_move in tt_scenarios(&position) {
                let mut picker = MovePicker::captures_only(tt_move, ordering);
                let yielded = drain(&mut picker, &position);

                let pseudo_legal = generate_pseudo_legal_moves(&position);
                let mut yielded_raw: Vec<_> = yielded.iter().map(|(mv, _)| mv.raw()).collect();
                let mut expected_raw: Vec<_> = pseudo_legal
                    .iter()
                    .copied()
                    .filter(|&mv| is_capture_family(mv))
                    .map(|mv| mv.raw())
                    .collect();
                yielded_raw.sort_unstable();
                expected_raw.sort_unstable();
                assert_eq!(yielded_raw, expected_raw, "{position:?} tt={tt_move:?}");

                let tt_capture =
                    tt_move.filter(|&mv| is_capture_family(mv) && pseudo_legal.contains(&mv));
                let mut rest = yielded.as_slice();
                if let Some(tt) = tt_capture {
                    assert_eq!(yielded[0].0, tt, "{position:?}: TT capture must be first");
                    rest = &yielded[1..];
                }
                let stages: Vec<_> = rest
                    .iter()
                    .map(|(mv, _)| stage_of(&position, *mv))
                    .collect();
                assert!(
                    stages.windows(2).all(|pair| pair[0] <= pair[1]),
                    "stages interleave: {position:?} tt={tt_move:?}"
                );
                for pair in rest.windows(2) {
                    let ((a, _), (b, _)) = (pair[0], pair[1]);
                    if stage_of(&position, a) == stage_of(&position, b) {
                        assert!(
                            capture_score(&position, a, ordering)
                                >= capture_score(&position, b, ordering),
                            "capture order violated: {position:?} {a:?} {b:?}"
                        );
                    }
                }
                for &(mv, see) in &yielded {
                    assert_eq!(
                        see,
                        Some(static_exchange_evaluation(&position, mv)),
                        "{position:?} {mv:?}"
                    );
                }
            }
        }
    }

    /// The lazy point of the staging: a picker that stops before its quiet stage must
    /// never have scored a quiet, and one that stops at the TT move must not even have
    /// generated captures. Observed through the score buffers' write masks rather than
    /// timing, so the property is deterministic.
    #[test]
    fn stages_are_not_generated_until_reached() {
        let history = SharedHistory::new();
        for position in generator_positions() {
            let ordering = empty_ordering(&history, &position);
            let pseudo_legal = generate_pseudo_legal_moves(&position);
            let Some(tt_move) = pseudo_legal.first().copied() else {
                continue;
            };

            // Only the TT move drawn: nothing may have been generated.
            let mut picker = MovePicker::new(Some(tt_move), [None, None], ordering);
            assert_eq!(picker.next(&position), Some(tt_move));
            assert!(
                picker.captures.is_empty() && picker.quiets.is_empty(),
                "{position:?}: yielding the TT move must not generate any list"
            );

            // Drawn past the TT move but not past the good captures: quiets must not
            // have been generated.
            let mut picker = MovePicker::new(Some(tt_move), [None, None], ordering);
            picker.next(&position);
            if picker.next(&position).is_some_and(is_capture_family) {
                assert!(
                    picker.quiets.is_empty(),
                    "{position:?}: quiets generated while good captures remain"
                );
            }
        }
    }

    /// A quiet rewarded in low-ply history is yielded ahead of the piece-square
    /// favourite below the ply ceiling, and the reward is invisible at and beyond it.
    ///
    /// With every shared table empty the startpos picker leads with b1c3, the best
    /// piece-square delta on the board. Rewarding the rim move g1h3 -- strictly worse
    /// on every other ordering term -- must put it first at plies 0 through 4 and
    /// change nothing from ply 5 up.
    #[test]
    fn low_ply_history_reorders_equal_quiets_below_the_ply_ceiling_only() {
        let position = Position::startpos();
        let history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let b1c3 = mf_core::parse_uci_move(&position, "b1c3", false).expect("legal quiet");
        let g1h3 = mf_core::parse_uci_move(&position, "g1h3", false).expect("legal quiet");

        for ply in 0..LOW_PLY_HISTORY_PLIES {
            low_ply_history.update(ply, g1h3, 1_024);
        }

        let ordering_at = |ply: usize, use_low_ply_history: bool| OrderingContext {
            history: &history,
            low_ply_history: &low_ply_history,
            ply,
            pawn_key: position.zobrist().pawn(),
            continuation: [None; CONTINUATION_PLIES.len()],
            use_butterfly_history: true,
            use_capture_history: true,
            use_pawn_history: false,
            use_continuation_history: true,
            use_low_ply_history,
        };
        let first_of = |ordering: OrderingContext<'_>| {
            let mut picker = MovePicker::new(None, [None, None], ordering);
            picker.next(&position).expect("startpos has quiet moves")
        };

        for ply in 0..LOW_PLY_HISTORY_PLIES {
            assert_eq!(
                first_of(ordering_at(ply, true)),
                g1h3,
                "the rewarded quiet must lead at ply {ply}"
            );
        }
        // At the ceiling the table is no longer consulted, so piece-square order wins.
        assert_eq!(first_of(ordering_at(LOW_PLY_HISTORY_PLIES, true)), b1c3);
        // The toggle gates the read: with it off the reward is invisible at any ply.
        assert_eq!(first_of(ordering_at(0, false)), b1c3);
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
    /// -12% NPS. M4-F2 fixed those three and missed the eager qsearch list builder,
    /// whose key calls `static_exchange_evaluation()` AND `capture_history()`;
    /// qsearch is the majority
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
            0,
            "no comparison sort remains in this module; the shipped paths select from \
             stack arrays with precomputed scores"
        );
    }
}
