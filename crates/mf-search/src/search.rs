use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mf_core::{
    Bitboard, CastlingSide, Color, Move, PieceKind, Position, Square, Undo, bishop_attacks,
    generate_legal_moves, has_legal_move, is_in_check, king_attacks, knight_attacks, pawn_attacks,
    rook_attacks, static_exchange_evaluation,
};
use mf_nnue::{ACCUMULATOR_STACK_CAPACITY, AccumulatorStack, AccumulatorStackError, Network};

use crate::history::{
    CONTINUATION_PLIES, CONTINUATION_WEIGHTS, CORRECTION_CONTINUATION_PLIES,
    CORRECTION_CONTINUATION_WEIGHT, CORRECTION_MAJOR, CORRECTION_MATERIAL, CORRECTION_MAX,
    CORRECTION_MINOR, CORRECTION_PAWN, CORRECTION_SCALE, CORRECTION_SOURCES, CORRECTION_WEIGHTS,
    ContinuationKey, KillerTable, SharedHistory, captured_kind,
};
use crate::move_ordering::{MovePicker, OrderingContext, captured_material, quiescence_moves};
use crate::repetition::RepetitionHistory;
use crate::{Bound, EntryData, TranspositionTable};

pub const MATE_SCORE: i32 = 30_000;
pub const MAX_SEARCH_PLY: usize = 128;
const _: () = assert!(MAX_SEARCH_PLY == ACCUMULATOR_STACK_CAPACITY);
/// Marks a TT entry whose static evaluation was intentionally not computed.
pub const UNEVALUATED_STATIC_EVAL: i16 = i16::MIN;
/// Largest magnitude a non-mate score may report. Keeps evaluation clear of the mate
/// band so a big positional score can never be mistaken for a forced mate.
const EVALUATION_LIMIT: i32 = 10_000;
const INFINITY: i32 = MATE_SCORE + 1;
const TABLEBASE_SCORE: i32 = MATE_SCORE - MAX_SEARCH_PLY as i32 - 1;
const TABLEBASE_WIN_IN_MAX_PLY: i32 = TABLEBASE_SCORE - MAX_SEARCH_PLY as i32;
const ASPIRATION_INITIAL_DELTA: i32 = 8;
const ASPIRATION_JITTER_BUCKETS: usize = 8;
/// Divisor on `previous_score^2` in the initial aspiration half-width.
const ASPIRATION_SCORE_DIVISOR: i32 = 16_053;
/// Ceiling on the score-scaled half-width, before per-worker jitter.
const ASPIRATION_MAX_DELTA: i32 = 512;
/// Most plies a repeated root fail high may shave off the re-search.
const ASPIRATION_MAX_DEPTH_REDUCTION: u32 = 3;
const DEFAULT_MAX_DEPTH: u32 = 64;
/// Deepest iteration the deepening loop will ever start.
///
/// Bounded by the ply ceiling because that is where the search stops descending: `pvs`
/// returns the static evaluation at `MAX_SEARCH_PLY` rather than recursing, so a nominal
/// depth beyond it buys nothing.
const MAX_ITERATIVE_DEEPENING_DEPTH: u32 = MAX_SEARCH_PLY as u32;
const TIME_CHECK_INTERVAL: u64 = 512;
const NODE_PUBLISH_INTERVAL: u64 = 1_024;
const NMP_MIN_DEPTH: i32 = 3;
const NMP_VERIFICATION_DEPTH: i32 = 6;
const RFP_MAX_DEPTH: i32 = 6;
const RAZOR_MAX_DEPTH: i32 = 3;
const RAZOR_BASE_MARGIN: i32 = 224;
const RAZOR_MARGIN_PER_DEPTH: i32 = 202;
/// Centipawns per ply of reverse-futility margin.
const RFP_MARGIN_PER_DEPTH: i32 = 105;
/// Extra margin demanded at a node the TT marked as PV, which is likelier to be worth
/// searching properly than to be a genuine cutoff.
const RFP_TT_PV_MARGIN: i32 = 21;
const LMP_MAX_DEPTH: i32 = 8;
/// Constant term in the late-move-pruning move-count table.
///
/// At 3 the non-improving row began at 2 moves at depth 1, which prunes the board flat
/// before the move ordering has shown anything. The reference uses 9.
const LMP_BASE: usize = 9;
const FUTILITY_MAX_EFFECTIVE_DEPTH: i32 = 6;
const FUTILITY_BASE_MARGIN: i32 = 124;
const FUTILITY_MARGIN_PER_DEPTH: i32 = 109;
/// Quiets may be pruned on SEE up to this reduced depth, matching the futility window.
const QUIET_SEE_MAX_EFFECTIVE_DEPTH: i32 = 7;
const QUIET_SEE_MARGIN_PER_DEPTH: i32 = 26;
const CAPTURE_SEE_MAX_DEPTH: i32 = 6;
const CAPTURE_SEE_MARGIN_PER_DEPTH: i32 = 99;
const SINGULAR_MIN_DEPTH: i32 = 6;
const IIR_MIN_DEPTH: i32 = 4;
const PROBCUT_MIN_DEPTH: i32 = 3;
const PROBCUT_BASE_MARGIN: i32 = 241;
const PROBCUT_IMPROVING_MARGIN: i32 = 64;
/// Minimum static exchange value a capture must promise before qsearch spends a node
/// on it.
///
/// Zero, not a positive number. Demanding a strictly WINNING exchange (+1) is the
/// tighter filter a capture-count argument predicts, and it was measured at 1.5x MORE
/// nodes to depth 12 in the endgame position: an equal trade is exactly how a recapture
/// resolves, so rejecting it leaves the standing pat sitting on an unresolved exchange
/// and pushes that work back into the main search, which pays for it at full width.
const QSEARCH_SEE_THRESHOLD: i32 = 0;
/// Material a capture must be able to add to the standing pat before qsearch will spend
/// a node on it.
const QSEARCH_DELTA_MARGIN: i32 = 196;
/// TT depth domain for a qsearch node that searched only captures.
const QSEARCH_CAPTURES_TT_DEPTH: i32 = -2;
/// TT depth domain for a qsearch node that searched more than the captures.
///
/// Two node kinds live here: one that was IN check and therefore searched every
/// evasion, and one at the first qsearch ply that searched the captures plus the quiet
/// moves giving check. Both are strictly more informative than a captures-only search,
/// so a checks entry satisfies a captures probe but not the reverse.
///
/// The two never collide, because whether a node is in check is a property of the
/// POSITION and the TT is keyed on the position: one board can never be probed as both
/// kinds. What the domain actually separates is the same non-checked board visited
/// once at the first qsearch ply, where quiet checks widen it, and again deeper in the
/// qsearch, where they do not.
const QSEARCH_CHECKS_TT_DEPTH: i32 = -1;
const _: () = assert!(
    QSEARCH_CAPTURES_TT_DEPTH < QSEARCH_CHECKS_TT_DEPTH && QSEARCH_CHECKS_TT_DEPTH < 0,
    "a captures-only qsearch entry must never satisfy an in-check or interior probe"
);
/// Bias applied to every stored TT depth so the negative qsearch domains survive the
/// `u8` field. Interior depths are stored biased and compared after decoding, so the
/// ordering between all three domains is preserved exactly.
const TT_DEPTH_OFFSET: i32 = -QSEARCH_CAPTURES_TT_DEPTH;
/// Number of consecutive iterations agreeing on the root move after which no further
/// time is saved. Past this the move is settled and the saving has been banked.
const TIME_STABILITY_CAP: u32 = 6;
/// Soft-limit percentage for a root move that has just changed, before any saving.
const TIME_STABILITY_BASE_PERCENT: u32 = 110;
/// Percentage points removed per consecutive iteration that kept the same root move.
const TIME_STABILITY_STEP_PERCENT: u32 = 5;
/// Centipawns of score drop that buy a full extra `TIME_FALLING_STEP_PERCENT`.
const TIME_FALLING_SCORE_STEP: i32 = 50;
const TIME_FALLING_STEP_PERCENT: u32 = 20;
/// Ceiling on the soft-limit scale. The hard limit is the real bound; this only stops
/// the scale itself from compounding without limit.
const TIME_SCALE_MAX_PERCENT: u32 = 180;
/// Lower anchor of the best-move effort ramp, in per-mille of the search's nodes.
///
/// Below this the best move is not yet dominating its own tree, so the position is
/// still being worked out and the extra time buys something.
const TIME_EFFORT_LOW_PERMILLE: u32 = 500;
/// Upper anchor of the best-move effort ramp, in per-mille of the search's nodes.
///
/// Above this nothing else at the root is competitive and further thinking mostly
/// re-confirms a decision already made.
const TIME_EFFORT_HIGH_PERMILLE: u32 = 900;
/// Effort factor applied at or below [`TIME_EFFORT_LOW_PERMILLE`], in percent.
const TIME_EFFORT_LOW_PERCENT: u32 = 110;
/// Effort factor applied at or above [`TIME_EFFORT_HIGH_PERMILLE`], in percent.
const TIME_EFFORT_HIGH_PERCENT: u32 = 90;
/// Neutral effort factor: used before any root move has accumulated a subtree, and
/// whenever the effort term is disabled.
const TIME_EFFORT_NEUTRAL_PERCENT: u32 = 100;
const LMR_TABLE_SIZE: usize = MAX_SEARCH_PLY + 1;
const LMP_TABLE: [[usize; LMP_MAX_DEPTH as usize + 1]; 2] = build_lmp_table();

struct SearchEvaluator<'network> {
    accumulators: AccumulatorStack<'network>,
    #[cfg(test)]
    network: &'network Network,
    #[cfg(test)]
    verify_incremental: bool,
}

impl<'network> SearchEvaluator<'network> {
    fn new(network: &'network Network, root: &Position) -> Self {
        Self {
            accumulators: AccumulatorStack::new_production(network, root),
            #[cfg(test)]
            network,
            #[cfg(test)]
            verify_incremental: false,
        }
    }

    #[inline]
    fn evaluate(&mut self, position: &Position) -> i32 {
        #[cfg(test)]
        if self.verify_incremental {
            let expected =
                mf_nnue::AccumulatorState::from_position_production(self.network, position);
            assert_eq!(self.accumulators.current(), &expected);
        }
        self.accumulators.evaluate(position)
    }

    fn push_real(
        &mut self,
        child: &Position,
        mv: Move,
        undo: &Undo,
    ) -> Result<(), AccumulatorStackError> {
        self.accumulators.push_real(child, mv, undo)
    }

    fn push_null(&mut self) -> Result<(), AccumulatorStackError> {
        self.accumulators.push_null()
    }

    fn pop(&mut self) -> Result<(), AccumulatorStackError> {
        self.accumulators.pop()
    }

    #[cfg(test)]
    fn depth(&self) -> usize {
        self.accumulators.depth()
    }

    #[cfg(test)]
    fn current(&mut self) -> &mf_nnue::AccumulatorState {
        self.accumulators.current()
    }

    #[cfg(test)]
    fn enable_verification(&mut self) {
        self.verify_incremental = true;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SearchLimits {
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub soft_time: Option<Duration>,
    pub hard_time: Option<Duration>,
    pub infinite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    pub use_nmp: bool,
    pub use_rfp: bool,
    pub use_razoring: bool,
    pub use_lmr: bool,
    pub use_lmp: bool,
    pub use_futility: bool,
    pub use_see_pruning: bool,
    pub use_qsearch_tt: bool,
    pub use_qsearch_delta_pruning: bool,
    /// Search quiet moves that give check at the first quiescence ply.
    ///
    /// Implemented, maintained, and toggleable, but ships **OFF** -- see the comment on
    /// [`SearchOptions::default`].
    pub use_qsearch_checks: bool,
    /// Reduce late captures, one ply less deeply than the same-index quiet.
    pub use_capture_lmr: bool,
    pub use_singular_ext: bool,
    pub use_check_ext: bool,
    pub use_multicut: bool,
    pub use_iir: bool,
    pub use_probcut: bool,
    pub use_butterfly_history: bool,
    pub use_capture_history: bool,
    pub use_pawn_history: bool,
    pub use_continuation_history: bool,
    pub use_history_pruning: bool,
    pub use_correction_history: bool,
    /// Per-variant read gates, indexed by the `CORRECTION_*` source constants plus a
    /// trailing slot for continuation correction history.
    ///
    /// Each variant gets its own gate because the validation contract requires every
    /// technique to be A/B testable externally, and because the six variants were
    /// invented in five different engines and two of them were later REMOVED from
    /// the reference engine. A single master toggle would measure the family, not the
    /// members.
    pub use_correction_sources: [bool; CORRECTION_SOURCES + 1],
    /// Scale the soft time limit by the fraction of the tree the best root move owns.
    ///
    /// Affects TIME-MANAGED searches only: with no `soft_time` there is nothing to
    /// scale, so `go depth`, `go nodes`, `go infinite` and `bench` are untouched.
    pub use_time_effort: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            use_nmp: true,
            use_rfp: true,
            use_razoring: true,
            use_lmr: true,
            use_lmp: true,
            use_futility: true,
            use_see_pruning: true,
            use_qsearch_tt: true,
            use_qsearch_delta_pruning: true,
            // Quiet checks in quiescence are implemented, maintained, and toggleable,
            // but ship OFF. Measured single-variable against the M2 kept build over 300
            // games at 8+0.08, Threads=1, `-use-affinity -concurrency 8`, zero forfeits
            // both sides:
            //
            //   * enabled : -12.75 +/- 23.01 Elo, Ptnml [5,38,74,29,4], LOS 13.8%
            //
            // That is a negative point estimate whose error bar still covers zero, so
            // the honest reading is "not shown to help", not "shown to hurt". It ships
            // off because the feature's own criterion was a positive point estimate and
            // because a technique with no demonstrated gain should not be the default.
            //
            // The mechanism is measured, not guessed. At `movetime 1000` over 24 book
            // positions the widening reaches **0.12 plies LESS depth** on average
            // (15.96 vs 16.08; deeper in only 7 of 24), and it costs +12.3% bench nodes
            // (45_036 -> 50_569). Every quiet check is a node that resolves no material,
            // so the qsearch grows without the standing pat converging faster, and the
            // extra time comes straight out of the iterative deepening that actually
            // finds moves. The tactics it does buy are real -- `search_invariants`
            // pins a quiet mate that a capture-only qsearch scores as merely losing --
            // but at this TC they are rarer than the ply they cost.
            //
            // Two things would change the picture and are the conditions for revisiting
            // it: a targeted gives-check generator in mf-core (the current
            // implementation filters a full pseudo-legal generation through
            // `move_gives_check` plus SEE, which is the honest first cut but not the
            // cheap one), and a longer TC, where a lost tenth of a ply buys back less.
            use_qsearch_checks: false,
            // Late captures are reduced by the SAME formula quiets use, fed a capture
            // `statScore` (captured material + capture history) instead of the
            // butterfly-and-continuation sum. One reduction shape, two kinds of
            // evidence. TT moves, checking captures, and queen promotions are exempt;
            // see `capture_reduction_allowed` for why each one is.
            //
            // Implemented, maintained, and toggleable, but ships **OFF**. Measured
            // single-variable against the M2 kept build over 300 games at 8+0.08,
            // Threads=1, `-use-affinity -concurrency 8`, zero forfeits both sides:
            //
            //   * enabled: -8.11 +/- 20.67 Elo, Ptnml [2,37,79,30,2], LOS 22.1%
            //
            // The error bar covers zero, so the honest reading is "not shown to help",
            // not "shown to hurt". It ships off because the feature's criterion was a
            // positive point estimate, and a technique with no demonstrated gain has no
            // claim on being the default.
            //
            // The mechanism is measured, not guessed, and it is a more interesting
            // negative than M3-F1's was. The node saving is LARGE and real -- -5.8% on
            // bench, and -24.7% / -33.1% / -21.6% at fixed depths 10 / 12 / 14 over six
            // tactical positions -- and it converts to almost nothing: at `movetime
            // 1000` over 24 book positions the enabled build reaches **+0.12 plies**
            // (15.88 vs 15.75, deeper in only 6 of 24). A 25% node saving worth a tenth
            // of a ply is a saving being handed straight back at the verification
            // re-search. A reduced capture that fails high is re-searched at full
            // depth, and captures fail high far more often than quiets at the same move
            // index -- the asymmetry the material term PRICES but cannot remove.
            //
            // Two designs were measured, and the first is recorded because the second
            // only looks obvious afterwards. A FLAT one-ply discount -- the design this
            // feature was specified with -- measured WORSE than no capture reduction at
            // all: +5.7% nodes at depth 10 and +51.6% at depth 12, because it shielded
            // a late pawn grab exactly as much as taking a hanging queen. Making the
            // protection proportional to captured material fixed the node counts
            // completely and moved the Elo not at all, which is what identifies the
            // re-search rather than the reduction as the binding constraint.
            //
            // Conditions for revisiting, both aimed at that re-search rather than at
            // the reduction: a post-LMR continuation-history update, and a
            // doDeeperSearch/doShallowerSearch adjustment that lets the re-search
            // depth respond to how badly the reduced scout missed. Full write-up in
            // `experiments/MSN-S2-capture-lmr/results.md`.
            use_capture_lmr: false,
            use_singular_ext: true,
            use_check_ext: true,
            use_multicut: true,
            use_iir: true,
            use_probcut: true,
            use_butterfly_history: true,
            use_capture_history: true,
            // Pawn history is implemented, maintained, and toggleable, but ships OFF.
            // Measured in isolation at bench depth 7 it COSTS nodes at every weight
            // tried (1, 2, 4, 8, 16 -> -2.57%, -1.34%, -0.68%, -2.60%, -1.89%), and
            // standalone with butterfly disabled it is 9.18% WORSE than no history at
            // all. In the reference engine pawn history is never a standalone ordering
            // signal: it is one small term in a sum dominated by continuation history, which this
            // engine does not have yet. Shipping it on would ship a measured
            // regression. Revisit when continuation history lands.
            use_pawn_history: false,
            use_continuation_history: true,
            // History pruning is implemented, maintained, and toggleable, but ships
            // OFF. It SAVES nodes (-2.87% at bench depth 7) and LOSES games:
            //
            //   * with it on : -103.68 +/- 46.31 Elo, SPRT H0 accepted, 138 games
            //   * with it off: +133.61 +/- 44.43 Elo, 150 games
            //
            // Both arms vs the same M3 baseline at 8+0.08, Threads=1, -use-affinity
            // -concurrency 8. That is a ~237 Elo swing from one toggle, and it is the
            // textbook case of AGENTS.md 4.52 read in reverse: a FAVOURABLE bench
            // delta is not evidence of strength. The nodes it saves are nodes that
            // contained the best move.
            //
            // The likely cause is that this engine has only butterfly history, so the
            // pruning decision rests on a single noisy statistic. The reference
            // thresholds the SUM of butterfly and several continuation-history plies, which is a
            // far more reliable signal. Revisit when continuation history lands.
            use_history_pruning: false,
            use_correction_history: true,
            // Pawn, minor, non-pawn(x2), continuation(2,4) ship ON. MAJOR-piece and
            // MATERIAL corrhist ship OFF, and this is the research's explicit
            // instruction, not a measurement of ours: the reference ADDED both (PR #5556
            // material, Sirius PR #178 major) and later REMOVED both ("Remove material
            // corrHist", "Remove major corrhist") because the non-pawn variants
            // subsume them. `research/search-and-eval-sota.md:1482` says verbatim
            // "Build pawn + minor + non-pawn(x2) + continuation(2,4); skip material and
            // major."
            //
            // Manifold's own instrumentation shows WHY, and shows that here they are
            // worse than redundant. Over a depth-12 startpos search the four hash-keyed
            // tables touched 3,683 / 2,981 / 324 / 39 distinct buckets. Major and
            // material barely vary from the root within one search -- both are hashes
            // of a piece set that only changes on a capture or promotion -- so
            // residuals from thousands of structurally unrelated positions pile into a
            // handful of buckets and SATURATE: mean |entry| 38 and 71 against 13 and 15
            // for pawn and minor, with maxima of 509 and 342 against a 1,024 limit.
            // A saturated shared bucket is not a learned residual, it is a constant
            // offset added to the eval of every position reaching it, which is exactly
            // the +34 cp score drift and the lost ply measured with all six on.
            //
            // The tables are still built, maintained, and individually toggleable so
            // the claim above is externally checkable rather than asserted.
            use_correction_sources: {
                let mut sources = [true; CORRECTION_SOURCES + 1];
                sources[CORRECTION_MAJOR] = false;
                sources[CORRECTION_MATERIAL] = false;
                sources
            },
            // The best root move's share of the tree, folded into the soft limit
            // multiplicatively with the stability governor.
            //
            // Implemented, maintained, and toggleable, but ships **OFF**. Measured
            // single-variable against the M2 kept build, Threads=1, zero forfeits on
            // both sides at both time controls:
            //
            //   * 8+0.08, 300 games: -17.39 +/- 18.99 Elo, Ptnml [1,40,82,27,0]
            //   * 30+0.3,  60 games: -34.86 +/- 44.35 Elo, Ptnml [0,11,14,5,0]
            //
            // The longer control was run because 8+0.08 is short for a time-management
            // change and the reference's own gain for this term is an LTC result. It
            // agreed in direction, which is what turns a marginal STC result into a
            // decision.
            //
            // The mechanism is REDUNDANCY, and it is measurable rather than guessed.
            // Over 299 iterations at depth >= 4 on 24 book positions, the correlation
            // between the stability count and the effort factor is r = -0.348: a high
            // node share and a settled root move are largely the SAME iterations. The
            // term therefore does not add a signal, it re-applies one the stability
            // governor already carries -- and that governor is tuned (+51 Elo in
            // M7-F2-v2), so multiplying a second uncalibrated discount onto it
            // overshoots. At stability 6, which is 38% of all iterations, the two
            // compound to 76.4% of the nominal budget where the tuned governor asked
            // for 80.4%. That 4.9% overshoot lands exactly on the settled positions
            // where the saved time was supposed to be BANKED for later moves.
            //
            // Note what the aggregate hides: the mean scale moves only -0.5% overall,
            // so a "does it change the time spent" check would have called this
            // harmless. The damage is in the conditional distribution, not the mean.
            //
            // Conditions for revisiting: fold the node share INTO the stability
            // governor as one term of a single calibrated formula (the reference
            // engine's five factors are jointly tuned, not stacked independently), or
            // re-derive the ramp anchors against the stability count they will
            // multiply. Effort was measured to fall at or below the low anchor on
            // 25.3% of iterations, on the ramp for 45.0%, and at or above the high
            // anchor for 29.7%, so the signal itself has range -- it is the
            // COMPOSITION that is wrong. Full write-up in
            // `experiments/MSN-S3-tm-effort/results.md`.
            use_time_effort: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterationInfo {
    pub depth: u32,
    pub seldepth: u32,
    pub score: i32,
    pub nodes: u64,
    pub hashfull: u16,
    pub elapsed: Duration,
    pub pv: Vec<Move>,
}

/// The root move currently being searched, for the UCI `currmove` line.
///
/// Kept separate from [`IterationInfo`] because it describes progress *within* an
/// iteration rather than a completed one: it carries no score, PV, or node count, and the
/// reference engine likewise models it as its own message rather than a field on the
/// iteration. GUIs render it as a "searching X (n/m)" status rather than an analysis row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootMoveInfo {
    pub depth: u32,
    pub best_move: Move,
    /// 1-based, matching the UCI `currmovenumber` field.
    pub move_number: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub best_move: Option<Move>,
    pub score: i32,
    pub depth: u32,
    pub seldepth: u32,
    pub nodes: u64,
    pub hashfull: u16,
    pub elapsed: Duration,
    pub pv: Vec<Move>,
    pub iterations: Vec<IterationInfo>,
}

pub(crate) struct WorkerParameters<'a> {
    worker_id: usize,
    generation: u8,
    node_counters: &'a [AtomicU64],
    history: &'a SharedHistory,
    network: &'a Network,
    root_move_reporter: Option<Box<dyn FnMut(RootMoveInfo) + 'a>>,
}

impl<'a> WorkerParameters<'a> {
    pub(crate) fn new(
        worker_id: usize,
        generation: u8,
        node_counters: &'a [AtomicU64],
        history: &'a SharedHistory,
        network: &'a Network,
    ) -> Self {
        assert!(worker_id < node_counters.len());
        Self {
            worker_id,
            generation: generation & 31,
            node_counters,
            history,
            network,
            root_move_reporter: None,
        }
    }

