use mf_core::{
    Move, PieceKind, Position, generate_pseudo_legal_moves, material_value,
    static_exchange_evaluation,
};

use crate::evaluation::piece_square_value;

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

        good_captures.sort_unstable_by_key(|&mv| core::cmp::Reverse(capture_score(position, mv)));
        bad_captures.sort_unstable_by_key(|&mv| core::cmp::Reverse(capture_score(position, mv)));
        quiets.sort_unstable_by(|&left, &right| {
            quiet_score(position, right, killers)
                .cmp(&quiet_score(position, left, killers))
                .then_with(|| left.raw().cmp(&right.raw()))
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

pub(crate) fn quiescence_moves(position: &Position) -> Vec<Move> {
    let mut moves: Vec<_> = generate_pseudo_legal_moves(position)
        .iter()
        .copied()
        .filter(|mv| mv.flag().is_capture() || mv.flag().promotion().is_some())
        .filter(|&mv| static_exchange_evaluation(position, mv) >= 0)
        .collect();
    moves.sort_unstable_by_key(|&mv| core::cmp::Reverse(capture_score(position, mv)));
    moves
}

fn capture_score(position: &Position, mv: Move) -> i32 {
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
}

fn quiet_score(position: &Position, mv: Move, killers: [Option<Move>; 2]) -> i32 {
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
    piece_square_value(piece.kind(), piece.color(), mv.to())
        - piece_square_value(piece.kind(), piece.color(), mv.from())
}
