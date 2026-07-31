//! The position filters applied to every emitted training record.
//!
//! These are the canonical filters everyone copies from bullet's `examples/simple.rs`
//! and from viriformat's `Filter`, and `research/rust-perf-and-nnue-training.md`
//! reproduces both. The reason they exist is that NNUE learns a *static* evaluation:
//! a position whose true value comes from a forced tactical sequence teaches the
//! network noise, because the network cannot see the sequence.
//!
//! Three exclusions, and one deliberate inclusion:
//!
//! * **In check** — the side to move being in check means the position's value is
//!   dominated by the forced replies, not by static features.
//! * **Best move is tactical** (capture, en passant, or promotion) — the same
//!   argument. Note that this filters on the *best move*, not on whether the position
//!   contains any capture; a quiet position that merely has captures available is
//!   exactly what we want to keep.
//! * **Score out of bounds**, including mate scores — a mate score is not a
//!   centipawn quantity at all, and clamping rather than dropping it would teach the
//!   network that the bound value is a real evaluation.
//! * **Castling is KEPT.** viriformat's `Filter` sets `filter_castling: false`, and
//!   the validation contract requires this to be positively reported, because a
//!   naive "is the move special?" test would silently discard castling and strip the
//!   corpus of exactly the king-safety positions the net most needs.

use mf_core::{Color, Move, Position, is_in_check};

/// The default bound on `|score|`, in centipawns.
///
/// This is the SF-binpack filter's value, which bullet's own example uses.
/// viriformat's `max_eval` of 31,339 is a different quantity — it bounds a
/// *search* score rather than the emitted training label.
pub const DEFAULT_SCORE_BOUND: i32 = 10_000;

/// The score magnitude at or above which a value is treated as a mate score.
///
/// `mf-search` reports mates as `MATE_SCORE - ply`, so anything within
/// `MAX_SEARCH_PLY` of `MATE_SCORE` is a mate announcement rather than a centipawn
/// evaluation.
pub const MATE_SCORE_THRESHOLD: i32 = mf_search::MATE_SCORE - mf_search::MAX_SEARCH_PLY as i32;

/// Why a position was not emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rejection {
    /// The side to move is in check.
    InCheck,
    /// The best move is a capture, en passant, or a promotion.
    TacticalMove,
    /// `|score|` exceeds the configured bound.
    ScoreOutOfBounds,
    /// The score is a mate announcement, not a centipawn evaluation.
    MateScore,
    /// The search returned no best move (stalemate or checkmate).
    NoBestMove,
}

impl Rejection {
    /// Every variant, in report order.
    pub const ALL: [Self; 5] = [
        Self::InCheck,
        Self::TacticalMove,
        Self::ScoreOutOfBounds,
        Self::MateScore,
        Self::NoBestMove,
    ];

    /// The stable identifier used in `--check-filters` reports.
    ///
    /// The validation contract greps for these exact names, so they are part of the
    /// tool's interface and must not be renamed casually.
    pub const fn label(self) -> &'static str {
        match self {
            Self::InCheck => "in_check",
            Self::TacticalMove => "tactical_move",
            Self::ScoreOutOfBounds => "score_out_of_bounds",
            Self::MateScore => "mate_scores",
            Self::NoBestMove => "no_best_move",
        }
    }
}

/// The filter configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    /// The inclusive bound on `|score|` in centipawns.
    pub score_bound: i32,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            score_bound: DEFAULT_SCORE_BOUND,
        }
    }
}

impl Filter {
    /// Returns the reason `position` should be skipped, or `None` to keep it.
    ///
    /// `best_move` is the move the search chose, and `score` is its side-to-move-relative
    /// centipawn evaluation.
    pub fn rejection(
        self,
        position: &Position,
        best_move: Option<Move>,
        score: i32,
    ) -> Option<Rejection> {
        let Some(best_move) = best_move else {
            return Some(Rejection::NoBestMove);
        };
        if is_in_check(position, position.side_to_move()) {
            return Some(Rejection::InCheck);
        }
        if is_tactical(best_move) {
            return Some(Rejection::TacticalMove);
        }
        if score.abs() >= MATE_SCORE_THRESHOLD {
            return Some(Rejection::MateScore);
        }
        if score.abs() > self.score_bound {
            return Some(Rejection::ScoreOutOfBounds);
        }
        None
    }

    /// Whether `position` should be emitted.
    pub fn keeps(self, position: &Position, best_move: Option<Move>, score: i32) -> bool {
        self.rejection(position, best_move, score).is_none()
    }
}

/// Whether `mv` is a capture, en passant, or a promotion.
///
/// **Castling is not tactical** and returns `false` here. `MoveFlag::is_capture`
/// already covers en passant and promotion-captures, so the promotion test only needs
/// to add the quiet promotions.
pub fn is_tactical(mv: Move) -> bool {
    let flag = mv.flag();
    flag.is_capture() || flag.promotion().is_some()
}