    /// Attaches a `currmove` sink. Only worker 0 should carry one: helpers search the same
    /// root moves in a different order, so reporting from all of them would interleave
    /// contradictory progress for the same depth.
    pub(crate) fn with_root_move_reporter(
        mut self,
        reporter: impl FnMut(RootMoveInfo) + 'a,
    ) -> Self {
        self.root_move_reporter = Some(Box::new(reporter));
        self
    }
}

/// One-shot single-threaded search from a root position.
///
/// The network is mandatory: this engine evaluates only with NNUE, so there is no
/// caller-visible mode in which it is absent.
pub fn search(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    network: &Network,
) -> SearchResult {
    let history = [position.repetition_key()];
    let stop = AtomicBool::new(false);
    search_with_callback(
        position,
        &history,
        transposition_table,
        limits,
        options,
        network,
        &stop,
        |_| {},
    )
}

/// Single-threaded search with full control over history, stopping, and per-iteration
/// reporting. This is the entry point the UCI layer drives.
#[allow(clippy::too_many_arguments)]
pub fn search_with_callback<F>(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    network: &Network,
    stop: &AtomicBool,
    on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    let node_counters = [AtomicU64::new(0)];
    let shared_history = SharedHistory::new(1);
    search_worker_with_history_callback_options(
        position,
        history,
        transposition_table,
        limits,
        options,
        stop,
        WorkerParameters::new(0, 0, &node_counters, &shared_history, network),
        on_iteration,
    )
}

/// Single-threaded search reusing a caller-owned [`SharedHistory`].
///
/// The other entry points construct a `SharedHistory` internally, which allocates and
/// zeroes tens of MiB. That is correct for a one-shot search but wrong inside a timed
/// benchmark loop: `bench` used to pay the allocation once per position *inside its own
/// timed region*, so bench NPS misstated the cost of adding history tables (mission
/// AGENTS.md 4.54). Match play allocates once per game, so the benchmark matches it by
/// hoisting the construction out and calling `clear()` between positions instead. Node
/// counts are unaffected: a cleared table is bit-identical to a fresh one.
pub fn search_with_shared_history(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    shared_history: &SharedHistory,
    network: &Network,
) -> SearchResult {
    let position_history = [position.repetition_key()];
    let node_counters = [AtomicU64::new(0)];
    let stop = AtomicBool::new(false);
    search_worker_with_history_callback_options(
        position,
        &position_history,
        transposition_table,
        limits,
        options,
        &stop,
        WorkerParameters::new(0, 0, &node_counters, shared_history, network),
        |_| {},
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_worker_with_history_callback_options<F>(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    stop: &AtomicBool,
    worker: WorkerParameters<'_>,
    mut on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    debug_assert!(worker.worker_id < worker.node_counters.len());
    let worker_id = worker.worker_id;
    let mut worker = worker;
    let root_move_reporter = worker.root_move_reporter.take();
    let started = if worker_id == 0 {
        Some(Instant::now())
    } else {
        None
    };
    let worker_limits = if worker_id == 0 {
        limits
    } else {
        SearchLimits {
            soft_time: None,
            hard_time: None,
            ..limits
        }
    };
    let maximum_depth = iteration_ceiling(&limits);
    let mut context = SearchContext::new(
        transposition_table,
        worker_limits,
        started,
        stop,
        position,
        history,
        worker.history,
        options,
        worker_id,
        worker.generation,
        worker.node_counters,
        worker.network,
    );
    context.root_move_reporter = root_move_reporter;
    let root_moves = generate_legal_moves(position);

    if root_moves.is_empty() {
        context.publish_nodes();
        let score = if is_in_check(position, position.side_to_move()) {
            -MATE_SCORE
        } else {
            0
        };
        return SearchResult {
            best_move: None,
            score,
            depth: 0,
            seldepth: 0,
            nodes: 0,
            hashfull: transposition_table.hashfull_per_mille(),
            elapsed: context.elapsed(),
            pv: Vec::new(),
            iterations: Vec::new(),
        };
    }

    let fallback_move = root_moves[0];
    if context.rule_draw_score(position, 0).is_some() {
        context.publish_nodes();
        return SearchResult {
            best_move: Some(fallback_move),
            score: 0,
            depth: 0,
            seldepth: 0,
            nodes: 0,
            hashfull: transposition_table.hashfull_per_mille(),
            elapsed: context.elapsed(),
            pv: vec![fallback_move],
            iterations: Vec::new(),
        };
    }
    let mut completed = None;
    let mut previous_score = 0;
    let mut previous_best_move = None;
    let mut stability = 0u32;

    for depth in 1..=maximum_depth {
        let iteration_start_nodes = context.nodes;
        context.seldepth = depth;
        let attempt = if depth >= 5 {
            aspiration_search(position, depth, previous_score, &mut context)
        } else {
            root_search(position, depth, -INFINITY, INFINITY, &mut context)
        };

        let Some((score, pv)) = attempt else {
            break;
        };
        // The hard limit is several times the soft one, which only pays for itself if
        // something decides WHEN to reach for it. Left ungoverned every iteration that
        // began just under the soft limit ran on to the hard one, so the engine
        // overspent on every move and lost three games on time in a 424-game match.
        //
        // Stability is that governor: while the root move keeps changing, or the score
        // is falling, the position is still being worked out and the extra time buys
        // something. Once the same move survives several iterations, the budget shrinks
        // back below the nominal share and the saved time funds the moves that need it.
        let best_move = pv.first().copied();
        if best_move.is_some() && best_move == previous_best_move {
            stability = (stability + 1).min(TIME_STABILITY_CAP);
        } else {
            stability = 0;
        }
        previous_best_move = best_move;
        // Composed multiplicatively with stability because the two measure different
        // things: stability asks whether the answer keeps CHANGING, effort asks whether
        // the alternatives are still being taken seriously. A move can be stable for
        // six iterations while a rival keeps costing a third of the tree.
        let effort_percent = context.best_move_effort_percent(best_move);
        context.set_time_scale(scaled_time_percent(
            time_scale_percent(stability, score - previous_score),
            effort_percent,
        ));
        previous_score = score;
        context.publish_nodes();
        let elapsed = context.elapsed();
        let info = IterationInfo {
            depth,
            seldepth: context.seldepth.max(depth),
            score,
            nodes: context.reported_nodes(),
            hashfull: context.transposition_table.hashfull_per_mille(),
            elapsed,
            pv,
        };
        completed = Some(info.clone());
        on_iteration(&info);
        context.iterations.push(info);

        if context.nodes == iteration_start_nodes
            || limits.depth == Some(depth)
            || context.should_stop_after_iteration()
            || (!limits.infinite && score.abs() >= MATE_SCORE - depth as i32)
        {
            break;
        }
    }

    let completed = completed.unwrap_or_else(|| IterationInfo {
        depth: 0,
        seldepth: 0,
        score: context.static_eval(position),
        nodes: context.reported_nodes(),
        hashfull: context.transposition_table.hashfull_per_mille(),
        elapsed: context.elapsed(),
        pv: vec![fallback_move],
    });
    let best_move = completed.pv.first().copied().or(Some(fallback_move));
    context.publish_nodes();

    SearchResult {
        best_move,
        score: completed.score,
        depth: completed.depth,
        seldepth: completed.seldepth,
        nodes: context.nodes,
        hashfull: completed.hashfull,
        elapsed: context.elapsed(),
        pv: completed.pv,
        iterations: context.iterations,
    }
}

/// The deepest iteration the deepening loop may start under these limits.
///
/// Bounded by `MAX_SEARCH_PLY` in every mode, `infinite` included. `pvs` returns the
/// static evaluation once `ply` reaches that ceiling rather than recursing, so an
/// iteration nominally deeper than it re-searches the same tree for the same answer.
/// Left unbounded, `go infinite` on a forced mate reached depth 322013 in six seconds:
/// each of those iterations emitted an info line, and the resulting backlog is what made
/// the engine look unstoppable.
///
/// Reaching the ceiling is not a reason to answer, though. UCI forbids a `bestmove`
/// before `stop` in infinite mode, so the caller idles there instead.
fn iteration_ceiling(limits: &SearchLimits) -> u32 {
    if limits.infinite {
        MAX_ITERATIVE_DEEPENING_DEPTH
    } else {
        limits
            .depth
            .unwrap_or(DEFAULT_MAX_DEPTH)
            .clamp(1, MAX_ITERATIVE_DEEPENING_DEPTH)
    }
}

fn aspiration_search(
    position: &Position,
    depth: u32,
    previous_score: i32,
    context: &mut SearchContext<'_>,
) -> Option<(i32, Vec<Move>)> {
    let mut delta = aspiration_delta(context.worker_id, previous_score);
    let mut alpha = (previous_score - delta).max(-INFINITY);
    let mut beta = (previous_score + delta).min(INFINITY);
    // A fail high means the root move is better than believed. Re-searching at full
    // depth to confirm a bound that is about to be raised again wastes the iteration,
    // so each successive fail high gives up a ply, down to a floor.
    let mut search_depth = depth;

    loop {
        let result = root_search(position, search_depth.max(1), alpha, beta, context)?;
        if result.0 <= alpha {
            // Fail low: the position is worse than believed, so the move about to be
            // played is in doubt and the full depth is worth paying for. Beta is pulled
            // back toward the score as alpha drops, so the window stays centred rather
            // than growing in one direction only.
            beta = (alpha + beta) / 2;
            alpha = (result.0 - delta).max(-INFINITY);
            search_depth = depth;
        } else if result.0 >= beta {
            beta = (result.0 + delta).min(INFINITY);
            search_depth = search_depth
                .saturating_sub(1)
                .max(depth.saturating_sub(ASPIRATION_MAX_DEPTH_REDUCTION));
        } else {
            return Some(result);
        }
        // Widen by a third per failure. Doubling was measured and did not pay: the
        // bundle containing it returned +10.72 +/- 11.00 Elo, LLR 0.82 after 1200 games
        // (experiments/M7-F4-aspiration), which straddles zero. The gentler ramp keeps
        // the re-search window tight when the first failure was a near miss.
        delta += (delta / 3).max(1);
        if alpha == -INFINITY && beta == INFINITY {
            return root_search(position, depth, alpha, beta, context);
        }
    }
}

/// Half-width of the first aspiration window at this iteration.
///
/// The width scales with the magnitude of the previous score: a search already several
/// pawns from equality is likelier to swing than one hovering near zero, and a fixed
/// width re-searches those positions repeatedly. A flat 25cp was simultaneously too
/// wide near equality -- where most of the tree lives -- and too narrow once decided.
#[inline]
fn aspiration_delta(worker_id: usize, previous_score: i32) -> i32 {
    let scaled = ASPIRATION_INITIAL_DELTA
        + previous_score.saturating_mul(previous_score) / ASPIRATION_SCORE_DIVISOR;
    scaled.min(ASPIRATION_MAX_DELTA) + (worker_id % ASPIRATION_JITTER_BUCKETS) as i32
}

fn published_node_total(counters: &[AtomicU64]) -> u64 {
    counters.iter().fold(0, |total, counter| {
        total.saturating_add(counter.load(Ordering::Relaxed))
    })
}

fn root_search(
    position: &Position,
    depth: u32,
    mut alpha: i32,
    beta: i32,
    context: &mut SearchContext<'_>,
) -> Option<(i32, Vec<Move>)> {
    let mut position = position.clone();
    // Effort is accounted per root search rather than per iteration, so an aspiration
    // re-search replaces the previous distribution instead of adding to it: the last
    // root search is the one whose result the iteration reports, and a failed window's
    // truncated subtrees describe a tree that was thrown away.
    context.begin_root_effort();
    let key = tt_key(&position, depth as i32);
    context.transposition_table.prefetch(key);
    let original_alpha = alpha;
    let tt_move = context
        .transposition_table
        .probe(key)
        .and_then(|entry| entry.best_move);
    let mut best_score = -INFINITY;
    let mut best_pv = Vec::new();
    let mut searched = 0usize;
    let opponent_was_already_in_check = is_in_check(&position, !position.side_to_move());

    let ordering = context.ordering(&position, 0);
    for mv in MovePicker::new(&position, tt_move, [None, None], ordering) {
        let mover = position.side_to_move();
        let continuation = continuation_key(&position, mv);
        let undo = position.make_move(mv);
        context
            .transposition_table
            .prefetch(tt_key(&position, depth as i32 - 1));
        if is_in_check(&position, mover) {
            position.unmake_move(mv, undo);
            continue;
        }
        // A move that captured a king leaves the board with a side missing one. That is
        // reachable here because the corpus below tolerates a position where the side
        // not to move is already in check, and `is_in_check` reports `false` for an
        // absent king rather than panicking -- so the capture passes the legality filter
        // above. NNUE then asks for the king square and panics inside the search thread,
        // killing the engine mid-analysis. Such a "move" is not a chess move at all, so
        // drop it here rather than teaching the evaluator to tolerate a kingless board.
        if position.pieces(!mover, PieceKind::King).first().is_none() {
            position.unmake_move(mv, undo);
            continue;
        }
        // The externally validated mate corpus contains legacy composed positions
        // where the side not to move starts in check. Match the reference engine's
        // convention by not treating a continued, pre-existing check as mate in one.
        if opponent_was_already_in_check
            && is_in_check(&position, position.side_to_move())
            && generate_legal_moves(&position).is_empty()
        {
            position.unmake_move(mv, undo);
            continue;
        }
        // Reported after the legality filters above, so `currmovenumber` counts the moves
        // actually searched. Numbering is 1-based per the UCI spec, hence `searched + 1`.
        context.report_root_move(depth, mv, searched + 1);
        let nodes_before_move = context.nodes;
        context.push_position(&position, 1, mv, continuation, &undo);
        let mut child_pv = Vec::new();
        let score = if searched == 0 {
            pvs(
                &mut position,
                depth as i32 - 1,
                -beta,
                -alpha,
                1,
                true,
                false,
                true,
                false,
                None,
                context,
                &mut child_pv,
            )
            .map(|score| -score)
        } else {
            let scout = pvs(
                &mut position,
                depth as i32 - 1,
                -alpha - 1,
                -alpha,
                1,
                false,
                true,
                true,
                false,
                None,
                context,
                &mut child_pv,
            )
            .map(|score| -score);
            scout.and_then(|score| {
                if score > alpha && score < beta {
                    child_pv.clear();
                    pvs(
                        &mut position,
                        depth as i32 - 1,
                        -beta,
                        -alpha,
                        1,
                        true,
                        false,
                        true,
                        false,
                        None,
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)
                } else {
                    Some(score)
                }
            })
        };
        context.pop_position(1);
        position.unmake_move(mv, undo);
        context.record_root_effort(mv, context.nodes.saturating_sub(nodes_before_move));
        let score = score?;
        searched += 1;

        if score > best_score {
            best_score = score;
            best_pv.clear();
            best_pv.push(mv);
            best_pv.extend_from_slice(&child_pv);
        }
        alpha = alpha.max(score);
        if alpha >= beta {
            break;
        }
    }

    if searched == 0 {
        let score = if is_in_check(&position, position.side_to_move()) {
            -MATE_SCORE
        } else {
            0
        };
        return Some((score, Vec::new()));
    }
    let bound = if best_score <= original_alpha {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    };
    context.transposition_table.store(
        key,
        EntryData {
            best_move: best_pv.first().copied(),
            score: best_score as i16,
            // Static evaluation is not probed by the M2 search. Avoid recomputing the full HCE
            // solely to populate a field that becomes useful with M3 pruning.
            static_eval: UNEVALUATED_STATIC_EVAL,
            depth: tt_stored_depth(depth as i32),
            bound,
            age: context.generation,
            pv: true,
        },
    );
    Some((best_score, best_pv))
}

#[allow(clippy::too_many_arguments)]
fn pvs(
    position: &mut Position,
    mut depth: i32,
    mut alpha: i32,
    mut beta: i32,
    ply: usize,
    pv_node: bool,
    cut_node: bool,
    allow_null: bool,
    verification_node: bool,
    excluded_move: Option<Move>,
    context: &mut SearchContext<'_>,
    pv: &mut Vec<Move>,
) -> Option<i32> {
    if !context.visit_node(ply) {
        return None;
    }
    if alpha < 0
        && context
            .repetition_history
            .upcoming_repetition(position, ply)
    {
        alpha = draw_value(context.nodes);
        if alpha >= beta {
            return Some(alpha);
        }
    }
    if let Some(draw_score) = context.rule_draw_score(position, ply) {
        return Some(draw_score);
    }
    if ply >= MAX_SEARCH_PLY {
        return Some(context.static_eval(position));
    }
    // Mate distance pruning. A mate already found closer to the root cannot be beaten
    // by anything below this node, so the window is clamped to the best and worst mate
    // still reachable from here. When that empties the window there is nothing left to
    // search.
    if ply > 0 {
        alpha = alpha.max(-MATE_SCORE + ply as i32);
        beta = beta.min(MATE_SCORE - ply as i32 - 1);
        if alpha >= beta {
            return Some(alpha);
        }
    }
    if depth <= 0 {
        return quiescence(
            position, alpha, beta, ply, pv_node, false, true, context, pv,
        );
    }

    let key = tt_key(position, depth);
    context.transposition_table.prefetch(key);
    let original_alpha = alpha;
    let mut tt_move = None;
    let tt_entry = tt_entry_for_node(context.transposition_table, key, verification_node);
    if let Some(entry) = tt_entry {
        tt_move = entry.best_move;
        if !pv_node && !verification_node && tt_entry_depth(entry.depth) >= depth {
            let score = value_from_tt(i32::from(entry.score), ply, position.halfmove_clock());
            let bound_allows_cutoff = match entry.bound {
                Bound::Exact => true,
                Bound::Lower => score >= beta,
                Bound::Upper => score <= alpha,
            };
            if bound_allows_cutoff
                && tt_cutoff_is_safe(
                    position,
                    entry,
                    score,
                    depth,
                    beta,
                    ply,
                    context.transposition_table,
                )
            {
                return Some(score);
            }
        }
    }
    if excluded_move.is_some() {
        tt_move = None;
    }

    let in_check = is_in_check(position, position.side_to_move());
    // The RAW static eval is what goes to the TT, deliberately. Storing the corrected
    // value would fold a residual that was learned at one point in the search into an
    // entry read at another, and the correction would then be applied a second time on
    // top of itself on every re-probe. The reference is explicit about this.
    let raw_static_eval = tt_entry
        .filter(|entry| entry.static_eval != UNEVALUATED_STATIC_EVAL)
        .map_or_else(
            || context.static_eval(position),
            |entry| i32::from(entry.static_eval),
        );
    let correction = correction_value(position, context, ply);
    let static_eval = to_corrected_static_eval(raw_static_eval, correction);
    // `improving` is a shared derived value: it feeds the LMR reduction, which feeds
    // `effective_depth`, which futility and SEE pruning read. Gating it on a set of
    // toggles is the AGENTS.md 4.4 defect class, so it is always computed.
    context.static_evals[ply] = (!in_check).then_some(static_eval);
    let mut improving = is_improving(
        static_eval,
        ply.checked_sub(2)
            .and_then(|previous_ply| context.static_evals[previous_ply]),
    );
    let tt_pv = tt_entry.is_some_and(|entry| entry.pv);
    if !in_check && !pv_node && excluded_move.is_none() {
        if context.options.use_razoring
            && depth <= RAZOR_MAX_DEPTH
            && !is_mate_score(alpha)
            && eval_pruning_rule50_safe(position, depth)
            && static_eval < alpha - razoring_margin(depth)
        {
            return quiescence(
                position, alpha, beta, ply, pv_node, false, true, context, pv,
            );
        }

        if context.options.use_rfp
            && depth <= RFP_MAX_DEPTH
            && !is_mate_score(beta)
            && eval_pruning_rule50_safe(position, depth)
            && static_eval - reverse_futility_margin(depth, improving, cut_node, tt_pv) >= beta
        {
            return Some((661 * beta + 363 * static_eval) / 1024);
        }

        // Null move applies at every non-PV node, not only at expected cut nodes. The
        // enclosing block already excludes PV nodes and check evasions, and restricting
        // it further to `cut_node` forfeited the cutoff at exactly the all-nodes where
        // a refutation search is cheapest.
        if context.options.use_nmp
            && allow_null
            && depth >= NMP_MIN_DEPTH
            && !is_mate_score(beta)
            && eval_pruning_rule50_safe(position, depth)
            && context.nmp_allowed_at(ply)
            && has_non_pawn_material(position)
            && static_eval >= beta - 13 * depth + 100
        {
            let reduction = null_move_reduction(depth, static_eval, beta);
            let undo = position.make_null_move();
            context.push_null_position(ply + 1);
            let mut null_pv = Vec::new();
            let null_value = pvs(
                position,
                depth - reduction,
                -beta,
                -beta + 1,
                ply + 1,
                false,
                false,
                false,
                false,
                None,
                context,
                &mut null_pv,
            )
            .map(|score| -score);
            context.pop_position(ply + 1);
            position.unmake_null_move(undo);
            let null_value = null_value?;

            if null_value >= beta && !is_mate_score(null_value) {
                if !requires_null_move_verification(context.nmp_min_ply, depth) {
                    return Some(null_value);
                }

                let verification_depth = depth - reduction;
                context.nmp_min_ply = null_move_verification_min_ply(ply, verification_depth);
                let mut verification_pv = Vec::new();
                let verification = pvs(
                    position,
                    verification_depth,
                    beta - 1,
                    beta,
                    ply,
                    false,
                    false,
                    true,
                    true,
                    None,
                    context,
                    &mut verification_pv,
                );
                context.nmp_min_ply = 0;
                let verification = verification?;
                if verification >= beta {
                    return Some(null_value);
                }
            }
        }
        improving |= static_eval >= beta;
    }

    if context.options.use_iir {
        depth -= internal_iterative_reduction(depth, in_check, tt_move, excluded_move);
    }

    if context.options.use_probcut
        && excluded_move.is_none()
        && !in_check
        && !pv_node
        && depth >= PROBCUT_MIN_DEPTH
        && !is_mate_score(beta)
        && eval_pruning_rule50_safe(position, depth)
    {
        let probcut_beta = probcut_beta(beta, improving);
        let tt_score = tt_entry
            .map(|entry| value_from_tt(i32::from(entry.score), ply, position.halfmove_clock()));
        if tt_score.is_none_or(|score| score >= probcut_beta) {
            let probcut_depth = probcut_depth(depth, improving);
            let see_threshold = probcut_beta - static_eval;
            let ordering = context.ordering(position, ply);
            for mv in MovePicker::new(position, tt_move, context.killers.killers(ply), ordering)
                .filter(|mv| mv.flag().is_capture() || mv.flag().promotion().is_some())
            {
                if static_exchange_evaluation(position, mv) < see_threshold {
                    continue;
                }
                let mover = position.side_to_move();
                let continuation = continuation_key(position, mv);
                let undo = position.make_move(mv);
                context
                    .transposition_table
                    .prefetch(tt_key(position, probcut_depth));
                if is_in_check(position, mover) {
                    position.unmake_move(mv, undo);
                    continue;
                }
                context.push_position(position, ply + 1, mv, continuation, &undo);
                let mut probcut_pv = Vec::new();
                let mut value = quiescence(
                    position,
                    -probcut_beta,
                    -probcut_beta + 1,
                    ply + 1,
                    false,
                    true,
                    // ProbCut's verification is an entry into quiescence FROM the
                    // interior search, so it is a first qsearch ply like any other.
                    // Exempting it would make the value ProbCut thresholds against
                    // come from a narrower search than the one the same board gets
                    // when the main search reaches it, and ProbCut's whole premise is
                    // that its shallow value predicts the deeper one.
                    true,
                    context,
                    &mut probcut_pv,
                )
                .map(|score| -score);
                if value.is_some_and(|score| score >= probcut_beta) && probcut_depth > 0 {
                    probcut_pv.clear();
                    value = pvs(
                        position,
                        probcut_depth,
                        -probcut_beta,
                        -probcut_beta + 1,
                        ply + 1,
                        false,
                        !cut_node,
                        true,
                        false,
                        None,
                        context,
                        &mut probcut_pv,
                    )
                    .map(|score| -score);
                }
                context.pop_position(ply + 1);
                position.unmake_move(mv, undo);
                let value = value?;
                if value >= probcut_beta {
                    if !verification_node {
                        context.transposition_table.store(
                            key,
                            EntryData {
                                best_move: Some(mv),
                                score: value_to_tt(value, ply) as i16,
                                static_eval: raw_static_eval as i16,
                                depth: tt_stored_depth(probcut_depth + 1),
                                bound: Bound::Lower,
                                age: context.generation,
                                pv: false,
                            },
                        );
                    }
                    if let Some(cutoff_value) = probcut_cutoff_value(value, beta, probcut_beta) {
                        return Some(cutoff_value);
                    }
                }
            }
        }
    }

    let mut best_score = -INFINITY;
    let mut best_move = None;
    let mut searched = 0usize;
    let mut child_pv = Vec::new();
    let mut searched_quiets = Vec::new();
    let mut searched_captures = Vec::new();

    let pawn_key = position.zobrist().pawn();
    let ordering = context.ordering(position, ply);
    for mv in MovePicker::new(position, tt_move, context.killers.killers(ply), ordering) {
        if excluded_move == Some(mv) {
            continue;
        }
        let mover = position.side_to_move();
        let quiet = !mv.flag().is_capture() && mv.flag().promotion().is_none();
        let needs_non_pawn_material = (quiet
            && (context.options.use_lmp || context.options.use_futility))
            || context.options.use_see_pruning;
        let mover_has_non_pawn_material =
            needs_non_pawn_material && has_non_pawn_material_for(position, mover);
        let gives_check = move_gives_check(position, mv);
        let move_count = searched + 1;
        // `history_score`, `reduction`, and `effective_depth` are DERIVED VALUES read by
        // futility and SEE pruning as well as by LMR. They must never be gated on
        // `use_lmr`: doing so silently weakened two separately-toggled techniques and
        // made roughly 35% of the apparent LMR effect actually be futility and SEE
        // getting weaker. See mission AGENTS.md 4.4 item 3. Only the LMR *reduction
        // application* below is gated on `use_lmr`.
        let history_score = if quiet {
            ordering.quiet_history(position, mover, mv)
        } else {
            0
        };
        let capture_history = if quiet {
            0
        } else {
            ordering.capture_history(position, mv)
        };
        let reduction = if quiet && !gives_check {
            late_move_reduction(depth, move_count, improving, cut_node, tt_pv, history_score)
        } else if !quiet && capture_reduction_allowed(mv, tt_move, gives_check) {
            capture_late_move_reduction(
                depth,
                move_count,
                improving,
                cut_node,
                tt_pv,
                captured_material(position, mv),
                capture_history,
            )
        } else {
            0
        };
        let new_depth = depth - 1;
        let effective_depth = (new_depth - reduction / 1024).max(0);

        // History pruning: a quiet whose accumulated history is strongly negative at
        // this depth has repeatedly failed to produce a cutoff, so skip it outright.
        // It reads `pruning_history` -- the SUM of the 1- and 2-ply continuation tables
        // and pawn history -- not the butterfly-dominated `history_score` that LMR uses.
        // Thresholding one butterfly statistic is what cost ~237 Elo in M4-F1
        // (AGENTS.md 4.53); see `OrderingContext::pruning_history`.
        if context.options.use_history_pruning
            && !pv_node
            && !in_check
            && quiet
            && !gives_check
            && effective_depth <= HISTORY_PRUNING_MAX_EFFECTIVE_DEPTH
            && best_move.is_some()
            && shallow_pruning_allowed(best_score)
            && ordering.pruning_history(position, mv) < history_pruning_threshold(effective_depth)
        {
            continue;
        }

        if context.options.use_lmp
            && !pv_node
            && !in_check
            && quiet
            && !gives_check
            && depth <= LMP_MAX_DEPTH
            && move_count >= late_move_pruning_threshold(depth, improving)
            && best_move.is_some()
            && shallow_pruning_allowed(best_score)
            && mover_has_non_pawn_material
        {
            continue;
        }

        if context.options.use_futility
            && !pv_node
            && !in_check
            && quiet
            && !gives_check
            && effective_depth <= FUTILITY_MAX_EFFECTIVE_DEPTH
            && best_move.is_some()
            && shallow_pruning_allowed(best_score)
            && !is_mate_score(alpha)
            && eval_pruning_rule50_safe(position, depth)
            && mover_has_non_pawn_material
        {
            let futility_value = static_eval + frontier_futility_margin(effective_depth);
            if futility_value <= alpha {
                best_score = best_score.max(futility_value);
                continue;
            }
        }

        if context.options.use_see_pruning
            && !pv_node
            && !in_check
            && !gives_check
            && !mv.flag().is_castling()
            && best_move.is_some()
            && shallow_pruning_allowed(best_score)
            && eval_pruning_rule50_safe(position, depth)
            && mover_has_non_pawn_material
        {
            // A capture with strong capture history earns a more forgiving SEE bar:
            // the table has evidence this exchange works out even when the static swap
            // says otherwise.
            // Each kind of move has its own depth window, so a threshold is only
            // consulted where it was calibrated: quiets up to a reduced depth of 7,
            // captures up to a nominal depth of 6.
            let within_window = if quiet {
                effective_depth <= QUIET_SEE_MAX_EFFECTIVE_DEPTH
            } else {
                depth <= CAPTURE_SEE_MAX_DEPTH
            };
            let threshold = if quiet {
                quiet_see_threshold(effective_depth)
            } else {
                capture_see_threshold(depth) - capture_history * 34 / 1024
            };
            if within_window
                && static_exchange_evaluation(position, mv) < threshold
                && (quiet || alpha >= 0 || has_other_non_pawn_material(position, mover, mv))
            {
                continue;
            }
        }

        let mut extension = 0;
        if (context.options.use_singular_ext || context.options.use_multicut)
            && excluded_move.is_none()
            && Some(mv) == tt_move
            && depth >= SINGULAR_MIN_DEPTH + i32::from(tt_pv)
            && !is_shuffling(
                mv,
                ply,
                position,
                &context.current_moves,
                context.repetition_history.plies_since_null(),
            )
            && tt_entry.is_some_and(|entry| {
                matches!(entry.bound, Bound::Lower | Bound::Exact)
                    && tt_entry_depth(entry.depth) >= depth - 3
                    && !is_mate_score(value_from_tt(
                        i32::from(entry.score),
                        ply,
                        position.halfmove_clock(),
                    ))
            })
        {
            let tt_score = value_from_tt(
                i32::from(tt_entry.expect("singular candidate has TT entry").score),
                ply,
                position.halfmove_clock(),
            );
            let singular_beta = singular_beta(tt_score, depth, tt_pv, pv_node);
            let singular_depth = (new_depth / 2).max(1);
            let mut singular_pv = Vec::new();
            let singular_value = pvs(
                position,
                singular_depth,
                singular_beta - 1,
                singular_beta,
                ply,
                false,
                cut_node,
                true,
                true,
                Some(mv),
                context,
                &mut singular_pv,
            )?;
            if context.options.use_multicut
                && let Some(multicut_value) =
                    singular_multicut_value(singular_value, singular_beta, beta)
            {
                return Some(multicut_value);
            }
            if context.options.use_singular_ext {
                extension = singular_extension(
                    singular_value,
                    singular_beta,
                    pv_node,
                    mv.flag().is_capture(),
                    cut_node,
                    tt_score,
                    beta,
                );
            }
        }
        if context.options.use_check_ext {
            extension = check_extension(gives_check, extension);
        }

        let child_depth = (new_depth + extension).max(0);
        let continuation = continuation_key(position, mv);
        let undo = position.make_move(mv);
        context
            .transposition_table
            .prefetch(tt_key(position, child_depth));
        if is_in_check(position, mover) {
            position.unmake_move(mv, undo);
            continue;
        }
        context.push_position(position, ply + 1, mv, continuation, &undo);
        child_pv.clear();
        let score = if searched == 0 {
            pvs(
                position,
                child_depth,
                -beta,
                -alpha,
                ply + 1,
                pv_node,
                if pv_node { false } else { !cut_node },
                true,
                false,
                None,
                context,
                &mut child_pv,
            )
            .map(|score| -score)
        } else {
            // Captures are reduced only when BOTH toggles are on. `use_capture_lmr` is
            // nested inside `use_lmr` deliberately: the `UseLMR=false` arm is the
            // control every other selectivity delta in `bench_cli.rs` is read against,
            // and it has to keep meaning "no late-move reduction of any kind" or every
            // historical LMR anchor silently changes meaning.
            let reduce = if quiet {
                context.options.use_lmr && !gives_check
            } else {
                context.options.use_lmr
                    && context.options.use_capture_lmr
                    && capture_reduction_allowed(mv, tt_move, gives_check)
            };
            let reduced_depth = if reduce && depth >= 2 {
                (child_depth - reduction / 1024).clamp(1, child_depth.max(1))
            } else {
                child_depth
            };
            let scout = pvs(
                position,
                reduced_depth,
                -alpha - 1,
                -alpha,
                ply + 1,
                false,
                if reduced_depth < child_depth {
                    true
                } else {
                    !cut_node
                },
                true,
                false,
                None,
                context,
                &mut child_pv,
            )
            .map(|score| -score);
            scout.and_then(|score| {
                let score = if score > alpha && reduced_depth < child_depth {
                    child_pv.clear();
                    pvs(
                        position,
                        child_depth,
                        -alpha - 1,
                        -alpha,
                        ply + 1,
                        false,
                        !cut_node,
                        true,
                        false,
                        None,
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)?
                } else {
                    score
                };
                if score > alpha && score < beta {
                    child_pv.clear();
                    pvs(
                        position,
                        child_depth,
                        -beta,
                        -alpha,
                        ply + 1,
                        pv_node,
                        if pv_node { false } else { !cut_node },
                        true,
                        false,
                        None,
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)
                } else {
                    Some(score)
                }
            })
        };
        context.pop_position(ply + 1);
        position.unmake_move(mv, undo);
        let score = score?;
        searched += 1;

        if score > best_score {
            best_score = score;
            best_move = Some(mv);
            pv.clear();
            pv.push(mv);
            pv.extend_from_slice(&child_pv);
        }
        alpha = alpha.max(score);
        if alpha >= beta {
            // History MAINTENANCE is unconditional. Every consumer gates only its own
            // READ. Gating writes on `use_lmr` was harmless while LMR was the sole
            // consumer, but becomes the AGENTS.md 4.4 confound now that move ordering,
            // history pruning, and the capture SEE margin all read these tables.
            update_histories(
                position,
                context,
                ply,
                depth,
                mover,
                pawn_key,
                mv,
                quiet,
                &searched_quiets,
                &searched_captures,
            );
            break;
        }
        if quiet {
            searched_quiets.push(mv);
        } else {
            searched_captures.push(mv);
        }
    }

    if searched == 0 {
        return Some(if excluded_move.is_some() {
            alpha
        } else if is_in_check(position, position.side_to_move()) {
            -MATE_SCORE + ply as i32
        } else {
            0
        });
    }

    let bound = if best_score <= original_alpha {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    };
    // Correction MAINTENANCE is unconditional, exactly as ordering-history maintenance
    // is (mission AGENTS.md 4.4): `use_correction_history` gates only the READ in
    // `correction_value`. That keeps the toggle a pure measurement control instead of
    // also changing which information the search collects.
    //
    // Skipped at verification nodes, which deliberately search a restricted move set and
    // whose scores are therefore not evidence about this position class.
    if !verification_node && excluded_move.is_none() {
        update_correction_histories(
            position,
            context,
            ply,
            CorrectionNode {
                depth,
                in_check,
                static_eval,
                best_score,
                best_move: (best_score > original_alpha).then_some(best_move).flatten(),
            },
        );
    }
    if !verification_node {
        context.transposition_table.store(
            key,
            EntryData {
                best_move,
                score: value_to_tt(best_score, ply) as i16,
                static_eval: raw_static_eval as i16,
                depth: tt_stored_depth(depth),
                bound,
                age: context.generation,
                pv: pv_node,
            },
        );
    }
    Some(best_score)
}

#[allow(clippy::too_many_arguments)]
fn quiescence(
    position: &mut Position,
    mut alpha: i32,
    beta: i32,
    ply: usize,
    pv_node: bool,
    count_node: bool,
    // True only at the ply the main search dropped into quiescence at, where quiet
    // checks widen the move list. Every recursive qsearch call passes `false`: the
    // widening is one ply deep because a quiet check costs a node without resolving any
    // material, so applying it at every qsearch ply grows the tree geometrically for
    // tactics the first ply has already had its chance to see.
    first_qsearch_ply: bool,
    context: &mut SearchContext<'_>,
    pv: &mut Vec<Move>,
) -> Option<i32> {
    if count_node && !context.visit_node(ply) {
        return None;
    }
    context.seldepth = context.seldepth.max(ply as u32);
    if alpha < 0
        && context
            .repetition_history
            .upcoming_repetition(position, ply)
    {
        alpha = draw_value(context.nodes);
        if alpha >= beta {
            return Some(alpha);
        }
    }
    if let Some(draw_score) = context.rule_draw_score(position, ply) {
        return Some(draw_score);
    }
    if ply >= MAX_SEARCH_PLY {
        return Some(context.static_eval(position));
    }

    let in_check = is_in_check(position, position.side_to_move());
    let searches_quiet_checks =
        context.options.use_qsearch_checks && first_qsearch_ply && !in_check;
    // Qsearch nodes carry their own TT depth domain. An in-check node searches EVERY
    // move, so its score is a full-width bound and must never be read by a node that
    // only searched captures; the two domains are stored under different depths and a
    // cutoff requires the stored depth to be at least as informative as this node's.
    //
    // A first-ply node that widened with quiet checks joins the checks domain for the
    // same reason: it searched strictly more than the captures, so its bound is
    // legitimate evidence for a captures-only probe, while a captures-only entry must
    // never satisfy IT -- taking that cutoff would silently discard the widening this
    // feature exists to perform.
    let qsearch_depth = if in_check || searches_quiet_checks {
        QSEARCH_CHECKS_TT_DEPTH
    } else {
        QSEARCH_CAPTURES_TT_DEPTH
    };
    let key = tt_key(position, 0);
    let original_alpha = alpha;
    // The toggle gates only what qsearch READS. Stores stay unconditional so that
    // turning it off changes which information the search consumes, never which
    // information it produces (AGENTS.md 4.4).
    let tt_entry = context
        .options
        .use_qsearch_tt
        .then(|| context.transposition_table.probe(key))
        .flatten();
    let mut tt_move = None;
    if let Some(entry) = tt_entry {
        tt_move = entry.best_move;
        if tt_entry_depth(entry.depth) >= qsearch_depth {
            let score = value_from_tt(i32::from(entry.score), ply, position.halfmove_clock());
            let bound_allows_cutoff = match entry.bound {
                Bound::Exact => true,
                Bound::Lower => score >= beta,
                Bound::Upper => score <= alpha,
            };
            // PV nodes are exempt, exactly as in the interior search. A PV qsearch node
            // has to RETURN a principal variation, and a bound cutoff returns a score
            // with no move attached; taking it truncates the PV, which then has to be
            // rebuilt by re-searching. Allowing the cutoff here measured 2.2x the nodes
            // to depth 12 in the endgame position.
            if !pv_node && bound_allows_cutoff && position.halfmove_clock() < 96 {
                return Some(score);
            }
        }
    }

    // The standing pat is corrected too. Qsearch is the majority of nodes, so leaving
    // it on the raw eval would confine corrhist to the small minority of nodes where it
    // matters least, and would also make the standing pat disagree with the parent's
    // corrected eval about the same position class.
    //
    // The RAW value is what the TT keeps, for the same reason as in `pvs`: a stored
    // correction would be applied a second time on top of itself on the next probe.
    let raw_static_eval = if in_check {
        i32::from(UNEVALUATED_STATIC_EVAL)
    } else {
        tt_entry
            .filter(|entry| entry.static_eval != UNEVALUATED_STATIC_EVAL)
            .map_or_else(
                || context.static_eval(position),
                |entry| i32::from(entry.static_eval),
            )
    };
    let mut best_score = if in_check {
        -INFINITY
    } else {
        to_corrected_static_eval(raw_static_eval, correction_value(position, context, ply))
    };
    if !in_check {
        if best_score >= beta {
            let score = if has_legal_move(position) {
                best_score
            } else {
                0
            };
            context.transposition_table.store(
                key,
                EntryData {
                    best_move: None,
                    score: value_to_tt(score, ply) as i16,
                    static_eval: raw_static_eval as i16,
                    depth: tt_stored_depth(qsearch_depth),
                    bound: Bound::Lower,
                    age: context.generation,
                    pv: pv_node,
                },
            );
            return Some(score);
        }
        alpha = alpha.max(best_score);
    }

    let ordering = context.ordering(position, ply);
    let moves: Vec<_> = if in_check {
        MovePicker::new(position, tt_move, [None, None], ordering).collect()
    } else {
        quiescence_moves(
            position,
            tt_move,
            qsearch_see_threshold(context.options.use_see_pruning),
            searches_quiet_checks,
            ordering,
        )
    };
    let mut searched = 0usize;
    let mut best_move = None;
    let mut child_pv = Vec::new();
    for mv in moves {
        // Delta pruning: a capture that cannot lift the standing pat to alpha even after
        // winning its victim outright is not worth a node. Checking moves and promotions
        // are exempt -- their value is not bounded by the material they capture -- and
        // the whole test is skipped when in check, where there is no standing pat and
        // every evasion must be searched.
        if context.options.use_qsearch_delta_pruning
            && !in_check
            && Some(mv) != tt_move
            && mv.flag().promotion().is_none()
            && !is_mate_score(alpha)
            && raw_static_eval + QSEARCH_DELTA_MARGIN + captured_material(position, mv) <= alpha
            && !move_gives_check(position, mv)
        {
            continue;
        }
        let mover = position.side_to_move();
        let continuation = continuation_key(position, mv);
        let undo = position.make_move(mv);
        context.transposition_table.prefetch(tt_key(position, 0));
        if is_in_check(position, mover) {
            position.unmake_move(mv, undo);
            continue;
        }
        context.push_position(position, ply + 1, mv, continuation, &undo);
        child_pv.clear();
        let score = quiescence(
            position,
            -beta,
            -alpha,
            ply + 1,
            pv_node && searched == 0,
            true,
            false,
            context,
            &mut child_pv,
        )
        .map(|score| -score);
        context.pop_position(ply + 1);
        position.unmake_move(mv, undo);
        let score = score?;
        searched += 1;

        if score > best_score {
            best_score = score;
            best_move = Some(mv);
            pv.clear();
            pv.push(mv);
            pv.extend_from_slice(&child_pv);
        }
        alpha = alpha.max(score);
        if alpha >= beta {
            break;
        }
    }

    let best_score = if searched != 0 {
        best_score
    } else if in_check {
        // A checkmate is exact at any depth, so it is stored under the checks domain and
        // is legitimately readable by capture-domain probes as well.
        -MATE_SCORE + ply as i32
    } else if has_legal_move(position) {
        best_score
    } else {
        0
    };

    let bound = if best_score >= beta {
        Bound::Lower
    } else if best_score > original_alpha {
        Bound::Exact
    } else {
        Bound::Upper
    };
    context.transposition_table.store(
        key,
        EntryData {
            best_move,
            score: value_to_tt(best_score, ply) as i16,
            static_eval: raw_static_eval as i16,
            depth: tt_stored_depth(qsearch_depth),
            bound,
            age: context.generation,
            pv: pv_node,
        },
    );
    Some(best_score)
}

fn value_to_tt(score: i32, ply: usize) -> i32 {
    if score >= TABLEBASE_WIN_IN_MAX_PLY {
        score + ply as i32
    } else if score <= -TABLEBASE_WIN_IN_MAX_PLY {
        score - ply as i32
    } else {
        score
    }
}

#[inline]
fn draw_value(nodes: u64) -> i32 {
    -1 + (nodes & 0x2) as i32
}

/// Percentage to scale the soft time limit by before the next iteration.
///
/// Two independent reasons to keep thinking, composed additively: the root move is
/// still moving, and the score is falling. A rising score buys nothing -- finding out
/// that the position is even better than believed does not change the move.
fn time_scale_percent(stability: u32, score_change: i32) -> u32 {
    let stability_percent = TIME_STABILITY_BASE_PERCENT
        .saturating_sub(TIME_STABILITY_STEP_PERCENT * stability.min(TIME_STABILITY_CAP));
    let falling = (-score_change).max(0);
    let falling_percent = (falling / TIME_FALLING_SCORE_STEP) as u32 * TIME_FALLING_STEP_PERCENT;
    stability_percent
        .saturating_add(falling_percent)
        .min(TIME_SCALE_MAX_PERCENT)
}

/// Percentage to scale the soft time limit by, from the best root move's share of the
/// tree the finished iteration searched.
///
/// The signal is a claim about the ALTERNATIVES, not about the best move. A low share
/// means the other root moves are still expensive to refute -- they keep failing high
/// on the null window and forcing full re-searches -- so the position has not decided
/// itself yet and another iteration is worth paying for. A high share means every rival
/// is being dismissed cheaply on a TT bound, and further thinking mostly re-confirms a
/// decision already made.
///
/// Linear between the two anchors rather than a step, per the reference engine's own
/// evolution of this term (hard stop -> multiplicative factor -> continuous
/// interpolation). Fractions are carried in PER-MILLE and the arithmetic is integer, so
/// the result is bit-reproducible on every target -- a float here would make the time
/// manager, and therefore the games, platform-dependent.
fn time_effort_percent(best_move_nodes: u64, total_nodes: u64) -> u32 {
    // No root move has finished a subtree yet: nothing has been measured, so claim
    // nothing. Reachable at depth 1 of a search stopped inside its first root move.
    if total_nodes == 0 {
        return TIME_EFFORT_NEUTRAL_PERCENT;
    }
    let permille = (best_move_nodes.min(total_nodes) * 1000 / total_nodes) as u32;
    if permille <= TIME_EFFORT_LOW_PERMILLE {
        return TIME_EFFORT_LOW_PERCENT;
    }
    if permille >= TIME_EFFORT_HIGH_PERMILLE {
        return TIME_EFFORT_HIGH_PERCENT;
    }
    let span = TIME_EFFORT_HIGH_PERMILLE - TIME_EFFORT_LOW_PERMILLE;
    let drop = TIME_EFFORT_LOW_PERCENT - TIME_EFFORT_HIGH_PERCENT;
    TIME_EFFORT_LOW_PERCENT - (permille - TIME_EFFORT_LOW_PERMILLE) * drop / span
}

/// Composes the stability and effort factors, both in percent.
///
/// Multiplicative, and clamped by the same ceiling the stability factor alone obeys, so
/// adding a second factor cannot raise the maximum a soft limit can be stretched to.
fn scaled_time_percent(stability_percent: u32, effort_percent: u32) -> u32 {
    (stability_percent * effort_percent / 100).min(TIME_SCALE_MAX_PERCENT)
}

/// Encodes a search depth into the biased `u8` the TT entry carries.
#[inline]
fn tt_stored_depth(depth: i32) -> u8 {
    (depth + TT_DEPTH_OFFSET).clamp(0, i32::from(u8::MAX)) as u8
}

/// Decodes a stored TT depth back into the search's own depth scale.
#[inline]
fn tt_entry_depth(stored: u8) -> i32 {
    i32::from(stored) - TT_DEPTH_OFFSET
}

#[inline]
fn tt_entry_for_node(
    transposition_table: &TranspositionTable,
    key: u64,
    verification_node: bool,
) -> Option<EntryData> {
    if verification_node {
        None
    } else {
        transposition_table.probe(key)
    }
}

fn tt_cutoff_is_safe(
    position: &Position,
    entry: EntryData,
    score: i32,
    depth: i32,
    beta: i32,
    ply: usize,
    transposition_table: &TranspositionTable,
) -> bool {
    if position.halfmove_clock() >= 96 {
        return false;
    }

    if depth <= 8
        && position.halfmove_clock() >= 80
        && entry.best_move.is_some_and(|mv| {
            mv.flag().is_capture()
                || position
                    .piece_at(mv.from())
                    .is_some_and(|piece| piece.kind() == PieceKind::Pawn)
        })
    {
        return false;
    }

    if depth < 7 || is_decisive_score(score) {
        return true;
    }
    let Some(tt_move) = entry.best_move else {
        return true;
    };
    if !generate_legal_moves(position).contains(&tt_move) {
        return true;
    }

    let mut child = position.clone();
    child.make_move(tt_move);
    let Some(child_entry) = transposition_table.probe(tt_key(&child, depth - 1)) else {
        return true;
    };
    let child_score = value_from_tt(
        i32::from(child_entry.score),
        ply + 1,
        child.halfmove_clock(),
    );
    (score >= beta) == (-child_score >= beta)
}

/// How far below alpha the static eval must sit before the node drops to quiescence.
///
/// Linear, and capped by `RAZOR_MAX_DEPTH`. The quadratic form reached 20835cp by
/// depth 8 and was unbounded in depth, which is another way of writing "never fires".
#[inline]
fn razoring_margin(depth: i32) -> i32 {
    RAZOR_BASE_MARGIN + RAZOR_MARGIN_PER_DEPTH * depth.max(0)
}

/// Margin the static eval must clear above beta before the node is cut without search.
///
/// Linear in depth. The previous form was quadratic-ish (`8 * (45 + 4d) * d`), which
/// demanded 392cp at depth 1 and 6800cp at depth 10 -- so large that the cutoff almost
/// never fired and the technique was decorative. A margin of ~105cp per ply is what
/// makes reverse futility actually prune, and reaching depth is what wins games: at
/// equal time this engine was searching 3.24 plies SHALLOWER than the reference.
///
/// `improving` shortens the effective depth by a ply, because a node whose eval is
/// rising is likelier to hold up; a node the TT marked PV pays a surcharge.
#[inline]
fn reverse_futility_margin(depth: i32, improving: bool, cut_node: bool, tt_pv: bool) -> i32 {
    let effective_depth = depth - i32::from(improving && !cut_node);
    RFP_MARGIN_PER_DEPTH * effective_depth.max(0) + RFP_TT_PV_MARGIN * i32::from(tt_pv)
}

#[inline]
fn is_improving(static_eval: i32, same_side_previous_eval: Option<i32>) -> bool {
    same_side_previous_eval.is_none_or(|previous| static_eval > previous)
}

const fn build_lmp_table() -> [[usize; LMP_MAX_DEPTH as usize + 1]; 2] {
    let mut table = [[0; LMP_MAX_DEPTH as usize + 1]; 2];
    let mut improving = 0;
    while improving < table.len() {
        let mut depth = 1;
        while depth < table[improving].len() {
            table[improving][depth] = (LMP_BASE + depth * depth) / (2 - improving);
            depth += 1;
        }
        improving += 1;
    }
    table
}

#[inline]
fn late_move_pruning_threshold(depth: i32, improving: bool) -> usize {
    let depth = depth.clamp(1, LMP_MAX_DEPTH) as usize;
    LMP_TABLE[usize::from(improving)][depth]
}

fn lmr_table() -> &'static [i32; LMR_TABLE_SIZE] {
    static TABLE: OnceLock<[i32; LMR_TABLE_SIZE]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0; LMR_TABLE_SIZE];
        for (index, reduction) in table.iter_mut().enumerate().skip(1) {
            *reduction = (2872.0 / 128.0 * (index as f64).ln()) as i32;
        }
        table
    })
}

