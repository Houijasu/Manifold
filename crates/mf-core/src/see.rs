use crate::attacks::{king_attacks, knight_attacks, pawn_attacks};
use crate::{
    Bitboard, Color, Move, Piece, PieceKind, Position, Square, bishop_attacks, rook_attacks,
};

const PIECE_KIND_COUNT: usize = 6;
#[cfg(test)]
const PROMOTIONS: [PieceKind; 4] = [
    PieceKind::Knight,
    PieceKind::Bishop,
    PieceKind::Rook,
    PieceKind::Queen,
];

/// Returns the optimal material result of exchanges on a legal move's destination square.
///
/// Values are measured from the moving side's perspective with pawn = 100. Castling is not
/// an exchange and is therefore outside this function's domain.
pub fn static_exchange_evaluation(position: &Position, mv: Move) -> i32 {
    static_exchange_evaluation_greedy(position, mv)
}

fn static_exchange_evaluation_greedy(position: &Position, mv: Move) -> i32 {
    let (mut state, mover, initial_gain) = prepare_exchange(position, mv);
    let mut gains = [0; 32];
    gains[0] = initial_gain;
    let mut depth = 0usize;
    let mut side = !mover.color();

    while depth + 1 < gains.len() {
        let Some(next) = state.best_greedy_recapture(mv.to(), side) else {
            break;
        };
        let victim = state
            .piece_at(mv.to())
            .expect("a recapture sequence must have a victim on the target");
        depth += 1;
        let exchange_value = piece_value(victim.kind()) + next.promotion_gain;
        gains[depth] = if depth.is_multiple_of(2) {
            gains[depth - 1] + exchange_value
        } else {
            gains[depth - 1] - exchange_value
        };
        state = next.state;
        side = !side;
    }

    let mut result = gains[depth];
    while depth > 0 {
        result = if depth.is_multiple_of(2) {
            gains[depth - 1].max(result)
        } else {
            gains[depth - 1].min(result)
        };
        depth -= 1;
    }
    result
}

#[cfg(test)]
fn static_exchange_evaluation_exhaustive(position: &Position, mv: Move) -> i32 {
    let (state, mover, initial_gain) = prepare_exchange(position, mv);
    initial_gain - state.best_recapture(mv.to(), !mover.color())
}

fn prepare_exchange(position: &Position, mv: Move) -> (SeeState, Piece, i32) {
    assert!(
        !mv.flag().is_castling(),
        "static exchange evaluation does not accept castling"
    );

    let mover = position
        .piece_at(mv.from())
        .expect("SEE requires a piece on the source square");
    assert_eq!(
        mover.color(),
        position.side_to_move(),
        "SEE requires a move by the side to move"
    );

    let mut state = SeeState::from_position(position);
    let captured = if mv.flag().is_en_passant() {
        let offset = if mover.color() == Color::White { -8 } else { 8 };
        let capture_square =
            Square::new((i16::from(mv.to().index()) + offset) as u8).expect("valid en passant");
        state
            .remove(capture_square)
            .expect("SEE en-passant move requires a captured pawn")
    } else if mv.flag().is_capture() {
        state
            .remove(mv.to())
            .expect("SEE capture move requires a victim")
    } else {
        assert!(
            state.piece_at(mv.to()).is_none(),
            "SEE quiet move requires an empty destination"
        );
        Piece::new(!mover.color(), PieceKind::Pawn)
    };

    let moved = state
        .remove(mv.from())
        .expect("SEE source piece disappeared");
    let placed = Piece::new(moved.color(), mv.flag().promotion().unwrap_or(moved.kind()));
    state.place(mv.to(), placed);

    let capture_gain = if mv.flag().is_capture() {
        piece_value(captured.kind())
    } else {
        0
    };
    let promotion_gain = mv
        .flag()
        .promotion()
        .map_or(0, |kind| piece_value(kind) - piece_value(PieceKind::Pawn));

    (state, mover, capture_gain + promotion_gain)
}

