use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mf_core::{Move, PieceKind, Position, generate_legal_moves, has_legal_move, is_in_check};

use crate::evaluation::{EVALUATION_LIMIT, evaluate};
use crate::move_ordering::{MovePicker, quiescence_moves};
use crate::{Bound, EntryData, TranspositionTable};

pub const MATE_SCORE: i32 = 30_000;
pub const MAX_SEARCH_PLY: usize = 128;
/// Marks a TT entry whose static evaluation was intentionally not computed.
pub const UNEVALUATED_STATIC_EVAL: i16 = i16::MIN;
const INFINITY: i32 = MATE_SCORE + 1;
const ASPIRATION_INITIAL_DELTA: i32 = 25;
const DEFAULT_MAX_DEPTH: u32 = 64;
const NMP_MIN_DEPTH: i32 = 3;
const NMP_VERIFICATION_DEPTH: i32 = 6;
const RFP_MAX_DEPTH: i32 = 3;

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
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            use_nmp: true,
            use_rfp: true,
            use_razoring: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterationInfo {
    pub depth: u32,
    pub seldepth: u32,
    pub score: i32,
    pub nodes: u64,
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
    pub elapsed: Duration,
    pub pv: Vec<Move>,
    pub iterations: Vec<IterationInfo>,
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
    mut on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
{
    let started = Instant::now();
    let maximum_depth = if limits.infinite {
        u32::MAX
    } else {
        limits.depth.unwrap_or(DEFAULT_MAX_DEPTH).max(1)
    };
    let mut context = SearchContext::new(
        transposition_table,
        limits,
        started,
        stop,
        position,
        history,
        options,
    );
    let root_moves = generate_legal_moves(position);

    if root_moves.is_empty() {
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
            elapsed: started.elapsed(),
            pv: Vec::new(),
            iterations: Vec::new(),
        };
    }

    let fallback_move = root_moves[0];
    if context.is_rule_draw(position) {
        return SearchResult {
            best_move: Some(fallback_move),
            score: 0,
            depth: 0,
            seldepth: 0,
            nodes: 0,
            elapsed: started.elapsed(),
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
        let elapsed = started.elapsed();
        let info = IterationInfo {
            depth,
            seldepth: context.seldepth.max(depth),
            score,
            nodes: context.nodes,
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
        nodes: context.nodes,
        elapsed: started.elapsed(),
        pv: vec![fallback_move],
    });
    let best_move = completed.pv.first().copied().or(Some(fallback_move));

    SearchResult {
        best_move,
        score: completed.score,
        depth: completed.depth,
        seldepth: completed.seldepth,
        nodes: context.nodes,
        elapsed: started.elapsed(),
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
    let mut delta = ASPIRATION_INITIAL_DELTA;
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

    for mv in MovePicker::new(&position, tt_move, [None, None]) {
        let mover = position.side_to_move();
        let undo = position.make_move(mv);
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
        context.push_position(&position);
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
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)
                } else {
                    Some(score)
                }
            })
        };
        context.pop_position();
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
            age: 0,
            pv: true,
        },
    );
    Some((best_score, best_pv))
}

