use crate::attacks::{king_attacks, knight_attacks, pawn_attacks};
use crate::{
    Bitboard, Color, Move, Piece, PieceKind, Position, Square, bishop_attacks, rook_attacks,
};

const PIECE_KIND_COUNT: usize = 6;
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

    capture_gain + promotion_gain - state.best_recapture(mv.to(), !mover.color())
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

    fn best_recapture(&self, target: Square, side: Color) -> i32 {
        let Some(victim) = self.piece_at(target) else {
            return 0;
        };
        if victim.color() == side {
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