#[derive(Clone)]
struct SeeState {
    board: [Option<Piece>; 64],
    pieces: [[Bitboard; PIECE_KIND_COUNT]; 2],
    occupancy: Bitboard,
    kings: [Square; 2],
}

impl SeeState {
    fn from_position(position: &Position) -> Self {
        let mut board = [None; 64];
        for (index, entry) in board.iter_mut().enumerate() {
            let square = Square::new(index as u8).expect("board index is valid");
            *entry = position.piece_at(square);
        }

        let mut pieces = [[Bitboard::EMPTY; PIECE_KIND_COUNT]; 2];
        let mut kings = [Square::new(0).unwrap(); 2];
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                pieces[color.index()][kind.index()] = position.pieces(color, kind);
            }
            kings[color.index()] = position
                .pieces(color, PieceKind::King)
                .first()
                .expect("SEE requires exactly one king per side");
        }

        Self {
            board,
            pieces,
            occupancy: position.occupancy(),
            kings,
        }
    }

    #[inline]
    fn piece_at(&self, square: Square) -> Option<Piece> {
        self.board[square.index() as usize]
    }

    fn remove(&mut self, square: Square) -> Option<Piece> {
        let piece = self.board[square.index() as usize]?;
        self.board[square.index() as usize] = None;
        self.pieces[piece.color().index()][piece.kind().index()].clear(square);
        self.occupancy.clear(square);
        Some(piece)
    }

    fn place(&mut self, square: Square, piece: Piece) {
        assert!(self.piece_at(square).is_none());
        self.board[square.index() as usize] = Some(piece);
        self.pieces[piece.color().index()][piece.kind().index()].set(square);
        self.occupancy.set(square);
        if piece.kind() == PieceKind::King {
            self.kings[piece.color().index()] = square;
        }
    }

    fn least_valuable_legal_recapture(
        &self,
        target: Square,
        side: Color,
    ) -> Option<LegalRecapture> {
        let victim = self.piece_at(target)?;
        if victim.color() == side || victim.kind() == PieceKind::King {
            return None;
        }

        let attackers = self.attackers_to(target, side);
        for from in attackers & self.pieces[side.index()][PieceKind::King.index()] {
            if let Some(recapture) = self.legal_recapture_from(target, side, from, PieceKind::King)
            {
                return Some(recapture);
            }
        }
        for kind in PieceKind::ALL {
            if kind == PieceKind::King {
                continue;
            }
            for from in attackers & self.pieces[side.index()][kind.index()] {
                let placed_kind = if kind == PieceKind::Pawn && target.rank() == (!side).back_rank()
                {
                    PieceKind::Queen
                } else {
                    kind
                };
                if let Some(recapture) = self.legal_recapture_from(target, side, from, placed_kind)
                {
                    return Some(recapture);
                }
            }
        }
        None
    }

    /// Selects the best current recapture using an LVA swap loop for each continuation.
    ///
    /// The normal path is the classic single-attacker LVA choice. When several pieces can
    /// recapture now, choosing only the cheapest can diverge after pins and x-rays change, so
    /// each current alternative is scored by one bounded LVA continuation. Candidate branches
    /// are not expanded recursively.
    fn best_greedy_recapture(&self, target: Square, side: Color) -> Option<LegalRecapture> {
        let victim = self.piece_at(target)?;
        if victim.color() == side || victim.kind() == PieceKind::King {
            return None;
        }

        let attackers = self.attackers_to(target, side);
        let lva = self.least_valuable_legal_recapture(target, side)?;
        if attackers.count() == 1 {
            return Some(lva);
        }

        let mut best = None;
        let mut best_gain = i32::MIN;
        for kind in PieceKind::ALL {
            for from in attackers & self.pieces[side.index()][kind.index()] {
                let placed_kind = if kind == PieceKind::Pawn && target.rank() == (!side).back_rank()
                {
                    PieceKind::Queen
                } else {
                    kind
                };
                let Some(recapture) = self.legal_recapture_from(target, side, from, placed_kind)
                else {
                    continue;
                };
                let gain = piece_value(victim.kind()) + recapture.promotion_gain
                    - recapture.state.greedy_lva_recapture_gain(target, !side);
                if gain > best_gain {
                    best_gain = gain;
                    best = Some(recapture);
                }
            }
        }
        best
    }

    fn greedy_lva_recapture_gain(&self, target: Square, side: Color) -> i32 {
        let mut state = self.clone();
        let mut gains = [0; 32];
        let mut depth = 0usize;
        let mut current = side;

        while depth + 1 < gains.len() {
            let Some(next) = state.least_valuable_legal_recapture(target, current) else {
                break;
            };
            let victim = state
                .piece_at(target)
                .expect("a recapture sequence must have a victim on the target");
            depth += 1;
            let exchange_value = piece_value(victim.kind()) + next.promotion_gain;
            gains[depth] = if depth.is_multiple_of(2) {
                gains[depth - 1] - exchange_value
            } else {
                gains[depth - 1] + exchange_value
            };
            state = next.state;
            current = !current;
        }

        let mut result = gains[depth];
        while depth > 0 {
            result = if depth.is_multiple_of(2) {
                gains[depth - 1].min(result)
            } else {
                gains[depth - 1].max(result)
            };
            depth -= 1;
        }
        result
    }

    fn legal_recapture_from(
        &self,
        target: Square,
        side: Color,
        from: Square,
        placed_kind: PieceKind,
    ) -> Option<LegalRecapture> {
        let attacker = self
            .piece_at(from)
            .expect("recapture source must contain an attacker");
        let mut state = self.clone();
        state
            .remove(target)
            .expect("recapture target must contain a victim");
        state
            .remove(from)
            .expect("recapture source must contain an attacker");
        state.place(target, Piece::new(side, placed_kind));

        if state.is_attacked(state.kings[side.index()], !side) {
            return None;
        }
        let promotion_gain = if attacker.kind() == PieceKind::Pawn && placed_kind != PieceKind::Pawn
        {
            piece_value(placed_kind) - piece_value(PieceKind::Pawn)
        } else {
            0
        };
        Some(LegalRecapture {
            state,
            promotion_gain,
        })
    }

    #[cfg(test)]
    fn best_recapture(&self, target: Square, side: Color) -> i32 {
        let Some(victim) = self.piece_at(target) else {
            return 0;
        };
        if victim.color() == side || victim.kind() == PieceKind::King {
            return 0;
        }

        let mut best = 0;
        let attackers = self.attackers_to(target, side);
        for from in attackers {
            let attacker = self
                .piece_at(from)
                .expect("attacker bitboard must contain a piece");
            if attacker.kind() == PieceKind::Pawn && target.rank() == (!side).back_rank() {
                for promotion in PROMOTIONS {
                    best = best.max(self.recapture_gain(from, target, side, promotion));
                }
            } else {
                best = best.max(self.recapture_gain(from, target, side, attacker.kind()));
            }
        }
        best
    }

    #[cfg(test)]
    fn recapture_gain(
        &self,
        from: Square,
        target: Square,
        side: Color,
        placed_kind: PieceKind,
    ) -> i32 {
        let mut next = self.clone();
        let victim = next
            .remove(target)
            .expect("recapture target must contain a victim");
        let attacker = next
            .remove(from)
            .expect("recapture source must contain an attacker");
        next.place(target, Piece::new(side, placed_kind));

        if next.is_attacked(next.kings[side.index()], !side) {
            return i32::MIN;
        }

        let promotion_gain = if attacker.kind() == PieceKind::Pawn && placed_kind != PieceKind::Pawn
        {
            piece_value(placed_kind) - piece_value(PieceKind::Pawn)
        } else {
            0
        };
        piece_value(victim.kind()) + promotion_gain - next.best_recapture(target, !side)
    }

    fn attackers_to(&self, target: Square, side: Color) -> Bitboard {
        let pawns =
            pawn_attacks(target, !side) & self.pieces[side.index()][PieceKind::Pawn.index()];
        let knights = knight_attacks(target) & self.pieces[side.index()][PieceKind::Knight.index()];
        let kings = king_attacks(target) & self.pieces[side.index()][PieceKind::King.index()];
        let diagonal = bishop_attacks(target, self.occupancy)
            & (self.pieces[side.index()][PieceKind::Bishop.index()]
                | self.pieces[side.index()][PieceKind::Queen.index()]);
        let orthogonal = rook_attacks(target, self.occupancy)
            & (self.pieces[side.index()][PieceKind::Rook.index()]
                | self.pieces[side.index()][PieceKind::Queen.index()]);
        pawns | knights | kings | diagonal | orthogonal
    }

    fn is_attacked(&self, target: Square, by: Color) -> bool {
        !self.attackers_to(target, by).is_empty()
    }
}

