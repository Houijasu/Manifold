use std::time::{Duration, Instant};

use mf_core::{Move, Position, generate_legal_moves, is_in_check};

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

#[derive(Clone, Copy, Debug, Default)]
pub struct SearchLimits {
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub soft_time: Option<Duration>,
    pub hard_time: Option<Duration>,
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
    let started = Instant::now();
    let maximum_depth = limits.depth.unwrap_or(DEFAULT_MAX_DEPTH).max(1);
    let mut context = SearchContext::new(transposition_table, limits, started);
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
        context.iterations.push(info);

        if context.nodes == iteration_start_nodes
            || limits.depth == Some(depth)
            || context.should_stop_after_iteration()
            || score.abs() >= MATE_SCORE - depth as i32
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
    let key = position.zobrist().main();
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
        let mut child_pv = Vec::new();
        let score = if searched == 0 {
            pvs(
                &mut position,
                depth as i32 - 1,
                -beta,
                -alpha,
                1,
                true,
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
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)
                } else {
                    Some(score)
                }
            })
        };
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
    context: &mut SearchContext<'_>,
    pv: &mut Vec<Move>,
) -> Option<i32> {
    if !context.visit_node(ply) {
        return None;
    }
    if ply >= MAX_SEARCH_PLY {
        return Some(evaluate(position));
    }
    if depth <= 0 {
        return quiescence(position, alpha, beta, ply, false, context, pv);
    }

    let key = position.zobrist().main();
    context.transposition_table.prefetch(key);
    let original_alpha = alpha;
    let mut tt_move = None;
    if let Some(entry) = context.transposition_table.probe(key) {
        tt_move = entry.best_move;
        if !pv_node && i32::from(entry.depth) >= depth {
            let score = score_from_tt(i32::from(entry.score), ply);
            match entry.bound {
                Bound::Exact => return Some(score),
                Bound::Lower if score >= beta => return Some(score),
                Bound::Upper if score <= alpha => return Some(score),
                _ => {}
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
        child_pv.clear();
        let score = if searched == 0 {
            pvs(
                position,
                depth - 1,
                -beta,
                -alpha,
                ply + 1,
                pv_node,
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
                        context,
                        &mut child_pv,
                    )
                    .map(|score| -score)
                } else {
                    Some(score)
                }
            })
        };
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
    context.transposition_table.store(
        key,
        EntryData {
            best_move,
            score: score_to_tt(best_score, ply) as i16,
            // Static evaluation is not probed by the M2 search. Avoid recomputing the full HCE
            // solely to populate a field that becomes useful with M3 pruning.
            static_eval: UNEVALUATED_STATIC_EVAL,
            depth: depth.min(i32::from(u8::MAX)) as u8,
            bound,
            age: 0,
            pv: pv_node,
        },
    );
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
            return Some(best_score);
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

    if in_check && searched == 0 {
        Some(-MATE_SCORE + ply as i32)
    } else {
        Some(best_score)
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
    limits: SearchLimits,
    started: Instant,
    nodes: u64,
    seldepth: u32,
    iterations: Vec<IterationInfo>,
    killers: [[Option<Move>; 2]; MAX_SEARCH_PLY],
    stopped: bool,
}

impl<'a> SearchContext<'a> {
    fn new(
        transposition_table: &'a TranspositionTable,
        limits: SearchLimits,
        started: Instant,
    ) -> Self {
        Self {
            transposition_table,
            limits,
            started,
            nodes: 0,
            seldepth: 0,
            iterations: Vec::new(),
            killers: [[None; 2]; MAX_SEARCH_PLY],
            stopped: false,
        }
    }

    fn visit_node(&mut self, ply: usize) -> bool {
        if self.stopped {
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