#[allow(clippy::too_many_arguments)]
fn pvs(
    position: &mut Position,
    depth: i32,
    mut alpha: i32,
    beta: i32,
    ply: usize,
    pv_node: bool,
    cut_node: bool,
    allow_null: bool,
    verification_node: bool,
    context: &mut SearchContext<'_>,
    pv: &mut Vec<Move>,
) -> Option<i32> {
    if !context.visit_node(ply) {
        return None;
    }
    if context.is_rule_draw(position) {
        return Some(0);
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
    let tt_entry = context.transposition_table.probe(key);
    if let Some(entry) = tt_entry {
        tt_move = entry.best_move;
        if !pv_node && !verification_node && i32::from(entry.depth) >= depth {
            let score = score_from_tt(i32::from(entry.score), ply);
            match entry.bound {
                Bound::Exact => return Some(score),
                Bound::Lower if score >= beta => return Some(score),
                Bound::Upper if score <= alpha => return Some(score),
                _ => {}
            }
        }
    }

    let in_check = is_in_check(position, position.side_to_move());
    let static_eval = tt_entry
        .filter(|entry| entry.static_eval != UNEVALUATED_STATIC_EVAL)
        .map_or_else(|| evaluate(position), |entry| i32::from(entry.static_eval));
    if !in_check && !pv_node {
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
            context.push_null_position();
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
                context,
                &mut null_pv,
            )
            .map(|score| -score);
            context.pop_position();
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
    }

    let mut best_score = -INFINITY;
    let mut best_move = None;
    let mut searched = 0usize;
    let mut child_pv = Vec::new();

    for mv in MovePicker::new(position, tt_move, context.killers[ply]) {
        let mover = position.side_to_move();
        let undo = position.make_move(mv);
        if is_in_check(position, mover) {
            position.unmake_move(mv, undo);
            continue;
        }
        context.push_position(position);
        child_pv.clear();
        let score = if searched == 0 {
            pvs(
                position,
                depth - 1,
                -beta,
                -alpha,
                ply + 1,
                pv_node,
                if pv_node { false } else { !cut_node },
                true,
                false,
                context,
                &mut child_pv,
            )
            .map(|score| -score)
        } else {
            let scout = pvs(
                position,
                depth - 1,
                -alpha - 1,
                -alpha,
                ply + 1,
                false,
                !cut_node,
                true,
                false,
                context,
                &mut child_pv,
            )
            .map(|score| -score);
            scout.and_then(|score| {
                if score > alpha && score < beta {
                    child_pv.clear();
                    pvs(
                        position,
                        depth - 1,
                        -beta,
                        -alpha,
                        ply + 1,
                        pv_node,
                        if pv_node { false } else { !cut_node },
                        true,
                        false,
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)
                } else {
                    Some(score)
                }
            })
        };
        context.pop_position();
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
            if !mv.flag().is_capture() && mv.flag().promotion().is_none() {
                context.record_killer(ply, mv);
            }
            break;
        }
    }

    if searched == 0 {
        return Some(if is_in_check(position, position.side_to_move()) {
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
                score: score_to_tt(best_score, ply) as i16,
                static_eval: static_eval as i16,
                depth: depth.min(i32::from(u8::MAX)) as u8,
                bound,
                age: 0,
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
    if context.is_rule_draw(position) {
        return Some(0);
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

    let moves: Vec<_> = if in_check {
        MovePicker::new(position, None, [None, None]).collect()
    } else {
        quiescence_moves(position)
    };
    let mut searched = 0usize;
    let mut child_pv = Vec::new();
    for mv in moves {
        let mover = position.side_to_move();
        let undo = position.make_move(mv);
        if is_in_check(position, mover) {
            position.unmake_move(mv, undo);
            continue;
        }
        context.push_position(position);
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
        context.pop_position();
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

fn score_to_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_SCORE - MAX_SEARCH_PLY as i32 {
        score + ply as i32
    } else if score <= -MATE_SCORE + MAX_SEARCH_PLY as i32 {
        score - ply as i32
    } else {
        score
    }
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
    PieceKind::NON_PAWN_MATERIAL
        .into_iter()
        .any(|kind| !position.pieces(position.side_to_move(), kind).is_empty())
}

fn tt_key(position: &Position, depth: i32) -> u64 {
    const RULE50_SALT: u64 = 0x6a09_e667_f3bc_c909;

    if i32::from(position.halfmove_clock()) + depth < 100 {
        return position.zobrist().main();
    }
    let mut value = u64::from(position.halfmove_clock()).wrapping_add(RULE50_SALT);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    position.zobrist().main() ^ (value ^ (value >> 31))
}

fn score_from_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_SCORE - MAX_SEARCH_PLY as i32 {
        score - ply as i32
    } else if score <= -MATE_SCORE + MAX_SEARCH_PLY as i32 {
        score + ply as i32
    } else {
        score
    }
}

struct SearchContext<'a> {
    transposition_table: &'a TranspositionTable,
    stop: &'a AtomicBool,
    limits: SearchLimits,
    options: SearchOptions,
    started: Instant,
    nodes: u64,
    seldepth: u32,
    iterations: Vec<IterationInfo>,
    killers: [[Option<Move>; 2]; MAX_SEARCH_PLY],
    position_history: Vec<Option<u64>>,
    nmp_min_ply: usize,
    stopped: bool,
}

impl<'a> SearchContext<'a> {
    fn new(
        transposition_table: &'a TranspositionTable,
        limits: SearchLimits,
        started: Instant,
        stop: &'a AtomicBool,
        position: &Position,
        history: &[u64],
        options: SearchOptions,
    ) -> Self {
        let root_key = position.repetition_key();
        let position_history = if history.is_empty() {
            vec![Some(root_key)]
        } else {
            assert_eq!(
                history.last().copied(),
                Some(root_key),
                "search history must end at the root position"
            );
            history.iter().copied().map(Some).collect()
        };
        Self {
            transposition_table,
            stop,
            limits,
            options,
            started,
            nodes: 0,
            seldepth: 0,
            iterations: Vec::new(),
            killers: [[None; 2]; MAX_SEARCH_PLY],
            position_history,
            nmp_min_ply: 0,
            stopped: false,
        }
    }

    fn visit_node(&mut self, ply: usize) -> bool {
        if self.stopped || self.stop.load(Ordering::Relaxed) {
            self.stopped = true;
            return false;
        }
        if self.limits.nodes.is_some_and(|limit| self.nodes >= limit) {
            self.stopped = true;
            return false;
        }
        if self
            .limits
            .hard_time
            .is_some_and(|limit| self.started.elapsed() >= limit)
        {
            self.stopped = true;
            return false;
        }

        self.nodes += 1;
        self.seldepth = self.seldepth.max(ply as u32);
        true
    }

    fn should_stop_after_iteration(&self) -> bool {
        self.stopped
            || self.limits.nodes.is_some_and(|limit| self.nodes >= limit)
            || self
                .limits
                .soft_time
                .is_some_and(|limit| self.started.elapsed() >= limit)
    }

    fn record_killer(&mut self, ply: usize, mv: Move) {
        if ply >= MAX_SEARCH_PLY || self.killers[ply][0] == Some(mv) {
            return;
        }
        self.killers[ply][1] = self.killers[ply][0];
        self.killers[ply][0] = Some(mv);
    }

    fn push_position(&mut self, position: &Position) {
        self.position_history.push(Some(position.repetition_key()));
    }

    fn push_null_position(&mut self) {
        self.position_history.push(None);
    }

    fn pop_position(&mut self) {
        self.position_history
            .pop()
            .expect("search position history must contain the root");
    }

    fn is_rule_draw(&self, position: &Position) -> bool {
        if position.is_insufficient_material() || self.is_threefold_repetition(position) {
            return true;
        }
        position.halfmove_clock() >= 100
            && (!is_in_check(position, position.side_to_move()) || has_legal_move(position))
    }

    fn is_threefold_repetition(&self, position: &Position) -> bool {
        let key = position.repetition_key();
        let boundary = self
            .position_history
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |index| index + 1);
        self.position_history[boundary..]
            .iter()
            .rev()
            .step_by(2)
            .filter(|&&previous| previous == Some(key))
            .take(3)
            .count()
            == 3
    }

    fn nmp_allowed_at(&self, ply: usize) -> bool {
        self.nmp_min_ply == 0 || ply >= self.nmp_min_ply
    }
}

pub fn is_mate_score(score: i32) -> bool {
    score.abs() >= MATE_SCORE - MAX_SEARCH_PLY as i32
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
    fn tt_key_distinguishes_every_halfmove_clock() {
        let clock_92 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 92 1", false).unwrap();
        let clock_95 = Position::from_fen("8/8/8/4k3/8/8/8/K1Q5 w - - 95 1", false).unwrap();

        assert_ne!(tt_key(&clock_92, 5), tt_key(&clock_95, 5));
        assert_eq!(tt_key(&clock_92, 5), clock_92.zobrist().main());
        assert_ne!(tt_key(&clock_92, 8), clock_92.zobrist().main());
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
        let position = Position::startpos();
        let key = position.repetition_key();
        let history = [key; 6];
        let table = TranspositionTable::new(1).expect("test TT should allocate");
        let stop = AtomicBool::new(false);
        let mut context = SearchContext::new(
            &table,
            SearchLimits::default(),
            Instant::now(),
            &stop,
            &position,
            &history,
            SearchOptions::default(),
        );

        assert!(context.is_threefold_repetition(&position));
        context.push_null_position();
        assert!(!context.is_threefold_repetition(&position));
    }
}