#[inline]
fn late_move_reduction(
    depth: i32,
    move_count: usize,
    improving: bool,
    cut_node: bool,
    tt_pv: bool,
    history_score: i32,
) -> i32 {
    let table = lmr_table();
    let depth_index = depth.clamp(1, MAX_SEARCH_PLY as i32) as usize;
    let move_index = move_count.clamp(1, MAX_SEARCH_PLY);
    let scale = table[depth_index] * table[move_index];
    let mut reduction = scale + 982;
    if !improving {
        reduction += scale * 197 / 512;
    }
    if cut_node {
        reduction += 1024;
    }
    if tt_pv {
        reduction -= 1024;
    }
    reduction -= history_score * 439 / 4096;
    reduction.max(0)
}

/// Weight on captured material in a capture's `statScore`, in 128ths.
///
/// A capture's protection from LMR is proportional to what it WINS, not a flat offset.
/// A flat one-ply discount was tried first and measured: it grew fixed-depth nodes by
/// +5.7% at depth 10 and +51.6% at depth 12, because it protected a late pawn grab
/// exactly as much as it protected taking a hanging queen, and the searches it then had
/// to re-run at full depth cost more than the reductions saved. Numbers in
/// `experiments/MSN-S2-capture-lmr/results.md`.
///
/// At 873/128 a pawn (100) contributes 682 and a queen (900) contributes 6,139, so the
/// `439/4096` divisor the quiet formula already applies turns those into 0.07 and 0.64
/// plies of protection respectively. That is the intended shape: a queen is worth more
/// than a ply back once its own capture history agrees, a pawn is worth almost nothing.
const CAPTURE_STAT_MATERIAL_WEIGHT: i32 = 873;

