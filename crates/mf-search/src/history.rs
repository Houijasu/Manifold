use mf_core::{Color, Move};

use crate::MAX_SEARCH_PLY;

const HISTORY_MAX: i32 = 16_384;

pub(crate) struct HistoryTables {
    killers: [[Option<Move>; 2]; MAX_SEARCH_PLY],
    quiet: Box<[[[i16; 64]; 64]; 2]>,
}

impl HistoryTables {
    pub(crate) fn new(thread_count: usize) -> Self {
        assert!(thread_count > 0, "thread count must be nonzero");

        Self {
            killers: [[None; 2]; MAX_SEARCH_PLY],
            quiet: Box::new([[[0; 64]; 64]; 2]),
        }
    }

    pub(crate) fn killers(&self, ply: usize) -> [Option<Move>; 2] {
        self.killers[ply]
    }

    pub(crate) fn record_killer(&mut self, ply: usize, mv: Move) {
        if ply >= MAX_SEARCH_PLY || self.killers[ply][0] == Some(mv) {
            return;
        }
        self.killers[ply][1] = self.killers[ply][0];
        self.killers[ply][0] = Some(mv);
    }

    pub(crate) fn quiet_score(&self, color: Color, mv: Move) -> i32 {
        i32::from(
            self.quiet[color.index()][usize::from(mv.from().index())][usize::from(mv.to().index())],
        )
    }

    pub(crate) fn update_quiet(&mut self, color: Color, mv: Move, bonus: i32) {
        let entry = &mut self.quiet[color.index()][usize::from(mv.from().index())]
            [usize::from(mv.to().index())];
        let current = i32::from(*entry);
        let bonus = bonus.clamp(-HISTORY_MAX, HISTORY_MAX);
        let updated = current + bonus - current * bonus.abs() / HISTORY_MAX;
        *entry = updated.clamp(-HISTORY_MAX, HISTORY_MAX) as i16;
    }
}

#[cfg(test)]
mod tests {
    use mf_core::{Color, Move, MoveFlag, Square};

    use super::{HISTORY_MAX, HistoryTables};

    fn first_move() -> Move {
        Move::new(
            Square::new(8).expect("valid square"),
            Square::new(16).expect("valid square"),
            MoveFlag::QUIET,
        )
    }

    fn second_move() -> Move {
        Move::new(
            Square::new(9).expect("valid square"),
            Square::new(17).expect("valid square"),
            MoveFlag::QUIET,
        )
    }

    #[test]
    fn killers_rotate_without_duplicates() {
        let mut history = HistoryTables::new(4);
        history.record_killer(3, first_move());
        history.record_killer(3, second_move());
        history.record_killer(3, second_move());

        assert_eq!(
            history.killers(3),
            [Some(second_move()), Some(first_move())]
        );
    }

    #[test]
    fn quiet_history_is_color_specific() {
        let mut history = HistoryTables::new(4);
        history.update_quiet(Color::White, first_move(), HISTORY_MAX);

        assert_eq!(history.quiet_score(Color::White, first_move()), HISTORY_MAX);
        assert_eq!(history.quiet_score(Color::Black, first_move()), 0);
    }

    #[test]
    fn quiet_history_uses_bounded_gravity_updates() {
        let mut history = HistoryTables::new(4);
        history.update_quiet(Color::White, first_move(), HISTORY_MAX / 2);
        history.update_quiet(Color::White, first_move(), HISTORY_MAX / 2);
        assert_eq!(
            history.quiet_score(Color::White, first_move()),
            3 * HISTORY_MAX / 4
        );

        history.update_quiet(Color::White, first_move(), -HISTORY_MAX / 2);
        assert_eq!(
            history.quiet_score(Color::White, first_move()),
            -HISTORY_MAX / 8
        );

        history.update_quiet(Color::White, first_move(), i32::MAX);
        assert_eq!(history.quiet_score(Color::White, first_move()), HISTORY_MAX);
        history.update_quiet(Color::White, first_move(), i32::MIN);
        assert_eq!(
            history.quiet_score(Color::White, first_move()),
            -HISTORY_MAX
        );
    }

    #[test]
    #[should_panic(expected = "thread count must be nonzero")]
    fn history_requires_a_nonzero_thread_count() {
        let _ = HistoryTables::new(0);
    }
}
