use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mf_core::{
    Bitboard, CastlingSide, Color, Move, PieceKind, Position, Square, bishop_attacks,
    generate_legal_moves, has_legal_move, is_in_check, king_attacks, knight_attacks, pawn_attacks,
    rook_attacks, static_exchange_evaluation,
};

use crate::evaluation::{EVALUATION_LIMIT, evaluate};
use crate::history::{
    CONTINUATION_PLIES, CONTINUATION_WEIGHTS, ContinuationKey, KillerTable, SharedHistory,
    captured_kind,
};
use crate::move_ordering::{MovePicker, OrderingContext, quiescence_moves};
use crate::repetition::RepetitionHistory;
use crate::{Bound, EntryData, TranspositionTable};

pub const MATE_SCORE: i32 = 30_000;
pub const MAX_SEARCH_PLY: usize = 128;
/// Marks a TT entry whose static evaluation was intentionally not computed.
pub const UNEVALUATED_STATIC_EVAL: i16 = i16::MIN;
const INFINITY: i32 = MATE_SCORE + 1;
const TABLEBASE_SCORE: i32 = MATE_SCORE - MAX_SEARCH_PLY as i32 - 1;
const TABLEBASE_WIN_IN_MAX_PLY: i32 = TABLEBASE_SCORE - MAX_SEARCH_PLY as i32;
const ASPIRATION_INITIAL_DELTA: i32 = 25;
const ASPIRATION_JITTER_BUCKETS: usize = 8;
const DEFAULT_MAX_DEPTH: u32 = 64;
const TIME_CHECK_INTERVAL: u64 = 512;
const NODE_PUBLISH_INTERVAL: u64 = 1_024;
const NMP_MIN_DEPTH: i32 = 3;
const NMP_VERIFICATION_DEPTH: i32 = 6;
const RFP_MAX_DEPTH: i32 = 3;
const LMP_MAX_DEPTH: i32 = 8;
const FUTILITY_MAX_EFFECTIVE_DEPTH: i32 = 3;
const SINGULAR_MIN_DEPTH: i32 = 6;
const IIR_MIN_DEPTH: i32 = 6;
const PROBCUT_MIN_DEPTH: i32 = 3;
const PROBCUT_BASE_MARGIN: i32 = 241;
const PROBCUT_IMPROVING_MARGIN: i32 = 64;
const QSEARCH_SEE_THRESHOLD: i32 = -74;
const LMR_TABLE_SIZE: usize = MAX_SEARCH_PLY + 1;
const LMP_TABLE: [[usize; LMP_MAX_DEPTH as usize + 1]; 2] = build_lmp_table();

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
            // all. In Stockfish pawn history is never a standalone ordering signal: it
            // is one small term in a sum dominated by continuation history, which this
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
            // pruning decision rests on a single noisy statistic. Stockfish thresholds
            // the SUM of butterfly and several continuation-history plies, which is a
            // far more reliable signal. Revisit when continuation history lands.
            use_history_pruning: false,
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
}

impl<'a> WorkerParameters<'a> {
    pub(crate) fn new(
        worker_id: usize,
        generation: u8,
        node_counters: &'a [AtomicU64],
        history: &'a SharedHistory,
    ) -> Self {
        assert!(worker_id < node_counters.len());
        Self {
            worker_id,
            generation: generation & 31,
            node_counters,
            history,
        }
    }
}

pub fn search(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
) -> SearchResult {
    search_with_options(
        position,
        transposition_table,
        limits,
        SearchOptions::default(),
    )
}

pub fn search_with_options(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
) -> SearchResult {
    let history = [position.repetition_key()];
    search_with_history_options(position, &history, transposition_table, limits, options)
}

/// Single-threaded search reusing a caller-owned [`SharedHistory`].
///
/// Every other single-threaded entry point constructs a `SharedHistory` internally,
/// which allocates and zeroes tens of MiB. That is correct for a one-shot search but
/// wrong inside a timed benchmark loop: `bench` used to pay the allocation once per
/// position *inside its own timed region*, so bench NPS misstated the cost of adding
/// history tables (mission AGENTS.md 4.54). Match play allocates once per game, so the
/// benchmark now matches it by hoisting the construction out and calling `clear()`
/// between positions instead. Node counts are unaffected: a cleared table is
/// bit-identical to a fresh one.
pub fn search_with_shared_history_options(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    shared_history: &SharedHistory,
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
        WorkerParameters::new(0, 0, &node_counters, shared_history),
        |_| {},
    )
}

pub fn search_with_history(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
) -> SearchResult {
    search_with_history_options(
        position,
        history,
        transposition_table,
        limits,
        SearchOptions::default(),
    )
}

pub fn search_with_history_options(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
) -> SearchResult {
    let stop = AtomicBool::new(false);
    search_with_history_callback_options(
        position,
        history,
        transposition_table,
        limits,
        options,
        &stop,
        |_| {},
    )
}