struct LegalRecapture {
    state: SeeState,
    promotion_gain: i32,
}

#[inline]
const fn piece_value(kind: PieceKind) -> i32 {
    match kind {
        PieceKind::Pawn => 100,
        PieceKind::Knight => 320,
        PieceKind::Bishop => 330,
        PieceKind::Rook => 500,
        PieceKind::Queen => 900,
        PieceKind::King => 20_000,
    }
}

#[cfg(test)]
mod tests {
    use crate::{generate_legal_moves, parse_uci_move};

    use super::*;

    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn greedy_see_matches_exhaustive_oracle_on_random_reachable_positions() {
        let mut compared = 0usize;

        for seed in [0x5eed, 0xcafe, 0xd00d, 0xfade] {
            let mut position = Position::startpos();
            let mut random = seed;
            for _ in 0..10_000 {
                let moves = generate_legal_moves(&position);
                for &mv in &moves {
                    if mv.flag().is_capture() || mv.flag().promotion().is_some() {
                        assert_eq!(
                            static_exchange_evaluation_greedy(&position, mv),
                            static_exchange_evaluation_exhaustive(&position, mv),
                            "greedy SEE diverged for move {mv:?} in position {position:?}"
                        );
                        compared += 1;
                    }
                }

                if moves.is_empty() {
                    position = Position::startpos();
                    continue;
                }
                let mv = moves[next_random(&mut random) as usize % moves.len()];
                position.make_move(mv);
                if position.halfmove_clock() >= 100 {
                    position = Position::startpos();
                }
            }
        }

        assert!(
            compared >= 40_000,
            "random differential corpus was too capture-sparse: {compared}"
        );
    }