/// The `statScore` a capture presents to the shared reduction formula.
///
/// Captured material plus capture history, in place of the butterfly-and-continuation
/// sum a quiet presents. Both are consumed by exactly the same `-statScore * 439 / 4096`
/// term, so the two move kinds share one reduction shape and differ only in the evidence
/// they feed it.
#[inline]
fn capture_stat_score(captured_material: i32, capture_history: i32) -> i32 {
    CAPTURE_STAT_MATERIAL_WEIGHT * captured_material / 128 + capture_history
}

/// LMR for a late capture: the quiet formula, fed a capture `statScore`.
#[inline]
#[allow(clippy::too_many_arguments)]
fn capture_late_move_reduction(
    depth: i32,
    move_count: usize,
    improving: bool,
    cut_node: bool,
    tt_pv: bool,
    captured_material: i32,
    capture_history: i32,
) -> i32 {
    late_move_reduction(
        depth,
        move_count,
        improving,
        cut_node,
        tt_pv,
        capture_stat_score(captured_material, capture_history),
    )
}

/// Whether a capture may be reduced at all, before the table is consulted.
///
/// Three exemptions, each for a different reason:
///
/// * The **TT move** is the engine's own best guess at this node. Reducing it reduces
///   the move most likely to be the answer, and the verification re-search then pays
///   for the reduction twice on the move that least needed it.
/// * A **checking capture** forces a reply, so the subtree under it bears no
///   resemblance to the one a reduced scout would search: the reply set is tiny and the
///   real cost is one ply deeper. This mirrors the quiet exemption on `gives_check`.
/// * A **queen promotion** adds a queen. Whatever the ordering thinks of the square, a
///   move that changes the material balance by nine points is not a late move in any
///   sense the reduction table models. Under-promotions are NOT exempt: they are
///   genuinely rare tactical shots and the ordering evidence against them is real.
#[inline]
fn capture_reduction_allowed(mv: Move, tt_move: Option<Move>, gives_check: bool) -> bool {
    Some(mv) != tt_move && !gives_check && mv.flag().promotion() != Some(PieceKind::Queen)
}

