use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use mf_core::{
    Bitboard, CastlingSide, Color, Move, MoveList, PieceKind, Position, Square, Undo,
    bishop_attacks, generate_legal_moves, has_legal_move, has_legal_move_in_place, is_in_check,
    is_legal, king_attacks, knight_attacks, pawn_attacks, rook_attacks, static_exchange_evaluation,
};
use mf_nnue::{ACCUMULATOR_STACK_CAPACITY, AccumulatorStack, AccumulatorStackError, Network};
use mf_tb::{Tablebases, Wdl};

use crate::history::{
    CONTINUATION_PLIES, CONTINUATION_WEIGHTS, CORRECTION_CONTINUATION_PLIES,
    CORRECTION_CONTINUATION_WEIGHT, CORRECTION_MAJOR, CORRECTION_MATERIAL, CORRECTION_MAX,
    CORRECTION_MINOR, CORRECTION_PAWN, CORRECTION_SCALE, CORRECTION_SOURCES, CORRECTION_WEIGHTS,
    ContinuationKey, KillerTable, LowPlyHistory, SharedHistory, TtMoveHistory, captured_kind,
};
use crate::move_ordering::{MovePicker, OrderingContext, captured_material};
use crate::repetition::RepetitionHistory;
use crate::{Bound, EntryData, TranspositionTable};

pub const MATE_SCORE: i32 = 30_000;
pub const MAX_SEARCH_PLY: usize = 128;
const _: () = assert!(MAX_SEARCH_PLY == ACCUMULATOR_STACK_CAPACITY);

/// The six correction-history entries visible when an ordinary PVS node begins.
#[cfg(feature = "corrhist-regression")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorrectionFeatures {
    pub pawn: i16,
    pub minor: i16,
    pub major: i16,
    pub material: i16,
    pub continuation_2: i16,
    pub continuation_4: i16,
}

/// One completed exact PVS observation for offline correction-history regression.
#[cfg(feature = "corrhist-regression")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorrectionSample {
    pub features: CorrectionFeatures,
    pub raw_static_eval: i32,
    pub search_value: i32,
    pub depth: u32,
    pub ply: usize,
    pub position_key: u64,
}

/// Fixed-capacity move buffer whose storage is deliberately left uninitialized.
///
/// One of these lives in every `pvs` and `quiescence` frame, so construction must be
/// free: zeroing the array (as `MoveList::new` does) is a per-node memset, and a heap
/// `Vec` is per-node allocator traffic once anything is pushed. Only the first `len`
/// slots are ever written, and `as_slice` exposes exactly that prefix, so the
/// uninitialized tail is never read.
struct MoveBuffer<const N: usize> {
    moves: [core::mem::MaybeUninit<Move>; N],
    len: usize,
}

impl<const N: usize> MoveBuffer<N> {
    const fn new() -> Self {
        Self {
            moves: [core::mem::MaybeUninit::uninit(); N],
            len: 0,
        }
    }

    /// Appends a move. Panics if the buffer is already at capacity.
    #[inline]
    fn push(&mut self, mv: Move) {
        assert!(self.len < N, "move buffer overflow");
        self.moves[self.len] = core::mem::MaybeUninit::new(mv);
        self.len += 1;
    }

    #[inline]
    fn as_slice(&self) -> &[Move] {
        // SAFETY: `push` and `PvLine::load` initialize every slot below `len` before
        // advancing it, and nothing ever lowers a slot back to uninitialized.
        unsafe { core::slice::from_raw_parts(self.moves.as_ptr().cast::<Move>(), self.len) }
    }
}

/// A principal variation collected on the stack.
///
/// `Move` is a `u16`, making the whole line 258 bytes -- small enough to live in every
/// frame of a `MAX_SEARCH_PLY`-deep recursion on the workers' 8 MiB stacks.
pub(crate) struct PvLine(MoveBuffer<MAX_SEARCH_PLY>);

impl PvLine {
    pub(crate) const fn new() -> Self {
        Self(MoveBuffer::new())
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        self.0.len = 0;
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[Move] {
        self.0.as_slice()
    }

    /// Replaces the line with `mv` followed by `child`, truncating at capacity.
    ///
    /// Truncation is unreachable in practice: a child line rooted one ply deeper is
    /// always shorter than the ceiling, so `mv` plus the child fits. The `min` merely
    /// keeps the copy in bounds without a panic path.
    #[inline]
    fn load(&mut self, mv: Move, child: &PvLine) {
        self.0.moves[0] = core::mem::MaybeUninit::new(mv);
        let tail = child.0.len.min(MAX_SEARCH_PLY - 1);
        self.0.moves[1..=tail].copy_from_slice(&child.0.moves[..tail]);
        self.0.len = tail + 1;
    }
}
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
/// Extra depth credited to a TT entry written from a Syzygy WDL verdict, which is exact
/// at any depth; the reference engine likewise stores probe results a few plies deep so
/// they outlive deeper re-visits instead of falling back to a file probe.
const SYZYGY_TT_DEPTH_BONUS: i32 = 6;
const NMP_MIN_DEPTH: i32 = 3;
const NMP_VERIFICATION_DEPTH: i32 = 6;
const RFP_MAX_DEPTH: i32 = 6;
const RAZOR_MAX_DEPTH: i32 = 3;
const RAZOR_BASE_MARGIN: i32 = 224;
const RAZOR_MARGIN_PER_DEPTH: i32 = 202;
/// Centipawns per ply of reverse-futility margin.
///
/// SPSA-tuned (M5-F5, 105 -> 95): the largest relative move of the eight parameters in
/// that session, and monotone across all four quarters of it.
const RFP_MARGIN_PER_DEPTH: i32 = 95;
/// Extra margin demanded at a node the TT marked as PV, which is likelier to be worth
/// searching properly than to be a genuine cutoff.
const RFP_TT_PV_MARGIN: i32 = 22;
const LMP_MAX_DEPTH: i32 = 8;
/// Constant term in the late-move-pruning move-count table.
///
/// At 3 the non-improving row began at 2 moves at depth 1, which prunes the board flat
/// before the move ordering has shown anything. The reference uses 9.
const LMP_BASE: usize = 9;
const FUTILITY_MAX_EFFECTIVE_DEPTH: i32 = 6;
const FUTILITY_BASE_MARGIN: i32 = 125;
const FUTILITY_MARGIN_PER_DEPTH: i32 = 106;
/// Quiets may be pruned on SEE up to this reduced depth, matching the futility window.
const QUIET_SEE_MAX_EFFECTIVE_DEPTH: i32 = 7;
const QUIET_SEE_MARGIN_PER_DEPTH: i32 = 26;
const CAPTURE_SEE_MAX_DEPTH: i32 = 6;
const CAPTURE_SEE_MARGIN_PER_DEPTH: i32 = 99;
const SINGULAR_MIN_DEPTH: i32 = 6;
/// ttMove-history gravity bonus when a non-PV node's best move was the TT move.
const TT_MOVE_HISTORY_HIT_BONUS: i32 = 918;
/// ttMove-history gravity malus when a non-PV node's best move was NOT the TT move.
const TT_MOVE_HISTORY_MISS_MALUS: i32 = -747;
/// Base of the ttMove-history malus applied on a singular multicut early return.
const TT_MOVE_HISTORY_MULTICUT_BASE: i32 = -421;
/// Per-depth slope of the multicut malus: a deep multicut is stronger evidence.
const TT_MOVE_HISTORY_MULTICUT_PER_DEPTH: i32 = 110;
/// Numerator of the ttMove-history adjustment to the singular double margin.
const TT_MOVE_HISTORY_MARGIN_NUMERATOR: i32 = 1_175;
/// Divisor of the ttMove-history adjustment to the singular double margin.
const TT_MOVE_HISTORY_MARGIN_DIVISOR: i32 = 114_178;
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

trait TablebaseProbe: Sync {
    fn max_pieces(&self) -> usize;
    fn probe_wdl(&self, position: &Position) -> Option<Wdl>;
    fn preserving_root_moves(&self, position: &Position) -> Option<Vec<Move>>;
}

impl TablebaseProbe for Tablebases {
    fn max_pieces(&self) -> usize {
        Tablebases::max_pieces(self)
    }

    fn probe_wdl(&self, position: &Position) -> Option<Wdl> {
        Tablebases::probe_wdl(self, position)
    }

