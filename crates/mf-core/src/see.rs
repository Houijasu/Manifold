use crate::attacks::{king_attacks, knight_attacks, pawn_attacks};
use crate::{
    Bitboard, Color, Move, Piece, PieceKind, Position, Square, bishop_attacks, material_value,
    rook_attacks,
};

#[cfg(test)]
const PROMOTIONS: [PieceKind; 4] = [
    PieceKind::Knight,
    PieceKind::Bishop,
    PieceKind::Rook,
    PieceKind::Queen,
];

pub fn static_exchange_evaluation(position: &Position, mv: Move) -> i32 {
    #[cfg(feature = "instrumentation")]
    let started = crate::instrumentation::cycles();
    let value = static_exchange_evaluation_greedy(position, mv);
    #[cfg(feature = "instrumentation")]
    crate::instrumentation::record_see(crate::instrumentation::cycles().wrapping_sub(started));
    value
}

fn static_exchange_evaluation_greedy(position: &Position, mv: Move) -> i32 {
    let (mut state, mover, initial_gain) = prepare_exchange(position, mv);
    let mut gains = [0; 32];
    gains[0] = initial_gain;
    let mut depth = 0usize;
    let mut side = !mover.color();
    let mut attackers = [
        state.attackers_to(state.target, Color::White),
        state.attackers_to(state.target, Color::Black),
    ];

    while depth + 1 < gains.len() {
        let Some(next) = state.best_greedy_recapture(side, &attackers) else {
            break;
        };
        let victim = state
            .target_piece
            .expect("a recapture sequence must have a victim on the target");
        depth += 1;
        let exchange_value = exchange_value(victim.kind()) + next.promotion_gain;
        gains[depth] = if depth.is_multiple_of(2) {
            gains[depth - 1] + exchange_value
        } else {
            gains[depth - 1] - exchange_value
        };
        let from = next.from;
        state.apply_recapture(next);
        state.reveal_xray(from, &mut attackers);
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

fn prepare_exchange(position: &Position, mv: Move) -> (SeeState<'_>, Piece, i32) {
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

    let mut state = SeeState::from_position(position, mv.to());
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
        exchange_value(captured.kind())
    } else {
        0
    };
    let promotion_gain = mv.flag().promotion().map_or(0, |kind| {
        exchange_value(kind) - exchange_value(PieceKind::Pawn)
    });

    (state, mover, capture_gain + promotion_gain)
}

/// An exchange in progress, projected over the originating position instead of copied from
/// it. Piece sets are `Position`'s bitboards masked by the live `occupied`; the only square
/// whose occupant differs from the projection rule is `target`, carried by `target_piece`.
/// That is also why `place` only ever lands on the target: nowhere else can receive a piece.
#[derive(Clone)]
struct SeeState<'p> {
    position: &'p Position,
    target: Square,
    occupied: Bitboard,
    target_piece: Option<Piece>,
    kings: [Square; 2],
    /// Empty-board diagonals from the target, constant for the whole exchange:
    /// `reveal_xray` recomputed these on every call, though the target never moves.
    target_diagonal: Bitboard,
    /// Empty-board orthogonals from the target, hoisted for the same reason.
    target_orthogonal: Bitboard,
}

impl<'p> SeeState<'p> {
    fn from_position(position: &'p Position, target: Square) -> Self {
        let mut kings = [Square::new(0).unwrap(); 2];
        for color in Color::ALL {
            kings[color.index()] = position
                .pieces(color, PieceKind::King)
                .first()
                .expect("SEE requires exactly one king per side");
        }

        Self {
            position,
            target,
            occupied: position.occupancy(),
            target_piece: position.piece_at(target),
            kings,
            target_diagonal: bishop_attacks(target, Bitboard::EMPTY),
            target_orthogonal: rook_attacks(target, Bitboard::EMPTY),
        }
    }

    /// The piece on a square right now: `target_piece` on the target; elsewhere the
    /// position's piece, provided its occupancy bit has not been cleared mid-exchange.
    #[inline]
    fn piece_at(&self, square: Square) -> Option<Piece> {
        if square == self.target {
            return self.target_piece;
        }
        if self.occupied.contains(square) {
            self.position.piece_at(square)
        } else {
            None
        }
    }