#[inline]
fn frontier_futility_margin(effective_depth: i32) -> i32 {
    FUTILITY_BASE_MARGIN + FUTILITY_MARGIN_PER_DEPTH * effective_depth.max(0)
}

#[inline]
fn quiet_see_threshold(effective_depth: i32) -> i32 {
    -QUIET_SEE_MARGIN_PER_DEPTH * effective_depth.max(0).pow(2)
}

#[inline]
fn capture_see_threshold(depth: i32) -> i32 {
    -CAPTURE_SEE_MARGIN_PER_DEPTH * depth.max(0)
}

#[inline]
const fn qsearch_see_threshold(enabled: bool) -> i32 {
    if enabled {
        QSEARCH_SEE_THRESHOLD
    } else {
        // With the toggle off qsearch must search EVERY capture, so the gate has to
        // admit arbitrarily losing exchanges. Returning 0 here would leave the shipped
        // threshold in place and make the toggle inert.
        i32::MIN
    }
}

#[inline]
fn internal_iterative_reduction(
    depth: i32,
    in_check: bool,
    tt_move: Option<Move>,
    excluded_move: Option<Move>,
) -> i32 {
    // A node with no TT move has nothing to order by, so its first search is worth a
    // ply less wherever it sits -- PV node or not, cut node or not. Restricting this to
    // expected cut nodes left every other move-less node paying full depth for an
    // unordered search.
    i32::from(depth >= IIR_MIN_DEPTH && !in_check && tt_move.is_none() && excluded_move.is_none())
}

#[inline]
fn probcut_beta(beta: i32, improving: bool) -> i32 {
    beta + PROBCUT_BASE_MARGIN - PROBCUT_IMPROVING_MARGIN * i32::from(improving)
}

#[inline]
fn probcut_depth(depth: i32, improving: bool) -> i32 {
    depth - if improving { 5 } else { 3 }
}

#[inline]
fn probcut_cutoff_value(value: i32, beta: i32, probcut_beta: i32) -> Option<i32> {
    (!is_mate_score(value)).then_some(value - (probcut_beta - beta))
}

#[inline]
fn singular_beta(tt_score: i32, depth: i32, tt_pv: bool, pv_node: bool) -> i32 {
    tt_score - (59 + 66 * i32::from(tt_pv && !pv_node)) * depth / 63
}

#[inline]
fn singular_extension(
    value: i32,
    singular_beta: i32,
    pv_node: bool,
    tt_capture: bool,
    cut_node: bool,
    tt_score: i32,
    beta: i32,
) -> i32 {
    if value < singular_beta {
        let double_margin = 16 + 16 * i32::from(pv_node) + 8 * i32::from(!tt_capture);
        1 + i32::from(value < singular_beta - double_margin)
    } else if tt_score >= beta && !is_mate_score(value) {
        -3
    } else if cut_node {
        -2
    } else {
        0
    }
}

#[inline]
fn singular_multicut_value(value: i32, singular_beta: i32, beta: i32) -> Option<i32> {
    (value >= singular_beta && value >= beta && !is_mate_score(value)).then_some(value)
}

#[inline]
fn check_extension(gives_check: bool, extension: i32) -> i32 {
    if gives_check {
        extension.max(1)
    } else {
        extension
    }
}

fn is_shuffling(
    mv: Move,
    ply: usize,
    position: &Position,
    current_moves: &[Option<Move>; MAX_SEARCH_PLY],
    plies_since_null: usize,
) -> bool {
    if mv.flag().is_capture() || position.halfmove_clock() < 10 {
        return false;
    }
    if plies_since_null < 6 || ply < 20 {
        return false;
    }
    let Some(two_plies_ago) = current_moves[ply - 2] else {
        return false;
    };
    let Some(four_plies_ago) = current_moves[ply - 4] else {
        return false;
    };
    mv.from() == two_plies_ago.to() && two_plies_ago.from() == four_plies_ago.to()
}

#[inline]
fn shallow_pruning_allowed(best_score: i32) -> bool {
    !(best_score < 0 && is_mate_score(best_score))
}

fn has_other_non_pawn_material(position: &Position, color: Color, mv: Move) -> bool {
    let moved_kind = position
        .piece_at(mv.from())
        .expect("candidate move must have a moving piece")
        .kind();
    PieceKind::NON_PAWN_MATERIAL.into_iter().any(|kind| {
        let count = position.pieces(color, kind).count();
        count > u32::from(kind == moved_kind)
    })
}

#[inline]
pub(crate) fn move_gives_check(position: &Position, mv: Move) -> bool {
    let moved = position
        .piece_at(mv.from())
        .expect("candidate move must have a moving piece");
    let color = moved.color();
    let Some(enemy_king) = position.pieces(!color, PieceKind::King).first() else {
        return false;
    };
    let mut occupancy = position.occupancy();
    let mut pieces = [Bitboard::EMPTY; 6];
    for kind in PieceKind::ALL {
        pieces[kind.index()] = position.pieces(color, kind);
    }

    if mv.flag().is_castling() {
        let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
        let king_destination = side.king_destination(color);
        let rook_destination = side.rook_destination(color);
        occupancy.clear(mv.from());
        occupancy.clear(mv.to());
        occupancy.set(king_destination);
        occupancy.set(rook_destination);
        pieces[PieceKind::King.index()].clear(mv.from());
        pieces[PieceKind::King.index()].set(king_destination);
        pieces[PieceKind::Rook.index()].clear(mv.to());
        pieces[PieceKind::Rook.index()].set(rook_destination);
    } else {
        occupancy.clear(mv.from());
        if mv.flag().is_en_passant() {
            let capture_offset = if color == Color::White { -8 } else { 8 };
            let capture_square = Square::new((i16::from(mv.to().index()) + capture_offset) as u8)
                .expect("en-passant capture square must be on the board");
            occupancy.clear(capture_square);
        }
        occupancy.set(mv.to());
        pieces[moved.kind().index()].clear(mv.from());
        let placed_kind = mv.flag().promotion().unwrap_or(moved.kind());
        pieces[placed_kind.index()].set(mv.to());
    }

    !(pawn_attacks(enemy_king, !color) & pieces[PieceKind::Pawn.index()]).is_empty()
        || !(knight_attacks(enemy_king) & pieces[PieceKind::Knight.index()]).is_empty()
        || !(king_attacks(enemy_king) & pieces[PieceKind::King.index()]).is_empty()
        || !(bishop_attacks(enemy_king, occupancy)
            & (pieces[PieceKind::Bishop.index()] | pieces[PieceKind::Queen.index()]))
        .is_empty()
        || !(rook_attacks(enemy_king, occupancy)
            & (pieces[PieceKind::Rook.index()] | pieces[PieceKind::Queen.index()]))
        .is_empty()
}

/// Resolves the continuation plane a move will select for its children.
///
/// Must be called BEFORE `make_move`, because the moving piece is no longer on `from`
/// afterwards. A promotion is keyed on the piece that lands on `to`, matching the
/// reference: children respond to the queen that appeared, not to the pawn that left.
#[inline]
fn continuation_key(position: &Position, mv: Move) -> Option<ContinuationKey> {
    let piece = position.piece_at(mv.from())?;
    let kind = mv.flag().promotion().unwrap_or(piece.kind());
    Some(ContinuationKey::new(
        mf_core::Piece::new(piece.color(), kind),
        mv.to(),
    ))
}

#[inline]
fn quiet_history_bonus(depth: i32) -> i32 {
    (32 * depth * depth).clamp(32, 2_048)
}

/// The malus is deliberately steeper in depth than the bonus (the reference uses 968
/// vs 133 per ply): punishing a move that failed to cut carries more information than
/// rewarding one that did.
#[inline]
fn quiet_history_malus(depth: i32) -> i32 {
    (64 * depth * depth).clamp(64, 3_072)
}

/// Maximum LMR-reduced depth at which a quiet may be pruned on history alone.
///
/// History pruning is a shallow-depth technique. A sweep at bench depth 7 showed that
/// applying it at every depth LOSES nodes (-1.7% at the best threshold) because deep
/// quiets get skipped on evidence gathered at shallow plies; capping it recovers the
/// gain. The cap is on `effective_depth`, so a move LMR already wants to reduce hard
/// is the one history is allowed to drop entirely.
const HISTORY_PRUNING_MAX_EFFECTIVE_DEPTH: i32 = 3;

/// A quiet whose combined butterfly-plus-pawn history is below this has repeatedly
/// failed to produce a cutoff and is skipped outright.
#[inline]
fn history_pruning_threshold(effective_depth: i32) -> i32 {
    -1_000 * effective_depth.max(0)
}

/// Applies the cutoff bonus to the move that caused the beta cutoff and the malus to
/// every move searched before it, across all three tables.
///
/// This runs on EVERY cutoff regardless of which read gates are enabled: the tables
/// must describe the same search whether a consumer is reading them or not, otherwise
/// a toggle changes the data as well as its use and the ablation is invalid.
#[allow(clippy::too_many_arguments)]
fn update_histories(
    position: &Position,
    context: &mut SearchContext<'_>,
    ply: usize,
    depth: i32,
    mover: Color,
    pawn_key: u64,
    cutoff_move: Move,
    quiet: bool,
    searched_quiets: &[Move],
    searched_captures: &[Move],
) {
    let history = context.history;
    let bonus = quiet_history_bonus(depth);
    let malus = -quiet_history_malus(depth);

    if quiet {
        context.killers.record_killer(ply, cutoff_move);
        history.update_butterfly(mover, cutoff_move, bonus);
        let planes = context.continuation_planes(ply);
        if let Some(piece) = position.piece_at(cutoff_move.from()) {
            history.update_pawn(pawn_key, piece, cutoff_move.to(), bonus);
            update_continuation_histories(history, &planes, piece, cutoff_move.to(), bonus);
        }
        for &previous in searched_quiets {
            history.update_butterfly(mover, previous, malus);
            if let Some(piece) = position.piece_at(previous.from()) {
                history.update_pawn(pawn_key, piece, previous.to(), malus);
                update_continuation_histories(history, &planes, piece, previous.to(), malus);
            }
        }
    } else if let Some(piece) = position.piece_at(cutoff_move.from()) {
        history.update_capture(
            piece,
            cutoff_move.to(),
            captured_kind(position, cutoff_move),
            bonus,
        );
    }

    // Captures searched before the cutoff are punished whether the cutoff itself was
    // quiet or not: they were tried first and did not work.
    for &previous in searched_captures {
        if previous == cutoff_move {
            continue;
        }
        if let Some(piece) = position.piece_at(previous.from()) {
            history.update_capture(
                piece,
                previous.to(),
                captured_kind(position, previous),
                malus,
            );
        }
    }
}

/// The Zobrist key each hash-keyed correction source is indexed by.
///
/// This is the whole reason M1-F3 built five Zobrist keys instead of one. `material`
/// uses `non_pawn_material`, which is a per-color piece-COUNT hash and is therefore
/// exactly Caissa's original material-configuration corrhist rather than a placement
/// hash. `minor` and `major` are placement hashes of {N,B} and {R,Q}, from Sirius.
#[inline]
fn correction_key(position: &Position, source: usize) -> u64 {
    let keys = position.zobrist();
    match source {
        CORRECTION_PAWN => keys.pawn(),
        CORRECTION_MINOR => keys.minor(),
        CORRECTION_MAJOR => keys.major(),
        CORRECTION_MATERIAL => keys.non_pawn_material(),
        _ => unreachable!("correction source index out of range"),
    }
}

/// The blended correction value for this position, PRE-division.
///
/// Callers divide by [`CORRECTION_SCALE`] to get eval units; the raw magnitude is also
/// the engine's complexity proxy, so it is returned unscaled.
///
/// Absent continuation planes contribute nothing. The reference substitutes a nonzero
/// `64049` when there is no previous move, but that constant is denominated in its
/// NNUE eval scale; against this engine's hand-crafted eval it would be an unmotivated
/// bias at exactly the nodes with the least information (mission AGENTS.md 4.55).
fn correction_value(position: &Position, context: &SearchContext<'_>, ply: usize) -> i32 {
    if !context.options.use_correction_history {
        return 0;
    }
    let color = position.side_to_move();
    let mut value = 0;
    for (source, weight) in CORRECTION_WEIGHTS.iter().enumerate() {
        if !context.options.use_correction_sources[source] {
            continue;
        }
        value += weight
            * context
                .history
                .correction_score(source, correction_key(position, source), color);
    }

    if !context.options.use_correction_sources[CORRECTION_SOURCES] {
        return value;
    }
    let mut continuation = 0;
    for (slot, plane) in context
        .correction_continuation_planes(ply)
        .iter()
        .enumerate()
    {
        if let (Some(plane), Some(entry)) = (*plane, context.previous_continuation_key(ply)) {
            continuation += context
                .history
                .correction_continuation_score(slot, plane, entry);
        }
    }
    value + CORRECTION_CONTINUATION_WEIGHT * continuation
}

/// Applies the learned residual to a static evaluation.
///
/// Clamped away from the decisive range so a correction can never manufacture a mate
/// or tablebase score out of a heuristic residual.
#[inline]
fn to_corrected_static_eval(static_eval: i32, correction: i32) -> i32 {
    (static_eval + correction / CORRECTION_SCALE)
        .clamp(-TABLEBASE_WIN_IN_MAX_PLY + 1, TABLEBASE_WIN_IN_MAX_PLY - 1)
}

/// Records the residual between the search result and the static evaluation.
///
/// The guard is the reference's verbatim, and each clause earns its place:
///
/// * `!in_check` — there is no meaningful static eval in check, so there is no residual.
/// * `!(best_move is a capture)` — a capture's value comes from material the static eval
///   already sees; crediting it to a position-class hash teaches the wrong feature.
/// * `(best_score > static_eval) == best_move.is_some()` — the residual must agree with
///   the bound direction. A fail-high with no move, or a fail-low with one, is a bound
///   artifact rather than evidence that this position class is mis-evaluated.
///
/// Note `best_move ? 12 : 18`: **fail-lows update ~50% more strongly than fail-highs**,
/// because a fail-low bounds the true value from above and is the more informative side.
///
/// **`best_move` here must mean "a move that RAISED ALPHA", not "the highest-scoring
/// move".** This engine's `pvs` sets its local `best_move` on any score improvement
/// starting from `-INFINITY`, so it is `Some` at essentially every node with a legal
/// move; the reference only assigns `bestMove` when `value > alpha`. Passing the local
/// variable straight through would make the fail-low arm unreachable and silently
/// delete the `18/128` half of the update rule. The caller therefore passes
/// `(best_score > original_alpha).then_some(best_move).flatten()`.
fn update_correction_histories(
    position: &Position,
    context: &mut SearchContext<'_>,
    ply: usize,
    node: CorrectionNode,
) {
    let CorrectionNode {
        depth,
        in_check,
        static_eval,
        best_score,
        best_move,
    } = node;
    if in_check
        || best_move.is_some_and(|mv| mv.flag().is_capture())
        || (best_score > static_eval) != best_move.is_some()
    {
        return;
    }

    let scale = if best_move.is_some() { 12 } else { 18 };
    let bonus = ((best_score - static_eval) * depth * scale / 128)
        .clamp(-CORRECTION_MAX / 4, CORRECTION_MAX / 4);
    let bonus = 1_061 * bonus / 1_024;

    let color = position.side_to_move();
    for source in 0..CORRECTION_SOURCES {
        context
            .history
            .update_correction(source, correction_key(position, source), color, bonus);
    }
    let entry = context.previous_continuation_key(ply);
    for (slot, plane) in context
        .correction_continuation_planes(ply)
        .iter()
        .enumerate()
    {
        if let (Some(plane), Some(entry)) = (*plane, entry) {
            context
                .history
                .update_correction_continuation(slot, plane, entry, bonus);
        }
    }
}

/// The node-local inputs to a correction-history update.
struct CorrectionNode {
    depth: i32,
    in_check: bool,
    static_eval: i32,
    best_score: i32,
    /// A move that RAISED ALPHA, not merely the highest-scoring one. See the note on
    /// [`update_correction_histories`].
    best_move: Option<Move>,
}

/// Applies one bonus across every continuation table, weighted by lookback distance.
///
/// The weights are non-monotone in the reference's full 1-6 set; over the `{1,2,4,6}`
/// subset this engine keeps they happen to decrease, which is asserted in
/// `history.rs`. Absent planes are skipped rather than updated with zero.
#[inline]
fn update_continuation_histories(
    history: &SharedHistory,
    planes: &[Option<ContinuationKey>; CONTINUATION_PLIES.len()],
    piece: mf_core::Piece,
    to: Square,
    bonus: i32,
) {
    for (slot, previous) in planes.iter().enumerate() {
        if let Some(previous) = *previous {
            history.update_continuation_at(
                slot,
                previous,
                piece,
                to,
                bonus * CONTINUATION_WEIGHTS[slot] / 1_024,
            );
        }
    }
}