    #[test]
    fn greedy_see_matches_exhaustive_with_multiple_legal_recaptures() {
        let position = Position::from_fen(
            "2B2br1/2pp4/4p1qn/p1kP1P1R/P2n2P1/RPBK3N/2P1QP2/1N6 w - - 17 35",
            false,
        )
        .expect("regression FEN should parse");
        let mv = parse_uci_move(&position, "d5e6", false).expect("capture should be legal");
        let greedy = static_exchange_evaluation_greedy(&position, mv);

        assert_eq!(greedy, 0);
        assert_eq!(greedy, static_exchange_evaluation_exhaustive(&position, mv));
    }

    #[test]
    fn greedy_see_prioritizes_a_legal_king_capture_that_ends_the_exchange() {
        let position =
            Position::from_fen("n7/6r1/3k2rp/2pPqp2/2p3QP/6P1/5KN1/5B1R b - - 1 51", false)
                .expect("regression FEN should parse");
        let mv = parse_uci_move(&position, "e5g3", false).expect("capture should be legal");
        let (state, mover, _) = prepare_exchange(&position, mv);

        let recapture = state
            .least_valuable_legal_recapture(mv.to(), !mover.color())
            .expect("white king on f2 can capture on g3");
        assert_eq!(
            recapture.state.piece_at(mv.to()),
            Some(Piece::new(Color::White, PieceKind::King))
        );
        assert_eq!(static_exchange_evaluation_greedy(&position, mv), -800);
        assert_eq!(
            static_exchange_evaluation_greedy(&position, mv),
            static_exchange_evaluation_exhaustive(&position, mv)
        );
    }
}