    /// The live set of one color/kind: the position's bitboard masked by live occupancy,
    /// with the target bit replaced by `target_piece`'s membership (the position still
    /// shows the pre-exchange occupant there, and promotions change the kind).
    #[inline]
    fn pieces_of(&self, color: Color, kind: PieceKind) -> Bitboard {
        let mut set = self.position.pieces(color, kind) & self.occupied;
        if let Some(piece) = self.target_piece
            && piece.color() == color
            && piece.kind() == kind
        {
            set.set(self.target);
            return set;
        }
        set.clear(self.target);
        set
    }

    fn remove(&mut self, square: Square) -> Option<Piece> {
        if square == self.target {
            let piece = self.target_piece?;
            self.target_piece = None;
            self.occupied.clear(square);
            return Some(piece);
        }
        if !self.occupied.contains(square) {
            return None;
        }
        self.occupied.clear(square);
        let piece = self
            .position
            .piece_at(square)
            .expect("set occupancy implies a position piece");
        Some(piece)
    }

    fn place(&mut self, square: Square, piece: Piece) {
        assert!(
            square == self.target,
            "SEE only ever places on the exchange target"
        );
        assert!(self.piece_at(square).is_none());
        self.target_piece = Some(piece);
        self.occupied.set(square);
        if piece.kind() == PieceKind::King {
            self.kings[piece.color().index()] = square;
        }
    }