    fn preserving_root_moves(&self, position: &Position) -> Option<Vec<Move>> {
        Tablebases::probe_root(self, position)
            .map(|probe| probe.preserving_moves().map(|entry| entry.mv).collect())
    }
}

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

/// Declares the tunable search hyperparameters once, and derives everything from it.
///
/// The struct field, its `Default`, and the UCI spin advertised for it all come from the
/// SAME line here. That is the point: a tuner discovers a parameter's range from the
/// handshake and then writes it back with `setoption`, so a default that disagreed with
/// the shipped constant would silently change the engine the moment a GUI echoed the
/// value it was just told. Deriving all three from one declaration makes that class of
/// drift unrepresentable rather than merely tested for.
macro_rules! search_parameters {
    ($(
        $(#[$meta:meta])*
        $field:ident : $name:literal = $default:expr, $range:expr;
    )*) => {
        /// Tunable search hyperparameters, exposed over UCI as spin options.
        ///
        /// Every field defaults to the constant the search shipped with, so
        /// `SearchParameters::default()` reproduces the pinned bench signature exactly.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct SearchParameters {
            $($(#[$meta])* pub $field: i32,)*
        }

        impl Default for SearchParameters {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        /// Every tunable parameter, in handshake order.
        pub const SEARCH_PARAMETERS: &[SearchParameterSpec] = &[
            $(SearchParameterSpec {
                name: $name,
                default: $default,
                min: *$range.start(),
                max: *$range.end(),
                get: |parameters| parameters.$field,
                set: |parameters, value| parameters.$field = value,
            },)*
        ];
    };
}

/// One tunable parameter's UCI identity: what it is called, and what it may be set to.
///
/// The bounds are part of the contract rather than documentation. A tuner samples inside
/// them without knowing what any parameter means, so a range that admits a divisor of
/// zero is a crash the tuner is entitled to find.
pub struct SearchParameterSpec {
    pub name: &'static str,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    get: fn(&SearchParameters) -> i32,
    set: fn(&mut SearchParameters, i32),
}

impl SearchParameterSpec {
    pub fn value(&self, parameters: &SearchParameters) -> i32 {
        (self.get)(parameters)
    }

    /// Writes `value`, clamped into the advertised range.
    ///
    /// Clamping rather than rejecting: a tuner that steps a parameter past its bound
    /// should get the bound, not a silently ignored `setoption` that leaves it tuning a
    /// value the engine never adopted.
    pub fn set(&self, parameters: &mut SearchParameters, value: i32) {
        (self.set)(parameters, value.clamp(self.min, self.max));
    }
}

/// Looks a parameter up by its UCI name, case-insensitively.
pub fn search_parameter(name: &str) -> Option<&'static SearchParameterSpec> {
    SEARCH_PARAMETERS
        .iter()
        .find(|spec| name.eq_ignore_ascii_case(spec.name))
}

search_parameters! {
    /// Centipawns of reverse-futility margin per ply. Source: `RFP_MARGIN_PER_DEPTH`.
    rfp_margin_per_depth: "RfpMarginPerDepth" = RFP_MARGIN_PER_DEPTH, 20 ..= 300;
    /// Surcharge demanded at a TT-PV node. Source: `RFP_TT_PV_MARGIN`.
    rfp_tt_pv_margin: "RfpTtPvMargin" = RFP_TT_PV_MARGIN, 0 ..= 150;
    /// Divisor turning the corrhist blend's magnitude into extra reverse-futility
    /// margin. Reference: `futilityMargin ... + |correctionValue| / 198435`.
    rfp_corrplexity_divisor: "RfpCorrplexityDivisor" = 198_435, 4_096 ..= 1_000_000;
    /// Constant term of the razoring margin. Source: `RAZOR_BASE_MARGIN`.
    razor_base_margin: "RazorBaseMargin" = RAZOR_BASE_MARGIN, 50 ..= 600;
    /// Razoring margin per ply. Source: `RAZOR_MARGIN_PER_DEPTH`.
    razor_margin_per_depth: "RazorMarginPerDepth" = RAZOR_MARGIN_PER_DEPTH, 50 ..= 600;
    /// Constant term of the frontier futility margin. Source: `FUTILITY_BASE_MARGIN`.
    futility_base_margin: "FutilityBaseMargin" = FUTILITY_BASE_MARGIN, 20 ..= 400;
    /// Futility margin per ply of reduced depth. Source: `FUTILITY_MARGIN_PER_DEPTH`.
    futility_margin_per_depth: "FutilityMarginPerDepth" = FUTILITY_MARGIN_PER_DEPTH, 20 ..= 400;
    /// Constant term of the late-move-pruning move-count table. Source: `LMP_BASE`.
    lmp_base: "LmpBase" = LMP_BASE as i32, 1 ..= 40;
    /// Numerator of the LMR log-table coefficient, over 128. Source: `lmr_table`.
    ///
    /// SPSA-tuned (M5-F5, 2_872 -> 2_754): the only parameter of the eight whose drift
    /// exceeded its own final perturbation width, and monotone across every quarter of
    /// the session — the tuner wants slightly SHALLOWER late-move reductions than the
    /// hand-calibrated value.
    lmr_coefficient: "LmrCoefficient" = 2_754, 1_000 ..= 6_000;
    /// Constant term added to every LMR reduction, in 1024ths of a ply.
    lmr_base: "LmrBase" = 996, -1_024 ..= 3_072;
    /// Extra reduction at a non-improving node, as a fraction of the table scale over 512.
    lmr_non_improving_numerator: "LmrNonImprovingNumerator" = 197, 0 ..= 1_024;
    /// Extra reduction at an expected cut node, in 1024ths of a ply.
    lmr_cut_node_bonus: "LmrCutNodeBonus" = 1_024, 0 ..= 3_072;
    /// Reduction refunded at a TT-PV node, in 1024ths of a ply.
    lmr_tt_pv_reduction: "LmrTtPvReduction" = 1_028, 0 ..= 3_072;
    /// Numerator of the LMR history term, over 4096.
    lmr_history_numerator: "LmrHistoryNumerator" = 459, 50 ..= 1_500;
    /// Divisor turning the corrhist blend's magnitude into an LMR reduction refund, in
    /// 1024ths of a ply. Reference: `r -= ... + |correctionValue| / 26310`.
    lmr_corrplexity_divisor: "LmrCorrplexityDivisor" = 26_310, 4_096 ..= 1_000_000;
    /// Weight on captured material in a capture's LMR `statScore`, over 128.
    /// Source: `CAPTURE_STAT_MATERIAL_WEIGHT`.
    capture_stat_material_weight: "CaptureStatMaterialWeight" = CAPTURE_STAT_MATERIAL_WEIGHT, 0 ..= 3_000;
    /// Slope of the null-move eval precondition, in centipawns per ply.
    nmp_margin_per_depth: "NmpMarginPerDepth" = 13, 0 ..= 60;
    /// Constant term of the null-move eval precondition.
    nmp_margin_base: "NmpMarginBase" = 100, 0 ..= 400;
    /// Constant term of the null-move reduction, in plies.
    nmp_reduction_base: "NmpReductionBase" = 5, 1 ..= 10;
    /// Divisor turning depth into extra null-move reduction.
    nmp_reduction_depth_divisor: "NmpReductionDepthDivisor" = 3, 1 ..= 10;
    /// Centipawns of eval surplus over beta that buy one extra ply of null-move reduction.
    nmp_eval_reduction_divisor: "NmpEvalReductionDivisor" = 200, 50 ..= 800;
    /// Ceiling on the eval-driven part of the null-move reduction, in plies.
    nmp_eval_reduction_max: "NmpEvalReductionMax" = 3, 0 ..= 8;
    /// Quadratic slope of the quiet SEE pruning threshold. Source: `QUIET_SEE_MARGIN_PER_DEPTH`.
    quiet_see_margin_per_depth: "QuietSeeMarginPerDepth" = QUIET_SEE_MARGIN_PER_DEPTH, 1 ..= 150;
    /// Linear slope of the capture SEE pruning threshold. Source: `CAPTURE_SEE_MARGIN_PER_DEPTH`.
    capture_see_margin_per_depth: "CaptureSeeMarginPerDepth" = CAPTURE_SEE_MARGIN_PER_DEPTH, 1 ..= 400;
    /// Numerator of the capture-history relief on the SEE threshold, over 1024.
    capture_see_history_numerator: "CaptureSeeHistoryNumerator" = 34, 0 ..= 256;
    /// Constant term of the first aspiration half-width. Source: `ASPIRATION_INITIAL_DELTA`.
    aspiration_initial_delta: "AspirationInitialDelta" = ASPIRATION_INITIAL_DELTA, 1 ..= 60;
    /// Divisor on `previous_score^2` in the aspiration half-width.
    /// Source: `ASPIRATION_SCORE_DIVISOR`.
    aspiration_score_divisor: "AspirationScoreDivisor" = ASPIRATION_SCORE_DIVISOR, 1_000 ..= 60_000;
    /// Ceiling on the score-scaled aspiration half-width. Source: `ASPIRATION_MAX_DELTA`.
    aspiration_max_delta: "AspirationMaxDelta" = ASPIRATION_MAX_DELTA, 16 ..= 2_048;
    /// Constant term of the singular beta margin slope, over 63.
    singular_beta_base: "SingularBetaBase" = 59, 10 ..= 150;
    /// Extra singular beta margin slope at a TT-PV non-PV node, over 63.
    singular_beta_tt_pv_bonus: "SingularBetaTtPvBonus" = 66, 0 ..= 200;
    /// Constant term of the double-extension margin.
    singular_double_margin: "SingularDoubleMargin" = 16, 0 ..= 100;
    /// Extra double-extension margin at a PV node.
    singular_double_margin_pv_bonus: "SingularDoubleMarginPvBonus" = 16, 0 ..= 100;
    /// Extra double-extension margin when the TT move is not a capture.
    singular_double_margin_quiet_bonus: "SingularDoubleMarginQuietBonus" = 8, 0 ..= 100;
    /// Divisor turning the corrhist blend's magnitude into a smaller double-extension
    /// margin. Reference: `doubleMargin ... - 148 * |correctionValue| / 29360128`,
    /// i.e. `|correctionValue| / 198368`.
    singular_corrplexity_divisor: "SingularCorrplexityDivisor" = 198_368, 4_096 ..= 1_000_000;
    /// Margin above the incumbent best score earning a deeper verification.
    /// Source: `POST_LMR_DEEPER_MARGIN`.
    post_lmr_deeper_margin: "PostLmrDeeperMargin" = POST_LMR_DEEPER_MARGIN, 0 ..= 300;
    /// Margin below which the verification is searched a ply shallower.
    /// Source: `POST_LMR_SHALLOWER_MARGIN`.
    post_lmr_shallower_margin: "PostLmrShallowerMargin" = POST_LMR_SHALLOWER_MARGIN, 0 ..= 150;
    /// Continuation-history bonus applied once a reduced scout beats alpha.
    /// Source: `POST_LMR_CONTINUATION_BONUS`.
    post_lmr_continuation_bonus: "PostLmrContinuationBonus" = POST_LMR_CONTINUATION_BONUS, 0 ..= 4_096;
    /// Constant term of the ProbCut margin. Source: `PROBCUT_BASE_MARGIN`.
    probcut_base_margin: "ProbCutBaseMargin" = PROBCUT_BASE_MARGIN, 50 ..= 600;
    /// ProbCut margin refunded at an improving node. Source: `PROBCUT_IMPROVING_MARGIN`.
    probcut_improving_margin: "ProbCutImprovingMargin" = PROBCUT_IMPROVING_MARGIN, 0 ..= 300;
}

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
    /// Whether the soft/hard pair came from normal side-to-move clock allocation.
    ///
    /// `movetime` deliberately keeps this false even though it supplies equal limits:
    /// exact requested time must not enter an adaptive between-iteration governor.
    pub use_clock_management: bool,
}

/// Shared latch between the UCI thread and a pondering search.
///
/// Armed at `go ponder` and flipped by `ponderhit`. While armed, worker 0 runs as if
/// the search were infinite -- the soft and hard time checks are skipped and the
/// mate-found early break is suppressed -- because UCI forbids a `bestmove` before
/// `ponderhit` or `stop`. The flip also re-bases the clock: the time budget was
/// computed from the clock tokens at `go ponder`, but that time only starts counting
/// against the engine when the predicted move is actually played, so worker 0 measures
/// every subsequent elapsed computation from the flip instant rather than from spawn.
pub struct PonderState {
    pondering: AtomicBool,
    rebased_start: Mutex<Option<Instant>>,
    released: Condvar,
}

impl PonderState {
    /// A freshly armed latch: the search it is handed to starts out pondering.
    pub fn new() -> Self {
        Self {
            pondering: AtomicBool::new(true),
            rebased_start: Mutex::new(None),
            released: Condvar::new(),
        }
    }

    /// Whether the search is still pondering.
    pub fn is_pondering(&self) -> bool {
        self.pondering.load(Ordering::Acquire)
    }

    /// Converts the ponder search into a normal timed one, with its clock starting now.
    ///
    /// Idempotent: only the first call records the clock base, so a duplicate
    /// `ponderhit` cannot quietly extend the time budget.
    pub fn ponderhit(&self) {
        let mut rebased = self
            .rebased_start
            .lock()
            .expect("ponder clock lock should not be poisoned");
        if rebased.is_none() {
            *rebased = Some(Instant::now());
        }
        drop(rebased);
        // Release-ordered after the clock base is written, so a worker that observes
        // the flip is guaranteed to see the instant it must measure from.
        self.pondering.store(false, Ordering::Release);
        self.released.notify_all();
    }

    /// Ends the ponder wait without converting to a timed search.
    ///
    /// This is the `stop`/`quit`/new-command path: the search is being terminated, so
    /// no clock base is recorded. It exists because the shared stop flag alone cannot
    /// end the wait -- the pool sets that same flag internally when worker 0 completes,
    /// which happens while still pondering whenever the search exhausts its depth
    /// ceiling or the root is terminal, and answering then would violate the protocol.
    pub fn abort(&self) {
        let rebased = self
            .rebased_start
            .lock()
            .expect("ponder clock lock should not be poisoned");
        self.pondering.store(false, Ordering::Release);
        drop(rebased);
        self.released.notify_all();
    }

    fn wait_until_released(&self) {
        let mut rebased = self
            .rebased_start
            .lock()
            .expect("ponder clock lock should not be poisoned");
        while self.is_pondering() {
            rebased = self
                .released
                .wait(rebased)
                .expect("ponder clock lock should not be poisoned");
        }
    }

    /// The instant `ponderhit` arrived, once it has.
    fn rebased_start(&self) -> Option<Instant> {
        *self
            .rebased_start
            .lock()
            .expect("ponder clock lock should not be poisoned")
    }
}

impl Default for PonderState {
    fn default() -> Self {
        Self::new()
    }
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
    /// Adjust the LMR verification re-search depth by the scout's margin over the
    /// incumbent best score (`doDeeperSearch` / `doShallowerSearch`).
    pub use_post_lmr_depth: bool,
    /// Bonus continuation history for a reduced move that beat alpha.
    ///
    /// A SEPARATE toggle from `use_post_lmr_depth` even though both hang off the same
    /// fail-high, because measured together they move fixed-depth nodes in opposite
    /// directions -- see the comment on [`SearchOptions::default`].
    pub use_post_lmr_conthist: bool,
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
    /// Let the per-worker ttMove-history scalar deepen or shallow the singular
    /// double-extension margin. Gates only that read; the scalar is maintained
    /// unconditionally.
    pub use_tt_move_history: bool,
    /// Read the per-worker low-ply history table when ordering quiets at ply < 5.
    /// Gates only the ordering term; the table is maintained unconditionally.
    pub use_low_ply_history: bool,
    /// Consume the corrhist blend's magnitude as a complexity proxy: refund LMR
    /// reduction, demand more reverse-futility margin, and shrink the singular
    /// double-extension margin where the correction is large. Gates only those three
    /// reads; corrhist maintenance and the eval correction itself are untouched.
    pub use_corrplexity: bool,
    /// Scale the soft time limit by the fraction of the tree the best root move owns.
    ///
    /// Affects TIME-MANAGED searches only: with no `soft_time` there is nothing to
    /// scale, so `go depth`, `go nodes`, `go infinite` and `bench` are untouched.
    pub use_time_effort: bool,
    /// Replace the legacy additive governor with the continuous between-iteration
    /// factor model for timed single-PV searches.
    pub use_interpolated_time_management: bool,
    /// Slow nominal depth growth after half of the effective soft budget is spent.
    pub use_search_again_depth: bool,
    /// Number of principal variations worker 0 reports at each completed depth.
    pub multi_pv: u32,
    /// The tunable margins, slopes, and divisors the enabled techniques are shaped by.
    ///
    /// Carried here rather than as a separate argument because every consumer of a
    /// toggle is also a consumer of the numbers behind it, and the two must travel
    /// together: a worker searching with a toggle its parameters were not sampled for
    /// is a tuning run measuring the wrong build.
    pub parameters: SearchParameters,
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
            // (15.96 vs 16.08; deeper in only 7 of 24), and it cost +12.3% bench nodes
            // on the build it was measured against (45_036 -> 50_569). Every quiet
            // check is a node that resolves no material,
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
            // Ships **OFF** after the authoritative post-fix primary and validation
            // matches both favored disabling it. The older measurements remain here
            // because they explain why the option was re-tested.
            //
            // M3-F2 measured it single-variable against the M2 kept build over 300
            // games at 8+0.08, Threads=1, `-use-affinity -concurrency 8`, zero
            // forfeits: **-8.11 +/- 20.67 Elo**, Ptnml [2,37,79,30,2]. It shipped off,
            // because the criterion was a positive point estimate.
            //
            // The mechanism behind that negative was measured rather than guessed, and
            // it is what made the re-measurement worth one match. The node saving was
            // LARGE and real -- -5.8% on bench, and -24.7% / -33.1% / -21.6% at fixed
            // depths 10 / 12 / 14 over six tactical positions -- and it converted to
            // almost nothing: at `movetime 1000` over 24 book positions the enabled
            // build reached **+0.12 plies**. A 25% node saving worth a tenth of a ply
            // is a saving handed straight back at the verification re-search, which at
            // the time always paid full `newDepth`. A reduced capture that fails high
            // is re-searched, and captures fail high far more often than quiets at the
            // same move index -- the asymmetry the material term PRICES but cannot
            // remove.
            //
            // M3-F4 then shipped `use_post_lmr_depth`, which is exactly the named
            // revisit condition: the re-search depth now responds to the scout's margin
            // instead of always paying full depth. M4-F1b re-measured on top of it, one
            // match, same conditions, against the M3 kept build (bench 44,737):
            // **+11.59 +/- 22.22 Elo**, Ptnml [3,31,72,41,3], LOS 84.7%, PairsRatio
            // 1.29, zero forfeits. That positive point estimate made it the shipped
            // default at the time. The error bar still covers zero and the two
            // intervals overlap heavily -- this is a ~20 Elo swing in the point
            // estimate after the constraint was removed, not a proof.
            //
            // The recommended-order post-fix binary measured the alternative OFF
            // against that shipped-ON default in two independent 300-game matches.
            // From the OFF arm's perspective: primary **+2.32 +/- 21.80 Elo**,
            // validation **+4.63 +/- 19.00 Elo**, pooled **+3.47 Elo**, with zero
            // integrity-guardrail failures. The decision policy requires positive
            // primary and pooled point estimates plus a non-negative validation
            // estimate, so all three conditions support flipping the default OFF. See
            // `experiments/2026-08-18-recommended-order/UseCaptureLMR/results.md`.
            //
            // Two designs were measured, and the first is recorded because the second
            // only looks obvious afterwards. A FLAT one-ply discount -- the design this
            // feature was specified with -- measured WORSE than no capture reduction at
            // all: +5.7% nodes at depth 10 and +51.6% at depth 12, because it shielded
            // a late pawn grab exactly as much as taking a hanging queen. Making the
            // protection proportional to captured material fixed the node counts.
            //
            // Write-ups: `experiments/MSN-S2-capture-lmr/results.md` (the negative and
            // the mechanism) and `experiments/MSN-S7-capture-lmr-v2/results.md` (the
            // re-measurement).
            use_capture_lmr: false,
            // M3-F4 was specified as ONE package of two sub-mechanisms hanging off the
            // same LMR fail-high, "unless the worker finds cause to split". A four-arm
            // fixed-depth sweep over 24 book positions found the cause: measured
            // against the bit-identical both-off control they move the tree in OPPOSITE
            // directions, so a single toggle would have measured their difference.
            //
            //   arm          d12 total  d12 median   d14 total  d14 median
            //   depth-only      +0.57%       0.935      -0.98%       0.960
            //   conthist-only   +5.92%       1.068      +1.30%       0.986
            //   both            +9.47%       1.053      +9.87%       1.061
            //
            // (`experiments/MSN-S4-postlmr/book-nodes.txt`, `-d14.txt`. Medians, not
            // sums: the per-position node distribution of a verification change is
            // long-tailed, and a six-position sweep let one position carry the whole
            // aggregate with the sign flipping between depths.)
            //
            // The verification-depth band is the sub-mechanism M3-F2 asked for and it
            // does what it was asked to do: it shrinks the median tree at both depths.
            // The continuation bonus grows it, and the two together are worse than
            // either alone, so they ship on DIFFERENT defaults.
            use_post_lmr_depth: true,
            // The post-LMR continuation bonus ships OFF. It is not a search change, it
            // is an ORDERING change, and it lands on a table this engine has already
            // tuned three separate consumers against (LMR statScore, pruning history,
            // move ordering). The reference applies it inside a history system with
            // different bonus magnitudes at every other site, so transplanting the
            // constant means adding a fourth writer to a jointly-tuned table -- the
            // same composition failure M3-F3 measured at ~20 Elo. Its measured cost
            // here is +5.9% median nodes at depth 12 for no depth at equal time.
            use_post_lmr_conthist: false,
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
            // Both ship OFF: the depth-13 toggle probes measured them multiplying
            // nodes-to-depth (ttMoveHistory 2.0-2.3x, corrplexity 1.4-1.9x, 3.0-3.7x
            // combined) with no match evidence behind the inflation. Each earns its
            // default back by winning a fixed-time match against this baseline.
            use_tt_move_history: false,
            use_low_ply_history: true,
            use_corrplexity: false,
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
            use_interpolated_time_management: false,
            use_search_again_depth: false,
            multi_pv: 1,
            parameters: SearchParameters::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterationInfo {
    pub depth: u32,
    pub seldepth: u32,
    /// 1-based analysis-line index for UCI `multipv`.
    pub multipv_index: u32,
    pub score: i32,
    pub nodes: u64,
    pub hashfull: u16,
    /// Successful tablebase probes so far; 0 when no tablebases are loaded.
    pub tbhits: u64,
    pub elapsed: Duration,
    /// Soft-time scale selected for the next iteration, in percent.
    pub time_scale_percent: u32,
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
    /// Successful tablebase probes; 0 when no tablebases are loaded.
    pub tbhits: u64,
    pub elapsed: Duration,
    pub pv: Vec<Move>,
    pub iterations: Vec<IterationInfo>,
}

/// A published node counter padded to its own cache line.
///
/// Without the padding, adjacent workers' counters share a line, and every publish
/// invalidates that line in every other publishing core's cache. Publishing is
/// amortized to once per `NODE_PUBLISH_INTERVAL` nodes, so the sharing is cheap
/// rather than free -- but the padding costs nothing.
#[repr(align(64))]
#[derive(Default)]
pub(crate) struct NodeCounter(AtomicU64);

impl NodeCounter {
    pub(crate) const fn new(nodes: u64) -> Self {
        Self(AtomicU64::new(nodes))
    }
}

impl std::ops::Deref for NodeCounter {
    type Target = AtomicU64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct SearchAgainObservation {
    pub(crate) worker_id: usize,
    pub(crate) increase_depth: bool,
    pub(crate) increase_depth_address: usize,
    pub(crate) search_again_counter: u32,
}

pub(crate) struct WorkerParameters<'a> {
    worker_id: usize,
    generation: u8,
    node_counters: &'a [NodeCounter],
    history: &'a SharedHistory,
    network: &'a Network,
    root_move_reporter: Option<Box<dyn FnMut(RootMoveInfo) + 'a>>,
    #[cfg(feature = "corrhist-regression")]
    correction_sample_reporter: Option<Box<dyn FnMut(CorrectionSample) + 'a>>,
    tablebases: Option<&'a dyn TablebaseProbe>,
    tb_hit_counters: Option<&'a [NodeCounter]>,
    root_moves: Option<Vec<Move>>,
    ponder: Option<&'a PonderState>,
    increase_depth: Option<&'a AtomicBool>,
    search_again_counter: u32,
    #[cfg(test)]
    search_again_observer: Option<&'a mpsc::Sender<SearchAgainObservation>>,
}

impl<'a> WorkerParameters<'a> {
    pub(crate) fn new(
        worker_id: usize,
        generation: u8,
        node_counters: &'a [NodeCounter],
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
            #[cfg(feature = "corrhist-regression")]
            correction_sample_reporter: None,
            tablebases: None,
            tb_hit_counters: None,
            root_moves: None,
            ponder: None,
            increase_depth: None,
            search_again_counter: 0,
            #[cfg(test)]
            search_again_observer: None,
        }
    }

    /// Attaches Syzygy tablebases and the per-worker tbhits publish slots.
    pub(crate) fn with_tablebases(
        mut self,
        tablebases: &'a Tablebases,
        tb_hit_counters: &'a [NodeCounter],
    ) -> Self {
        assert!(self.worker_id < tb_hit_counters.len());
        self.tablebases = Some(tablebases);
        self.tb_hit_counters = Some(tb_hit_counters);
        self
    }

    #[cfg(test)]
    fn with_tablebase_probe(
        mut self,
        tablebases: &'a dyn TablebaseProbe,
        tb_hit_counters: &'a [NodeCounter],
    ) -> Self {
        assert!(self.worker_id < tb_hit_counters.len());
        self.tablebases = Some(tablebases);
        self.tb_hit_counters = Some(tb_hit_counters);
        self
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

    #[cfg(feature = "corrhist-regression")]
    fn with_correction_sample_reporter(
        mut self,
        reporter: impl FnMut(CorrectionSample) + 'a,
    ) -> Self {
        self.correction_sample_reporter = Some(Box::new(reporter));
        self
    }

    /// Restricts the root to the given moves, the UCI `go searchmoves` contract.
    pub(crate) fn with_root_moves(mut self, root_moves: Vec<Move>) -> Self {
        self.root_moves = Some(root_moves);
        self
    }

    /// Attaches the `go ponder` latch: the search ignores its time budget and the
    /// mate-found break until the latch flips, and measures elapsed time from the
    /// `ponderhit` instant afterwards.
    pub(crate) fn with_ponder(mut self, ponder: &'a PonderState) -> Self {
        self.ponder = Some(ponder);
        self
    }

    pub(crate) fn with_increase_depth(mut self, increase_depth: &'a AtomicBool) -> Self {
        self.increase_depth = Some(increase_depth);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_search_again_test_state(
        mut self,
        search_again_counter: u32,
        observer: &'a mpsc::Sender<SearchAgainObservation>,
    ) -> Self {
        self.search_again_counter = search_again_counter;
        self.search_again_observer = Some(observer);
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
        None,
        None,
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
    tablebases: Option<&Tablebases>,
    root_moves: Option<Vec<Move>>,
    stop: &AtomicBool,
    on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    let node_counters = [NodeCounter::new(0)];
    let tb_hit_counters = [NodeCounter::new(0)];
    let shared_history = SharedHistory::new();
    let increase_depth = Arc::new(AtomicBool::new(true));
    let mut parameters = WorkerParameters::new(0, 0, &node_counters, &shared_history, network)
        .with_increase_depth(&increase_depth);
    if let Some(tablebases) = tablebases {
        parameters = parameters.with_tablebases(tablebases, &tb_hit_counters);
    }
    if let Some(root_moves) = root_moves {
        parameters = parameters.with_root_moves(root_moves);
    }
    search_worker_with_history_callback_options(
        position,
        history,
        transposition_table,
        limits,
        options,
        stop,
        parameters,
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
#[allow(clippy::too_many_arguments)]
pub fn search_with_shared_history(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    shared_history: &SharedHistory,
    network: &Network,
    tablebases: Option<&Tablebases>,
    root_moves: Option<Vec<Move>>,
) -> SearchResult {
    let position_history = [position.repetition_key()];
    let node_counters = [NodeCounter::new(0)];
    let tb_hit_counters = [NodeCounter::new(0)];
    let stop = AtomicBool::new(false);
    let increase_depth = Arc::new(AtomicBool::new(true));
    let mut parameters = WorkerParameters::new(0, 0, &node_counters, shared_history, network)
        .with_increase_depth(&increase_depth);
    if let Some(tablebases) = tablebases {
        parameters = parameters.with_tablebases(tablebases, &tb_hit_counters);
    }
    if let Some(root_moves) = root_moves {
        parameters = parameters.with_root_moves(root_moves);
    }
    search_worker_with_history_callback_options(
        position,
        &position_history,
        transposition_table,
        limits,
        options,
        &stop,
        parameters,
        |_| {},
    )
}

/// Single-threaded fixed-configuration search that reports eligible exact PVS nodes.
///
/// The caller owns correction history so roots in one dataset split can retain their
/// learned predictors without sharing them with another split. Tablebases and root
/// filters are intentionally absent from this research-only entry point.
#[cfg(feature = "corrhist-regression")]
pub fn search_with_correction_samples<F>(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    shared_history: &SharedHistory,
    network: &Network,
    on_sample: F,
) -> SearchResult
where
    F: FnMut(CorrectionSample),
{
    let position_history = [position.repetition_key()];
    let node_counters = [NodeCounter::new(0)];
    let stop = AtomicBool::new(false);
    let increase_depth = Arc::new(AtomicBool::new(true));
    let parameters = WorkerParameters::new(0, 0, &node_counters, shared_history, network)
        .with_increase_depth(&increase_depth)
        .with_correction_sample_reporter(on_sample);
    search_worker_with_history_callback_options(
        position,
        &position_history,
        transposition_table,
        limits,
        options,
        &stop,
        parameters,
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
    #[cfg(feature = "corrhist-regression")]
    let correction_sample_reporter = worker.correction_sample_reporter.take();
    let root_moves = worker.root_moves.take();
    let tablebases = worker.tablebases;
    let tb_hit_counters = worker.tb_hit_counters;
    let ponder = worker.ponder;
    let increase_depth = worker.increase_depth;
    let mut search_again_counter = worker.search_again_counter;
    #[cfg(test)]
    let search_again_observer = worker.search_again_observer;
    let use_search_again_depth = search_again_depth_active(&limits, &options);
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
            use_clock_management: false,
            ..limits
        }
    };
    let maximum_depth = iteration_ceiling(&limits, ponder.is_some());
    // Allocated once per worker per search, outside the deepening loop; each iteration
    // only refills it in place.
    let low_ply_history = LowPlyHistory::new();
    let mut context = SearchContext::new(
        transposition_table,
        worker_limits,
        started,
        stop,
        position,
        history,
        worker.history,
        &low_ply_history,
        options,
        worker_id,
        worker.generation,
        worker.node_counters,
        worker.network,
    );
    context.root_move_reporter = root_move_reporter;
    #[cfg(feature = "corrhist-regression")]
    {
        context.correction_sample_reporter = correction_sample_reporter;
    }
    context.tablebases = tablebases;
    context.tb_hit_counters = tb_hit_counters;
    context.root_move_filter = root_moves;
    context.ponder = ponder;
    // Root DTZ probe: when the root itself is in table range, restrict the root move
    // list to the moves that preserve the tablebase verdict for the whole search. DTZ
    // handles the fifty-move rule itself, so a nonzero halfmove clock is fine here.
    // A caller-supplied restriction (UCI `searchmoves`) is intersected rather than
    // replaced: both constraints must hold.
    if let Some(tablebases) = tablebases
        && position.occupancy().count() as usize <= tablebases.max_pieces()
        && let Some(mut allowed) = tablebases.preserving_root_moves(position)
    {
        if let Some(requested) = context.root_move_filter.as_ref() {
            allowed.retain(|mv| requested.contains(mv));
        }
        if !allowed.is_empty() {
            context.root_move_filter = Some(allowed);
        }
    }
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
            tbhits: 0,
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
            tbhits: 0,
            elapsed: context.elapsed(),
            pv: vec![fallback_move],
            iterations: Vec::new(),
        };
    }
    let mut completed = None;
    let mut previous_score = 0;
    let mut previous_best_move = None;
    let mut stability = 0u32;
    let mut previous_average_score = 0;
    let mut score_history = [0; 4];
    let mut score_history_index = 0usize;
    let mut has_score_history = false;
    let mut last_best_move_change_depth = 0u32;
    let multi_pv_search = if worker_id == 0 && options.multi_pv > 1 {
        Some((
            options.multi_pv.min(
                multipv_allowed_moves(&root_moves, context.root_move_filter.as_deref(), &[]).len()
                    as u32,
            ),
            context.root_move_filter.clone(),
        ))
    } else {
        None
    };

    for nominal_depth in 1..=maximum_depth {
        let increase_depth_decision =
            increase_depth.is_none_or(|decision| decision.load(Ordering::Relaxed));
        let (next_counter, depth) = search_again_iteration(
            nominal_depth,
            search_again_counter,
            use_search_again_depth,
            increase_depth_decision,
        );
        search_again_counter = next_counter;
        #[cfg(test)]
        if let Some(observer) = search_again_observer {
            let _ = observer.send(SearchAgainObservation {
                worker_id,
                increase_depth: increase_depth_decision,
                increase_depth_address: increase_depth
                    .map_or(0, |decision| std::ptr::from_ref(decision).addr()),
                search_again_counter,
            });
        }
        // Refilled with its prior rather than carried over: the previous iteration's
        // near-root preferences are stale once the root depth moves on.
        low_ply_history.fill_prior();
        context.begin_root_iteration();
        let iteration_start_nodes = context.nodes;
        context.seldepth = depth;
        let attempt = if depth >= 5 {
            aspiration_search(position, depth, previous_score, &mut context)
        } else {
            root_search(position, depth, -INFINITY, INFINITY, true, &mut context)
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
        if context.uses_interpolated_time_management() {
            if best_move.is_some() && best_move != previous_best_move {
                last_best_move_change_depth = depth;
            }
            previous_best_move = best_move;
            let previous_average = if has_score_history {
                previous_average_score
            } else {
                score
            };
            let older_score = if has_score_history {
                score_history[score_history_index]
            } else {
                score
            };
            context.set_time_scale(interpolated_time_scale_percent(
                previous_average,
                older_score,
                score,
                depth.saturating_sub(last_best_move_change_depth),
                context.root_time_statistics.best_move_changes,
                context.node_counters.len(),
                context.root_time_statistics.nodes_effort(best_move),
            ));
            context.recompute_soft_time_reached();

            if has_score_history {
                previous_average_score = (previous_average_score + score) / 2;
            } else {
                previous_average_score = score;
                score_history.fill(score);
                has_score_history = true;
            }
            score_history[score_history_index] = score;
            score_history_index = (score_history_index + 1) % score_history.len();
        } else {
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
        }
        previous_score = score;
        if worker_id == 0
            && use_search_again_depth
            && let Some(increase_depth) = increase_depth
        {
            increase_depth.store(
                should_increase_search_depth(
                    context.is_pondering(),
                    context.elapsed(),
                    context.scaled_soft_time(),
                ),
                Ordering::Relaxed,
            );
        }

        if let Some((multi_pv, base_root_move_filter)) = multi_pv_search
            .as_ref()
            .filter(|(multi_pv, _)| *multi_pv > 1)
        {
            context.publish_nodes();
            completed = Some(IterationInfo {
                depth,
                seldepth: context.seldepth.max(depth),
                multipv_index: 1,
                score,
                nodes: context.reported_nodes(),
                hashfull: context.transposition_table.hashfull_per_mille(),
                tbhits: context.reported_tb_hits(),
                elapsed: context.elapsed(),
                time_scale_percent: context.time_scale_percent,
                pv: pv.clone(),
            });
            let mut lines = vec![(score, pv)];
            let mut found_moves = Vec::with_capacity(*multi_pv as usize);
            found_moves.push(
                lines[0]
                    .1
                    .first()
                    .copied()
                    .expect("a searched root line should have a first move"),
            );
            for _ in 1..*multi_pv {
                context.root_move_filter = Some(multipv_allowed_moves(
                    &root_moves,
                    base_root_move_filter.as_deref(),
                    &found_moves,
                ));
                // ponytail: secondary lines use full-width searches; add per-line
                // aspiration windows if MultiPV analysis speed becomes a bottleneck.
                let Some(line) =
                    root_search(position, depth, -INFINITY, INFINITY, false, &mut context)
                else {
                    break;
                };
                found_moves.push(
                    line.1
                        .first()
                        .copied()
                        .expect("a searched root line should have a first move"),
                );
                lines.push(line);
            }
            context.root_move_filter = base_root_move_filter.clone();
            lines.sort_by_key(|line| std::cmp::Reverse(line.0));
            context.publish_nodes();
            let elapsed = context.elapsed();
            let seldepth = context.seldepth.max(depth);
            let nodes = context.reported_nodes();
            let hashfull = context.transposition_table.hashfull_per_mille();
            let tbhits = context.reported_tb_hits();
            for (index, (score, pv)) in lines.into_iter().enumerate() {
                let info = IterationInfo {
                    depth,
                    seldepth,
                    multipv_index: index as u32 + 1,
                    score,
                    nodes,
                    hashfull,
                    tbhits,
                    elapsed,
                    time_scale_percent: context.time_scale_percent,
                    pv,
                };
                if index == 0 {
                    completed = Some(info.clone());
                }
                on_iteration(&info);
                context.iterations.push(info);
            }

            if context.nodes == iteration_start_nodes
                || limits.depth == Some(nominal_depth)
                || context.should_stop_after_iteration()
            {
                break;
            }
            continue;
        }

        context.publish_nodes();
        let elapsed = context.elapsed();
        let info = IterationInfo {
            depth,
            seldepth: context.seldepth.max(depth),
            multipv_index: 1,
            score,
            nodes: context.reported_nodes(),
            hashfull: context.transposition_table.hashfull_per_mille(),
            tbhits: context.reported_tb_hits(),
            elapsed,
            time_scale_percent: context.time_scale_percent,
            pv,
        };
        completed = Some(info.clone());
        on_iteration(&info);
        context.iterations.push(info);

        if context.nodes == iteration_start_nodes
            || limits.depth == Some(nominal_depth)
            || context.should_stop_after_iteration()
            // A pondering search may not answer even from a found mate: UCI forbids a
            // `bestmove` before `ponderhit`/`stop`, so it keeps searching instead.
            || (!limits.infinite
                && !context.is_pondering()
                && options.multi_pv == 1
                && score.abs() >= MATE_SCORE - depth as i32)
        {
            break;
        }
    }

    let saturated_clocked_ponder = ponder.is_some()
        && limits.depth.is_none()
        && limits.nodes.is_none()
        && !limits.infinite
        && limits.soft_time.is_some()
        && limits.hard_time.is_some()
        && completed
            .as_ref()
            .is_some_and(|iteration| iteration.depth == maximum_depth);
    if saturated_clocked_ponder {
        let ponder = ponder.expect("a pondering context should retain its latch");
        ponder.wait_until_released();

        if ponder.rebased_start().is_some() {
            let root_move_reporter = context.root_move_reporter.take();
            loop {
                low_ply_history.fill_prior();
                context.begin_root_iteration();
                context.seldepth = maximum_depth;
                let attempt =
                    aspiration_search(position, maximum_depth, previous_score, &mut context);
                let Some((score, pv)) = attempt else {
                    break;
                };

                let best_move = pv.first().copied();
                if context.uses_interpolated_time_management() {
                    if best_move.is_some() && best_move != previous_best_move {
                        last_best_move_change_depth = maximum_depth;
                    }
                    previous_best_move = best_move;
                    let previous_average = if has_score_history {
                        previous_average_score
                    } else {
                        score
                    };
                    let older_score = if has_score_history {
                        score_history[score_history_index]
                    } else {
                        score
                    };
                    context.set_time_scale(interpolated_time_scale_percent(
                        previous_average,
                        older_score,
                        score,
                        maximum_depth.saturating_sub(last_best_move_change_depth),
                        context.root_time_statistics.best_move_changes,
                        context.node_counters.len(),
                        context.root_time_statistics.nodes_effort(best_move),
                    ));

                    if has_score_history {
                        previous_average_score = (previous_average_score + score) / 2;
                    } else {
                        previous_average_score = score;
                        score_history.fill(score);
                        has_score_history = true;
                    }
                    score_history[score_history_index] = score;
                    score_history_index = (score_history_index + 1) % score_history.len();
                } else {
                    if best_move.is_some() && best_move == previous_best_move {
                        stability = (stability + 1).min(TIME_STABILITY_CAP);
                    } else {
                        stability = 0;
                    }
                    previous_best_move = best_move;
                    let effort_percent = context.best_move_effort_percent(best_move);
                    context.set_time_scale(scaled_time_percent(
                        time_scale_percent(stability, score - previous_score),
                        effort_percent,
                    ));
                }
                previous_score = score;
                context.recompute_soft_time_reached();
                context.publish_nodes();
                completed = Some(IterationInfo {
                    depth: maximum_depth,
                    seldepth: context.seldepth.max(maximum_depth),
                    multipv_index: 1,
                    score,
                    nodes: context.reported_nodes(),
                    hashfull: context.transposition_table.hashfull_per_mille(),
                    tbhits: context.reported_tb_hits(),
                    elapsed: context.elapsed(),
                    time_scale_percent: context.time_scale_percent,
                    pv,
                });

                if context.should_stop_after_iteration() {
                    break;
                }
            }
            context.root_move_reporter = root_move_reporter;

            context.publish_nodes();
            if let Some(info) = completed.as_mut() {
                info.nodes = context.reported_nodes();
                info.hashfull = context.transposition_table.hashfull_per_mille();
                info.tbhits = context.reported_tb_hits();
                info.elapsed = context.elapsed();
                info.time_scale_percent = context.time_scale_percent;
                on_iteration(info);
                context.iterations.push(info.clone());
            }
        }
    }

    let completed = completed.unwrap_or_else(|| IterationInfo {
        depth: 0,
        seldepth: 0,
        multipv_index: 1,
        score: context.static_eval(position),
        nodes: context.reported_nodes(),
        hashfull: context.transposition_table.hashfull_per_mille(),
        tbhits: context.reported_tb_hits(),
        elapsed: context.elapsed(),
        time_scale_percent: context.time_scale_percent,
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
        tbhits: context.tb_hits,
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
fn iteration_ceiling(limits: &SearchLimits, pondering: bool) -> u32 {
    if limits.infinite || (pondering && limits.depth.is_none() && limits.nodes.is_none()) {
        MAX_ITERATIVE_DEEPENING_DEPTH
    } else {
        limits
            .depth
            .unwrap_or(DEFAULT_MAX_DEPTH)
            .clamp(1, MAX_ITERATIVE_DEEPENING_DEPTH)
    }
}

fn effective_search_depth(
    nominal_depth: u32,
    search_again_counter: u32,
    search_again_depth_active: bool,
) -> u32 {
    if !search_again_depth_active {
        return nominal_depth;
    }
    let reduction = 3u32.saturating_mul(search_again_counter.saturating_add(1)) / 4;
    nominal_depth.saturating_sub(reduction).max(1)
}

fn search_again_iteration(
    nominal_depth: u32,
    search_again_counter: u32,
    search_again_depth_active: bool,
    increase_depth: bool,
) -> (u32, u32) {
    let search_again_counter = if search_again_depth_active && !increase_depth {
        search_again_counter.saturating_add(1)
    } else {
        search_again_counter
    };
    (
        search_again_counter,
        effective_search_depth(
            nominal_depth,
            search_again_counter,
            search_again_depth_active,
        ),
    )
}

fn should_increase_search_depth(
    pondering: bool,
    elapsed: Duration,
    effective_soft_time: Option<Duration>,
) -> bool {
    pondering || effective_soft_time.is_some_and(|soft_time| elapsed <= soft_time / 2)
}

fn multipv_allowed_moves(
    legal_moves: &[Move],
    base_allowed: Option<&[Move]>,
    found_moves: &[Move],
) -> Vec<Move> {
    legal_moves
        .iter()
        .copied()
        .filter(|mv| base_allowed.is_none_or(|allowed| allowed.contains(mv)))
        .filter(|mv| !found_moves.contains(mv))
        .collect()
}

fn aspiration_search(
    position: &Position,
    depth: u32,
    previous_score: i32,
    context: &mut SearchContext<'_>,
) -> Option<(i32, Vec<Move>)> {
    let mut delta = aspiration_delta(
        &context.options.parameters,
        context.worker_id,
        previous_score,
    );
    let mut alpha = (previous_score - delta).max(-INFINITY);
    let mut beta = (previous_score + delta).min(INFINITY);
    // A fail high means the root move is better than believed. Re-searching at full
    // depth to confirm a bound that is about to be raised again wastes the iteration,
    // so each successive fail high gives up a ply, down to a floor.
    let mut search_depth = depth;

    loop {
        let result = root_search(position, search_depth.max(1), alpha, beta, true, context)?;
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
            return root_search(position, depth, alpha, beta, true, context);
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
fn aspiration_delta(parameters: &SearchParameters, worker_id: usize, previous_score: i32) -> i32 {
    let scaled = parameters.aspiration_initial_delta
        + previous_score.saturating_mul(previous_score) / parameters.aspiration_score_divisor;
    scaled.min(parameters.aspiration_max_delta) + (worker_id % ASPIRATION_JITTER_BUCKETS) as i32
}

fn published_node_total(counters: &[NodeCounter]) -> u64 {
    counters.iter().fold(0, |total, counter| {
        total.saturating_add(counter.load(Ordering::Relaxed))
    })
}

fn root_search(
    position: &Position,
    depth: u32,
    mut alpha: i32,
    beta: i32,
    store_root_entry: bool,
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

    let ordering = context.ordering(&position, 0);
    let mut picker = MovePicker::new(tt_move, [None, None], ordering);
    while let Some(mv) = picker.next(&position) {
        if context
            .root_move_filter
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&mv))
        {
            continue;
        }
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
        // A move that captured a king leaves the board with a side missing one.
        // `Position::from_fen` rejects the position classes that make such a move
        // generatable, but a caller can still hand the search a manually built
        // position, and `is_in_check` reports `false` for an *absent* king rather than
        // panicking -- so the capture would pass the legality filter above. NNUE then
        // asks for the king square and panics inside the search thread, killing the
        // engine mid-analysis. Such a "move" is not a chess move at all, so drop it
        // here rather than teaching the evaluator to tolerate a kingless board.
        if position.pieces(!mover, PieceKind::King).first().is_none() {
            position.unmake_move(mv, undo);
            continue;
        }
        // Reported after the legality filters above, so `currmovenumber` counts the moves
        // actually searched. Numbering is 1-based per the UCI spec, hence `searched + 1`.
        context.report_root_move(depth, mv, searched + 1);
        let nodes_before_move = context.nodes;
        context.push_position(&position, 1, mv, continuation, &undo);
        let mut child_pv = PvLine::new();
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
            context.record_root_best_move(mv, store_root_entry);
            best_score = score;
            best_pv.clear();
            best_pv.push(mv);
            best_pv.extend_from_slice(child_pv.as_slice());
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
    if store_root_entry {
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
    }
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
    pv: &mut PvLine,
) -> Option<i32> {
    #[cfg(feature = "instrumentation")]
    crate::instrumentation::record(|counters| counters.interior_nodes += 1);
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
        #[cfg(feature = "instrumentation")]
        crate::instrumentation::record(|counters| counters.interior_static_evals += 1);
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
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.tt_cutoffs += 1);
                return Some(score);
            }
        }
    }
    if excluded_move.is_some() {
        tt_move = None;
    }

    // Syzygy WDL probe, after the TT failed to cut off (reference-engine convention).
    // Only exact when the halfmove clock is zero, and `probe_wdl` itself declines when
    // castling rights remain. `pvs` never runs at the root (`root_search` owns ply 0),
    // so no root gate is needed here.
    //
    // When the verdict cannot cut off (a win below beta or a loss above alpha at a PV
    // node's wide window), it still bounds this node's value: `syzygy_floor` seeds the
    // best score for a proven win and `syzygy_ceiling` caps it for a proven loss, which
    // is how the reference engine propagates the tablebase band through PV nodes.
    let mut syzygy_floor = -INFINITY;
    let mut syzygy_ceiling = INFINITY;
    if let Some(tablebases) = context.tablebases
        && excluded_move.is_none()
        && position.halfmove_clock() == 0
        && position.occupancy().count() as usize <= tablebases.max_pieces()
        && let Some(wdl) = tablebases.probe_wdl(position)
    {
        context.tb_hits += 1;
        let (value, bound) = match wdl {
            Wdl::Win => (TABLEBASE_SCORE - ply as i32, Bound::Lower),
            Wdl::Loss => (-(TABLEBASE_SCORE - ply as i32), Bound::Upper),
            Wdl::Draw | Wdl::BlessedLoss | Wdl::CursedWin => (0, Bound::Exact),
        };
        let bound_allows_cutoff = match bound {
            Bound::Exact => true,
            Bound::Lower => value >= beta,
            Bound::Upper => value <= alpha,
        };
        if bound_allows_cutoff {
            // Stored a few plies deeper than searched so the entry survives deeper
            // re-visits: the verdict is exact regardless of depth, and re-probing the
            // table files costs more than a TT hit. `value_to_tt` restores the
            // ply-independent band, so `value_from_tt`'s rule50 headroom logic applies
            // on the way back out.
            context.transposition_table.store(
                key,
                EntryData {
                    best_move: None,
                    score: value_to_tt(value, ply) as i16,
                    static_eval: UNEVALUATED_STATIC_EVAL,
                    depth: tt_stored_depth(depth + SYZYGY_TT_DEPTH_BONUS),
                    bound,
                    age: context.generation,
                    pv: pv_node,
                },
            );
            return Some(value);
        }
        if pv_node {
            match bound {
                Bound::Lower => {
                    syzygy_floor = value;
                    alpha = alpha.max(value);
                }
                Bound::Upper => syzygy_ceiling = value,
                Bound::Exact => {}
            }
        }
    }

    let in_check = is_in_check(position, position.side_to_move());
    #[cfg(feature = "instrumentation")]
    if in_check {
        crate::instrumentation::record(|counters| counters.checked_interior_nodes += 1);
    }
    // The RAW static eval is what goes to the TT, deliberately. Storing the corrected
    // value would fold a residual that was learned at one point in the search into an
    // entry read at another, and the correction would then be applied a second time on
    // top of itself on every re-probe. The reference is explicit about this.
    let raw_static_eval = tt_entry
        .filter(|entry| entry.static_eval != UNEVALUATED_STATIC_EVAL)
        .map_or_else(
            || {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.interior_static_evals += 1);
                context.static_eval(position)
            },
            |entry| i32::from(entry.static_eval),
        );
    #[cfg(feature = "corrhist-regression")]
    let correction_features = correction_features(position, context, ply);
    let correction = correction_value(position, context, ply);
    let static_eval = to_corrected_static_eval(raw_static_eval, correction);
    // The blend's magnitude doubles as the engine's complexity proxy: LMR, RFP, and
    // the singular double margin all consume it below. `use_corrplexity` gates those
    // three READS only; the correction applied to the eval above is untouched.
    let corrplexity = if context.options.use_corrplexity {
        correction.abs()
    } else {
        0
    };
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
            && {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.razoring_attempts += 1);
                static_eval < alpha - razoring_margin(&context.options.parameters, depth)
            }
        {
            #[cfg(feature = "instrumentation")]
            crate::instrumentation::record(|counters| counters.razoring_cutoffs += 1);
            return quiescence(
                position, alpha, beta, ply, pv_node, false, true, context, pv,
            );
        }

        if context.options.use_rfp
            && depth <= RFP_MAX_DEPTH
            && !is_mate_score(beta)
            && eval_pruning_rule50_safe(position, depth)
            && {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.rfp_attempts += 1);
                static_eval
                    - reverse_futility_margin(
                        &context.options.parameters,
                        depth,
                        improving,
                        cut_node,
                        tt_pv,
                        corrplexity,
                    )
                    >= beta
            }
        {
            #[cfg(feature = "instrumentation")]
            crate::instrumentation::record(|counters| counters.rfp_cutoffs += 1);
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
            && {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.nmp_attempts += 1);
                static_eval
                    >= beta - context.options.parameters.nmp_margin_per_depth * depth
                        + context.options.parameters.nmp_margin_base
            }
        {
            let reduction =
                null_move_reduction(&context.options.parameters, depth, static_eval, beta);
            let undo = position.make_null_move();
            context.push_null_position(ply + 1);
            let mut null_pv = PvLine::new();
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
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| counters.nmp_cutoffs += 1);
                    return Some(null_value);
                }

                let verification_depth = depth - reduction;
                context.nmp_min_ply = null_move_verification_min_ply(ply, verification_depth);
                let mut verification_pv = PvLine::new();
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
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| counters.nmp_cutoffs += 1);
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
        #[cfg(feature = "instrumentation")]
        crate::instrumentation::record(|counters| counters.probcut_attempts += 1);
        let probcut_beta = probcut_beta(&context.options.parameters, beta, improving);
        let tt_score = tt_entry
            .map(|entry| value_from_tt(i32::from(entry.score), ply, position.halfmove_clock()));
        if tt_score.is_none_or(|score| score >= probcut_beta) {
            let probcut_depth = probcut_depth(depth, improving);
            let see_threshold = probcut_beta - static_eval;
            let ordering = context.ordering(position, ply);
            let mut picker = MovePicker::captures_only(tt_move, ordering);
            while let Some(mv) = picker.next(position) {
                // Every yielded move is from the captures family here, so the picker's
                // memoized exact SEE always answers the threshold test.
                if picker
                    .current_capture_see()
                    .expect("captures_only yields only capture-family moves")
                    < see_threshold
                {
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
                let mut probcut_pv = PvLine::new();
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
                        #[cfg(feature = "instrumentation")]
                        crate::instrumentation::record(|counters| counters.probcut_cutoffs += 1);
                        return Some(cutoff_value);
                    }
                }
            }
        }
    }

    let mut best_score = -INFINITY;
    let mut best_move = None;
    let mut searched = 0usize;
    let mut child_pv = PvLine::new();
    let mut searched_quiets: MoveBuffer<256> = MoveBuffer::new();
    let mut searched_captures: MoveBuffer<256> = MoveBuffer::new();

    let pawn_key = position.zobrist().pawn();
    let ordering = context.ordering(position, ply);
    let check_info = CheckInfo::new(position);
    // Loop-invariant: the mover and its material are the same at every move of this
    // node, but the compiler cannot prove make/unmake restores the position, so
    // hoisting is ours to do. The pruning consumers are each gated on their own
    // toggle, so computing the shared value when ANY of them is on is exact.
    let mover = position.side_to_move();
    let needs_non_pawn_material =
        context.options.use_lmp || context.options.use_futility || context.options.use_see_pruning;
    let mover_has_non_pawn_material =
        needs_non_pawn_material && has_non_pawn_material_for(position, mover);
    let mut picker = MovePicker::new(tt_move, context.killers.killers(ply), ordering);
    while let Some(mv) = picker.next(position) {
        if excluded_move == Some(mv) {
            continue;
        }
        let quiet = !mv.flag().is_capture() && mv.flag().promotion().is_none();
        let gives_check = check_info.gives_check(position, mv);
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
            late_move_reduction(
                &context.lmr_table,
                &context.options.parameters,
                depth,
                move_count,
                improving,
                cut_node,
                tt_pv,
                history_score,
                corrplexity,
            )
        } else if !quiet && capture_reduction_allowed(mv, tt_move, gives_check) {
            capture_late_move_reduction(
                &context.lmr_table,
                &context.options.parameters,
                depth,
                move_count,
                improving,
                cut_node,
                tt_pv,
                captured_material(position, mv),
                capture_history,
                corrplexity,
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
            && {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| {
                    counters.history_pruning_attempts += 1;
                });
                ordering.pruning_history(position, mv) < history_pruning_threshold(effective_depth)
            }
        {
            #[cfg(feature = "instrumentation")]
            crate::instrumentation::record(|counters| counters.history_pruning_cutoffs += 1);
            continue;
        }

        if context.options.use_lmp
            && !pv_node
            && !in_check
            && quiet
            && !gives_check
            && depth <= LMP_MAX_DEPTH
            && {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.lmp_attempts += 1);
                move_count
                    >= late_move_pruning_threshold(depth, improving, &context.options.parameters)
            }
            && best_move.is_some()
            && shallow_pruning_allowed(best_score)
            && mover_has_non_pawn_material
        {
            #[cfg(feature = "instrumentation")]
            crate::instrumentation::record(|counters| counters.lmp_cutoffs += 1);
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
            #[cfg(feature = "instrumentation")]
            crate::instrumentation::record(|counters| counters.futility_attempts += 1);
            let futility_value = static_eval
                + frontier_futility_margin(&context.options.parameters, effective_depth);
            if futility_value <= alpha {
                best_score = best_score.max(futility_value);
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.futility_cutoffs += 1);
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
            let parameters = &context.options.parameters;
            let threshold = if quiet {
                quiet_see_threshold(parameters, effective_depth)
            } else {
                capture_see_threshold(parameters, depth)
                    - capture_history * parameters.capture_see_history_numerator / 1024
            };
            // Captures reuse the exact SEE the picker computed while loading its
            // capture list; only quiets (whose SEE the picker never computes) still
            // walk the exchange here, and only inside the depth window.
            if within_window
                && {
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| counters.see_pruning_attempts += 1);
                    picker
                        .current_capture_see()
                        .unwrap_or_else(|| static_exchange_evaluation(position, mv))
                        < threshold
                }
                && (quiet || alpha >= 0 || has_other_non_pawn_material(position, mover, mv))
            {
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.see_pruning_cutoffs += 1);
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
            let singular_beta =
                singular_beta(&context.options.parameters, tt_score, depth, tt_pv, pv_node);
            let singular_depth = (new_depth / 2).max(1);
            let mut singular_pv = PvLine::new();
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
                // Maintenance is unconditional on the multicut path being taken: a
                // multicut is direct evidence the TT move is NOT uniquely best here.
                context.tt_move_history.apply(
                    TT_MOVE_HISTORY_MULTICUT_BASE - TT_MOVE_HISTORY_MULTICUT_PER_DEPTH * depth,
                );
                return Some(multicut_value);
            }
            if context.options.use_singular_ext {
                extension = singular_extension(
                    &context.options.parameters,
                    singular_value,
                    singular_beta,
                    pv_node,
                    mv.flag().is_capture(),
                    cut_node,
                    tt_score,
                    beta,
                    tt_move_history_margin_adjustment(&context.options, &context.tt_move_history),
                    corrplexity,
                );
            }
        }
        if context.options.use_check_ext {
            extension = check_extension(gives_check, extension);
        }

        let child_depth = (new_depth + extension).max(0);
        let continuation = continuation_key(position, mv);
        // Resolved BEFORE the move is made, because `mv.from()` is empty afterwards and
        // the post-LMR update site runs while the child is still on the board. This is
        // the pre-promotion piece, matching the cutoff site's `piece_at(from)`.
        let moved_piece = position.piece_at(mv.from());
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
            #[cfg(feature = "instrumentation")]
            if reduced_depth < child_depth {
                crate::instrumentation::record(|counters| counters.lmr_reductions += 1);
            }
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
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| counters.reduced_fail_highs += 1);
                    // Post-LMR re-search handling (M3-F4). Two things happen once a
                    // reduced scout beats alpha, and both target the VERIFICATION
                    // rather than the reduction, because M3-F2 measured a 25-33%
                    // fixed-depth node saving converting to +0.12 plies at equal time
                    // and identified this always-full-depth re-search as where the
                    // saving went.
                    let verification_depth = if context.options.use_post_lmr_depth {
                        post_lmr_verification_depth(
                            &context.options.parameters,
                            child_depth,
                            reduced_depth,
                            best_score,
                            score,
                        )
                    } else {
                        child_depth
                    };
                    // The continuation bonus is applied at the FAIL HIGH, not after the
                    // verification resolves: the fact being recorded is that a move the
                    // ordering demoted beat alpha at all, which is evidence about the
                    // ORDERING regardless of what full depth then says. The planes are
                    // read at `ply` -- the same predecessors the cutoff site uses -- and
                    // the moving piece comes from the continuation key resolved before
                    // the move was made, because `mv.from()` is empty by now.
                    if context.options.use_post_lmr_conthist
                        && let Some(piece) = moved_piece
                    {
                        let planes = context.continuation_planes(ply);
                        update_continuation_histories(
                            context.history,
                            &planes,
                            piece,
                            mv.to(),
                            context.options.parameters.post_lmr_continuation_bonus,
                        );
                    }
                    if verification_depth <= reduced_depth {
                        score
                    } else {
                        child_pv.clear();
                        #[cfg(feature = "instrumentation")]
                        crate::instrumentation::record_full_depth_research(
                            reduced_depth,
                            verification_depth,
                            child_depth,
                        );
                        pvs(
                            position,
                            verification_depth,
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
                    }
                } else {
                    score
                };
                if score > alpha && score < beta {
                    child_pv.clear();
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record_full_depth_research(
                        reduced_depth,
                        child_depth,
                        child_depth,
                    );
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
            // PV maintenance is a pure OUTPUT: no search decision reads the line back,
            // and non-PV callers never consume the PvLine they pass, so the copy of up
            // to MAX_SEARCH_PLY moves is skipped at non-PV nodes.
            if pv_node {
                pv.load(mv, &child_pv);
            }
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
                searched_quiets.as_slice(),
                searched_captures.as_slice(),
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

    // A tablebase verdict outranks the searched score: a proven win floors it and a
    // proven loss caps it, so heuristic evaluations inside the subtree cannot pull a
    // decided position back into the ordinary evaluation band.
    best_score = best_score.clamp(syzygy_floor, syzygy_ceiling);

    let bound = if best_score <= original_alpha {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    };
    // ttMove-history MAINTENANCE is unconditional (its `Use*` flag gates only the
    // singular-margin read). Non-PV nodes only, and only when a TT move existed to be
    // judged; verification nodes are excluded automatically because they clear
    // `tt_move`. The "best move found" convention matches the correction update below.
    if !pv_node
        && best_score > original_alpha
        && let (Some(best), Some(tt)) = (best_move, tt_move)
    {
        context.tt_move_history.apply(if best == tt {
            TT_MOVE_HISTORY_HIT_BONUS
        } else {
            TT_MOVE_HISTORY_MISS_MALUS
        });
    }
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
    #[cfg(feature = "corrhist-regression")]
    if bound == Bound::Exact
        && !in_check
        && !verification_node
        && excluded_move.is_none()
        && best_score.abs() <= EVALUATION_LIMIT
        && let Some(reporter) = context.correction_sample_reporter.as_mut()
    {
        reporter(CorrectionSample {
            features: correction_features,
            raw_static_eval,
            search_value: best_score,
            depth: depth as u32,
            ply,
            position_key: position.repetition_key(),
        });
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
    pv: &mut PvLine,
) -> Option<i32> {
    #[cfg(feature = "instrumentation")]
    crate::instrumentation::record(|counters| counters.qsearch_nodes += 1);
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
        #[cfg(feature = "instrumentation")]
        crate::instrumentation::record(|counters| counters.qsearch_static_evals += 1);
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
                #[cfg(feature = "instrumentation")]
                crate::instrumentation::record(|counters| counters.tt_cutoffs += 1);
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
                || {
                    #[cfg(feature = "instrumentation")]
                    crate::instrumentation::record(|counters| counters.qsearch_static_evals += 1);
                    context.static_eval(position)
                },
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
            // Exact stalemate detection, kept cheap by a legality-verified
            // captures-first probe: `has_legal_move_in_place` tries the capture family
            // first and returns on the first legal one, so at the typical qsearch
            // terminal the probe ends within a few candidates; the quiet family is
            // only generated in barren positions where stalemate is plausible.
            let score = if has_legal_move_in_place(position) {
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
    // The in-check branch searches every evasion, so its yielded sequence is frozen
    // into a stack list before the first child search: the search below updates
    // `position` and the history tables mid-loop, so the full-width order has to be
    // pinned up front. The non-check branch iterates its staged picker lazily inside
    // the child-search loop, exactly as the interior loop does — sound mid-loop
    // because `make_move`/`unmake_move` restore the position bit-for-bit before the
    // loop asks for the next move (the `MovePicker` doc comment pins this), so each
    // `next` call sees the node's own position. The picker's gate drops below-
    // threshold captures at load, keeps promotions unconditionally, and yields the
    // quiet-check widening only after every capture.
    let mut evasions = MoveList::new();
    let mut qsearch_picker = if in_check {
        let mut picker = MovePicker::new(tt_move, [None, None], ordering);
        while let Some(mv) = picker.next(position) {
            evasions.push(mv);
        }
        None
    } else {
        Some(MovePicker::qsearch(
            tt_move,
            qsearch_see_threshold(context.options.use_see_pruning),
            searches_quiet_checks,
            ordering,
        ))
    };
    let mut searched = 0usize;
    let mut best_move = None;
    let mut child_pv = PvLine::new();
    // Built on the first move that reaches the check test rather than per node: most
    // qsearch moves resolve before it, and the cache pays for itself only when read.
    let mut check_info: Option<CheckInfo> = None;
    let mut evasion_index = 0usize;
    while let Some(mv) = if in_check {
        (evasion_index < evasions.len()).then(|| {
            let mv = evasions[evasion_index];
            evasion_index += 1;
            mv
        })
    } else {
        qsearch_picker
            .as_mut()
            .expect("the non-check branch owns the picker")
            .next(position)
    } {
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
            && !check_info
                .get_or_insert_with(|| CheckInfo::new(position))
                .gives_check(position, mv)
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
            // Same gate as the interior search: the PV is an output consumed only
            // through the PV-node chain rooted at the root search.
            if pv_node {
                pv.load(mv, &child_pv);
            }
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
        // is legitimately readable by capture-domain probes as well. The in-check node
        // searched EVERY evasion, so "none searched" is a real mate.
        -MATE_SCORE + ply as i32
    } else if has_legal_move_in_place(position) {
        best_score
    } else {
        // Same captures-first probe as the stand-pat exit: a captureless leaf with no
        // legal move at all is a stalemate, scored as the draw it is.
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

fn interpolate(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    y0 + (x - x0) * (y1 - y0) / (x1 - x0)
}

fn falling_eval_factor(previous_average: i32, older_score: i32, best_score: i32) -> f64 {
    ((11.48
        + 2.30 * (f64::from(previous_average) - f64::from(best_score))
        + 1.1 * (f64::from(older_score) - f64::from(best_score)))
        / 100.0)
        .clamp(0.576, 1.728)
}

fn time_reduction_factor(depth_since_change: u32) -> f64 {
    interpolate(f64::from(depth_since_change), 4.96, 18.79, 0.639, 1.712).clamp(0.629, 1.544)
}

fn stability_time_factor(depth_since_change: u32) -> f64 {
    1.0 / time_reduction_factor(depth_since_change)
}

fn best_move_instability_factor(best_move_changes: u32, worker_count: usize) -> f64 {
    1.077 + 2.229 * f64::from(best_move_changes) / worker_count.max(1) as f64
}

fn root_effort_factor(nodes_effort: u32) -> f64 {
    interpolate(f64::from(nodes_effort), 75_800.0, 104_510.0, 0.969, 0.714).clamp(0.693, 0.838)
}

fn interpolated_time_scale_percent(
    previous_average: i32,
    older_score: i32,
    best_score: i32,
    depth_since_best_move_change: u32,
    best_move_changes: u32,
    worker_count: usize,
    nodes_effort: u32,
) -> u32 {
    let scale = falling_eval_factor(previous_average, older_score, best_score)
        * stability_time_factor(depth_since_best_move_change)
        * best_move_instability_factor(best_move_changes, worker_count)
        * root_effort_factor(nodes_effort);
    (scale * 100.0)
        .round()
        .clamp(1.0, f64::from(TIME_SCALE_MAX_PERCENT)) as u32
}

fn interpolated_time_management_active(limits: &SearchLimits, options: &SearchOptions) -> bool {
    options.use_interpolated_time_management && limits.use_clock_management && options.multi_pv == 1
}

fn search_again_depth_active(limits: &SearchLimits, options: &SearchOptions) -> bool {
    options.use_search_again_depth && limits.use_clock_management && options.multi_pv == 1
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
    if !is_legal(position, tt_move) {
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
fn razoring_margin(parameters: &SearchParameters, depth: i32) -> i32 {
    parameters.razor_base_margin + parameters.razor_margin_per_depth * depth.max(0)
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
///
/// `corrplexity` is the corrhist blend's magnitude: a large learned residual means the
/// static eval is unreliable here, so the cutoff demands a larger margin before it
/// trusts that eval to stand in for a search.
#[inline]
fn reverse_futility_margin(
    parameters: &SearchParameters,
    depth: i32,
    improving: bool,
    cut_node: bool,
    tt_pv: bool,
    corrplexity: i32,
) -> i32 {
    let effective_depth = depth - i32::from(improving && !cut_node);
    parameters.rfp_margin_per_depth * effective_depth.max(0)
        + parameters.rfp_tt_pv_margin * i32::from(tt_pv)
        + corrplexity / parameters.rfp_corrplexity_divisor
}

#[inline]
fn is_improving(static_eval: i32, same_side_previous_eval: Option<i32>) -> bool {
    same_side_previous_eval.is_none_or(|previous| static_eval > previous)
}

#[inline]
fn late_move_pruning_threshold(
    depth: i32,
    improving: bool,
    parameters: &SearchParameters,
) -> usize {
    let depth = depth.clamp(1, LMP_MAX_DEPTH) as usize;
    (parameters.lmp_base.max(0) as usize + depth * depth) / (2 - usize::from(improving))
}

/// The log-log reduction table, built from the tunable coefficient.
///
/// Built once per search rather than looked up from a process-wide static, because the
/// coefficient is a tunable the GUI may change between searches. 128 logarithms at the
/// start of a `go` is nothing against the tree that follows.
fn build_lmr_table(parameters: &SearchParameters) -> [i32; LMR_TABLE_SIZE] {
    let mut table = [0; LMR_TABLE_SIZE];
    let coefficient = f64::from(parameters.lmr_coefficient) / 128.0;
    for (index, reduction) in table.iter_mut().enumerate().skip(1) {
        *reduction = (coefficient * (index as f64).ln()) as i32;
    }
    table
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn late_move_reduction(
    table: &[i32; LMR_TABLE_SIZE],
    parameters: &SearchParameters,
    depth: i32,
    move_count: usize,
    improving: bool,
    cut_node: bool,
    tt_pv: bool,
    history_score: i32,
    corrplexity: i32,
) -> i32 {
    let depth_index = depth.clamp(1, MAX_SEARCH_PLY as i32) as usize;
    let move_index = move_count.clamp(1, MAX_SEARCH_PLY);
    let scale = table[depth_index] * table[move_index];
    let mut reduction = scale + parameters.lmr_base;
    if !improving {
        reduction += scale * parameters.lmr_non_improving_numerator / 512;
    }
    if cut_node {
        reduction += parameters.lmr_cut_node_bonus;
    }
    if tt_pv {
        reduction -= parameters.lmr_tt_pv_reduction;
    }
    reduction -= history_score * parameters.lmr_history_numerator / 4096;
    // A large corrhist residual marks the position as one the static eval keeps
    // getting wrong, so a late move here is searched less shallowly than the table
    // alone would say (reference: `r -= ... |correctionValue| / 26310`).
    reduction -= corrplexity / parameters.lmr_corrplexity_divisor;
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
/// `459/4096` divisor the quiet formula already applies turns those into 0.07 and 0.67
/// plies of protection respectively. That is the intended shape: a queen is worth more
/// than a ply back once its own capture history agrees, a pawn is worth almost nothing.
const CAPTURE_STAT_MATERIAL_WEIGHT: i32 = 873;

/// The `statScore` a capture presents to the shared reduction formula.
///
/// Captured material plus capture history, in place of the butterfly-and-continuation
/// sum a quiet presents. Both are consumed by exactly the same `-statScore * 459 / 4096`
/// term, so the two move kinds share one reduction shape and differ only in the evidence
/// they feed it.
#[inline]
fn capture_stat_score(
    parameters: &SearchParameters,
    captured_material: i32,
    capture_history: i32,
) -> i32 {
    parameters.capture_stat_material_weight * captured_material / 128 + capture_history
}

/// LMR for a late capture: the quiet formula, fed a capture `statScore`.
#[inline]
#[allow(clippy::too_many_arguments)]
fn capture_late_move_reduction(
    table: &[i32; LMR_TABLE_SIZE],
    parameters: &SearchParameters,
    depth: i32,
    move_count: usize,
    improving: bool,
    cut_node: bool,
    tt_pv: bool,
    captured_material: i32,
    capture_history: i32,
    corrplexity: i32,
) -> i32 {
    late_move_reduction(
        table,
        parameters,
        depth,
        move_count,
        improving,
        cut_node,
        tt_pv,
        capture_stat_score(parameters, captured_material, capture_history),
        corrplexity,
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

/// Margin above the incumbent best score that earns the verification a DEEPER search.
///
/// Denominated in this engine's centipawn-scaled NNUE units, which are the same units
/// every other search margin here uses (`RFP_MARGIN_PER_DEPTH = 95`,
/// `FUTILITY_BASE_MARGIN = 125`). The reference's 53 sits in its own internal eval
/// scale; the value is kept because both scales are pinned to "roughly half a pawn per
/// hundred", and because a re-tune is a second variable this feature is not measuring.
const POST_LMR_DEEPER_MARGIN: i32 = 53;

/// Margin below which the verification is searched one ply SHALLOWER.
///
/// A scout that beat alpha by less than this beat it by rounding: it is worth
/// confirming, but not worth the full depth the unreduced move would have cost.
const POST_LMR_SHALLOWER_MARGIN: i32 = 8;

/// Continuation-history bonus applied once a reduced scout beats alpha.
///
/// The reference constant, applied unconditionally at the fail-high rather than
/// conditioned on the verification's outcome. The evidence being recorded is "a move
/// the ordering demoted turned out to beat alpha at all", which is the same size of
/// fact wherever it happens, so it is deliberately NOT the depth-scaled
/// `quiet_history_bonus` the cutoff site uses.
const POST_LMR_CONTINUATION_BONUS: i32 = 1_334;

/// The depth a fail-high verification re-search runs at.
///
/// M3-F2 measured a 25-33% fixed-depth node saving converting to +0.12 plies at equal
/// time, and identified the always-full-depth verification re-search as where the
/// saving went (`experiments/MSN-S2-capture-lmr/results.md` section 5). This lets the
/// verification depth respond to how far the reduced scout actually beat the incumbent:
/// a scout that cleared it comfortably is worth confirming a ply deeper, one that
/// scraped past it is worth a ply less than full.
///
/// Only a scout that WAS reduced can earn the deeper band: deepening an already
/// full-depth scout would grow the tree on evidence that was never discounted in the
/// first place. The shallower band carries no such condition, matching the reference.
#[inline]
fn post_lmr_verification_depth(
    parameters: &SearchParameters,
    child_depth: i32,
    reduced_depth: i32,
    best_score: i32,
    scout_score: i32,
) -> i32 {
    let deeper =
        reduced_depth < child_depth && scout_score > best_score + parameters.post_lmr_deeper_margin;
    let shallower = scout_score < best_score + parameters.post_lmr_shallower_margin;
    (child_depth + i32::from(deeper) - i32::from(shallower)).max(1)
}

#[inline]
fn frontier_futility_margin(parameters: &SearchParameters, effective_depth: i32) -> i32 {
    parameters.futility_base_margin + parameters.futility_margin_per_depth * effective_depth.max(0)
}

#[inline]
fn quiet_see_threshold(parameters: &SearchParameters, effective_depth: i32) -> i32 {
    -parameters.quiet_see_margin_per_depth * effective_depth.max(0).pow(2)
}

#[inline]
fn capture_see_threshold(parameters: &SearchParameters, depth: i32) -> i32 {
    -parameters.capture_see_margin_per_depth * depth.max(0)
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
fn probcut_beta(parameters: &SearchParameters, beta: i32, improving: bool) -> i32 {
    beta + parameters.probcut_base_margin
        - parameters.probcut_improving_margin * i32::from(improving)
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
fn singular_beta(
    parameters: &SearchParameters,
    tt_score: i32,
    depth: i32,
    tt_pv: bool,
    pv_node: bool,
) -> i32 {
    tt_score
        - (parameters.singular_beta_base
            + parameters.singular_beta_tt_pv_bonus * i32::from(tt_pv && !pv_node))
            * depth
            / 63
}

/// The ttMove-history term subtracted from the singular double-extension margin.
///
/// A TT move that keeps being best raises the scalar and SHRINKS the margin, making the
/// double extension harder to earn: a reliably-best TT move needs no extra depth to be
/// trusted. This is the scalar's ONLY consumer, and the read is what
/// `use_tt_move_history` gates; maintenance is unconditional.
#[inline]
fn tt_move_history_margin_adjustment(options: &SearchOptions, history: &TtMoveHistory) -> i32 {
    if !options.use_tt_move_history {
        return 0;
    }
    TT_MOVE_HISTORY_MARGIN_NUMERATOR * history.value() / TT_MOVE_HISTORY_MARGIN_DIVISOR
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn singular_extension(
    parameters: &SearchParameters,
    value: i32,
    singular_beta: i32,
    pv_node: bool,
    tt_capture: bool,
    cut_node: bool,
    tt_score: i32,
    beta: i32,
    double_margin_adjustment: i32,
    corrplexity: i32,
) -> i32 {
    if value < singular_beta {
        // A large corrhist residual shrinks the margin like a reliable ttMove-history
        // scalar does: where the static eval is untrustworthy, extra depth on the
        // singular move is cheap insurance (reference: `- |correctionValue| / 198368`).
        let double_margin = parameters.singular_double_margin
            + parameters.singular_double_margin_pv_bonus * i32::from(pv_node)
            + parameters.singular_double_margin_quiet_bonus * i32::from(!tt_capture)
            - double_margin_adjustment
            - corrplexity / parameters.singular_corrplexity_divisor;
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

/// Per-node cache answering "does this move give check?" (the reference engine's
/// `CheckInfo` pattern).
///
/// [`move_gives_check`] rebuilds occupancy and five attack sets per MOVE; this builds
/// them once per NODE and answers each normal move with two bitboard tests. Castling,
/// en passant, and promotions still take the slow path: each changes the board in a
/// way the cached sets cannot describe (a rook appears mid-line, a captured pawn
/// vanishes from a third square, the arriving piece changes kind).
pub(crate) struct CheckInfo {
    enemy_king: Option<Square>,
    /// Squares from which a piece of each kind checks the enemy king, given the
    /// occupancy at node entry.
    check_squares: [Bitboard; 6],
    /// Pieces (of either colour) that are the sole blocker between one of the moving
    /// side's sliders and the enemy king. Only the mover's own pieces can ever
    /// coincide with a move's origin square, so enemy blockers are harmless here.
    discovered_candidates: Bitboard,
}

impl CheckInfo {
    pub(crate) fn new(position: &Position) -> Self {
        let color = position.side_to_move();
        let Some(enemy_king) = position.pieces(!color, PieceKind::King).first() else {
            return Self {
                enemy_king: None,
                check_squares: [Bitboard::EMPTY; 6],
                discovered_candidates: Bitboard::EMPTY,
            };
        };
        let occupancy = position.occupancy();
        let bishop_rays = bishop_attacks(enemy_king, occupancy);
        let rook_rays = rook_attacks(enemy_king, occupancy);
        let mut check_squares = [Bitboard::EMPTY; 6];
        check_squares[PieceKind::Pawn.index()] = pawn_attacks(enemy_king, !color);
        check_squares[PieceKind::Knight.index()] = knight_attacks(enemy_king);
        check_squares[PieceKind::Bishop.index()] = bishop_rays;
        check_squares[PieceKind::Rook.index()] = rook_rays;
        check_squares[PieceKind::Queen.index()] = bishop_rays | rook_rays;
        check_squares[PieceKind::King.index()] = king_attacks(enemy_king);

        let straight =
            position.pieces(color, PieceKind::Rook) | position.pieces(color, PieceKind::Queen);
        let diagonal =
            position.pieces(color, PieceKind::Bishop) | position.pieces(color, PieceKind::Queen);
        let snipers = (rook_attacks(enemy_king, Bitboard::EMPTY) & straight)
            | (bishop_attacks(enemy_king, Bitboard::EMPTY) & diagonal);
        let mut discovered_candidates = Bitboard::EMPTY;
        for sniper in snipers {
            let blockers = between_squares(sniper, enemy_king) & occupancy;
            if blockers.count() == 1 {
                discovered_candidates |= blockers;
            }
        }
        Self {
            enemy_king: Some(enemy_king),
            check_squares,
            discovered_candidates,
        }
    }

    /// Answers exactly as [`move_gives_check`] would for any move of the position this
    /// cache was built from; the parity test below pins that over every pseudo-legal
    /// move of a random-walk corpus.
    ///
    /// A direct check reads the cached attack set of the moving kind. A discovered
    /// check happens when the origin square is a sole blocker and the move leaves the
    /// blocked line; a move ALONG the line keeps blocking it (a slider cannot cross
    /// the endpoints, a knight is never collinear with its origin), and any check the
    /// arriving piece then gives is the direct test's to answer.
    #[inline]
    pub(crate) fn gives_check(&self, position: &Position, mv: Move) -> bool {
        let flag = mv.flag();
        if flag.is_castling() || flag.is_en_passant() || flag.promotion().is_some() {
            return move_gives_check(position, mv);
        }
        let Some(enemy_king) = self.enemy_king else {
            return false;
        };
        let moved = position
            .piece_at(mv.from())
            .expect("candidate move must have a moving piece");
        self.check_squares[moved.kind().index()].contains(mv.to())
            || (self.discovered_candidates.contains(mv.from())
                && !collinear(mv.from(), mv.to(), enemy_king))
    }
}

/// Squares strictly between two squares known to share a rank, file, or diagonal.
fn between_squares(a: Square, b: Square) -> Bitboard {
    if a.file() == b.file() || a.rank() == b.rank() {
        rook_attacks(a, b.bitboard()) & rook_attacks(b, a.bitboard())
    } else {
        bishop_attacks(a, b.bitboard()) & bishop_attacks(b, a.bitboard())
    }
}

/// Whether three squares lie on one line. Callers pass two squares already known to
/// share a chess line (rank, file, or diagonal), so plain collinearity is exact: every
/// lattice point on a line of slope 0, infinity, or +/-1 is a square of that line.
#[inline]
fn collinear(a: Square, b: Square, c: Square) -> bool {
    let (af, ar) = (i32::from(a.file()), i32::from(a.rank()));
    let (bf, br) = (i32::from(b.file()), i32::from(b.rank()));
    let (cf, cr) = (i32::from(c.file()), i32::from(c.rank()));
    (bf - af) * (cr - ar) == (br - ar) * (cf - af)
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
        // Low-ply history follows the butterfly bonus/malus stream at plies below 5;
        // `update` is a no-op deeper down.
        context.low_ply_history.update(ply, cutoff_move, bonus);
        let planes = context.continuation_planes(ply);
        if let Some(piece) = position.piece_at(cutoff_move.from()) {
            history.update_pawn(pawn_key, piece, cutoff_move.to(), bonus);
            update_continuation_histories(history, &planes, piece, cutoff_move.to(), bonus);
        }
        for &previous in searched_quiets {
            history.update_butterfly(mover, previous, malus);
            context.low_ply_history.update(ply, previous, malus);
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

#[cfg(feature = "corrhist-regression")]
fn correction_features(
    position: &Position,
    context: &SearchContext<'_>,
    ply: usize,
) -> CorrectionFeatures {
    let color = position.side_to_move();
    let source = |index| {
        context
            .history
            .correction_score(index, correction_key(position, index), color) as i16
    };
    let entry = context.previous_continuation_key(ply);
    let planes = context.correction_continuation_planes(ply);
    let continuation_at = |slot| match (planes[slot], entry) {
        (Some(plane), Some(entry)) => context
            .history
            .correction_continuation_score(slot, plane, entry)
            as i16,
        _ => 0,
    };
    CorrectionFeatures {
        pawn: source(CORRECTION_PAWN),
        minor: source(CORRECTION_MINOR),
        major: source(CORRECTION_MAJOR),
        material: source(CORRECTION_MATERIAL),
        continuation_2: continuation_at(0),
        continuation_4: continuation_at(1),
    }
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
fn null_move_reduction(
    parameters: &SearchParameters,
    depth: i32,
    static_eval: i32,
    beta: i32,
) -> i32 {
    parameters.nmp_reduction_base
        + depth / parameters.nmp_reduction_depth_divisor
        + ((static_eval - beta).max(0) / parameters.nmp_eval_reduction_divisor)
            .min(parameters.nmp_eval_reduction_max)
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

#[derive(Default)]
struct RootTimeStatistics {
    effort: Vec<(Move, u64)>,
    total_effort: u64,
    current_best_move: Option<Move>,
    best_move_changes: u32,
}

impl RootTimeStatistics {
    fn begin_iteration(&mut self, interpolated: bool) {
        if interpolated {
            self.effort.clear();
            self.total_effort = 0;
            self.current_best_move = None;
            self.best_move_changes = 0;
        }
    }

    fn begin_root_search(&mut self, interpolated: bool) {
        if !interpolated {
            self.effort.clear();
            self.total_effort = 0;
        }
    }

    fn record_effort(&mut self, mv: Move, nodes: u64) {
        self.total_effort = self.total_effort.saturating_add(nodes);
        if let Some(entry) = self.effort.iter_mut().find(|(move_, _)| *move_ == mv) {
            entry.1 = entry.1.saturating_add(nodes);
        } else {
            self.effort.push((mv, nodes));
        }
    }

    fn record_best_move(&mut self, mv: Move, line_one: bool, interpolated: bool) {
        if !interpolated || !line_one {
            return;
        }
        if self.current_best_move.is_some_and(|current| current != mv) {
            self.best_move_changes = self.best_move_changes.saturating_add(1);
        }
        self.current_best_move = Some(mv);
    }

    fn nodes_effort(&self, best_move: Option<Move>) -> u32 {
        if self.total_effort == 0 {
            return 0;
        }
        let Some(best_move) = best_move else {
            return 0;
        };
        let best_nodes = self
            .effort
            .iter()
            .find(|(move_, _)| *move_ == best_move)
            .map_or(0, |(_, nodes)| *nodes);
        ((u128::from(best_nodes.min(self.total_effort)) * 100_000) / u128::from(self.total_effort))
            as u32
    }
}

struct SearchContext<'a> {
    transposition_table: &'a TranspositionTable,
    stop: &'a AtomicBool,
    limits: SearchLimits,
    options: SearchOptions,
    /// The log-log LMR table, built from `options.parameters.lmr_coefficient` at
    /// construction. Cached per search rather than recomputed per node, and per search
    /// rather than per process because the coefficient is tunable.
    lmr_table: [i32; LMR_TABLE_SIZE],
    started: Option<Instant>,
    worker_id: usize,
    node_counters: &'a [NodeCounter],
    nodes: u64,
    seldepth: u32,
    iterations: Vec<IterationInfo>,
    history: &'a SharedHistory,
    evaluator: SearchEvaluator<'a>,
    killers: KillerTable,
    /// Per-worker low-ply ordering history, owned by the worker loop and refilled with
    /// its prior between root iterations. Borrowed rather than owned so `ordering` can
    /// hand the move picker a reference that does not borrow the whole context.
    low_ply_history: &'a LowPlyHistory,
    /// Per-worker "is my TT move usually best right now?" scalar. Fresh per search,
    /// like `killers`.
    tt_move_history: TtMoveHistory,
    static_evals: [Option<i32>; MAX_SEARCH_PLY],
    repetition_history: RepetitionHistory,
    current_moves: [Option<Move>; MAX_SEARCH_PLY],
    /// The continuation plane each searched ply's move selects, resolved at push time
    /// because the moving piece is no longer on `from` once the move is made. `None`
    /// marks a null move, which BREAKS the chain: "the reply to the opponent's last
    /// move" is meaningless when the opponent did not move.
    continuation_keys: [Option<ContinuationKey>; MAX_SEARCH_PLY],
    nmp_min_ply: usize,
    /// Syzygy tablebases, absent when the caller loaded none.
    tablebases: Option<&'a dyn TablebaseProbe>,
    /// Published tbhits slots parallel to `node_counters`, absent without tablebases.
    tb_hit_counters: Option<&'a [NodeCounter]>,
    /// This worker's successful tablebase probes.
    tb_hits: u64,
    /// Root moves the DTZ probe allows; when set, `root_search` skips every other move.
    root_move_filter: Option<Vec<Move>>,
    /// The `go ponder` latch, absent outside a ponder search.
    ponder: Option<&'a PonderState>,
    stopped: bool,
    soft_time_reached: bool,
    /// Percentage the soft limit is scaled by, updated between iterations.
    time_scale_percent: u32,
    root_time_statistics: RootTimeStatistics,
    generation: u8,
    /// Sink for `currmove` progress, set only for the worker that owns the clock.
    ///
    /// Boxed rather than generic because `SearchContext` is threaded through every node
    /// of the search; adding a type parameter for a callback used once per root move
    /// would infect the whole recursion for no benefit.
    root_move_reporter: Option<Box<dyn FnMut(RootMoveInfo) + 'a>>,
    #[cfg(feature = "corrhist-regression")]
    correction_sample_reporter: Option<Box<dyn FnMut(CorrectionSample) + 'a>>,
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
        low_ply_history: &'a LowPlyHistory,
        options: SearchOptions,
        worker_id: usize,
        generation: u8,
        node_counters: &'a [NodeCounter],
        network: &'a Network,
    ) -> Self {
        Self {
            transposition_table,
            stop,
            limits,
            options,
            lmr_table: build_lmr_table(&options.parameters),
            started,
            worker_id,
            node_counters,
            nodes: 0,
            seldepth: 0,
            iterations: Vec::new(),
            history,
            evaluator: SearchEvaluator::new(network, position),
            killers: KillerTable::new(),
            low_ply_history,
            tt_move_history: TtMoveHistory::new(),
            static_evals: [None; MAX_SEARCH_PLY],
            repetition_history: RepetitionHistory::new(position, position_history),
            current_moves: [None; MAX_SEARCH_PLY],
            continuation_keys: [None; MAX_SEARCH_PLY],
            nmp_min_ply: 0,
            tablebases: None,
            tb_hit_counters: None,
            tb_hits: 0,
            root_move_filter: None,
            ponder: None,
            stopped: false,
            soft_time_reached: false,
            time_scale_percent: 100,
            root_time_statistics: RootTimeStatistics::default(),
            generation,
            root_move_reporter: None,
            #[cfg(feature = "corrhist-regression")]
            correction_sample_reporter: None,
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
            low_ply_history: self.low_ply_history,
            ply,
            pawn_key: position.zobrist().pawn(),
            continuation: self.continuation_planes(ply),
            use_butterfly_history: self.options.use_butterfly_history,
            use_capture_history: self.options.use_capture_history,
            use_pawn_history: self.options.use_pawn_history,
            use_continuation_history: self.options.use_continuation_history,
            use_low_ply_history: self.options.use_low_ply_history,
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
        if self.worker_id == 0
            && self.nodes.is_multiple_of(TIME_CHECK_INTERVAL)
            // A pondering search must not answer, so its time budget is not in force:
            // the clock only starts when `ponderhit` re-bases it.
            && !self.is_pondering()
        {
            let elapsed = self.elapsed();
            self.recompute_soft_time_reached();
            if self.limits.hard_time.is_some_and(|limit| elapsed >= limit) {
                self.stopped = true;
                return false;
            }
        }
        true
    }

    /// Whether the attached `go ponder` latch, if any, is still armed.
    fn is_pondering(&self) -> bool {
        self.ponder.is_some_and(PonderState::is_pondering)
    }

    fn should_stop_after_iteration(&self) -> bool {
        self.stopped
            || self.limits.nodes.is_some_and(|limit| self.nodes >= limit)
            || (self.soft_time_reached && !self.is_pondering())
    }

    /// Sets the percentage the soft limit is scaled by before the next iteration.
    fn set_time_scale(&mut self, percent: u32) {
        self.time_scale_percent = percent;
    }

    fn uses_interpolated_time_management(&self) -> bool {
        interpolated_time_management_active(&self.limits, &self.options)
    }

    fn begin_root_iteration(&mut self) {
        let interpolated = self.uses_interpolated_time_management();
        self.root_time_statistics.begin_iteration(interpolated);
    }

    /// Discards the previous root search's per-move node distribution.
    fn begin_root_effort(&mut self) {
        let interpolated = self.uses_interpolated_time_management();
        self.root_time_statistics.begin_root_search(interpolated);
    }

    /// Credits `nodes` to `mv`'s subtree in the current root search.
    fn record_root_effort(&mut self, mv: Move, nodes: u64) {
        self.root_time_statistics.record_effort(mv, nodes);
    }

    fn record_root_best_move(&mut self, mv: Move, line_one: bool) {
        let interpolated = self.uses_interpolated_time_management();
        self.root_time_statistics
            .record_best_move(mv, line_one, interpolated);
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
            .root_time_statistics
            .effort
            .iter()
            .find(|(move_, _)| *move_ == best_move)
            .map_or(0, |(_, nodes)| *nodes);
        time_effort_percent(best_nodes, self.root_time_statistics.total_effort)
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

    fn recompute_soft_time_reached(&mut self) {
        if self.is_pondering() {
            self.soft_time_reached = false;
            return;
        }
        self.soft_time_reached = self
            .scaled_soft_time()
            .is_some_and(|limit| self.elapsed() >= limit);
    }

    fn publish_nodes(&self) {
        self.node_counters[self.worker_id].store(self.nodes, Ordering::Relaxed);
        if let Some(counters) = self.tb_hit_counters {
            counters[self.worker_id].store(self.tb_hits, Ordering::Relaxed);
        }
    }

    fn reported_nodes(&self) -> u64 {
        if self.worker_id == 0 {
            published_node_total(self.node_counters)
        } else {
            self.nodes
        }
    }

    fn reported_tb_hits(&self) -> u64 {
        match self.tb_hit_counters {
            Some(counters) if self.worker_id == 0 => published_node_total(counters),
            _ => self.tb_hits,
        }
    }

    fn elapsed(&self) -> Duration {
        // A ponder search's time budget counts from `ponderhit`, not from spawn: the
        // engine was thinking on the opponent's time until the predicted move was
        // actually played. Until the latch flips (and when there is no latch) the spawn
        // instant stands.
        let started = self
            .ponder
            .and_then(PonderState::rebased_start)
            .or(self.started);
        started.map_or(Duration::ZERO, |started| started.elapsed())
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
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicU32;
    use std::thread;

    use super::*;

    /// The shipped parameter values, which every formula test below is pinned against.
    fn shipped() -> SearchParameters {
        SearchParameters::default()
    }

    fn shipped_lmr_table() -> [i32; LMR_TABLE_SIZE] {
        build_lmr_table(&shipped())
    }

    #[test]
    fn search_again_counter_repeats_depth_before_advancing() {
        let effective_depths = (5..=12)
            .zip(1..=8)
            .map(|(nominal, counter)| effective_search_depth(nominal, counter, true))
            .collect::<Vec<_>>();

        assert_eq!(effective_depths, [4, 4, 4, 5, 5, 5, 5, 6]);
    }

    #[test]
    fn effective_depth_never_falls_below_one() {
        assert_eq!(effective_search_depth(1, u32::MAX, true), 1);
        assert_eq!(effective_search_depth(2, u32::MAX, true), 1);
    }

    #[test]
    fn disabled_search_again_depth_returns_the_nominal_depth() {
        assert_eq!(effective_search_depth(17, u32::MAX, false), 17);
    }

    #[test]
    fn false_decision_increments_before_the_next_effective_depth_is_computed() {
        assert_eq!(search_again_iteration(8, 0, true, false), (1, 7));
    }

    #[test]
    fn search_again_depth_activation_is_strictly_timed_single_pv() {
        let options = SearchOptions {
            use_search_again_depth: true,
            ..SearchOptions::default()
        };
        let timed = SearchLimits {
            soft_time: Some(Duration::from_millis(100)),
            hard_time: Some(Duration::from_millis(400)),
            use_clock_management: true,
            ..SearchLimits::default()
        };

        assert!(search_again_depth_active(&timed, &options));
        assert!(!search_again_depth_active(
            &SearchLimits {
                use_clock_management: false,
                ..timed
            },
            &options
        ));
        assert!(!search_again_depth_active(
            &timed,
            &SearchOptions {
                multi_pv: 2,
                ..options
            }
        ));
        assert!(!search_again_depth_active(
            &timed,
            &SearchOptions {
                use_search_again_depth: false,
                ..options
            }
        ));
    }

    #[test]
    fn ponder_searches_always_allow_depth_growth() {
        assert!(should_increase_search_depth(
            true,
            Duration::from_secs(10),
            Some(Duration::from_millis(100))
        ));
    }

    #[test]
    fn ponder_wait_wakes_on_ponderhit_and_records_one_clock_base() {
        let ponder = PonderState::new();
        thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let ponder = &ponder;
            scope.spawn(move || {
                ponder.wait_until_released();
                tx.send(()).expect("wake should be observable");
            });
            assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
            ponder.ponderhit();
            assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
        });
        let first = ponder.rebased_start().expect("ponderhit records a base");
        ponder.ponderhit();
        assert_eq!(ponder.rebased_start(), Some(first));
    }

    #[test]
    fn ponder_wait_wakes_on_abort_without_recording_a_clock_base() {
        let ponder = PonderState::new();
        thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let ponder = &ponder;
            scope.spawn(move || {
                ponder.wait_until_released();
                tx.send(()).expect("wake should be observable");
            });
            assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
            ponder.abort();
            assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
        });
        assert_eq!(ponder.rebased_start(), None);
    }

    #[test]
    fn ponder_hit_from_the_ceiling_callback_runs_one_quiet_rebased_iteration() {
        let Some(network) = local_network() else {
            return;
        };
        let position =
            Position::from_fen("7k/8/6QK/8/8/8/8/8 w - - 0 1", false).expect("valid test FEN");
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let ponder = PonderState::new();
        let ceiling_iterations = Cell::new(0);
        let post_hit_currmoves = Cell::new(0);
        let parameters = WorkerParameters::new(0, 0, &counters, &shared_history, network)
            .with_ponder(&ponder)
            .with_root_move_reporter(|root_move| {
                if !ponder.is_pondering() && root_move.depth == MAX_ITERATIVE_DEEPENING_DEPTH {
                    post_hit_currmoves.set(post_hit_currmoves.get() + 1);
                }
            });

        let result = search_worker_with_history_callback_options(
            &position,
            &history,
            &table,
            SearchLimits {
                soft_time: Some(Duration::from_millis(100)),
                hard_time: Some(Duration::from_millis(400)),
                use_clock_management: true,
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            parameters,
            |iteration| {
                if iteration.depth == MAX_ITERATIVE_DEEPENING_DEPTH {
                    ceiling_iterations.set(ceiling_iterations.get() + 1);
                    if ponder.is_pondering() {
                        ponder.ponderhit();
                    }
                }
            },
        );

        assert_eq!(
            ceiling_iterations.get(),
            2,
            "the pre-hit ceiling callback must be followed by one rebased callback"
        );
        assert_eq!(
            post_hit_currmoves.get(),
            0,
            "repeated ceiling searches must not emit currmove progress"
        );
        assert!(
            result.elapsed >= Duration::from_millis(40),
            "the callback-seam ponderhit must spend the rebased budget, got {:?}",
            result.elapsed
        );
    }

    #[test]
    fn clocked_ponder_without_a_primary_limit_uses_the_analysis_ceiling() {
        let Some(network) = local_network() else {
            return;
        };
        let position =
            Position::from_fen("7k/8/6QK/8/8/8/8/8 w - - 0 1", false).expect("valid test FEN");
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let ponder = PonderState::new();
        let deepest = AtomicU32::new(0);
        let parameters =
            WorkerParameters::new(0, 0, &counters, &shared_history, network).with_ponder(&ponder);

        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_secs(2));
                ponder.abort();
            });
            let result = search_worker_with_history_callback_options(
                &position,
                &history,
                &table,
                SearchLimits {
                    soft_time: Some(Duration::from_millis(100)),
                    hard_time: Some(Duration::from_millis(400)),
                    use_clock_management: true,
                    ..SearchLimits::default()
                },
                SearchOptions::default(),
                &stop,
                parameters,
                |iteration| {
                    deepest.fetch_max(iteration.depth, Ordering::Relaxed);
                },
            );

            assert_eq!(deepest.load(Ordering::Relaxed), MAX_SEARCH_PLY as u32);
            assert_eq!(result.depth, MAX_SEARCH_PLY as u32);
        });
    }

    #[test]
    fn late_timed_iteration_repeats_effective_depth_next() {
        assert!(!should_increase_search_depth(
            false,
            Duration::from_millis(51),
            Some(Duration::from_millis(100))
        ));
        assert!(should_increase_search_depth(
            false,
            Duration::from_millis(50),
            Some(Duration::from_millis(100))
        ));
    }

    #[test]
    fn interpolated_scale_precedes_the_search_again_depth_decision() {
        let nominal_soft_time = Duration::from_millis(100);
        let scale = interpolated_time_scale_percent(0, 0, 0, 0, 0, 1, 75_800);
        let scaled_soft_time = nominal_soft_time.saturating_mul(scale) / 100;
        let elapsed = Duration::from_millis(45);

        assert_eq!(scale, 83);
        assert!(should_increase_search_depth(
            false,
            elapsed,
            Some(nominal_soft_time)
        ));
        assert!(!should_increase_search_depth(
            false,
            elapsed,
            Some(scaled_soft_time)
        ));

        let first = search_again_iteration(7, 0, true, true);
        let second = search_again_iteration(8, first.0, true, false);
        assert_eq!((first.1, second.1), (7, 7));
    }

    fn local_network() -> Option<&'static mf_nnue::Network> {
        static NETWORK: OnceLock<Option<mf_nnue::Network>> = OnceLock::new();
        NETWORK
            .get_or_init(|| {
                let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
                let path = explicit_path.clone().map_or_else(
                    || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
                    PathBuf::from,
                );
                if !path.is_file() {
                    assert!(
                        explicit_path.is_none(),
                        "MF_NNUE_TEST_NET requires an existing network file: {}",
                        path.display()
                    );
                    eprintln!("SKIPPED: NNUE search tests are missing {}", path.display());
                    return None;
                }
                Some(mf_nnue::Network::load(&path).unwrap_or_else(|error| {
                    panic!("test NNUE network {}: {error}", path.display())
                }))
            })
            .as_ref()
    }

    fn tablebase_test_network() -> &'static mf_nnue::Network {
        local_network().expect("tablebase search tests require copied nets/main.nnue")
    }

    struct FakeTablebase {
        root_moves: Option<(u64, Vec<Move>)>,
        wdl: Option<(u64, Wdl)>,
    }

    impl TablebaseProbe for FakeTablebase {
        fn max_pieces(&self) -> usize {
            32
        }

        fn probe_wdl(&self, position: &Position) -> Option<Wdl> {
            self.wdl
                .filter(|(key, _)| *key == position.repetition_key())
                .map(|(_, wdl)| wdl)
        }

        fn preserving_root_moves(&self, position: &Position) -> Option<Vec<Move>> {
            self.root_moves
                .as_ref()
                .filter(|(key, _)| *key == position.repetition_key())
                .map(|(_, moves)| moves.clone())
        }
    }

    struct EveryPositionTablebase(Wdl);

    impl TablebaseProbe for EveryPositionTablebase {
        fn max_pieces(&self) -> usize {
            32
        }

        fn probe_wdl(&self, _position: &Position) -> Option<Wdl> {
            Some(self.0)
        }

        fn preserving_root_moves(&self, _position: &Position) -> Option<Vec<Move>> {
            None
        }
    }

    struct DirectTablebaseResult {
        score: i32,
        entry: EntryData,
        tb_hits: u64,
    }

    fn direct_tablebase_pvs(
        fen: &str,
        wdl: Wdl,
        depth: i32,
        alpha: i32,
        beta: i32,
        pv_node: bool,
    ) -> DirectTablebaseResult {
        let network = tablebase_test_network();
        let mut position = Position::from_fen(fen, false).expect("test FEN should parse");
        let probe = FakeTablebase {
            root_moves: None,
            wdl: Some((position.repetition_key(), wdl)),
        };
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );
        context.tablebases = Some(&probe);
        let mut pv = PvLine::new();
        let score = pvs(
            &mut position,
            depth,
            alpha,
            beta,
            1,
            pv_node,
            !pv_node,
            true,
            false,
            None,
            &mut context,
            &mut pv,
        )
        .expect("unbounded test search should complete");
        let entry = table
            .probe(position.repetition_key())
            .expect("tablebase cutoff should write a TT entry");
        DirectTablebaseResult {
            score,
            entry,
            tb_hits: context.tb_hits,
        }
    }

    fn fixed_depth_tablebase_search(
        position: &Position,
        tablebases: Option<&dyn TablebaseProbe>,
    ) -> SearchResult {
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let tb_hit_counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let mut worker =
            WorkerParameters::new(0, 0, &counters, &shared_history, tablebase_test_network());
        if let Some(tablebases) = tablebases {
            worker = worker.with_tablebase_probe(tablebases, &tb_hit_counters);
        }
        search_worker_with_history_callback_options(
            position,
            &history,
            &table,
            SearchLimits {
                depth: Some(2),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            &stop,
            worker,
            |_| {},
        )
    }

    #[test]
    fn tablebase_root_probe_restricts_the_searched_moves() {
        let position =
            Position::from_fen("8/8/8/8/8/2k5/8/KQ6 w - - 0 1", false).expect("valid test FEN");
        let unrestricted = fixed_depth_tablebase_search(&position, None)
            .best_move
            .expect("unrestricted search should select a move");
        let legal_moves = generate_legal_moves(&position);
        let allowed = legal_moves
            .iter()
            .copied()
            .find(|mv| *mv != unrestricted)
            .expect("test position should have an alternative legal move");
        let probe = FakeTablebase {
            root_moves: Some((position.repetition_key(), vec![allowed])),
            wdl: None,
        };
        let restricted = fixed_depth_tablebase_search(&position, Some(&probe));

        assert_ne!(Some(unrestricted), restricted.best_move);
        assert_eq!(restricted.best_move, Some(allowed));
    }

    #[test]
    fn tablebase_wdl_cutoff_records_hit_and_six_ply_tt_depth_bonus() {
        let probe =
            direct_tablebase_pvs("8/8/8/8/8/2k5/8/KQ6 w - - 0 1", Wdl::Win, 4, -1, 0, false);

        assert_eq!(probe.score, TABLEBASE_SCORE - 1);
        assert_eq!(probe.entry.bound, Bound::Lower);
        assert_eq!(tt_entry_depth(probe.entry.depth), 4 + SYZYGY_TT_DEPTH_BONUS);
        assert_eq!(probe.tb_hits, 1);
    }

    #[test]
    fn tablebase_draw_cutoff_is_exact_with_six_ply_tt_depth_bonus() {
        let probe =
            direct_tablebase_pvs("8/8/8/8/8/2k5/8/KQ6 w - - 0 1", Wdl::Draw, 4, -1, 1, false);

        assert_eq!(probe.score, 0);
        assert_eq!(probe.entry.bound, Bound::Exact);
        assert_eq!(tt_entry_depth(probe.entry.depth), 4 + SYZYGY_TT_DEPTH_BONUS);
        assert_eq!(probe.tb_hits, 1);
    }

    #[test]
    fn tablebase_loss_upper_cutoff_has_six_ply_tt_depth_bonus() {
        let probe =
            direct_tablebase_pvs("8/8/8/8/8/2k5/8/K2Q4 b - - 0 1", Wdl::Loss, 4, 0, 1, false);

        assert_eq!(probe.score, -(TABLEBASE_SCORE - 1));
        assert_eq!(probe.entry.bound, Bound::Upper);
        assert_eq!(tt_entry_depth(probe.entry.depth), 4 + SYZYGY_TT_DEPTH_BONUS);
        assert_eq!(probe.tb_hits, 1);
    }

    #[test]
    fn fixed_depth_search_publishes_interior_tablebase_hits() {
        let position = Position::startpos();
        let result =
            fixed_depth_tablebase_search(&position, Some(&EveryPositionTablebase(Wdl::Draw)));

        assert!(result.tbhits > 0);
        assert!(!result.iterations.is_empty());
        assert_eq!(
            result.iterations.last().map(|iteration| iteration.tbhits),
            Some(result.tbhits)
        );
    }

    /// Black to move is in check from the rook on b1; the mate in one (`Qxb1#`) is the
    /// mate corpus's in-check case.
    const IN_CHECK_FEN: &str = "8/8/8/1q6/8/1k6/8/KR6 b - - 0 1";

    /// The pinned values themselves: a skipped checked node carries the sentinel, no
    /// correction, a zero complexity proxy, and no improving evidence.
    #[test]
    fn node_eval_pins_the_bundle_at_a_skipped_checked_node() {
        let Some(network) = local_network() else {
            return;
        };
        let run = |use_checked_node_eval: bool| {
            let position = Position::from_fen(IN_CHECK_FEN, false)
                .expect("in-check FEN should parse");
            assert!(is_in_check(&position, position.side_to_move()));
            let table = TranspositionTable::new(1).expect("test TT should allocate");
            let stop = AtomicBool::new(false);
            let counters = [NodeCounter::new(0)];
            let history = [position.repetition_key()];
            let shared_history = SharedHistory::new();
            let low_ply_history = LowPlyHistory::new();
            let mut context = SearchContext::new(
                &table,
                SearchLimits::default(),
                None,
                &stop,
                &position,
                &history,
                &shared_history,
                &low_ply_history,
                SearchOptions {
                    use_checked_node_eval,
                    ..SearchOptions::default()
                },
                0,
                0,
                &counters,
                network,
            );
            let eval = node_eval(&position, None, true, 1, &mut context);
            (eval, context.static_evals[1])
        };

        let (skipped, history_slot) = run(false);
        assert_eq!(
            skipped.raw_static_eval,
            i32::from(UNEVALUATED_STATIC_EVAL),
            "a skipped checked node carries the sentinel, matching qsearch"
        );
        assert_eq!(skipped.corrplexity, 0, "no eval means a zero proxy");
        assert!(!skipped.improving, "no eval means no improving evidence");
        assert_eq!(
            history_slot, None,
            "a skipped checked node must leave the eval history unpopulated"
        );

        let (evaluated, history_slot) = run(true);
        assert_ne!(
            evaluated.raw_static_eval,
            i32::from(UNEVALUATED_STATIC_EVAL),
            "the default arm evaluates checked nodes"
        );
        assert!(
            history_slot.is_none(),
            "static_evals stays unpopulated in check either way"
        );
    }

    /// At the `pvs` boundary: with the toggle off an in-check node stores the sentinel
    /// in the TT, so later probes can never reuse a static eval that was not computed.
    #[test]
    fn skipped_checked_node_stores_the_sentinel_in_the_tt() {
        let Some(network) = local_network() else {
            return;
        };
        let mut position =
            Position::from_fen(IN_CHECK_FEN, false).expect("in-check FEN should parse");
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
            SearchOptions {
                use_checked_node_eval: false,
                ..SearchOptions::default()
            },
            0,
            0,
            &counters,
            network,
        );
        let mut pv = PvLine::new();
        pvs(
            &mut position,
            4,
            -INFINITY,
            INFINITY,
            1,
            true,
            false,
            true,
            false,
            None,
            &mut context,
            &mut pv,
        )
        .expect("unbounded test search should complete");

        assert!(
            context.static_evals[1].is_none(),
            "a skipped checked node must leave the eval history unpopulated"
        );
        let entry = table
            .probe(position.repetition_key())
            .expect("pvs should store a TT entry");
        assert_eq!(
            entry.static_eval, UNEVALUATED_STATIC_EVAL,
            "a skipped checked node must store the sentinel, matching qsearch"
        );
    }

    #[test]
    fn noncutting_tablebase_win_sets_a_search_floor() {
        let probe = direct_tablebase_pvs(
            "8/8/8/8/8/2k5/8/KQ6 w - - 0 1",
            Wdl::Win,
            4,
            -INFINITY,
            INFINITY,
            true,
        );

        assert!(probe.score >= TABLEBASE_SCORE - 1);
        assert_eq!(probe.tb_hits, 1);
    }

    #[test]
    fn noncutting_tablebase_loss_sets_a_search_ceiling() {
        let probe = direct_tablebase_pvs(
            "8/8/8/8/8/2k5/8/K2Q4 b - - 0 1",
            Wdl::Loss,
            4,
            -INFINITY,
            INFINITY,
            true,
        );

        assert!(probe.score <= -(TABLEBASE_SCORE - 1));
        assert_eq!(probe.tb_hits, 1);
    }

    #[test]
    fn multipv_exclusion_filter_removes_found_moves_from_the_existing_allowed_set() {
        let legal_moves = generate_legal_moves(&Position::startpos());
        let base_allowed = vec![legal_moves[1], legal_moves[3], legal_moves[5]];
        let found = [legal_moves[1], legal_moves[5]];

        assert_eq!(
            multipv_allowed_moves(&legal_moves, Some(&base_allowed), &found),
            vec![legal_moves[3]]
        );
        assert_eq!(
            multipv_allowed_moves(&legal_moves, None, &found),
            legal_moves
                .iter()
                .copied()
                .filter(|mv| !found.contains(mv))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn interrupted_secondary_pass_keeps_the_current_depth_primary_result() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let expected_table = TranspositionTable::new(1).expect("test TT should allocate");
        let expected = search(
            &position,
            &expected_table,
            SearchLimits {
                depth: Some(1),
                ..SearchLimits::default()
            },
            SearchOptions::default(),
            network,
        );
        assert!(
            expected.nodes < 30,
            "the node limit must leave room to begin a secondary pass"
        );

        let interrupted_table = TranspositionTable::new(1).expect("test TT should allocate");
        let interrupted = search(
            &position,
            &interrupted_table,
            SearchLimits {
                nodes: Some(30),
                ..SearchLimits::default()
            },
            SearchOptions {
                multi_pv: 2,
                ..SearchOptions::default()
            },
            network,
        );

        assert_eq!(interrupted.nodes, 30);
        assert_eq!(interrupted.depth, expected.depth);
        assert_eq!(interrupted.score, expected.score);
        assert_eq!(interrupted.best_move, expected.best_move);
        assert_eq!(interrupted.pv, expected.pv);
        assert_eq!(interrupted.iterations.len(), 1);
        assert_eq!(interrupted.iterations[0].multipv_index, 1);
        assert_eq!(interrupted.iterations[0].pv, expected.pv);
        assert_eq!(
            interrupted_table
                .probe(tt_key(&position, interrupted.depth as i32))
                .expect("interrupted root search should leave a TT entry")
                .best_move,
            expected.best_move
        );
    }

    #[test]
    fn multipv_secondary_passes_leave_the_primary_move_in_the_root_tt() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let result = search(
            &position,
            &table,
            SearchLimits {
                depth: Some(4),
                ..SearchLimits::default()
            },
            SearchOptions {
                multi_pv: 3,
                ..SearchOptions::default()
            },
            network,
        );

        assert_eq!(
            table
                .probe(tt_key(&position, result.depth as i32))
                .expect("root search should leave a TT entry")
                .best_move,
            result.best_move
        );
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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
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
            let counters = [NodeCounter::new(0)];
            let history = [position.repetition_key()];
            let shared_history = SharedHistory::new();
            let low_ply_history = LowPlyHistory::new();
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
                &low_ply_history,
                SearchOptions::default(),
                0,
                0,
                &counters,
                network,
            );
            let root_state = context.evaluator.current().clone();
            context.evaluator.enable_verification();

            let result = root_search(&position, 4, -INFINITY, INFINITY, true, &mut context);

            assert!(result.is_some());
            assert_eq!(context.evaluator.depth(), 0);
            assert_eq!(context.evaluator.current(), &root_state);
        }
    }

    #[test]
    fn worker_zero_preserves_the_original_aspiration_delta() {
        assert_eq!(aspiration_delta(&shipped(), 0, 0), ASPIRATION_INITIAL_DELTA);
    }

    #[test]
    fn helpers_receive_distinct_bounded_aspiration_deltas() {
        let values: Vec<_> = (0..8)
            .map(|worker| aspiration_delta(&shipped(), worker, 0))
            .collect();
        assert_eq!(values[0], ASPIRATION_INITIAL_DELTA);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn aspiration_window_widens_with_the_magnitude_of_the_previous_score() {
        let parameters = shipped();
        // Near equality the window is tight, because that is where the score is most
        // predictable and most of the tree lives.
        assert_eq!(aspiration_delta(&parameters, 0, 0), 8);
        // `100^2 / ASPIRATION_SCORE_DIVISOR` truncates to zero, so a hundredth of a pawn
        // does not widen the window at all.
        assert_eq!(aspiration_delta(&parameters, 0, 100), 8);
        // A decided position swings more, so it starts wider instead of paying for a
        // chain of re-searches.
        assert!(aspiration_delta(&parameters, 0, 800) > aspiration_delta(&parameters, 0, 200));
        // Sign does not matter, only distance from equality.
        assert_eq!(
            aspiration_delta(&parameters, 0, -600),
            aspiration_delta(&parameters, 0, 600)
        );
        // And the width is capped rather than exploding near mate scores.
        assert_eq!(
            aspiration_delta(&parameters, 0, MATE_SCORE),
            ASPIRATION_MAX_DELTA
        );
    }

    #[test]
    fn aggregate_nodes_sum_published_worker_counts() {
        let counters = [NodeCounter::new(10), NodeCounter::new(20)];
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
        assert_eq!(
            iteration_ceiling(&infinite, false),
            MAX_ITERATIVE_DEEPENING_DEPTH
        );

        // An `infinite` request carrying a depth is still infinite: `search_limits`
        // drops the depth, and the ceiling must not resurrect it.
        assert_eq!(
            iteration_ceiling(
                &SearchLimits {
                    depth: Some(4),
                    ..infinite
                },
                true,
            ),
            MAX_ITERATIVE_DEEPENING_DEPTH
        );

        let at = |depth| {
            iteration_ceiling(
                &SearchLimits {
                    depth: Some(depth),
                    ..SearchLimits::default()
                },
                true,
            )
        };
        assert_eq!(at(0), 1, "depth 0 still owes the GUI one iteration");
        assert_eq!(at(12), 12, "a depth within the ceiling is honoured exactly");
        assert_eq!(at(MAX_ITERATIVE_DEEPENING_DEPTH), MAX_SEARCH_PLY as u32);
        assert_eq!(at(200), MAX_ITERATIVE_DEEPENING_DEPTH);
        assert_eq!(at(u32::MAX), MAX_ITERATIVE_DEEPENING_DEPTH);

        // No depth at all keeps the historical default.
        assert_eq!(
            iteration_ceiling(&SearchLimits::default(), false),
            DEFAULT_MAX_DEPTH
        );
        assert_eq!(
            iteration_ceiling(&SearchLimits::default(), true),
            MAX_ITERATIVE_DEEPENING_DEPTH
        );
        assert_eq!(
            iteration_ceiling(
                &SearchLimits {
                    nodes: Some(10_000),
                    ..SearchLimits::default()
                },
                true,
            ),
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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();

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
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();

        for limit in [1_023, 1_024, 1_025] {
            let counters = [NodeCounter::new(0)];
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
                &low_ply_history,
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
        let counters = [NodeCounter::new(1_000), NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();

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
        let counters = [NodeCounter::new(0), NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();

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
        let counters = [NodeCounter::new(0), NodeCounter::new(37)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();

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
        assert_eq!(razoring_margin(&shipped(), 1), 224 + 202);
        assert_eq!(razoring_margin(&shipped(), 3), 224 + 202 * 3);
        assert_eq!(RAZOR_MAX_DEPTH, 3);
    }

    #[test]
    fn reverse_futility_margin_is_linear_in_depth_with_improving_and_tt_pv_adjustments() {
        // Linear, and small enough to actually fire. The previous quadratic form asked
        // for 392cp at depth 1, which is most of a minor piece.
        let parameters = shipped();
        assert_eq!(
            reverse_futility_margin(&parameters, 1, false, false, false, 0),
            95
        );
        assert_eq!(
            reverse_futility_margin(&parameters, 6, false, false, false, 0),
            570
        );
        // A rising eval is worth a ply of margin, but not at an expected cut node.
        assert_eq!(
            reverse_futility_margin(&parameters, 6, true, false, false, 0),
            475
        );
        assert_eq!(
            reverse_futility_margin(&parameters, 6, true, true, false, 0),
            570
        );
        // A TT-PV node pays a surcharge.
        assert_eq!(
            reverse_futility_margin(&parameters, 6, false, false, true, 0),
            570 + 22
        );
    }

    /// The complexity proxy must make the RFP cutoff HARDER and LMR SOFTER, never the
    /// other way round: an unreliable eval is a reason to search more, not less.
    #[test]
    fn a_larger_correction_magnitude_never_shrinks_the_rfp_margin_or_deepens_lmr() {
        let parameters = shipped();
        let table = shipped_lmr_table();
        // CORRECTION_MAX (1_024) saturated across every weighted source bounds the
        // real blend; sweep to well past it so the property covers the whole range.
        let magnitudes = [0, 50_000, 200_000, 400_000, 800_000, 1_600_000];
        for window in magnitudes.windows(2) {
            let (smaller, larger) = (window[0], window[1]);
            assert!(
                reverse_futility_margin(&parameters, 6, false, false, false, larger)
                    >= reverse_futility_margin(&parameters, 6, false, false, false, smaller),
                "a larger |correction| must never shrink the RFP margin"
            );
            assert!(
                late_move_reduction(&table, &parameters, 8, 8, true, false, false, 0, larger)
                    <= late_move_reduction(
                        &table,
                        &parameters,
                        8,
                        8,
                        true,
                        false,
                        false,
                        0,
                        smaller
                    ),
                "a larger |correction| must never increase the LMR reduction"
            );
            assert!(
                capture_late_move_reduction(
                    &table,
                    &parameters,
                    12,
                    12,
                    true,
                    false,
                    false,
                    100,
                    0,
                    larger
                ) <= capture_late_move_reduction(
                    &table,
                    &parameters,
                    12,
                    12,
                    true,
                    false,
                    false,
                    100,
                    0,
                    smaller
                ),
                "a larger |correction| must never increase the capture LMR reduction"
            );
        }
        // And the divisors actually bite: a magnitude past one divisor moves each site.
        assert!(
            reverse_futility_margin(&parameters, 6, false, false, false, 800_000)
                > reverse_futility_margin(&parameters, 6, false, false, false, 0)
        );
        assert!(
            late_move_reduction(&table, &parameters, 8, 8, true, false, false, 0, 800_000)
                < late_move_reduction(&table, &parameters, 8, 8, true, false, false, 0, 0)
        );
    }

    /// A large proxy must make the DOUBLE extension easier to earn (margin shrinks),
    /// and must never turn an extension into a reduction elsewhere in the ladder.
    #[test]
    fn a_larger_correction_magnitude_only_widens_the_singular_double_extension() {
        let parameters = shipped();
        let singular_beta = singular_beta(&parameters, 200, 8, false, false);
        // 10 below singular_beta: inside the shipped 16-point margin (single
        // extension), outside it once the proxy shrinks the margin below 10.
        let value = singular_beta - 10;
        assert_eq!(
            singular_extension(
                &parameters,
                value,
                singular_beta,
                false,
                true,
                false,
                200,
                300,
                0,
                0
            ),
            1
        );
        assert_eq!(
            singular_extension(
                &parameters,
                value,
                singular_beta,
                false,
                true,
                false,
                200,
                300,
                0,
                4_000_000
            ),
            2,
            "a large |correction| must shrink the double margin and earn the second ply"
        );
    }

    #[test]
    fn null_move_reduction_scales_with_depth_and_eval_surplus() {
        let parameters = shipped();
        assert_eq!(null_move_reduction(&parameters, 6, 100, 100), 7);
        assert_eq!(null_move_reduction(&parameters, 6, 700, 100), 10);
        assert_eq!(null_move_reduction(&parameters, 16, 100, 100), 10);
        assert_eq!(null_move_reduction(&parameters, 16, 10_000, 100), 13);
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
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Some(Instant::now()),
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
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
        let parameters = shipped();
        assert_eq!(late_move_pruning_threshold(1, false, &parameters), 5);
        assert_eq!(late_move_pruning_threshold(1, true, &parameters), 10);
        assert_eq!(late_move_pruning_threshold(4, false, &parameters), 12);
        assert_eq!(late_move_pruning_threshold(4, true, &parameters), 25);
        assert!(
            late_move_pruning_threshold(6, false, &parameters)
                > late_move_pruning_threshold(5, false, &parameters)
        );
    }

    #[test]
    fn lmr_base_table_and_adjustments_move_reduction_in_the_expected_direction() {
        let table = shipped_lmr_table();
        let parameters = shipped();
        let reduction = |depth, moves, improving, cut, tt_pv, history| {
            late_move_reduction(
                &table,
                &parameters,
                depth,
                moves,
                improving,
                cut,
                tt_pv,
                history,
                0,
            )
        };
        let baseline = reduction(8, 8, true, false, false, 0);

        assert!(reduction(12, 8, true, false, false, 0) > baseline);
        assert!(reduction(8, 12, true, false, false, 0) > baseline);
        assert!(reduction(8, 8, false, false, false, 0) > baseline);
        assert!(reduction(8, 8, true, true, false, 0) > baseline);
        assert!(reduction(8, 8, true, false, true, 0) < baseline);
        assert!(reduction(8, 8, true, false, false, 4_000) < baseline);
        assert!(reduction(8, 8, true, false, false, -4_000) > baseline);
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

        let table = shipped_lmr_table();
        let parameters = shipped();
        let capture = |victim| {
            capture_late_move_reduction(
                &table,
                &parameters,
                12,
                12,
                true,
                false,
                false,
                victim,
                0,
                0,
            )
        };
        let pawn = capture(material_value(PieceKind::Pawn));
        let knight = capture(material_value(PieceKind::Knight));
        let rook = capture(material_value(PieceKind::Rook));
        let queen = capture(material_value(PieceKind::Queen));
        let quiet = late_move_reduction(&table, &parameters, 12, 12, true, false, false, 0, 0);

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
        let table = shipped_lmr_table();
        let parameters = shipped();
        let reduction = |depth, moves, improving, cut, tt_pv, history| {
            capture_late_move_reduction(
                &table,
                &parameters,
                depth,
                moves,
                improving,
                cut,
                tt_pv,
                pawn,
                history,
                0,
            )
        };
        let baseline = reduction(12, 12, true, false, false, 0);

        assert!(reduction(20, 12, true, false, false, 0) > baseline);
        assert!(reduction(12, 24, true, false, false, 0) > baseline);
        assert!(reduction(12, 12, false, false, false, 0) > baseline);
        assert!(reduction(12, 12, true, true, false, 0) > baseline);
        assert!(reduction(12, 12, true, false, true, 0) < baseline);
        // Capture history saturates at CAPTURE_MAX (10_692), so these are in range.
        assert!(reduction(12, 12, true, false, false, 8_000) < baseline);
        assert!(reduction(12, 12, true, false, false, -8_000) > baseline);
    }

    /// A capture reduction is the QUIET formula fed a capture `statScore`.
    ///
    /// Pinned as an identity rather than described in prose, because the thing that
    /// makes this feature single-variable is that it introduces no second reduction
    /// shape: same table, same base, same improving/cut/ttPv adjustments, same
    /// `459/4096` history divisor. Only the statistic changes.
    #[test]
    fn the_capture_reduction_is_the_quiet_formula_with_a_capture_stat_score() {
        let table = shipped_lmr_table();
        let parameters = shipped();
        for (victim, history) in [(100, 0), (900, 4_000), (500, -3_000), (320, 10_692)] {
            let stat_score = capture_stat_score(&parameters, victim, history);
            assert_eq!(
                capture_late_move_reduction(
                    &table,
                    &parameters,
                    14,
                    9,
                    false,
                    true,
                    false,
                    victim,
                    history,
                    0
                ),
                late_move_reduction(
                    &table,
                    &parameters,
                    14,
                    9,
                    false,
                    true,
                    false,
                    stat_score,
                    0
                )
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

    /// The verification re-search depth must respond to HOW FAR the scout beat alpha.
    ///
    /// This is the M3-F4 mechanism. Before it, every reduced move that failed high was
    /// re-searched at exactly `child_depth`, which is where M3-F2 measured its whole
    /// node saving going back (`experiments/MSN-S2-capture-lmr/results.md` section 5).
    /// Three bands, keyed on the scout score's margin over the best score so far:
    ///
    /// * comfortably above  -> one ply DEEPER, but only when the scout was reduced at
    ///   all (a scout already searched at full depth has nothing to verify deeper),
    /// * barely above       -> one ply SHALLOWER,
    /// * in between         -> the unchanged full depth.
    #[test]
    fn the_verification_depth_follows_the_scout_score_margin() {
        // Scout comfortably above the incumbent: verify one ply deeper.
        assert_eq!(
            post_lmr_verification_depth(&shipped(), 10, 8, 500, 500 + POST_LMR_DEEPER_MARGIN + 1),
            11
        );
        // Exactly AT the deeper margin is not enough -- the reference uses a strict
        // inequality, and a boundary that drifts silently changes the tree.
        assert_eq!(
            post_lmr_verification_depth(&shipped(), 10, 8, 500, 500 + POST_LMR_DEEPER_MARGIN),
            10
        );
        // Barely above the incumbent: verify one ply shallower.
        assert_eq!(
            post_lmr_verification_depth(
                &shipped(),
                10,
                8,
                500,
                500 + POST_LMR_SHALLOWER_MARGIN - 1
            ),
            9
        );
        // Exactly at the shallower margin is already out of the band.
        assert_eq!(
            post_lmr_verification_depth(&shipped(), 10, 8, 500, 500 + POST_LMR_SHALLOWER_MARGIN),
            10
        );
        // An UNREDUCED scout can never earn the deeper band: there is no reduction to
        // pay back, and deepening past `child_depth` there would extend the tree on
        // evidence that was already full-depth.
        assert_eq!(
            post_lmr_verification_depth(&shipped(), 10, 10, 500, 500 + POST_LMR_DEEPER_MARGIN + 1),
            10
        );
        // The result is a search depth, so it can never go below 1.
        assert_eq!(
            post_lmr_verification_depth(&shipped(), 1, 1, 500, 500 + POST_LMR_SHALLOWER_MARGIN - 1),
            1
        );
    }

    /// The post-LMR continuation bonus is a plain positive bonus at the reference size.
    ///
    /// The reference applies `update_continuation_histories(ss, movedPiece, to, 1334)`
    /// unconditionally once a reduced scout beats alpha, i.e. the SIGN is fixed by
    /// having failed high at all rather than by the outcome of the verification. That
    /// is deliberately not a depth-scaled `quiet_history_bonus`: the evidence here is
    /// "a move the ordering demoted turned out to beat alpha", which is worth the same
    /// wherever it happens.
    ///
    /// Still pinned even though the mechanism ships OFF, for the same reason the
    /// capture-LMR anchors are: a measured negative stays measurable.
    #[test]
    fn the_post_lmr_continuation_bonus_is_the_reference_constant() {
        assert_eq!(POST_LMR_CONTINUATION_BONUS, 1_334);
        const { assert!(POST_LMR_CONTINUATION_BONUS > 0) };
        // It rides the same weighted fan-out every other continuation update uses, so
        // it must stay inside the table's saturation bound at the heaviest ply weight.
        const {
            assert!(
                POST_LMR_CONTINUATION_BONUS * CONTINUATION_WEIGHTS[0] / 1_024
                    < crate::history::CONTINUATION_MAX
            )
        };
    }

    /// The post-LMR update must reach EVERY continuation plane, with the shared weights.
    ///
    /// Instrumented against a real table rather than asserted in prose: this is the one
    /// new write site the feature adds, and "it fires with the right sign at the right
    /// planes" is the whole claim.
    #[test]
    fn the_post_lmr_update_writes_a_bonus_to_every_continuation_plane() {
        let history = SharedHistory::new();
        let piece = mf_core::Piece::new(Color::White, PieceKind::Knight);
        let to = Square::new(30).expect("test square is in range");
        let planes = CONTINUATION_PLIES.map(|distance| {
            Some(ContinuationKey::new(
                mf_core::Piece::new(Color::Black, PieceKind::Rook),
                Square::new(u8::try_from(distance).expect("test distance fits") + 40)
                    .expect("test square is in range"),
            ))
        });

        update_continuation_histories(&history, &planes, piece, to, POST_LMR_CONTINUATION_BONUS);

        for (slot, previous) in planes.iter().enumerate() {
            let previous = previous.expect("every test plane is populated");
            let score = history.continuation_score_at(slot, previous, piece, to);
            assert!(
                score > 0,
                "plane {slot} must receive a POSITIVE post-LMR bonus, got {score}"
            );
            assert_eq!(
                score,
                POST_LMR_CONTINUATION_BONUS * CONTINUATION_WEIGHTS[slot] / 1_024,
                "plane {slot} must use the shared continuation weight"
            );
        }

        // An absent plane is an absence of information, not a zero: the update must
        // skip it rather than writing to a sentinel.
        let sparse = SharedHistory::new();
        let mut one_plane = planes;
        one_plane[1] = None;
        update_continuation_histories(&sparse, &one_plane, piece, to, POST_LMR_CONTINUATION_BONUS);
        assert_eq!(
            sparse.continuation_score_at(
                1,
                planes[1].expect("plane exists in the dense copy"),
                piece,
                to
            ),
            0
        );
    }

    #[test]
    fn frontier_futility_margin_grows_with_effective_depth() {
        assert_eq!(frontier_futility_margin(&shipped(), 0), 125);
        assert_eq!(frontier_futility_margin(&shipped(), 1), 231);
        assert_eq!(frontier_futility_margin(&shipped(), 6), 761);
        // The window now reaches a reduced depth of 6, matching where the margin was
        // calibrated, instead of stopping at 3.
        assert_eq!(FUTILITY_MAX_EFFECTIVE_DEPTH, 6);
    }

    #[test]
    fn see_pruning_uses_separate_main_search_and_qsearch_thresholds() {
        assert_eq!(quiet_see_threshold(&shipped(), 0), 0);
        assert_eq!(quiet_see_threshold(&shipped(), 3), -26 * 9);
        assert_eq!(capture_see_threshold(&shipped(), 3), -99 * 3);
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
    fn interpolation_reaches_both_anchor_values() {
        assert_eq!(interpolate(2.0, 2.0, 6.0, 10.0, 30.0), 10.0);
        assert_eq!(interpolate(6.0, 2.0, 6.0, 10.0, 30.0), 30.0);
    }

    #[test]
    fn interpolated_time_factors_are_clamped_to_reference_ranges() {
        assert_eq!(falling_eval_factor(i32::MIN, i32::MIN, i32::MAX), 0.576);
        assert_eq!(falling_eval_factor(i32::MAX, i32::MAX, i32::MIN), 1.728);
        assert_eq!(time_reduction_factor(0), 0.629);
        assert_eq!(time_reduction_factor(u32::MAX), 1.544);
        assert_eq!(root_effort_factor(0), 0.838);
        assert_eq!(root_effort_factor(u32::MAX), 0.693);
    }

    #[test]
    fn interpolated_time_factor_is_converted_once_and_bounded() {
        assert_eq!(
            interpolated_time_scale_percent(0, 0, 0, 0, 0, 1, 75_800),
            83
        );
        assert_eq!(
            interpolated_time_scale_percent(i32::MAX, i32::MAX, i32::MIN, u32::MAX, u32::MAX, 1, 0),
            TIME_SCALE_MAX_PERCENT
        );
    }

    #[test]
    fn falling_scores_receive_more_time_than_rising_scores() {
        let falling = falling_eval_factor(100, 120, 0);
        let unchanged = falling_eval_factor(100, 100, 100);
        let rising = falling_eval_factor(0, -20, 100);

        assert!(falling > unchanged);
        assert!(unchanged >= rising);
    }

    #[test]
    fn stable_best_moves_receive_less_time_than_recent_changes() {
        assert!(stability_time_factor(18) < stability_time_factor(4));
    }

    #[test]
    fn increasing_stable_duration_progressively_lowers_time() {
        let factors = [0, 5, 10, 15, 20].map(stability_time_factor);
        assert!(factors.windows(2).all(|pair| pair[1] < pair[0]));
    }

    #[test]
    fn interpolated_time_management_requires_a_clock_managed_single_pv_search() {
        let clock = SearchLimits {
            use_clock_management: true,
            ..SearchLimits::default()
        };
        let enabled = SearchOptions {
            use_interpolated_time_management: true,
            ..SearchOptions::default()
        };

        assert!(interpolated_time_management_active(&clock, &enabled));
        assert!(!interpolated_time_management_active(
            &SearchLimits::default(),
            &enabled
        ));
        assert!(!interpolated_time_management_active(
            &clock,
            &SearchOptions {
                multi_pv: 2,
                ..enabled
            }
        ));
    }

    #[test]
    fn concentrated_root_effort_receives_less_time() {
        assert_eq!(root_effort_factor(75_800), 0.838);
        assert_eq!(root_effort_factor(104_510), 0.714);
        assert!(root_effort_factor(95_000) < root_effort_factor(75_800));
    }

    #[test]
    fn aspiration_researches_accumulate_root_effort_when_interpolated_tm_is_enabled() {
        let position = Position::startpos();
        let moves = generate_legal_moves(&position);
        let mut statistics = RootTimeStatistics::default();

        statistics.begin_iteration(true);
        statistics.begin_root_search(true);
        statistics.record_effort(moves[0], 100);
        statistics.begin_root_search(true);
        statistics.record_effort(moves[0], 200);
        statistics.record_effort(moves[1], 100);

        assert_eq!(statistics.total_effort, 400);
        assert_eq!(statistics.nodes_effort(Some(moves[0])), 75_000);
    }

    #[test]
    fn legacy_mode_resets_root_effort_for_each_root_search() {
        let position = Position::startpos();
        let moves = generate_legal_moves(&position);
        let mut statistics = RootTimeStatistics::default();

        statistics.begin_root_search(false);
        statistics.record_effort(moves[0], 100);
        statistics.begin_root_search(false);
        statistics.record_effort(moves[1], 50);

        assert_eq!(statistics.total_effort, 50);
        assert_eq!(statistics.nodes_effort(Some(moves[0])), 0);
        assert_eq!(statistics.nodes_effort(Some(moves[1])), 100_000);
    }

    #[test]
    fn root_best_move_replacements_increment_instability_only_for_line_one() {
        let position = Position::startpos();
        let moves = generate_legal_moves(&position);
        let mut statistics = RootTimeStatistics::default();

        statistics.begin_iteration(true);
        statistics.record_best_move(moves[0], true, true);
        statistics.record_best_move(moves[1], false, true);
        statistics.record_best_move(moves[1], true, true);
        statistics.record_best_move(moves[1], true, true);
        statistics.record_best_move(moves[2], true, true);
        statistics.record_best_move(moves[3], true, false);

        assert_eq!(statistics.best_move_changes, 2);
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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
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
            &low_ply_history,
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
    fn interpolated_soft_limit_waits_for_the_ponder_clock_rebase() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let stop = AtomicBool::new(false);
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let ponder = PonderState::new();
        let pre_hit_started = Instant::now() - Duration::from_millis(200);
        let mut context = SearchContext::new(
            &table,
            SearchLimits {
                soft_time: Some(Duration::from_millis(100)),
                hard_time: Some(Duration::from_millis(400)),
                use_clock_management: true,
                ..SearchLimits::default()
            },
            Some(pre_hit_started),
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
            SearchOptions {
                use_interpolated_time_management: true,
                ..SearchOptions::default()
            },
            0,
            0,
            &counters,
            network,
        );
        context.ponder = Some(&ponder);

        context.recompute_soft_time_reached();
        assert!(!context.soft_time_reached);

        ponder.ponderhit();
        context.recompute_soft_time_reached();
        assert!(
            !context.soft_time_reached,
            "ponderhit must rebase elapsed time instead of charging the 200 ms pre-hit interval"
        );

        *ponder
            .rebased_start
            .lock()
            .expect("ponder clock lock should not be poisoned") =
            Some(Instant::now() - Duration::from_millis(150));
        context.recompute_soft_time_reached();
        assert!(context.soft_time_reached);
    }

    #[test]
    fn probcut_margin_and_depth_follow_the_reference_formulas() {
        assert_eq!(probcut_beta(&shipped(), 100, false), 341);
        assert_eq!(probcut_beta(&shipped(), 100, true), 277);
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
        let parameters = shipped();
        let singular_beta = singular_beta(&parameters, 200, 8, false, false);
        assert_eq!(singular_beta, 193);
        assert_eq!(
            singular_extension(
                &parameters,
                192,
                singular_beta,
                false,
                true,
                false,
                200,
                300,
                0,
                0
            ),
            1
        );
        assert_eq!(
            singular_extension(
                &parameters,
                150,
                singular_beta,
                true,
                true,
                false,
                200,
                300,
                0,
                0
            ),
            2
        );
        assert_eq!(
            singular_extension(
                &parameters,
                210,
                singular_beta,
                false,
                true,
                false,
                400,
                300,
                0,
                0
            ),
            -3
        );
        assert_eq!(
            singular_extension(
                &parameters,
                210,
                singular_beta,
                false,
                true,
                true,
                200,
                300,
                0,
                0
            ),
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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Some(Instant::now()),
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );
        let mut searched = position.clone();
        let mut pv = PvLine::new();

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
        let counters = [NodeCounter::new(0)];
        let history = [position.repetition_key()];
        let shared_history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Some(Instant::now()),
            &stop,
            &position,
            &history,
            &shared_history,
            &low_ply_history,
            SearchOptions::default(),
            0,
            0,
            &counters,
            network,
        );
        let mut searched = position.clone();
        let mut pv = PvLine::new();

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

    /// The per-node cache must answer exactly as the slow function for EVERY move --
    /// direct, discovered, castling, en passant, promotion -- across hand-picked
    /// positions and a long random walk. The fast path is the one the search takes,
    /// so any disagreement moves the bench signature.
    #[test]
    fn cached_check_info_agrees_with_move_gives_check_on_every_pseudo_legal_move() {
        let mut positions = vec![
            Position::startpos(),
            Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", false).unwrap(),
            Position::from_fen("4k3/8/8/8/8/8/4B3/4R1K1 w - - 0 1", false).unwrap(),
            Position::from_fen("8/8/8/R2pP2k/8/8/8/K7 w - d6 0 1", false).unwrap(),
            Position::from_fen("8/4P3/8/8/7k/8/8/K7 w - - 0 1", false).unwrap(),
            Position::from_fen(
                "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
                false,
            )
            .unwrap(),
        ];
        let mut random_walk = Position::startpos();
        for sample in 0..512 {
            positions.push(random_walk.clone());
            let moves = generate_legal_moves(&random_walk);
            if moves.is_empty() {
                random_walk = Position::startpos();
            } else {
                random_walk.make_move(moves[(sample * 17 + 3) % moves.len()]);
            }
        }

        for position in positions {
            let check_info = CheckInfo::new(&position);
            for &mv in &mf_core::generate_pseudo_legal_moves(&position) {
                assert_eq!(
                    check_info.gives_check(&position, mv),
                    move_gives_check(&position, mv),
                    "{position:?} {mv:?}"
                );
            }
        }
    }

    #[cfg(feature = "corrhist-regression")]
    #[test]
    fn correction_features_reproduce_the_existing_blend_with_independent_missing_continuations() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let position_history = [position.repetition_key()];
        let transposition_table =
            TranspositionTable::new(1).expect("test transposition table should allocate");
        let stop = AtomicBool::new(false);
        let node_counters = [NodeCounter::new(0)];
        let history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let options = SearchOptions {
            use_correction_sources: [true; CORRECTION_SOURCES + 1],
            ..SearchOptions::default()
        };
        let mut context = SearchContext::new(
            &transposition_table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &position_history,
            &history,
            &low_ply_history,
            options,
            0,
            0,
            &node_counters,
            network,
        );

        for source in 0..CORRECTION_SOURCES {
            history.update_correction(
                source,
                correction_key(&position, source),
                position.side_to_move(),
                64 * (source as i32 + 1),
            );
        }

        let ply = 5;
        let entry = ContinuationKey::new(
            mf_core::Piece::new(Color::White, PieceKind::Knight),
            Square::new(20).expect("test square"),
        );
        let near = ContinuationKey::new(
            mf_core::Piece::new(Color::Black, PieceKind::Bishop),
            Square::new(30).expect("test square"),
        );
        let far = ContinuationKey::new(
            mf_core::Piece::new(Color::White, PieceKind::Rook),
            Square::new(40).expect("test square"),
        );
        context.continuation_keys[ply - 1] = Some(entry);
        context.continuation_keys[ply - 2] = Some(near);
        context.continuation_keys[ply - 4] = Some(far);
        history.update_correction_continuation(0, near, entry, 128);
        history.update_correction_continuation(1, far, entry, 256);

        let features = correction_features(&position, &context, ply);
        assert_eq!(
            correction_value(&position, &context, ply),
            CORRECTION_WEIGHTS[CORRECTION_PAWN] * i32::from(features.pawn)
                + CORRECTION_WEIGHTS[CORRECTION_MINOR] * i32::from(features.minor)
                + CORRECTION_WEIGHTS[CORRECTION_MAJOR] * i32::from(features.major)
                + CORRECTION_WEIGHTS[CORRECTION_MATERIAL] * i32::from(features.material)
                + CORRECTION_CONTINUATION_WEIGHT
                    * (i32::from(features.continuation_2) + i32::from(features.continuation_4))
        );

        context.continuation_keys[ply - 2] = None;
        let without_near = correction_features(&position, &context, ply);
        assert_eq!(without_near.continuation_2, 0);
        assert_eq!(without_near.continuation_4, features.continuation_4);

        context.continuation_keys[ply - 2] = Some(near);
        context.continuation_keys[ply - 4] = None;
        let without_far = correction_features(&position, &context, ply);
        assert_eq!(without_far.continuation_2, features.continuation_2);
        assert_eq!(without_far.continuation_4, 0);
    }

    #[cfg(feature = "corrhist-regression")]
    #[test]
    fn snapshotted_correction_features_do_not_change_after_history_updates() {
        let Some(network) = local_network() else {
            return;
        };
        let position = Position::startpos();
        let position_history = [position.repetition_key()];
        let transposition_table =
            TranspositionTable::new(1).expect("test transposition table should allocate");
        let stop = AtomicBool::new(false);
        let node_counters = [NodeCounter::new(0)];
        let history = SharedHistory::new();
        let low_ply_history = LowPlyHistory::new();
        let context = SearchContext::new(
            &transposition_table,
            SearchLimits::default(),
            None,
            &stop,
            &position,
            &position_history,
            &history,
            &low_ply_history,
            SearchOptions::default(),
            0,
            0,
            &node_counters,
            network,
        );

        let snapshot = correction_features(&position, &context, 0);
        history.update_correction(
            CORRECTION_PAWN,
            correction_key(&position, CORRECTION_PAWN),
            position.side_to_move(),
            512,
        );

        assert_eq!(snapshot.pawn, 0);
        assert_ne!(correction_features(&position, &context, 0).pawn, 0);
    }
}
