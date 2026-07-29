use std::collections::HashMap;

use mf_core::Move;

use crate::{MATE_SCORE, MAX_SEARCH_PLY, SearchResult};

const DECISIVE_SCORE: i32 = MATE_SCORE - 2 * MAX_SEARCH_PLY as i32 - 1;

#[allow(dead_code)]
pub(crate) fn select_best_result(results: &[SearchResult]) -> usize {
    let eligible: Vec<_> = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| {
            (result.depth > 0)
                .then_some(result.best_move)
                .flatten()
                .map(|best_move| (index, result, best_move))
        })
        .collect();

    let Some(min_score) = eligible.iter().map(|(_, result, _)| result.score).min() else {
        return 0;
    };

    let mut votes = HashMap::<Move, i64>::new();
    for (_, result, best_move) in &eligible {
        let weight = i64::from(result.score) - i64::from(min_score) + 14;
        *votes.entry(*best_move).or_default() += weight;
    }

    let mut best = eligible[0];
    for candidate in eligible.into_iter().skip(1) {
        let best_is_decisive = is_decisive(best.1.score);
        let candidate_is_decisive = is_decisive(candidate.1.score);

        if best_is_decisive {
            let consistent_outcome = best.1.score.signum() == candidate.1.score.signum();
            if candidate_is_decisive
                && consistent_outcome
                && score_magnitude(candidate.1.score) > score_magnitude(best.1.score)
            {
                best = candidate;
            }
            continue;
        }

        if candidate_is_decisive {
            best = candidate;
            continue;
        }

        let best_vote = votes[&best.2];
        let candidate_vote = votes[&candidate.2];
        if candidate_vote > best_vote
            || (candidate_vote == best_vote && candidate.1.pv.len() > best.1.pv.len())
        {
            best = candidate;
        }
    }

    best.0
}

fn is_decisive(score: i32) -> bool {
    score_magnitude(score) >= i64::from(DECISIVE_SCORE)
}

fn score_magnitude(score: i32) -> i64 {
    i64::from(score).abs()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mf_core::{Move, MoveFlag, Square};

    use super::select_best_result;
    use crate::SearchResult;

    fn chess_move(from: u8, to: u8) -> Move {
        Move::new(
            Square::new(from).expect("test square must be valid"),
            Square::new(to).expect("test square must be valid"),
            MoveFlag::QUIET,
        )
    }

    fn result(depth: u32, score: i32, best_move: Option<Move>, pv_len: usize) -> SearchResult {
        SearchResult {
            best_move,
            score,
            depth,
            seldepth: depth,
            nodes: 0,
            hashfull: 0,
            elapsed: Duration::ZERO,
            pv: best_move.into_iter().cycle().take(pv_len).collect(),
            iterations: Vec::new(),
        }
    }

    #[test]
    fn depth_zero_workers_do_not_vote() {
        let move_a = chess_move(0, 1);
        let move_b = chess_move(2, 3);
        let results = [
            result(0, 500, Some(move_a), 1),
            result(4, 20, Some(move_b), 2),
        ];

        assert_eq!(select_best_result(&results), 1);
    }

    #[test]
    fn workers_choosing_the_same_move_combine_their_votes() {
        let move_a = chess_move(0, 1);
        let move_b = chess_move(2, 3);
        let results = [
            result(4, 20, Some(move_a), 1),
            result(4, 20, Some(move_a), 1),
            result(4, 30, Some(move_b), 1),
        ];

        assert_eq!(select_best_result(&results), 0);
    }

    #[test]
    fn decisive_result_overrides_the_ordinary_vote_winner() {
        let move_a = chess_move(0, 1);
        let move_b = chess_move(2, 3);
        let results = [
            result(4, 0, Some(move_a), 1),
            result(4, 0, Some(move_a), 1),
            result(4, -29_743, Some(move_b), 1),
        ];

        assert_eq!(select_best_result(&results), 2);
    }

    #[test]
    fn larger_absolute_decisive_score_selects_the_shortest_conversion() {
        let move_a = chess_move(0, 1);
        let move_b = chess_move(2, 3);
        let results = [
            result(4, 29_750, Some(move_a), 1),
            result(4, 29_760, Some(move_b), 1),
        ];

        assert_eq!(select_best_result(&results), 1);
    }

    #[test]
    fn equal_vote_totals_prefer_the_longer_pv() {
        let move_a = chess_move(0, 1);
        let move_b = chess_move(2, 3);
        let results = [
            result(4, 20, Some(move_a), 1),
            result(4, 20, Some(move_b), 2),
        ];

        assert_eq!(select_best_result(&results), 1);
    }

    #[test]
    fn equal_votes_and_pv_lengths_prefer_the_lower_worker_index() {
        let move_a = chess_move(0, 1);
        let move_b = chess_move(2, 3);
        let results = [
            result(4, 20, Some(move_a), 1),
            result(4, 20, Some(move_b), 1),
        ];

        assert_eq!(select_best_result(&results), 0);
    }

    #[test]
    fn no_move_results_are_ineligible_and_empty_voting_falls_back_to_worker_zero() {
        let results = [
            result(5, -29_743, None, 0),
            result(0, 10, Some(chess_move(0, 1)), 1),
        ];

        assert_eq!(select_best_result(&results), 0);
        assert_eq!(select_best_result(&[]), 0);
    }
}