    /// The first legal recapture in king-first, then kind-ascending order. Callers must
    /// pass `attackers_to(target, side)` and must have already excluded the terminal cases
    /// (no victim, own piece, or king on the target).
    fn least_valuable_legal_recapture(
        &mut self,
        side: Color,
        attackers: Bitboard,
    ) -> Option<Recapture> {
        let target = self.target;
        for from in attackers & self.pieces_of(side, PieceKind::King) {
            if let Some(recapture) = self.legal_recapture_from(side, from, PieceKind::King) {
                return Some(recapture);
            }
        }
        for kind in PieceKind::ALL {
            if kind == PieceKind::King {
                continue;
            }
            for from in attackers & self.pieces_of(side, kind) {
                let placed_kind = if kind == PieceKind::Pawn && target.rank() == (!side).back_rank()
                {
                    PieceKind::Queen
                } else {
                    kind
                };
                if let Some(recapture) = self.legal_recapture_from(side, from, placed_kind) {
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
    ///
    /// Takes both colors' maintained attacker sets (the ones the caller's walk owns)
    /// rather than rebuilding them: the trial recapture has already been applied, and the
    /// caller maintains the sets across it with the same `reveal_xray` cadence the main
    /// walk uses, so a rebuild would recompute what the maintained sets already say.
    fn best_greedy_recapture(
        &mut self,
        side: Color,
        attackers_by_color: &[Bitboard; 2],
    ) -> Option<Recapture> {
        let victim = self.target_piece?;
        if victim.color() == side || victim.kind() == PieceKind::King {
            return None;
        }

        let attackers = attackers_by_color[side.index()] & self.occupied;
        if attackers.is_empty() {
            return None;
        }
        let lva = self.least_valuable_legal_recapture(side, attackers)?;
        if attackers.count() == 1 {
            return Some(lva);
        }

        let target = self.target;
        let mut best = None;
        let mut best_gain = i32::MIN;
        for kind in PieceKind::ALL {
            for from in attackers & self.pieces_of(side, kind) {
                let placed_kind = if kind == PieceKind::Pawn && target.rank() == (!side).back_rank()
                {
                    PieceKind::Queen
                } else {
                    kind
                };
                let Some(recapture) = self.legal_recapture_from(side, from, placed_kind) else {
                    continue;
                };
                let undo = self.apply_recapture(recapture);
                // Maintain the caller's sets across the trial with the same rule the
                // main walk applies: the vacated square can only reveal new target
                // attackers, and the vacated square itself is filtered out by the
                // `occupied` mask inside the continuation.
                let mut trial_attackers = *attackers_by_color;
                self.reveal_xray(from, &mut trial_attackers);
                let continuation = self.greedy_lva_recapture_gain(!side, trial_attackers);
                self.undo_recapture(undo);
                let gain = exchange_value(victim.kind()) + recapture.promotion_gain - continuation;
                if gain > best_gain {
                    best_gain = gain;
                    best = Some(recapture);
                }
            }
        }
        best
    }

    /// Evaluates the alternating LVA continuation from the current state, restoring the
    /// state exactly as found before returning. The attacker sets arrive maintained by
    /// the caller (see `best_greedy_recapture`): this path is only entered from the rare
    /// multi-attacker branch, which applies its trial recapture and reveals the x-rays
    /// behind it before handing the sets down.
    fn greedy_lva_recapture_gain(&mut self, side: Color, mut attackers: [Bitboard; 2]) -> i32 {
        let mut gains = [0; 32];
        let mut undos = [None; 32];
        let mut depth = 0usize;
        let mut current = side;

        while depth + 1 < gains.len() {
            let current_attackers = attackers[current.index()] & self.occupied;
            if current_attackers.is_empty() {
                break;
            }
            let Some(next) = self.least_valuable_legal_recapture(current, current_attackers) else {
                break;
            };
            let victim = self
                .target_piece
                .expect("a recapture sequence must have a victim on the target");
            depth += 1;
            let exchange_value = exchange_value(victim.kind()) + next.promotion_gain;
            gains[depth] = if depth.is_multiple_of(2) {
                gains[depth - 1] - exchange_value
            } else {
                gains[depth - 1] + exchange_value
            };
            let from = next.from;
            undos[depth - 1] = Some(self.apply_recapture(next));
            self.reveal_xray(from, &mut attackers);
            current = !current;
        }

        let mut result = gains[depth];
        while depth > 0 {
            result = if depth.is_multiple_of(2) {
                gains[depth - 1].min(result)
            } else {
                gains[depth - 1].max(result)
            };
            self.undo_recapture(
                undos[depth - 1]
                    .take()
                    .expect("every applied step has an undo"),
            );
            depth -= 1;
        }
        result
    }

    /// Reveals the piece a vacated source square was x-raying, if any. A removal can only
    /// reveal a slider on the single ray through `from` and the target: diagonal sources
    /// reveal bishops/queens, rank/file sources reveal rooks/queens, and knights, pawns,
    /// and kings never hide a target attacker. The revealed square belongs to whichever
    /// side owns the piece.
    fn reveal_xray(&self, from: Square, attackers: &mut [Bitboard; 2]) {
        let target = self.target;
        let diagonal = self.target_diagonal;
        let orthogonal = self.target_orthogonal;
        let sliders = if diagonal.contains(from) {
            bishop_attacks(target, self.occupied)
                & (self.pieces_of(Color::White, PieceKind::Bishop)
                    | self.pieces_of(Color::White, PieceKind::Queen)
                    | self.pieces_of(Color::Black, PieceKind::Bishop)
                    | self.pieces_of(Color::Black, PieceKind::Queen))
                & diagonal
        } else if orthogonal.contains(from) {
            rook_attacks(target, self.occupied)
                & (self.pieces_of(Color::White, PieceKind::Rook)
                    | self.pieces_of(Color::White, PieceKind::Queen)
                    | self.pieces_of(Color::Black, PieceKind::Rook)
                    | self.pieces_of(Color::Black, PieceKind::Queen))
                & orthogonal
        } else {
            Bitboard::EMPTY
        };
        for square in sliders {
            let color = self
                .piece_at(square)
                .expect("a revealed slider must have a piece")
                .color();
            attackers[color.index()].set(square);
        }
    }

    /// Trials a recapture on the live state and reports it if it keeps `side`'s king safe.
    /// The state is restored exactly as found, whether or not the trial was legal.
    ///
    /// The trial runs for EVERY recapture, not only king recaptures. A restricted
    /// king-only trial was tried and REJECTED by the differential below: a pinned
    /// non-king piece whose pin ray does not pass through the exchange target is a
    /// genuinely illegal recapture that no x-ray reveal corrects, so admitting it
    /// diverges from the exhaustive oracle (repro: FEN
    /// `2B2br1/2pp4/4p1qn/p1kP1P1R/P2n2P1/RPBK3N/2P1QP2/1N6 w - - 17 35`, `d5e6`
    /// reads 100 against the oracle's 0).
    fn legal_recapture_from(
        &mut self,
        side: Color,
        from: Square,
        placed_kind: PieceKind,
    ) -> Option<Recapture> {
        let attacker = self
            .piece_at(from)
            .expect("recapture source must contain an attacker");
        let undo = self.apply_recapture(Recapture {
            from,
            placed_kind,
            promotion_gain: 0,
        });
        let legal = !self.is_attacked(self.kings[side.index()], !side);
        self.undo_recapture(undo);
        if !legal {
            return None;
        }
        let promotion_gain = if attacker.kind() == PieceKind::Pawn && placed_kind != PieceKind::Pawn
        {
            exchange_value(placed_kind) - exchange_value(PieceKind::Pawn)
        } else {
            0
        };
        Some(Recapture {
            from,
            placed_kind,
            promotion_gain,
        })
    }

    /// Applies a recapture to the live state, removing the victim and the attacker and
    /// standing the (possibly promoted) attacker on the target.
    fn apply_recapture(&mut self, recapture: Recapture) -> RecaptureUndo {
        let target = self.target;
        let victim = self
            .remove(target)
            .expect("recapture target must contain a victim");
        let attacker = self
            .remove(recapture.from)
            .expect("recapture source must contain an attacker");
        self.place(target, Piece::new(attacker.color(), recapture.placed_kind));
        RecaptureUndo {
            from: recapture.from,
            attacker,
            victim,
        }
    }

    /// Inverts `apply_recapture` exactly. The attacker never goes back through `place`
    /// (only the target can receive a piece), so a king recapture restores `kings` here
    /// rather than as a side effect of placing.
    fn undo_recapture(&mut self, undo: RecaptureUndo) {
        let target = self.target;
        self.remove(target);
        self.place(target, undo.victim);
        self.occupied.set(undo.from);
        if undo.attacker.kind() == PieceKind::King {
            self.kings[undo.attacker.color().index()] = undo.from;
        }
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
            exchange_value(placed_kind) - exchange_value(PieceKind::Pawn)
        } else {
            0
        };
        exchange_value(victim.kind()) + promotion_gain - next.best_recapture(target, !side)
    }

    fn attackers_to(&self, target: Square, side: Color) -> Bitboard {
        let pawns = pawn_attacks(target, !side) & self.pieces_of(side, PieceKind::Pawn);
        let knights = knight_attacks(target) & self.pieces_of(side, PieceKind::Knight);
        let kings = king_attacks(target) & self.pieces_of(side, PieceKind::King);
        let diagonal = bishop_attacks(target, self.occupied)
            & (self.pieces_of(side, PieceKind::Bishop) | self.pieces_of(side, PieceKind::Queen));
        let orthogonal = rook_attacks(target, self.occupied)
            & (self.pieces_of(side, PieceKind::Rook) | self.pieces_of(side, PieceKind::Queen));
        pawns | knights | kings | diagonal | orthogonal
    }

    fn is_attacked(&self, target: Square, by: Color) -> bool {
        !self.attackers_to(target, by).is_empty()
    }
}

/// A chosen recapture: which piece moves onto the target, and what it promotes to.
#[derive(Clone, Copy)]
struct Recapture {
    from: Square,
    placed_kind: PieceKind,
    promotion_gain: i32,
}

/// What `apply_recapture` changed, exactly what `undo_recapture` needs to invert it.
#[derive(Clone, Copy)]
struct RecaptureUndo {
    from: Square,
    attacker: Piece,
    victim: Piece,
}

#[inline]
const fn exchange_value(kind: PieceKind) -> i32 {
    if matches!(kind, PieceKind::King) {
        20_000
    } else {
        material_value(kind)
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
        let (mut state, mover, _) = prepare_exchange(&position, mv);

        let recapture = state
            .least_valuable_legal_recapture(
                !mover.color(),
                state.attackers_to(mv.to(), !mover.color()),
            )
            .expect("white king on f2 can capture on g3");
        state.apply_recapture(recapture);
        assert_eq!(
            state.piece_at(mv.to()),
            Some(Piece::new(Color::White, PieceKind::King))
        );
        assert_eq!(static_exchange_evaluation_greedy(&position, mv), -800);
        assert_eq!(
            static_exchange_evaluation_greedy(&position, mv),
            static_exchange_evaluation_exhaustive(&position, mv)
        );
    }
}