pub fn search_with_callback<F>(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    stop: &AtomicBool,
    on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    search_with_callback_options(
        position,
        transposition_table,
        limits,
        SearchOptions::default(),
        stop,
        on_iteration,
    )
}

pub fn search_with_callback_options<F>(
    position: &Position,
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    stop: &AtomicBool,
    on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    let history = [position.repetition_key()];
    search_with_history_callback_options(
        position,
        &history,
        transposition_table,
        limits,
        options,
        stop,
        on_iteration,
    )
}

pub fn search_with_history_callback<F>(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    stop: &AtomicBool,
    on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    search_with_history_callback_options(
        position,
        history,
        transposition_table,
        limits,
        SearchOptions::default(),
        stop,
        on_iteration,
    )
}

pub fn search_with_history_callback_options<F>(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
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
        WorkerParameters::new(0, 0, &node_counters, &shared_history),
        on_iteration,
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
    let maximum_depth = if limits.infinite {
        u32::MAX
    } else {
        limits.depth.unwrap_or(DEFAULT_MAX_DEPTH).max(1)
    };
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
    );
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
        score: evaluate(position),
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

fn aspiration_search(
    position: &Position,
    depth: u32,
    previous_score: i32,
    context: &mut SearchContext<'_>,
) -> Option<(i32, Vec<Move>)> {
    let mut delta = aspiration_delta(context.worker_id);
    let mut alpha = (previous_score - delta).max(-INFINITY);
    let mut beta = (previous_score + delta).min(INFINITY);

    loop {
        let result = root_search(position, depth, alpha, beta, context)?;
        if result.0 <= alpha {
            alpha = (result.0 - delta).max(-INFINITY);
        } else if result.0 >= beta {
            beta = (result.0 + delta).min(INFINITY);
        } else {
            return Some(result);
        }
        delta += (47 * delta / 128).max(1);
        if alpha == -INFINITY && beta == INFINITY {
            return root_search(position, depth, alpha, beta, context);
        }
    }
}