#[inline]
fn null_move_reduction(depth: i32, static_eval: i32, beta: i32) -> i32 {
    5 + depth / 3 + ((static_eval - beta).max(0) / 200).min(3)
}

#[inline]
fn requires_null_move_verification(nmp_min_ply: usize, depth: i32) -> bool {
    nmp_min_ply == 0 && depth >= NMP_VERIFICATION_DEPTH
}

#[inline]
fn null_move_verification_min_ply(ply: usize, verification_depth: i32) -> usize {
    let verification_span = (3 * usize::try_from(verification_depth.max(0)).unwrap_or(0)) / 4;
    ply + verification_span.max(1)
}

#[inline]
fn eval_pruning_rule50_safe(position: &Position, depth: i32) -> bool {
    i32::from(position.halfmove_clock()) + depth < 100
}

fn has_non_pawn_material(position: &Position) -> bool {
    has_non_pawn_material_for(position, position.side_to_move())
}

fn has_non_pawn_material_for(position: &Position, color: Color) -> bool {
    PieceKind::NON_PAWN_MATERIAL
        .into_iter()
        .any(|kind| !position.pieces(color, kind).is_empty())
}

fn tt_key(position: &Position, _depth: i32) -> u64 {
    adjust_key50(position.repetition_key(), position.halfmove_clock())
}

#[inline]
fn adjust_key50(key: u64, rule50: u16) -> u64 {
    const RULE50_SALT: u64 = 0x6a09_e667_f3bc_c909;

    if rule50 < 14 {
        return key;
    }
    let bucket = (rule50 - 14) / 8;
    let mut value = u64::from(bucket).wrapping_add(RULE50_SALT);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    key ^ (value ^ (value >> 31))
}

fn value_from_tt(score: i32, ply: usize, rule50: u16) -> i32 {
    let headroom = 100 - i32::from(rule50.min(100));
    if score >= TABLEBASE_WIN_IN_MAX_PLY {
        if is_mate_score(score) && MATE_SCORE - score > headroom {
            return TABLEBASE_WIN_IN_MAX_PLY - 1;
        }
        if TABLEBASE_SCORE - score > headroom {
            return TABLEBASE_WIN_IN_MAX_PLY - 1;
        }
        return score - ply as i32;
    }
    if score <= -TABLEBASE_WIN_IN_MAX_PLY {
        if is_mate_score(score) && MATE_SCORE + score > headroom {
            return -TABLEBASE_WIN_IN_MAX_PLY + 1;
        }
        if TABLEBASE_SCORE + score > headroom {
            return -TABLEBASE_WIN_IN_MAX_PLY + 1;
        }
        return score + ply as i32;
    }
    score
}

struct SearchContext<'a> {
    transposition_table: &'a TranspositionTable,
    stop: &'a AtomicBool,
    limits: SearchLimits,
    options: SearchOptions,
    started: Option<Instant>,
    worker_id: usize,
    node_counters: &'a [AtomicU64],
    nodes: u64,
    seldepth: u32,
    iterations: Vec<IterationInfo>,
    history: &'a SharedHistory,
    evaluator: SearchEvaluator<'a>,
    killers: KillerTable,
    static_evals: [Option<i32>; MAX_SEARCH_PLY],
    repetition_history: RepetitionHistory,
    current_moves: [Option<Move>; MAX_SEARCH_PLY],
    /// The continuation plane each searched ply's move selects, resolved at push time
    /// because the moving piece is no longer on `from` once the move is made. `None`
    /// marks a null move, which BREAKS the chain: "the reply to the opponent's last
    /// move" is meaningless when the opponent did not move.
    continuation_keys: [Option<ContinuationKey>; MAX_SEARCH_PLY],
    nmp_min_ply: usize,
    stopped: bool,
    soft_time_reached: bool,
    /// Percentage the soft limit is scaled by, updated between iterations.
    time_scale_percent: u32,
    /// Nodes spent under each root move during the CURRENT root search, and their sum.
    ///
    /// A flat vector rather than a map: a root move list is at most 218 entries and is
    /// walked once per iteration, so a linear scan is cheaper than hashing. Reset at
    /// the start of every root search, so an aspiration re-search replaces the previous
    /// distribution rather than adding to it.
    root_effort: Vec<(Move, u64)>,
    root_effort_total: u64,
    generation: u8,
    /// Sink for `currmove` progress, set only for the worker that owns the clock.
    ///
    /// Boxed rather than generic because `SearchContext` is threaded through every node
    /// of the search; adding a type parameter for a callback used once per root move
    /// would infect the whole recursion for no benefit.
    root_move_reporter: Option<Box<dyn FnMut(RootMoveInfo) + 'a>>,
}

impl<'a> SearchContext<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        transposition_table: &'a TranspositionTable,
        limits: SearchLimits,
        started: Option<Instant>,
        stop: &'a AtomicBool,
        position: &Position,
        position_history: &[u64],
        history: &'a SharedHistory,
        options: SearchOptions,
        worker_id: usize,
        generation: u8,
        node_counters: &'a [AtomicU64],
        network: &'a Network,
    ) -> Self {
        Self {
            transposition_table,
            stop,
            limits,
            options,
            started,
            worker_id,
            node_counters,
            nodes: 0,
            seldepth: 0,
            iterations: Vec::new(),
            history,
            evaluator: SearchEvaluator::new(network, position),
            killers: KillerTable::new(),
            static_evals: [None; MAX_SEARCH_PLY],
            repetition_history: RepetitionHistory::new(position, position_history),
            current_moves: [None; MAX_SEARCH_PLY],
            continuation_keys: [None; MAX_SEARCH_PLY],
            nmp_min_ply: 0,
            stopped: false,
            soft_time_reached: false,
            time_scale_percent: 100,
            root_effort: Vec::new(),
            root_effort_total: 0,
            generation,
            root_move_reporter: None,
        }
    }

    /// Reports the root move about to be searched, if this worker reports progress.
    ///
    /// Reported from the first move of the first depth. The reference engine withholds
    /// these lines until a few seconds in, on the reasoning that early depths resolve
    /// faster than a GUI can draw them; we report unconditionally instead, so a GUI that
    /// drives short searches still gets progress rather than silence.
    fn report_root_move(&mut self, depth: u32, best_move: Move, move_number: usize) {
        if let Some(reporter) = self.root_move_reporter.as_mut() {
            reporter(RootMoveInfo {
                depth,
                best_move,
                move_number,
            });
        }
    }

    /// Bundles the shared tables with the per-consumer read gates.
    ///
    /// The `pawn_key` is read from the position at each call site rather than cached,
    /// because pawn history must follow the pawn structure of the node being ordered.
    fn ordering(&self, position: &Position, ply: usize) -> OrderingContext<'a> {
        OrderingContext {
            history: self.history,
            pawn_key: position.zobrist().pawn(),
            continuation: self.continuation_planes(ply),
            use_butterfly_history: self.options.use_butterfly_history,
            use_capture_history: self.options.use_capture_history,
            use_pawn_history: self.options.use_pawn_history,
            use_continuation_history: self.options.use_continuation_history,
        }
    }

    fn visit_node(&mut self, ply: usize) -> bool {
        if self.stopped || self.stop.load(Ordering::Relaxed) {
            self.stopped = true;
            return false;
        }
        if self.node_counters.len() == 1
            && self.limits.nodes.is_some_and(|limit| self.nodes >= limit)
        {
            self.stopped = true;
            return false;
        }

        self.nodes += 1;
        self.seldepth = self.seldepth.max(ply as u32);
        if self.nodes.is_multiple_of(NODE_PUBLISH_INTERVAL) {
            self.publish_nodes();
            if self.node_counters.len() > 1
                && self
                    .limits
                    .nodes
                    .is_some_and(|limit| published_node_total(self.node_counters) >= limit)
            {
                self.stopped = true;
                return false;
            }
        }
        if self.worker_id == 0 && self.nodes.is_multiple_of(TIME_CHECK_INTERVAL) {
            let elapsed = self.elapsed();
            self.soft_time_reached = self
                .scaled_soft_time()
                .is_some_and(|limit| elapsed >= limit);
            if self.limits.hard_time.is_some_and(|limit| elapsed >= limit) {
                self.stopped = true;
                return false;
            }
        }
        true
    }

    fn should_stop_after_iteration(&self) -> bool {
        self.stopped
            || self.limits.nodes.is_some_and(|limit| self.nodes >= limit)
            || self.soft_time_reached
    }

    /// Sets the percentage the soft limit is scaled by before the next iteration.
    fn set_time_scale(&mut self, percent: u32) {
        self.time_scale_percent = percent;
    }

    /// Discards the previous root search's per-move node distribution.
    fn begin_root_effort(&mut self) {
        self.root_effort.clear();
        self.root_effort_total = 0;
    }

    /// Credits `nodes` to `mv`'s subtree in the current root search.
    fn record_root_effort(&mut self, mv: Move, nodes: u64) {
        self.root_effort_total = self.root_effort_total.saturating_add(nodes);
        if let Some(entry) = self.root_effort.iter_mut().find(|(move_, _)| *move_ == mv) {
            entry.1 = entry.1.saturating_add(nodes);
        } else {
            self.root_effort.push((mv, nodes));
        }
    }

    /// The effort factor for the move the finished iteration chose.
    ///
    /// Reads THIS worker's own counts, never the aggregated pool total. Only worker 0
    /// owns the clock, and only worker 0's root loop filled `root_effort`; the helpers
    /// search their own trees with their own root move orders, so summing the pool
    /// would divide one worker's subtree by every worker's nodes and drive the
    /// fraction toward zero as threads are added. Reading worker 0 alone makes the
    /// factor thread-count invariant by construction.
    fn best_move_effort_percent(&self, best_move: Option<Move>) -> u32 {
        if !self.options.use_time_effort {
            return TIME_EFFORT_NEUTRAL_PERCENT;
        }
        let Some(best_move) = best_move else {
            return TIME_EFFORT_NEUTRAL_PERCENT;
        };
        let best_nodes = self
            .root_effort
            .iter()
            .find(|(move_, _)| *move_ == best_move)
            .map_or(0, |(_, nodes)| *nodes);
        time_effort_percent(best_nodes, self.root_effort_total)
    }

    /// The soft limit actually in force, after stability scaling.
    fn scaled_soft_time(&self) -> Option<Duration> {
        self.limits.soft_time.map(|limit| {
            let scaled = limit
                .saturating_mul(self.time_scale_percent)
                .checked_div(100)
                .unwrap_or(limit);
            // Scaling may only ever move the soft limit within the hard one; the hard
            // limit is the promise that the engine does not forfeit.
            self.limits
                .hard_time
                .map_or(scaled, |hard| scaled.min(hard))
        })
    }

    fn publish_nodes(&self) {
        self.node_counters[self.worker_id].store(self.nodes, Ordering::Relaxed);
    }

    fn reported_nodes(&self) -> u64 {
        if self.worker_id == 0 {
            published_node_total(self.node_counters)
        } else {
            self.nodes
        }
    }

    fn elapsed(&self) -> Duration {
        self.started
            .map_or(Duration::ZERO, |started| started.elapsed())
    }

    fn push_position(
        &mut self,
        position: &Position,
        ply: usize,
        mv: Move,
        continuation: Option<ContinuationKey>,
        undo: &Undo,
    ) {
        self.evaluator
            .push_real(position, mv, undo)
            .expect("search ply is bounded by the NNUE accumulator capacity");
        self.current_moves[ply - 1] = Some(mv);
        self.continuation_keys[ply - 1] = continuation;
        self.repetition_history.push_position(position);
    }

    fn push_null_position(&mut self, ply: usize) {
        self.evaluator
            .push_null()
            .expect("search ply is bounded by the NNUE accumulator capacity");
        self.current_moves[ply - 1] = None;
        self.continuation_keys[ply - 1] = None;
        self.repetition_history.push_null();
    }

    fn pop_position(&mut self, ply: usize) {
        self.evaluator
            .pop()
            .expect("every searched child position has a matching accumulator frame");
        self.repetition_history.pop();
        self.current_moves[ply - 1] = None;
        self.continuation_keys[ply - 1] = None;
    }

    #[inline]
    fn static_eval(&mut self, position: &Position) -> i32 {
        self.evaluator.evaluate(position)
    }

    /// The predecessor plane at each lookback distance in `CONTINUATION_PLIES`.
    ///
    /// A `None` entry means the stack does not reach back that far, or a null move sits
    /// at that distance. Both are genuine absences of information rather than a zero
    /// score, so the corresponding table is skipped entirely rather than contributing 0.
    fn continuation_planes(
        &self,
        ply: usize,
    ) -> [Option<ContinuationKey>; CONTINUATION_PLIES.len()] {
        CONTINUATION_PLIES.map(|distance| {
            ply.checked_sub(distance)
                .and_then(|index| self.continuation_keys[index])
        })
    }

    /// The predecessor plane at each distance in `CORRECTION_CONTINUATION_PLIES`.
    ///
    /// Distinct from `continuation_planes`: correction history looks back 2 and 4 plies
    /// at OUR OWN previous moves, where ordering history looks back 1/2/4/6 to find what
    /// refutes the opponent's move. Reusing the ordering planes here would index the
    /// residual on the wrong side's move.
    fn correction_continuation_planes(
        &self,
        ply: usize,
    ) -> [Option<ContinuationKey>; CORRECTION_CONTINUATION_PLIES.len()] {
        CORRECTION_CONTINUATION_PLIES.map(|distance| {
            ply.checked_sub(distance)
                .and_then(|index| self.continuation_keys[index])
        })
    }

    /// The immediately preceding move, which supplies the `[piece][to]` ENTRY index
    /// inside whichever plane a further-back move selected.
    fn previous_continuation_key(&self, ply: usize) -> Option<ContinuationKey> {
        ply.checked_sub(1)
            .and_then(|index| self.continuation_keys[index])
    }

    fn rule_draw_score(&self, position: &Position, ply: usize) -> Option<i32> {
        if position.is_insufficient_material() {
            return Some(0);
        }
        if self.repetition_history.is_repetition(ply) {
            return Some(draw_value(self.nodes));
        }
        (position.halfmove_clock() >= 100
            && (!is_in_check(position, position.side_to_move()) || has_legal_move(position)))
        .then_some(0)
    }

    fn nmp_allowed_at(&self, ply: usize) -> bool {
        self.nmp_min_ply == 0 || ply >= self.nmp_min_ply
    }
}

pub fn is_mate_score(score: i32) -> bool {
    score.abs() >= MATE_SCORE - MAX_SEARCH_PLY as i32
}

#[inline]
fn is_decisive_score(score: i32) -> bool {
    score.abs() >= TABLEBASE_WIN_IN_MAX_PLY
}

pub fn score_to_uci_mate(score: i32) -> Option<i32> {
    if score >= MATE_SCORE - MAX_SEARCH_PLY as i32 {
        Some((MATE_SCORE - score + 1) / 2)
    } else if score <= -MATE_SCORE + MAX_SEARCH_PLY as i32 {
        Some(-((MATE_SCORE + score) / 2))
    } else {
        None
    }
}