/// Whether the side to move is in check.
pub fn in_check(position: &Position) -> bool {
    is_in_check(position, position.side_to_move())
}

/// Whether `color` is in check.
pub fn color_in_check(position: &Position, color: Color) -> bool {
    is_in_check(position, color)
}

#[cfg(test)]
mod tests {
    use super::{Filter, Rejection, is_tactical};
    use mf_core::{Move, MoveFlag, Position, Square, parse_uci_move};

    fn position(fen: &str) -> Position {
        Position::from_fen(fen, false).expect("test FEN parses")
    }

    fn quiet() -> Move {
        Move::new(
            Square::new(12).expect("e2"),
            Square::new(28).expect("e4"),
            MoveFlag::DOUBLE_PAWN_PUSH,
        )
    }

    #[test]
    fn a_quiet_position_with_a_quiet_best_move_is_kept() {
        assert_eq!(
            Filter::default().rejection(&Position::startpos(), Some(quiet()), 25),
            None
        );
    }

    #[test]
    fn a_position_where_the_side_to_move_is_in_check_is_rejected() {
        let checked = position("rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3");
        assert_eq!(
            Filter::default().rejection(&checked, Some(quiet()), 0),
            Some(Rejection::InCheck)
        );
    }

    #[test]
    fn a_position_whose_best_move_is_a_capture_is_rejected() {
        let pos = position("rnbqkbnr/ppp1pppp/8/3p4/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2");
        let capture = parse_uci_move(&pos, "e4d5", false).expect("exd5 is legal");
        assert!(capture.flag().is_capture());
        assert_eq!(
            Filter::default().rejection(&pos, Some(capture), 15),
            Some(Rejection::TacticalMove)
        );
    }

    #[test]
    fn a_position_whose_best_move_is_a_promotion_is_rejected() {
        let pos = position("8/4P3/8/8/8/8/6k1/4K3 w - - 0 1");
        let promotion = parse_uci_move(&pos, "e7e8q", false).expect("promotion is legal");
        assert!(promotion.flag().promotion().is_some());
        assert_eq!(
            Filter::default().rejection(&pos, Some(promotion), 900),
            Some(Rejection::TacticalMove)
        );
    }

    #[test]
    fn a_position_whose_best_move_is_en_passant_is_rejected() {
        let pos = position("rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3");
        let en_passant = parse_uci_move(&pos, "e5f6", false).expect("en passant is legal");
        assert!(en_passant.flag().is_en_passant());
        assert_eq!(
            Filter::default().rejection(&pos, Some(en_passant), 30),
            Some(Rejection::TacticalMove)
        );
    }

    #[test]
    fn castling_is_kept_because_the_canonical_filter_keeps_it() {
        let pos = position("rnbqk2r/pppp1ppp/5n2/2b1p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4");
        let castle = parse_uci_move(&pos, "e1g1", false).expect("O-O is legal");
        assert!(castle.flag().is_castling());
        assert!(!is_tactical(castle), "castling must not count as tactical");
        assert_eq!(Filter::default().rejection(&pos, Some(castle), 20), None);
    }

    #[test]
    fn a_score_beyond_the_bound_is_rejected_and_the_bound_itself_is_kept() {
        let filter = Filter { score_bound: 1_000 };
        let pos = Position::startpos();
        assert_eq!(filter.rejection(&pos, Some(quiet()), 1_000), None);
        assert_eq!(filter.rejection(&pos, Some(quiet()), -1_000), None);
        assert_eq!(
            filter.rejection(&pos, Some(quiet()), 1_001),
            Some(Rejection::ScoreOutOfBounds)
        );
        assert_eq!(
            filter.rejection(&pos, Some(quiet()), -1_001),
            Some(Rejection::ScoreOutOfBounds)
        );
    }

    #[test]
    fn a_mate_score_is_rejected_as_a_mate_rather_than_as_an_out_of_bounds_score() {
        let mate = mf_search::MATE_SCORE - 5;
        assert_eq!(
            Filter::default().rejection(&Position::startpos(), Some(quiet()), mate),
            Some(Rejection::MateScore)
        );
        assert_eq!(
            Filter::default().rejection(&Position::startpos(), Some(quiet()), -mate),
            Some(Rejection::MateScore)
        );
    }

    #[test]
    fn a_position_with_no_best_move_is_rejected() {
        assert_eq!(
            Filter::default().rejection(&Position::startpos(), None, 0),
            Some(Rejection::NoBestMove)
        );
    }

    #[test]
    fn every_rejection_reason_has_a_stable_report_label() {
        let labels: Vec<&str> = Rejection::ALL.iter().map(|r| r.label()).collect();
        assert_eq!(
            labels,
            [
                "in_check",
                "tactical_move",
                "score_out_of_bounds",
                "mate_scores",
                "no_best_move"
            ]
        );
    }
}