#[inline]
fn aspiration_delta(worker_id: usize) -> i32 {
    ASPIRATION_INITIAL_DELTA + (worker_id % ASPIRATION_JITTER_BUCKETS) as i32
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
        context.push_position(&position, 1, mv, continuation);
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
            depth: depth.min(u32::from(u8::MAX)) as u8,
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
    beta: i32,
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
        return Some(evaluate(position));
    }
    if depth <= 0 {
        return quiescence(position, alpha, beta, ply, false, context, pv);
    }

    let key = tt_key(position, depth);
    context.transposition_table.prefetch(key);
    let original_alpha = alpha;
    let mut tt_move = None;
    let tt_entry = tt_entry_for_node(context.transposition_table, key, verification_node);
    if let Some(entry) = tt_entry {
        tt_move = entry.best_move;
        if !pv_node && !verification_node && i32::from(entry.depth) >= depth {
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
    let static_eval = tt_entry
        .filter(|entry| entry.static_eval != UNEVALUATED_STATIC_EVAL)
        .map_or_else(|| evaluate(position), |entry| i32::from(entry.static_eval));
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
            && !is_mate_score(alpha)
            && eval_pruning_rule50_safe(position, depth)
            && static_eval < alpha - razoring_margin(depth)
        {
            return quiescence(position, alpha, beta, ply, false, context, pv);
        }

        if context.options.use_rfp
            && !tt_entry.is_some_and(|entry| entry.pv)
            && depth <= RFP_MAX_DEPTH
            && !is_mate_score(beta)
            && eval_pruning_rule50_safe(position, depth)
            && tt_move.is_none_or(|mv| mv.flag().is_capture())
            && static_eval - reverse_futility_margin(depth, tt_entry.is_some()) >= beta
        {
            return Some((661 * beta + 363 * static_eval) / 1024);
        }

        if context.options.use_nmp
            && cut_node
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
        depth -= internal_iterative_reduction(
            depth,
            pv_node,
            cut_node,
            in_check,
            tt_move,
            excluded_move,
        );
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
                context.push_position(position, ply + 1, mv, continuation);
                let mut probcut_pv = Vec::new();
                let mut value = quiescence(
                    position,
                    -probcut_beta,
                    -probcut_beta + 1,
                    ply + 1,
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
                                static_eval: static_eval as i16,
                                depth: (probcut_depth + 1).clamp(0, i32::from(u8::MAX)) as u8,
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
            let threshold = if quiet {
                quiet_see_threshold(effective_depth)
            } else {
                capture_see_threshold(depth) - capture_history * 34 / 1024
            };
            if static_exchange_evaluation(position, mv) < threshold
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
                    && i32::from(entry.depth) >= depth - 3
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
        context.push_position(position, ply + 1, mv, continuation);
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
            let reduced_depth = if context.options.use_lmr && depth >= 2 && quiet && !gives_check {
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
    if !verification_node {
        context.transposition_table.store(
            key,
            EntryData {
                best_move,
                score: value_to_tt(best_score, ply) as i16,
                static_eval: static_eval as i16,
                depth: depth.min(i32::from(u8::MAX)) as u8,
                bound,
                age: context.generation,
                pv: pv_node,
            },
        );
    }
    Some(best_score)
}

fn quiescence(
    position: &mut Position,
    mut alpha: i32,
    beta: i32,
    ply: usize,
    count_node: bool,
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
        return Some(evaluate(position));
    }

    let in_check = is_in_check(position, position.side_to_move());
    let mut best_score = if in_check {
        -INFINITY
    } else {
        evaluate(position)
    };
    if !in_check {
        if best_score >= beta {
            return Some(if has_legal_move(position) {
                best_score
            } else {
                0
            });
        }
        alpha = alpha.max(best_score);
    }

    let ordering = context.ordering(position, ply);
    let moves: Vec<_> = if in_check {
        MovePicker::new(position, None, [None, None], ordering).collect()
    } else {
        quiescence_moves(
            position,
            qsearch_see_threshold(context.options.use_see_pruning),
            ordering,
        )
    };
    let mut searched = 0usize;
    let mut child_pv = Vec::new();
    for mv in moves {
        let mover = position.side_to_move();
        let continuation = continuation_key(position, mv);
        let undo = position.make_move(mv);
        context.transposition_table.prefetch(tt_key(position, 0));
        if is_in_check(position, mover) {
            position.unmake_move(mv, undo);
            continue;
        }
        context.push_position(position, ply + 1, mv, continuation);
        child_pv.clear();
        let score = quiescence(
            position,
            -beta,
            -alpha,
            ply + 1,
            true,
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
            pv.clear();
            pv.push(mv);
            pv.extend_from_slice(&child_pv);
        }
        alpha = alpha.max(score);
        if alpha >= beta {
            break;
        }
    }

    if searched != 0 {
        return Some(best_score);
    }
    if in_check {
        Some(-MATE_SCORE + ply as i32)
    } else if has_legal_move(position) {
        Some(best_score)
    } else {
        Some(0)
    }
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

#[inline]
fn razoring_margin(depth: i32) -> i32 {
    483 + 318 * depth * depth
}

#[inline]
fn reverse_futility_margin(depth: i32, tt_hit: bool) -> i32 {
    let multiplier = (45 + depth * 4).min(85) - 20 * i32::from(!tt_hit);
    8 * multiplier * depth
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
            table[improving][depth] = (3 + depth * depth) / (2 - improving);
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

#[inline]
fn frontier_futility_margin(effective_depth: i32) -> i32 {
    39 + 119 * effective_depth.max(0)
}

#[inline]
fn quiet_see_threshold(effective_depth: i32) -> i32 {
    -23 * effective_depth.max(0).pow(2)
}

#[inline]
fn capture_see_threshold(depth: i32) -> i32 {
    -177 * depth.max(0)
}

#[inline]
const fn qsearch_see_threshold(enabled: bool) -> i32 {
    if enabled { QSEARCH_SEE_THRESHOLD } else { 0 }
}

#[inline]
fn internal_iterative_reduction(
    depth: i32,
    pv_node: bool,
    cut_node: bool,
    in_check: bool,
    tt_move: Option<Move>,
    excluded_move: Option<Move>,
) -> i32 {
    i32::from(
        depth >= IIR_MIN_DEPTH
            && !pv_node
            && cut_node
            && !in_check
            && tt_move.is_none()
            && excluded_move.is_none(),
    )
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
fn move_gives_check(position: &Position, mv: Move) -> bool {
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

/// The malus is deliberately steeper in depth than the bonus (Stockfish uses 968 vs
/// 133 per ply): punishing a move that failed to cut carries more information than
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
    generation: u8,
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
            killers: KillerTable::new(),
            static_evals: [None; MAX_SEARCH_PLY],
            repetition_history: RepetitionHistory::new(position, position_history),
            current_moves: [None; MAX_SEARCH_PLY],
            continuation_keys: [None; MAX_SEARCH_PLY],
            nmp_min_ply: 0,
            stopped: false,
            soft_time_reached: false,
            generation,
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
            self.soft_time_reached = self.limits.soft_time.is_some_and(|limit| elapsed >= limit);
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
    ) {
        self.current_moves[ply - 1] = Some(mv);
        self.continuation_keys[ply - 1] = continuation;
        self.repetition_history.push_position(position);
    }

    fn push_null_position(&mut self, ply: usize) {
        self.current_moves[ply - 1] = None;
        self.continuation_keys[ply - 1] = None;
        self.repetition_history.push_null();
    }

    fn pop_position(&mut self, ply: usize) {
        self.repetition_history.pop();
        self.current_moves[ply - 1] = None;
        self.continuation_keys[ply - 1] = None;
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
    use super::*;

    #[test]
    fn worker_zero_preserves_the_original_aspiration_delta() {
        assert_eq!(aspiration_delta(0), ASPIRATION_INITIAL_DELTA);
    }

    #[test]
    fn helpers_receive_distinct_bounded_aspiration_deltas() {
        let values: Vec<_> = (0..8).map(aspiration_delta).collect();
        assert_eq!(values[0], ASPIRATION_INITIAL_DELTA);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn aggregate_nodes_sum_published_worker_counts() {
        let counters = [AtomicU64::new(10), AtomicU64::new(20)];
        assert_eq!(published_node_total(&counters), 30);
    }

    #[test]
    fn one_worker_node_limit_is_exact() {
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
            WorkerParameters::new(0, 0, &counters, &shared_history),
            |_| {},
        );

        assert_eq!(result.nodes, 1_500);
        assert_eq!(counters[0].load(Ordering::Relaxed), 1_500);
    }

    #[test]
    fn one_worker_accepts_the_limit_node_across_publication_boundary() {
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
            WorkerParameters::new(1, 0, &counters, &shared_history),
            |_| {},
        );

        assert_eq!(result.nodes, NODE_PUBLISH_INTERVAL);
        assert_eq!(counters[1].load(Ordering::Relaxed), NODE_PUBLISH_INTERVAL);
    }

    #[test]
    fn helper_search_does_not_observe_clock_limits() {
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
            WorkerParameters::new(1, 0, &counters, &shared_history),
            |_| {},
        );

        assert_eq!(result.depth, 2);
        assert!(result.nodes > 0);
        assert_eq!(result.elapsed, Duration::ZERO);
    }

    #[test]
    fn worker_zero_iteration_nodes_include_published_helpers() {
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
            WorkerParameters::new(0, 0, &counters, &shared_history),
            |info| reported_nodes = Some(info.nodes),
        );

        assert_eq!(reported_nodes, Some(result.nodes + 37));
    }

    #[test]
    fn worker_generation_is_written_to_root_tt_entries() {
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
            WorkerParameters::new(0, 9, &counters, &shared_history),
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
        assert_eq!(razoring_margin(1), 483 + 318);
        assert_eq!(razoring_margin(3), 483 + 318 * 9);
        assert_eq!(razoring_margin(8), 483 + 318 * 64);
    }

    #[test]
    fn reverse_futility_margin_scales_then_flattens() {
        assert_eq!(reverse_futility_margin(1, true), 392);
        assert_eq!(reverse_futility_margin(1, false), 232);
        assert_eq!(reverse_futility_margin(10, true), 6_800);
        assert_eq!(reverse_futility_margin(10, false), 5_200);
        assert_eq!(reverse_futility_margin(18, true), 12_240);
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
        assert_eq!(late_move_pruning_threshold(1, false), 2);
        assert_eq!(late_move_pruning_threshold(1, true), 4);
        assert_eq!(late_move_pruning_threshold(4, false), 9);
        assert_eq!(late_move_pruning_threshold(4, true), 19);
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

    #[test]
    fn frontier_futility_margin_grows_with_effective_depth() {
        assert_eq!(frontier_futility_margin(0), 39);
        assert_eq!(frontier_futility_margin(1), 158);
        assert_eq!(frontier_futility_margin(3), 396);
    }

    #[test]
    fn see_pruning_uses_separate_main_search_and_qsearch_thresholds() {
        assert_eq!(quiet_see_threshold(0), 0);
        assert_eq!(quiet_see_threshold(3), -207);
        assert_eq!(capture_see_threshold(3), -531);
        assert_eq!(qsearch_see_threshold(true), -74);
        assert_eq!(qsearch_see_threshold(false), 0);
    }

    #[test]
    fn iir_reduces_only_deep_tt_move_less_expected_cut_nodes() {
        let tt_move = generate_legal_moves(&Position::startpos())[0];

        assert_eq!(
            internal_iterative_reduction(5, false, true, false, None, None),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, true, true, false, None, None),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, false, false, false, None, None),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, false, true, false, Some(tt_move), None),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, false, true, false, None, Some(tt_move)),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, false, true, true, None, None),
            0
        );
        assert_eq!(
            internal_iterative_reduction(6, false, true, false, None, None),
            1
        );
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
        let position = Position::startpos();
        let depth = 6;
        let key = tt_key(&position, depth);
        let tt_move = generate_legal_moves(&position)[0];
        let original = EntryData {
            best_move: Some(tt_move),
            score: 100,
            static_eval: evaluate(&position) as i16,
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