pub fn clamp_centipawn_score(score: i32) -> i32 {
    score.clamp(-EVALUATION_LIMIT, EVALUATION_LIMIT)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use super::*;

    fn local_network() -> Option<&'static mf_nnue::Network> {
        static NETWORK: OnceLock<Option<mf_nnue::Network>> = OnceLock::new();
        NETWORK
            .get_or_init(|| {
                let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
                if !path.is_file() {
                    eprintln!("SKIPPED: NNUE search tests are missing {}", path.display());
                    return None;
                }
                Some(mf_nnue::Network::load(&path).unwrap_or_else(|error| {
                    panic!("test NNUE network {}: {error}", path.display())
                }))
            })
            .as_ref()
    }

    #[test]
    fn search_evaluator_reports_the_network_score_unrescaled() {
        let position = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .expect("test FEN should parse");
        let Some(network) = local_network() else {
            return;
        };
        let mut evaluator = SearchEvaluator::new(network, &position);

        assert_eq!(
            evaluator.evaluate(&position),
            network.evaluate_production(&position)
        );
    }

    #[test]
    fn search_evaluator_tracks_real_and_null_moves_and_unwinds_to_root() {
        let Some(network) = local_network() else {
            return;
        };
        let mut position = Position::startpos();
        let root = position.clone();
        let mut evaluator = SearchEvaluator::new(network, &position);
        let mv = mf_core::parse_uci_move(&position, "e2e4", false).expect("test move should parse");
        let undo = position.make_move(mv);

        evaluator
            .push_real(&position, mv, &undo)
            .expect("first child should fit");
        assert_eq!(evaluator.depth(), 1);
        assert_eq!(
            evaluator.evaluate(&position),
            network.evaluate_production(&position)
        );

        let null_undo = position.make_null_move();
        evaluator.push_null().expect("null child should fit");
        assert_eq!(evaluator.depth(), 2);
        assert_eq!(
            evaluator.evaluate(&position),
            network.evaluate_production(&position)
        );

        evaluator.pop().expect("null child should pop");
        position.unmake_null_move(null_undo);
        evaluator.pop().expect("real child should pop");
        position.unmake_move(mv, undo);

        assert_eq!(position, root);
        assert_eq!(evaluator.depth(), 0);
        assert_eq!(
            evaluator.evaluate(&position),
            network.evaluate_production(&position)
        );
    }

    #[test]
    fn search_evaluator_capacity_exactly_matches_search_ply_limit() {
        let position = Position::startpos();
        let Some(network) = local_network() else {
            return;
        };
        let mut evaluator = SearchEvaluator::new(network, &position);

        for _ in 0..MAX_SEARCH_PLY {
            evaluator
                .push_null()
                .expect("every searchable child ply should fit");
        }

        assert_eq!(evaluator.depth(), MAX_SEARCH_PLY);
    }

    #[test]
    fn search_context_position_stack_keeps_nnue_in_lockstep() {
        let Some(network) = local_network() else {
            return;
        };
        let mut position = Position::startpos();
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );
        let mv = mf_core::parse_uci_move(&position, "e2e4", false).expect("test move should parse");
        let continuation = continuation_key(&position, mv);
        let undo = position.make_move(mv);

        context.push_position(&position, 1, mv, continuation, &undo);
        assert_eq!(context.evaluator.depth(), 1);
        assert_eq!(
            context.static_eval(&position),
            network.evaluate_production(&position)
        );

        let null_undo = position.make_null_move();
        context.push_null_position(2);
        assert_eq!(context.evaluator.depth(), 2);
        context.pop_position(2);
        position.unmake_null_move(null_undo);
        context.pop_position(1);
        position.unmake_move(mv, undo);

        assert_eq!(context.evaluator.depth(), 0);
        assert_eq!(position, Position::startpos());
    }

    #[test]
    fn public_search_scores_the_root_with_the_network() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::from_fen("4k3/8/8/8/8/8/8/3QK3 w - - 0 1", false)
            .expect("test FEN should parse");
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let limits = SearchLimits {
            nodes: Some(0),
            ..SearchLimits::default()
        };

        let result = search(&position, &table, limits, SearchOptions::default(), network);

        assert_eq!(result.score, network.evaluate_production(&position));
    }

    #[test]
    #[ignore = "slow incremental NNUE verification at every search evaluation"]
    fn incremental_nnue_matches_full_rebuild_at_every_search_evaluation() {
        let Some(network) = local_network() else {
            return;
        };
        for (fen, chess960) in [
            (
                "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
                false,
            ),
            ("rk6/8/8/8/8/8/8/RK6 w Aa - 0 1", true),
        ] {
            let position =
                Position::from_fen(fen, chess960).expect("verification FEN should parse");
            let table = TranspositionTable::new(4).expect("test TT should allocate");
            let stop = AtomicBool::new(false);
            let counters = [AtomicU64::new(0)];
            let history = [position.repetition_key()];
            let shared_history = SharedHistory::new(1);
            let mut context = SearchContext::new(
                &table,
                SearchLimits {
                    depth: Some(4),
                    ..SearchLimits::default()
                },
                None,
                &stop,
                &position,
                &history,
                &shared_history,
                SearchOptions::default(),
                0,
                0,
                &counters,
                network,
            );
            let root_state = context.evaluator.current().clone();
            context.evaluator.enable_verification();

            let result = root_search(&position, 4, -INFINITY, INFINITY, &mut context);

            assert!(result.is_some());
            assert_eq!(context.evaluator.depth(), 0);
            assert_eq!(context.evaluator.current(), &root_state);
        }
    }

    #[test]
    fn worker_zero_preserves_the_original_aspiration_delta() {
        assert_eq!(aspiration_delta(0, 0), ASPIRATION_INITIAL_DELTA);
    }

    #[test]
    fn helpers_receive_distinct_bounded_aspiration_deltas() {
        let values: Vec<_> = (0..8).map(|worker| aspiration_delta(worker, 0)).collect();
        assert_eq!(values[0], ASPIRATION_INITIAL_DELTA);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn aspiration_window_widens_with_the_magnitude_of_the_previous_score() {
        // Near equality the window is tight, because that is where the score is most
        // predictable and most of the tree lives.
        assert_eq!(aspiration_delta(0, 0), 8);
        // `100^2 / ASPIRATION_SCORE_DIVISOR` truncates to zero, so a hundredth of a pawn
        // does not widen the window at all.
        assert_eq!(aspiration_delta(0, 100), 8);
        // A decided position swings more, so it starts wider instead of paying for a
        // chain of re-searches.
        assert!(aspiration_delta(0, 800) > aspiration_delta(0, 200));
        // Sign does not matter, only distance from equality.
        assert_eq!(aspiration_delta(0, -600), aspiration_delta(0, 600));
        // And the width is capped rather than exploding near mate scores.
        assert_eq!(aspiration_delta(0, MATE_SCORE), ASPIRATION_MAX_DELTA);
    }

    #[test]
    fn aggregate_nodes_sum_published_worker_counts() {
        let counters = [AtomicU64::new(10), AtomicU64::new(20)];
        assert_eq!(published_node_total(&counters), 30);
    }

    /// Every mode is bounded by the ply ceiling, `infinite` most of all: a user ran
    /// `go infinite` on a forced mate and watched the loop iterate past depth 3000.
    #[test]
    fn iterative_deepening_is_bounded_by_the_ply_ceiling() {
        assert_eq!(MAX_ITERATIVE_DEEPENING_DEPTH, MAX_SEARCH_PLY as u32);

        let infinite = SearchLimits {
            infinite: true,
            ..SearchLimits::default()
        };
        assert_eq!(iteration_ceiling(&infinite), MAX_ITERATIVE_DEEPENING_DEPTH);

        // An `infinite` request carrying a depth is still infinite: `search_limits`
        // drops the depth, and the ceiling must not resurrect it.
        assert_eq!(
            iteration_ceiling(&SearchLimits {
                depth: Some(4),
                ..infinite
            }),
            MAX_ITERATIVE_DEEPENING_DEPTH
        );

        let at = |depth| {
            iteration_ceiling(&SearchLimits {
                depth: Some(depth),
                ..SearchLimits::default()
            })
        };
        assert_eq!(at(0), 1, "depth 0 still owes the GUI one iteration");
        assert_eq!(at(12), 12, "a depth within the ceiling is honoured exactly");
        assert_eq!(at(MAX_ITERATIVE_DEEPENING_DEPTH), MAX_SEARCH_PLY as u32);
        assert_eq!(at(200), MAX_ITERATIVE_DEEPENING_DEPTH);
        assert_eq!(at(u32::MAX), MAX_ITERATIVE_DEEPENING_DEPTH);

        // No depth at all keeps the historical default.
        assert_eq!(
            iteration_ceiling(&SearchLimits::default()),
            DEFAULT_MAX_DEPTH
        );
    }

    #[test]
    fn one_worker_node_limit_is_exact() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).unwrap();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);

        let result = search_worker_with_history_callback_options(
            &position,
            &history,
            &table,
            SearchLimits {
                nodes: Some(1_500),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            WorkerParameters::new(0, 0, &counters, &shared_history, network),
            |_| {},
        );

        assert_eq!(result.nodes, 1_500);
        assert_eq!(counters[0].load(Ordering::Relaxed), 1_500);
    }

    #[test]
    fn one_worker_accepts_the_limit_node_across_publication_boundary() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).unwrap();
        let stop = AtomicBool::new(false);
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);

        for limit in [1_023, 1_024, 1_025] {
            let counters = [AtomicU64::new(0)];
            let mut context = SearchContext::new(
                &table,
                SearchLimits {
                    nodes: Some(limit),
                    ..SearchLimits::default()
                },
                None,
                &stop,
                &position,
                &history,
                &shared_history,
                SearchOptions::default(),
                0,
                0,
                &counters,
                network,
            );

            for node in 1..=limit {
                assert!(context.visit_node(1), "limit {limit} rejected node {node}");
            }
            assert!(
                !context.visit_node(1),
                "limit {limit} accepted a node past its budget"
            );
        }
    }

    #[test]
    fn helper_uses_aggregate_published_node_limit() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).unwrap();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(1_000), AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);

        let result = search_worker_with_history_callback_options(
            &position,
            &history,
            &table,
            SearchLimits {
                nodes: Some(1_500),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            WorkerParameters::new(1, 0, &counters, &shared_history, network),
            |_| {},
        );

        assert_eq!(result.nodes, NODE_PUBLISH_INTERVAL);
        assert_eq!(counters[1].load(Ordering::Relaxed), NODE_PUBLISH_INTERVAL);
    }

    #[test]
    fn helper_search_does_not_observe_clock_limits() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).unwrap();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0), AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);

        let result = search_worker_with_history_callback_options(
            &position,
            &history,
            &table,
            SearchLimits {
                depth: Some(2),
                soft_time: Some(Duration::ZERO),
                hard_time: Some(Duration::ZERO),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            WorkerParameters::new(1, 0, &counters, &shared_history, network),
            |_| {},
        );

        assert_eq!(result.depth, 2);
        assert!(result.nodes > 0);
        assert_eq!(result.elapsed, Duration::ZERO);
    }

    #[test]
    fn worker_zero_iteration_nodes_include_published_helpers() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).unwrap();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0), AtomicU64::new(37)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);
        let mut reported_nodes = None;

        let result = search_worker_with_history_callback_options(
            &position,
            &history,
            &table,
            SearchLimits {
                depth: Some(1),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            WorkerParameters::new(0, 0, &counters, &shared_history, network),
            |info| reported_nodes = Some(info.nodes),
        );

        assert_eq!(reported_nodes, Some(result.nodes + 37));
    }

    #[test]
    fn worker_generation_is_written_to_root_tt_entries() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).unwrap();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);

        search_worker_with_history_callback_options(
            &position,
            &history,
            &table,
            SearchLimits {
                depth: Some(2),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            WorkerParameters::new(0, 9, &counters, &shared_history, network),
            |_| {},
        );

        assert_eq!(table.probe(tt_key(&position, 2)).unwrap().age, 9);
    }

    #[test]
    fn tt_key_uses_coarse_rule_fifty_shards_independent_of_depth() {
        let clock_13 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 13 1", false).unwrap();
        let clock_14 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 14 1", false).unwrap();
        let clock_21 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 21 1", false).unwrap();
        let clock_22 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 22 1", false).unwrap();

        assert_eq!(tt_key(&clock_13, 99), clock_13.zobrist().main());
        assert_ne!(tt_key(&clock_14, 1), clock_14.zobrist().main());
        assert_eq!(tt_key(&clock_14, 1), tt_key(&clock_21, 99));
        assert_ne!(tt_key(&clock_21, 1), tt_key(&clock_22, 1));
    }

    #[test]
    fn tt_key_canonicalizes_legally_irrelevant_en_passant_targets() {
        let pinned = Position::from_fen("k3r3/8/8/3pP3/8/8/8/4K3 w - d6 20 1", false).unwrap();
        let no_target = Position::from_fen("k3r3/8/8/3pP3/8/8/8/4K3 w - - 20 1", false).unwrap();
        assert_eq!(tt_key(&pinned, 6), tt_key(&no_target, 6));

        let capturable = Position::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - d6 20 1", false).unwrap();
        let capturable_without_target =
            Position::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - - 20 1", false).unwrap();
        assert_ne!(
            tt_key(&capturable, 6),
            tt_key(&capturable_without_target, 6)
        );
    }

    #[test]
    fn draw_scores_are_fuzzed_by_node_parity() {
        assert_eq!(draw_value(0), -1);
        assert_eq!(draw_value(1), -1);
        assert_eq!(draw_value(2), 1);
        assert_eq!(draw_value(3), 1);
    }

    #[test]
    fn tt_values_adjust_mate_and_tablebase_distance_and_respect_rule_fifty() {
        let mate = MATE_SCORE - 20;
        let stored_mate = value_to_tt(mate, 7);
        assert_eq!(value_from_tt(stored_mate, 7, 0), mate);
        assert_eq!(value_from_tt(stored_mate, 12, 0), mate - 5);
        assert_eq!(
            value_from_tt(stored_mate, 7, 90),
            TABLEBASE_WIN_IN_MAX_PLY - 1
        );

        let tablebase = TABLEBASE_SCORE - 20;
        let stored_tablebase = value_to_tt(tablebase, 7);
        assert_eq!(value_from_tt(stored_tablebase, 7, 0), tablebase);
        assert_eq!(
            value_from_tt(stored_tablebase, 7, 90),
            TABLEBASE_WIN_IN_MAX_PLY - 1
        );

        let stored_loss = value_to_tt(-mate, 7);
        assert_eq!(
            value_from_tt(stored_loss, 7, 90),
            -TABLEBASE_WIN_IN_MAX_PLY + 1
        );
    }

    #[test]
    fn tt_cutoffs_are_blocked_by_high_rule_fifty_and_zeroing_moves() {
        let high_clock = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 96 1", false).unwrap();
        let table = TranspositionTable::new(1).unwrap();
        let entry = EntryData {
            best_move: None,
            score: 500,
            static_eval: 500,
            depth: 8,
            bound: Bound::Lower,
            age: 0,
            pv: false,
        };
        assert!(!tt_cutoff_is_safe(
            &high_clock,
            entry,
            500,
            8,
            100,
            1,
            &table
        ));

        let pawn_clock = Position::from_fen("4k3/8/8/8/8/8/P7/4K3 w - - 80 1", false).unwrap();
        let pawn_move = generate_legal_moves(&pawn_clock)
            .into_iter()
            .copied()
            .find(|mv| mv.from() == Square::new(8).unwrap())
            .unwrap();
        let pawn_entry = EntryData {
            best_move: Some(pawn_move),
            ..entry
        };
        assert!(!tt_cutoff_is_safe(
            &pawn_clock,
            pawn_entry,
            500,
            8,
            100,
            1,
            &table
        ));
        assert!(tt_cutoff_is_safe(
            &pawn_clock,
            pawn_entry,
            500,
            9,
            100,
            1,
            &table
        ));
    }

    #[test]
    fn deep_tt_cutoff_requires_one_ply_child_consistency() {
        let position = Position::startpos();
        let tt_move = generate_legal_moves(&position)
            .into_iter()
            .copied()
            .find(|mv| mv.from() == Square::new(6).unwrap() && mv.to() == Square::new(21).unwrap())
            .unwrap();
        let parent = EntryData {
            best_move: Some(tt_move),
            score: 500,
            static_eval: 0,
            depth: 7,
            bound: Bound::Lower,
            age: 0,
            pv: false,
        };
        let table = TranspositionTable::new(1).unwrap();
        assert!(tt_cutoff_is_safe(&position, parent, 500, 7, 100, 1, &table));

        let mut child = position.clone();
        child.make_move(tt_move);
        table.store(
            tt_key(&child, 6),
            EntryData {
                best_move: None,
                score: value_to_tt(500, 2) as i16,
                static_eval: 0,
                depth: 6,
                bound: Bound::Exact,
                age: 0,
                pv: false,
            },
        );
        assert!(!tt_cutoff_is_safe(
            &position, parent, 500, 7, 100, 1, &table
        ));
    }

    #[test]
    fn shuffling_detection_matches_the_four_ply_retrace_pattern() {
        let position = Position::from_fen("4k3/8/8/8/8/7N/8/4K1N1 w - - 10 1", false).unwrap();
        let candidate = Move::new(
            Square::new(6).unwrap(),
            Square::new(21).unwrap(),
            mf_core::MoveFlag::QUIET,
        );
        let mut current_moves = [None; MAX_SEARCH_PLY];
        current_moves[18] = Some(Move::new(
            Square::new(23).unwrap(),
            Square::new(6).unwrap(),
            mf_core::MoveFlag::QUIET,
        ));
        current_moves[16] = Some(Move::new(
            Square::new(0).unwrap(),
            Square::new(23).unwrap(),
            mf_core::MoveFlag::QUIET,
        ));

        assert!(is_shuffling(candidate, 20, &position, &current_moves, 6));
        assert!(!is_shuffling(candidate, 19, &position, &current_moves, 6));
    }

    #[test]
    fn razoring_margin_uses_the_required_quadratic_formula() {
        // Linear and bounded by RAZOR_MAX_DEPTH. The quadratic form reached 20835cp by
        // depth 8, which is not a margin, it is an off switch.
        assert_eq!(razoring_margin(1), 224 + 202);
        assert_eq!(razoring_margin(3), 224 + 202 * 3);
        assert_eq!(RAZOR_MAX_DEPTH, 3);
    }

    #[test]
    fn reverse_futility_margin_is_linear_in_depth_with_improving_and_tt_pv_adjustments() {
        // Linear, and small enough to actually fire. The previous quadratic form asked
        // for 392cp at depth 1, which is most of a minor piece.
        assert_eq!(reverse_futility_margin(1, false, false, false), 105);
        assert_eq!(reverse_futility_margin(6, false, false, false), 630);
        // A rising eval is worth a ply of margin, but not at an expected cut node.
        assert_eq!(reverse_futility_margin(6, true, false, false), 525);
        assert_eq!(reverse_futility_margin(6, true, true, false), 630);
        // A TT-PV node pays a surcharge.
        assert_eq!(reverse_futility_margin(6, false, false, true), 630 + 21);
    }

    #[test]
    fn null_move_reduction_scales_with_depth_and_eval_surplus() {
        assert_eq!(null_move_reduction(6, 100, 100), 7);
        assert_eq!(null_move_reduction(6, 700, 100), 10);
        assert_eq!(null_move_reduction(16, 100, 100), 10);
        assert_eq!(null_move_reduction(16, 10_000, 100), 13);
    }

    #[test]
    fn null_move_verification_starts_at_high_depth_and_does_not_nest() {
        assert!(!requires_null_move_verification(
            0,
            NMP_VERIFICATION_DEPTH - 1
        ));
        assert!(requires_null_move_verification(0, NMP_VERIFICATION_DEPTH));
        assert!(!requires_null_move_verification(4, NMP_VERIFICATION_DEPTH));
    }

    #[test]
    fn null_move_verification_blocks_nested_nulls_across_the_reduced_subtree() {
        assert_eq!(null_move_verification_min_ply(5, 8), 11);
        assert_eq!(null_move_verification_min_ply(5, 0), 6);
    }

    #[test]
    fn eval_pruning_requires_rule_fifty_headroom_for_the_remaining_depth() {
        let clock_98 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 98 1", false).unwrap();
        let clock_99 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 99 1", false).unwrap();

        assert!(eval_pruning_rule50_safe(&clock_98, 1));
        assert!(!eval_pruning_rule50_safe(&clock_98, 2));
        assert!(!eval_pruning_rule50_safe(&clock_99, 1));
    }

    #[test]
    fn null_move_is_a_hard_boundary_for_repetition_history() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::from_fen(
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 10 1",
            false,
        )
        .unwrap();
        let key = position.repetition_key();
        let history = [key; 6];
        let shared_history = SharedHistory::new(1);
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Some(Instant::now()),
            &stop,
            &position,
            &history,
            &shared_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );

        assert!(context.repetition_history.is_repetition(0));
        context.push_null_position(1);
        assert!(!context.repetition_history.is_repetition(20));
    }

    #[test]
    fn improving_uses_the_same_side_static_evaluation_from_two_plies_ago() {
        assert!(is_improving(25, Some(24)));
        assert!(!is_improving(24, Some(24)));
        assert!(!is_improving(23, Some(24)));
        assert!(is_improving(0, None));
    }

    #[test]
    fn lmp_movecount_table_is_indexed_by_depth_and_improving() {
        // Base 9, not 3: at depth 1 the non-improving row now keeps 5 moves rather than
        // pruning the position down to 2 before the ordering has demonstrated anything.
        assert_eq!(late_move_pruning_threshold(1, false), 5);
        assert_eq!(late_move_pruning_threshold(1, true), 10);
        assert_eq!(late_move_pruning_threshold(4, false), 12);
        assert_eq!(late_move_pruning_threshold(4, true), 25);
        assert!(late_move_pruning_threshold(6, false) > late_move_pruning_threshold(5, false));
    }

    #[test]
    fn lmr_base_table_and_adjustments_move_reduction_in_the_expected_direction() {
        let baseline = late_move_reduction(8, 8, true, false, false, 0);

        assert!(late_move_reduction(12, 8, true, false, false, 0) > baseline);
        assert!(late_move_reduction(8, 12, true, false, false, 0) > baseline);
        assert!(late_move_reduction(8, 8, false, false, false, 0) > baseline);
        assert!(late_move_reduction(8, 8, true, true, false, 0) > baseline);
        assert!(late_move_reduction(8, 8, true, false, true, 0) < baseline);
        assert!(late_move_reduction(8, 8, true, false, false, 4_000) < baseline);
        assert!(late_move_reduction(8, 8, true, false, false, -4_000) > baseline);
    }

    /// A capture's protection from LMR must scale with the MATERIAL it wins.
    ///
    /// This is the whole design. A capture is not uniformly safer to reduce than a
    /// quiet: taking a hanging queen is, taking a pawn on move 30 is not. So the
    /// discount is proportional to the victim rather than a flat base offset, and the
    /// ordering below is the property that makes it a material term and not a constant.
    #[test]
    fn capture_reduction_falls_as_the_victim_gets_more_valuable() {
        use mf_core::material_value;

        let pawn = capture_late_move_reduction(
            12,
            12,
            true,
            false,
            false,
            material_value(PieceKind::Pawn),
            0,
        );
        let knight = capture_late_move_reduction(
            12,
            12,
            true,
            false,
            false,
            material_value(PieceKind::Knight),
            0,
        );
        let rook = capture_late_move_reduction(
            12,
            12,
            true,
            false,
            false,
            material_value(PieceKind::Rook),
            0,
        );
        let queen = capture_late_move_reduction(
            12,
            12,
            true,
            false,
            false,
            material_value(PieceKind::Queen),
            0,
        );
        let quiet = late_move_reduction(12, 12, true, false, false, 0);

        assert!(queen < rook && rook < knight && knight < pawn);
        // Even the cheapest victim buys some protection relative to a no-history quiet.
        assert!(pawn < quiet);
        // Proportionality, stated as a ratio so it survives any retune of the weight:
        // a queen must buy back several times what a pawn does. A flat discount -- the
        // design this replaced -- scores 1.0 here and is what the +51.6% depth-12
        // measurement rejected.
        assert!(
            (quiet - queen) > 5 * (quiet - pawn),
            "queen protection {} must be several times pawn protection {}",
            quiet - queen,
            quiet - pawn
        );
        // And a pawn must NOT buy a whole ply: a late pawn grab is exactly as
        // speculative as a late quiet move.
        assert!(
            quiet - pawn < 1_024,
            "a pawn must not buy a whole ply back: quiet={quiet}, pawn={pawn}"
        );
    }

    /// The capture reduction must move with the same signals the quiet one does, and it
    /// must read CAPTURE history rather than the quiet `statScore`.
    #[test]
    fn capture_reduction_scales_with_depth_move_count_and_capture_history() {
        let pawn = mf_core::material_value(PieceKind::Pawn);
        let baseline = capture_late_move_reduction(12, 12, true, false, false, pawn, 0);

        assert!(capture_late_move_reduction(20, 12, true, false, false, pawn, 0) > baseline);
        assert!(capture_late_move_reduction(12, 24, true, false, false, pawn, 0) > baseline);
        assert!(capture_late_move_reduction(12, 12, false, false, false, pawn, 0) > baseline);
        assert!(capture_late_move_reduction(12, 12, true, true, false, pawn, 0) > baseline);
        assert!(capture_late_move_reduction(12, 12, true, false, true, pawn, 0) < baseline);
        // Capture history saturates at CAPTURE_MAX (10_692), so these are in range.
        assert!(capture_late_move_reduction(12, 12, true, false, false, pawn, 8_000) < baseline);
        assert!(capture_late_move_reduction(12, 12, true, false, false, pawn, -8_000) > baseline);
    }

    /// A capture reduction is the QUIET formula fed a capture `statScore`.
    ///
    /// Pinned as an identity rather than described in prose, because the thing that
    /// makes this feature single-variable is that it introduces no second reduction
    /// shape: same table, same base, same improving/cut/ttPv adjustments, same
    /// `439/4096` history divisor. Only the statistic changes.
    #[test]
    fn the_capture_reduction_is_the_quiet_formula_with_a_capture_stat_score() {
        for (victim, history) in [(100, 0), (900, 4_000), (500, -3_000), (320, 10_692)] {
            let stat_score = capture_stat_score(victim, history);
            assert_eq!(
                capture_late_move_reduction(14, 9, false, true, false, victim, history),
                late_move_reduction(14, 9, false, true, false, stat_score)
            );
        }
    }

    /// Three classes of capture are never reduced, whatever the table says.
    #[test]
    fn the_tt_move_checks_and_queen_promotions_are_exempt_from_capture_reduction() {
        let from = Square::new(8).unwrap();
        let to = Square::new(16).unwrap();
        let capture = Move::new(from, to, mf_core::MoveFlag::CAPTURE);
        let other = Move::new(from, Square::new(17).unwrap(), mf_core::MoveFlag::CAPTURE);

        assert!(capture_reduction_allowed(capture, Some(other), false));
        // The TT move is the engine's own best guess; reducing it reduces the move most
        // likely to be the answer.
        assert!(!capture_reduction_allowed(capture, Some(capture), false));
        // A checking capture forces a reply, so the reduced scout searches a tree that
        // bears no resemblance to the real one.
        assert!(!capture_reduction_allowed(capture, Some(other), true));

        let queen_promotion = Move::new(
            Square::new(48).unwrap(),
            Square::new(56).unwrap(),
            mf_core::MoveFlag::QUEEN_PROMOTION,
        );
        assert!(!capture_reduction_allowed(queen_promotion, None, false));
        let knight_promotion = Move::new(
            Square::new(48).unwrap(),
            Square::new(56).unwrap(),
            mf_core::MoveFlag::KNIGHT_PROMOTION,
        );
        assert!(capture_reduction_allowed(knight_promotion, None, false));
    }

    #[test]
    fn frontier_futility_margin_grows_with_effective_depth() {
        assert_eq!(frontier_futility_margin(0), 124);
        assert_eq!(frontier_futility_margin(1), 233);
        assert_eq!(frontier_futility_margin(6), 778);
        // The window now reaches a reduced depth of 6, matching where the margin was
        // calibrated, instead of stopping at 3.
        assert_eq!(FUTILITY_MAX_EFFECTIVE_DEPTH, 6);
    }

    #[test]
    fn see_pruning_uses_separate_main_search_and_qsearch_thresholds() {
        assert_eq!(quiet_see_threshold(0), 0);
        assert_eq!(quiet_see_threshold(3), -26 * 9);
        assert_eq!(capture_see_threshold(3), -99 * 3);
        // Qsearch admits equal trades and rejects losing ones. The old -74 admitted
        // captures that lose most of a pawn; demanding a strictly winning +1 instead
        // measured 1.5x MORE nodes, because an equal trade is how a recapture resolves.
        assert_eq!(qsearch_see_threshold(true), 0);
        // Off must admit every capture, not merely re-impose the shipped threshold.
        assert_eq!(qsearch_see_threshold(false), i32::MIN);
    }

    #[test]
    fn iir_reduces_every_deep_node_that_has_no_tt_move_to_order_by() {
        let tt_move = generate_legal_moves(&Position::startpos())[0];

        // Too shallow to be worth re-ordering.
        assert_eq!(internal_iterative_reduction(3, false, None, None), 0);
        // A move to order by, an evasion, or an exclusion search: nothing to gain.
        assert_eq!(
            internal_iterative_reduction(6, false, Some(tt_move), None),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, false, None, Some(tt_move)),
            0
        );
        assert_eq!(internal_iterative_reduction(6, true, None, None), 0);
        // Node type no longer gates the reduction: an unordered PV or all-node is just
        // as unordered as an unordered cut node.
        assert_eq!(internal_iterative_reduction(4, false, None, None), 1);
        assert_eq!(internal_iterative_reduction(6, false, None, None), 1);
    }

    #[test]
    fn tt_depth_encoding_round_trips_and_orders_the_two_qsearch_domains_below_interior_depths() {
        // The domain ordering itself is asserted at compile time next to the constants.
        // What needs a test is that the u8 encoding PRESERVES that ordering.
        for depth in [QSEARCH_CAPTURES_TT_DEPTH, QSEARCH_CHECKS_TT_DEPTH, 0, 1, 64] {
            assert_eq!(tt_entry_depth(tt_stored_depth(depth)), depth);
        }
        assert!(
            tt_stored_depth(QSEARCH_CAPTURES_TT_DEPTH) < tt_stored_depth(QSEARCH_CHECKS_TT_DEPTH)
        );
        assert!(tt_stored_depth(QSEARCH_CHECKS_TT_DEPTH) < tt_stored_depth(0));
        // The bias must not push a legal search depth past the u8 the entry carries.
        assert_eq!(tt_entry_depth(tt_stored_depth(253)), 253);
    }

    #[test]
    fn qsearch_delta_pruning_margin_admits_only_captures_that_can_reach_alpha() {
        let position = Position::startpos();
        let mv = generate_legal_moves(&position)[0];
        // A quiet move wins no material, so the margin alone decides.
        assert_eq!(captured_material(&position, mv), 0);

        let capture = Position::from_fen(
            "rnbqkbnr/ppp1pppp/8/3p4/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2",
            false,
        )
        .expect("test FEN should parse");
        let exd5 = generate_legal_moves(&capture)
            .iter()
            .copied()
            .find(|mv| mv.flag().is_capture())
            .expect("the position offers a pawn capture");
        assert_eq!(
            captured_material(&capture, exd5),
            mf_core::material_value(PieceKind::Pawn)
        );
        assert_eq!(QSEARCH_DELTA_MARGIN, 196);
    }

    #[test]
    fn time_scaling_spends_more_while_the_root_move_moves_and_saves_once_it_settles() {
        // A root move that just changed is worth more than its nominal share.
        assert_eq!(time_scale_percent(0, 0), 110);
        // Every iteration that agrees with the last one banks a little of the budget.
        assert_eq!(time_scale_percent(1, 0), 105);
        assert_eq!(time_scale_percent(6, 0), 80);
        // Past the cap there is nothing further to save.
        assert_eq!(time_scale_percent(50, 0), 80);
        // A falling score buys time; a rising one buys nothing, since finding out the
        // position is better than believed does not change which move to play.
        assert_eq!(time_scale_percent(6, -100), 80 + 40);
        assert_eq!(time_scale_percent(6, 100), 80);
        assert!(time_scale_percent(0, -100_000) <= TIME_SCALE_MAX_PERCENT);
    }

    #[test]
    fn the_effort_factor_extends_time_while_the_rivals_are_still_expensive() {
        // A best move owning half the tree or less leaves the other root moves costing
        // the other half: they are still being taken seriously, so keep thinking.
        assert_eq!(time_effort_percent(500, 1000), 110);
        assert_eq!(time_effort_percent(100, 1000), 110);
        assert_eq!(time_effort_percent(0, 1000), 110);
        // Owning nearly the whole tree means every rival is dismissed on a TT bound.
        assert_eq!(time_effort_percent(900, 1000), 90);
        assert_eq!(time_effort_percent(1000, 1000), 90);
        // Between the anchors the factor interpolates rather than stepping.
        assert_eq!(time_effort_percent(700, 1000), 100);
        assert_eq!(time_effort_percent(600, 1000), 105);
        assert_eq!(time_effort_percent(800, 1000), 95);
        // Monotone non-increasing across the whole domain, which is the property the
        // ramp exists to have -- more effort on the best move never buys MORE time.
        let mut previous = 0;
        for permille in 0..=1000u64 {
            let percent = time_effort_percent(permille, 1000);
            assert!(
                previous == 0 || percent <= previous,
                "effort factor rose at {permille} per mille"
            );
            previous = percent;
        }
        // A search that has not finished a root subtree has measured nothing.
        assert_eq!(time_effort_percent(0, 0), 100);
        // Absurd inputs cannot divide by zero or overflow the ramp.
        assert_eq!(time_effort_percent(u64::MAX, 1), 90);
    }

    #[test]
    fn the_effort_factor_composes_with_stability_without_raising_the_ceiling() {
        // Neutral effort leaves the stability factor exactly as it was.
        assert_eq!(scaled_time_percent(110, 100), 110);
        // A dominant best move shaves the stability budget; a contested one extends it.
        assert_eq!(scaled_time_percent(80, 90), 72);
        assert_eq!(scaled_time_percent(110, 110), 121);
        // The composed factor obeys the SAME ceiling the stability factor alone does,
        // so adding a second term cannot stretch the soft limit further than before.
        assert_eq!(
            scaled_time_percent(TIME_SCALE_MAX_PERCENT, TIME_EFFORT_LOW_PERCENT),
            TIME_SCALE_MAX_PERCENT
        );
    }

    #[test]
    fn the_effort_factor_is_neutral_before_anything_is_measured_and_when_disabled() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            // Explicitly ENABLED: the technique ships off after measuring -17.39 Elo,
            // and this test is about the factor's arithmetic, not its default.
            SearchOptions {
                use_time_effort: true,
                ..SearchOptions::default()
            },
            0,
            0,
            &counters,
            network,
        );

        let moves = generate_legal_moves(&position);
        let (best, rival) = (moves[0], moves[1]);

        // No root move searched yet, and no best move yet: nothing to claim.
        assert_eq!(context.best_move_effort_percent(Some(best)), 100);
        assert_eq!(context.best_move_effort_percent(None), 100);

        context.begin_root_effort();
        context.record_root_effort(best, 900);
        context.record_root_effort(rival, 100);
        assert_eq!(context.best_move_effort_percent(Some(best)), 90);
        assert_eq!(context.best_move_effort_percent(Some(rival)), 110);

        // Repeated credits to the same move accumulate rather than overwrite: the root
        // loop pays for a scout and its verification re-search separately.
        context.record_root_effort(rival, 800);
        assert_eq!(context.best_move_effort_percent(Some(best)), 110);

        // A new root search -- an aspiration re-search, say -- starts the accounting
        // over, because the tree the failed window built was thrown away.
        context.begin_root_effort();
        assert_eq!(context.best_move_effort_percent(Some(best)), 100);

        // The toggle silences the term completely.
        context.options.use_time_effort = false;
        context.begin_root_effort();
        context.record_root_effort(best, 1000);
        assert_eq!(context.best_move_effort_percent(Some(best)), 100);
    }

    #[test]
    fn the_scaled_soft_limit_can_never_exceed_the_hard_limit() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let mut context = SearchContext::new(
            &table,
            SearchLimits {
                soft_time: Some(Duration::from_millis(100)),
                hard_time: Some(Duration::from_millis(150)),
                ..SearchLimits::default()
            },
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );

        context.set_time_scale(100);
        assert_eq!(context.scaled_soft_time(), Some(Duration::from_millis(100)));
        context.set_time_scale(80);
        assert_eq!(context.scaled_soft_time(), Some(Duration::from_millis(80)));
        // 180% of 100ms is 180ms, but the hard limit is the promise not to forfeit.
        context.set_time_scale(TIME_SCALE_MAX_PERCENT);
        assert_eq!(context.scaled_soft_time(), Some(Duration::from_millis(150)));
    }

    #[test]
    fn probcut_margin_and_depth_follow_the_reference_formulas() {
        assert_eq!(probcut_beta(100, false), 341);
        assert_eq!(probcut_beta(100, true), 277);
        assert_eq!(probcut_depth(8, false), 5);
        assert_eq!(probcut_depth(8, true), 3);
    }

    #[test]
    fn probcut_does_not_adjust_or_return_decisive_scores() {
        assert_eq!(probcut_cutoff_value(500, 100, 341), Some(259));
        assert_eq!(probcut_cutoff_value(MATE_SCORE - 5, 100, 341), None);
    }

    #[test]
    fn singular_extensions_support_single_double_negative_and_check_extensions() {
        let singular_beta = singular_beta(200, 8, false, false);
        assert_eq!(singular_beta, 193);
        assert_eq!(
            singular_extension(192, singular_beta, false, true, false, 200, 300),
            1
        );
        assert_eq!(
            singular_extension(150, singular_beta, true, true, false, 200, 300),
            2
        );
        assert_eq!(
            singular_extension(210, singular_beta, false, true, false, 400, 300),
            -3
        );
        assert_eq!(
            singular_extension(210, singular_beta, false, true, true, 200, 300),
            -2
        );
        assert_eq!(singular_multicut_value(350, 300, 320), Some(350));
        assert_eq!(singular_multicut_value(250, 300, 200), None);
        assert_eq!(singular_multicut_value(MATE_SCORE - 5, 300, 320), None);
        assert_eq!(check_extension(true, 0), 1);
        assert_eq!(check_extension(false, 0), 0);
    }

    #[test]
    fn singular_verification_does_not_probe_or_overwrite_the_parent_tt_entry() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let depth = 6;
        let key = tt_key(&position, depth);
        let tt_move = generate_legal_moves(&position)[0];
        let original = EntryData {
            best_move: Some(tt_move),
            score: 100,
            // Any value works here; the test only checks the entry survives untouched.
            static_eval: 42,
            depth: depth as u8,
            bound: Bound::Lower,
            age: 0,
            pv: false,
        };
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        table.store(key, original);
        assert_eq!(tt_entry_for_node(&table, key, false), Some(original));
        assert_eq!(tt_entry_for_node(&table, key, true), None);
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Some(Instant::now()),
            &stop,
            &position,
            &history,
            &shared_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );
        let mut searched = position.clone();
        let mut pv = Vec::new();

        let result = pvs(
            &mut searched,
            depth / 2,
            49,
            50,
            1,
            false,
            true,
            true,
            true,
            Some(tt_move),
            &mut context,
            &mut pv,
        );

        assert!(result.is_some());
        assert_eq!(table.probe(key), Some(original));
    }

    #[test]
    fn excluded_move_search_without_alternatives_returns_alpha_not_draw() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::from_fen("8/8/8/8/8/k7/8/KQ6 b - - 0 1", false).unwrap();
        let legal_moves = generate_legal_moves(&position);
        assert_eq!(legal_moves.len(), 1);
        let excluded_move = legal_moves[0];
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [AtomicU64::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new(1);
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Some(Instant::now()),
            &stop,
            &position,
            &history,
            &shared_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );
        let mut searched = position.clone();
        let mut pv = Vec::new();

        let result = pvs(
            &mut searched,
            2,
            -100,
            -99,
            1,
            false,
            true,
            true,
            true,
            Some(excluded_move),
            &mut context,
            &mut pv,
        );

        assert_eq!(result, Some(-100));
    }

    #[test]
    fn losing_mate_score_disables_lmp_and_futility_pruning() {
        assert!(!shallow_pruning_allowed(-MATE_SCORE + 5));
        assert!(shallow_pruning_allowed(-500));
        assert!(shallow_pruning_allowed(MATE_SCORE - 5));
    }

    #[test]
    fn quiet_check_detection_matches_make_move_for_direct_discovered_and_castling_checks() {
        let mut positions = vec![
            Position::startpos(),
            Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", false).unwrap(),
            Position::from_fen("4k3/8/8/8/8/8/4B3/4R1K1 w - - 0 1", false).unwrap(),
        ];
        let mut random_walk = Position::startpos();
        for sample in 0..64 {
            positions.push(random_walk.clone());
            let moves = generate_legal_moves(&random_walk);
            if moves.is_empty() {
                random_walk = Position::startpos();
            } else {
                random_walk.make_move(moves[(sample * 17 + 3) % moves.len()]);
            }
        }

        for position in positions {
            for mv in generate_legal_moves(&position)
                .iter()
                .copied()
                .filter(|mv| !mv.flag().is_capture())
            {
                let mut after = position.clone();
                after.make_move(mv);
                assert_eq!(
                    move_gives_check(&position, mv),
                    is_in_check(&after, after.side_to_move()),
                    "{position:?} {mv:?}"
                );
            }
        }
    }

    #[test]
    fn check_detection_handles_en_passant_discovered_checks() {
        let position = Position::from_fen("8/8/8/R2pP2k/8/8/8/K7 w - d6 0 1", false).unwrap();
        let mv = generate_legal_moves(&position)
            .into_iter()
            .copied()
            .find(|mv| mv.flag().is_en_passant())
            .expect("test position should contain an en-passant capture");
        let mut after = position.clone();
        after.make_move(mv);

        assert!(is_in_check(&after, after.side_to_move()));
        assert!(move_gives_check(&position, mv));
    }
}
