use crate::{
    Bitboard, CastlingRights, CastlingSide, Color, Move, Piece, PieceKind, Square, ZobristKeys,
};

const COLOR_COUNT: usize = 2;
const PIECE_KIND_COUNT: usize = 6;

/// Complete reversible state for a chess position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    board: [Option<Piece>; 64],
    pieces: [[Bitboard; PIECE_KIND_COUNT]; COLOR_COUNT],
    colors: [Bitboard; COLOR_COUNT],
    side_to_move: Color,
    castling: CastlingRights,
    en_passant: Option<Square>,
    halfmove_clock: u16,
    fullmove_number: u16,
    zobrist: ZobristKeys,
}

/// Information required to restore a position after [`Position::make_move`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undo {
    moved: Piece,
    captured: Option<(Square, Piece)>,
    previous: ReversibleState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReversibleState {
    side_to_move: Color,
    castling: CastlingRights,
    en_passant: Option<Square>,
    halfmove_clock: u16,
    fullmove_number: u16,
    zobrist: ZobristKeys,
}

impl Position {
    /// Creates a position without pieces, castling rights, or en-passant state.
    pub fn empty(side_to_move: Color) -> Self {
        let mut position = Self {
            board: [None; 64],
            pieces: [[Bitboard::EMPTY; PIECE_KIND_COUNT]; COLOR_COUNT],
            colors: [Bitboard::EMPTY; COLOR_COUNT],
            side_to_move: Color::White,
            castling: CastlingRights::default(),
            en_passant: None,
            halfmove_clock: 0,
            fullmove_number: 1,
            zobrist: ZobristKeys::with_empty_material_counts(),
        };
        position.set_side_to_move(side_to_move);
        position
    }

    /// Creates the standard chess starting position.
    pub fn startpos() -> Self {
        let mut position = Self::empty(Color::White);
        let back_rank = [
            PieceKind::Rook,
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Queen,
            PieceKind::King,
            PieceKind::Bishop,
            PieceKind::Knight,
            PieceKind::Rook,
        ];

        for (file, kind) in back_rank.into_iter().enumerate() {
            position.place_piece(square(file as u8, 0), Piece::new(Color::White, kind));
            position.place_piece(square(file as u8, 7), Piece::new(Color::Black, kind));
            position.place_piece(
                square(file as u8, 1),
                Piece::new(Color::White, PieceKind::Pawn),
            );
            position.place_piece(
                square(file as u8, 6),
                Piece::new(Color::Black, PieceKind::Pawn),
            );
        }

        position.set_castling_rook(Color::White, CastlingSide::QueenSide, Some(square(0, 0)));
        position.set_castling_rook(Color::White, CastlingSide::KingSide, Some(square(7, 0)));
        position.set_castling_rook(Color::Black, CastlingSide::QueenSide, Some(square(0, 7)));
        position.set_castling_rook(Color::Black, CastlingSide::KingSide, Some(square(7, 7)));
        position
    }

    #[inline]
    pub const fn side_to_move(&self) -> Color {
        self.side_to_move
    }

    pub fn set_side_to_move(&mut self, color: Color) {
        if self.side_to_move != color {
            self.zobrist.toggle_side();
            self.side_to_move = color;
        }
    }

    #[inline]
    pub const fn castling_rights(&self) -> CastlingRights {
        self.castling
    }

    #[inline]
    pub const fn castling_rook(&self, color: Color, side: CastlingSide) -> Option<Square> {
        self.castling.rook(color, side)
    }

    /// Sets or clears one castling rook.
    ///
    /// A present right must name the matching friendly rook on the same back rank and wing as
    /// the king. This keeps the side metadata and Chess960 rook geometry consistent.
    pub fn set_castling_rook(&mut self, color: Color, side: CastlingSide, rook: Option<Square>) {
        if let Some(rook) = rook {
            let king = self
                .pieces(color, PieceKind::King)
                .first()
                .expect("setting a castling rook requires the king to be present");
            assert_eq!(
                self.piece_at(rook),
                Some(Piece::new(color, PieceKind::Rook)),
                "castling right must reference a friendly rook"
            );
            assert_eq!(
                king.rank(),
                color.back_rank(),
                "castling king must be on its back rank"
            );
            assert_eq!(
                rook.rank(),
                color.back_rank(),
                "castling rook must be on its back rank"
            );
            assert_eq!(
                CastlingSide::from_rook_origin(king, rook),
                side,
                "castling side must match king and rook geometry"
            );
        }

        let previous = self.castling.rook(color, side);
        if previous == rook {
            return;
        }
        if let Some(square) = previous {
            self.zobrist.toggle_castling(color, side, square);
        }
        self.castling.set_rook(color, side, rook);
        if let Some(square) = rook {
            self.zobrist.toggle_castling(color, side, square);
        }
    }

    #[inline]
    pub const fn en_passant(&self) -> Option<Square> {
        self.en_passant
    }

    pub fn set_en_passant(&mut self, en_passant: Option<Square>) {
        if self.en_passant == en_passant {
            return;
        }
        if let Some(square) = self.en_passant {
            self.zobrist.toggle_en_passant(square);
        }
        self.en_passant = en_passant;
        if let Some(square) = en_passant {
            self.zobrist.toggle_en_passant(square);
        }
    }

    #[inline]
    pub const fn halfmove_clock(&self) -> u16 {
        self.halfmove_clock
    }

    #[inline]
    pub fn set_halfmove_clock(&mut self, clock: u16) {
        self.halfmove_clock = clock;
    }

    #[inline]
    pub const fn fullmove_number(&self) -> u16 {
        self.fullmove_number
    }

    #[inline]
    pub fn set_fullmove_number(&mut self, number: u16) {
        self.fullmove_number = number;
    }

    #[inline]
    pub const fn piece_at(&self, square: Square) -> Option<Piece> {
        self.board[square.index() as usize]
    }

    #[inline]
    pub const fn pieces(&self, color: Color, kind: PieceKind) -> Bitboard {
        self.pieces[color.index()][kind.index()]
    }

    #[inline]
    pub const fn color_occupancy(&self, color: Color) -> Bitboard {
        self.colors[color.index()]
    }

    #[inline]
    pub const fn occupancy(&self) -> Bitboard {
        Bitboard::new(
            self.colors[Color::White.index()].bits() | self.colors[Color::Black.index()].bits(),
        )
    }

    #[inline]
    pub const fn zobrist(&self) -> ZobristKeys {
        self.zobrist
    }

    /// Adds or replaces a piece while maintaining all bitboards and hashes.
    pub fn place_piece(&mut self, square: Square, piece: Piece) {
        if self.piece_at(square) == Some(piece) {
            return;
        }
        self.remove_piece_at(square);
        self.add_piece(square, piece);
    }

    /// Recomputes all five keys from board state for differential validation.
    pub fn recompute_zobrist(&self) -> ZobristKeys {
        let mut keys = ZobristKeys::with_empty_material_counts();

        for index in 0..64 {
            if let Some(piece) = self.board[index] {
                keys.toggle_piece(piece, Square::new(index as u8).unwrap());
            }
        }
        if self.side_to_move == Color::Black {
            keys.toggle_side();
        }
        if let Some(en_passant) = self.en_passant {
            keys.toggle_en_passant(en_passant);
        }
        for color in Color::ALL {
            for side in CastlingSide::ALL {
                if let Some(rook) = self.castling.rook(color, side) {
                    keys.toggle_castling(color, side, rook);
                }
            }
            for kind in PieceKind::NON_PAWN_MATERIAL {
                keys.toggle_material_count(color, kind, 0);
                keys.toggle_material_count(
                    color,
                    kind,
                    self.pieces[color.index()][kind.index()].count() as u8,
                );
            }
        }
        keys
    }

    /// Applies a legal move in place and returns the state needed to unmake it.
    pub fn make_move(&mut self, mv: Move) -> Undo {
        let mover = self.side_to_move;
        let moved = self
            .piece_at(mv.from())
            .expect("make_move requires a piece on the source square");
        assert_eq!(
            moved.color(),
            mover,
            "make_move requires a piece belonging to the side to move"
        );

        let previous = self.reversible_state();
        self.set_en_passant(None);

        let captured = if mv.flag().is_castling() {
            self.make_castling(mv, moved);
            None
        } else {
            self.make_non_castling(mv, moved)
        };

        if moved.kind() == PieceKind::Pawn || captured.is_some() {
            self.halfmove_clock = 0;
        } else {
            self.halfmove_clock = self.halfmove_clock.saturating_add(1);
        }
        if mover == Color::Black {
            self.fullmove_number = self.fullmove_number.saturating_add(1);
        }
        self.set_side_to_move(!mover);

        Undo {
            moved,
            captured,
            previous,
        }
    }

    /// Restores the exact state that preceded a matching [`Position::make_move`].
    pub fn unmake_move(&mut self, mv: Move, undo: Undo) {
        if mv.flag().is_castling() {
            let color = undo.moved.color();
            let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
            let king_destination = side.king_destination(color);
            let rook_destination = side.rook_destination(color);
            let king = self
                .remove_piece_raw(king_destination)
                .expect("castled king must occupy its destination");
            let rook = self
                .remove_piece_raw(rook_destination)
                .expect("castled rook must occupy its destination");
            self.add_piece_raw(mv.from(), king);
            self.add_piece_raw(mv.to(), rook);
        } else {
            self.remove_piece_raw(mv.to())
                .expect("made move must occupy its destination");
            self.add_piece_raw(mv.from(), undo.moved);
            if let Some((square, piece)) = undo.captured {
                self.add_piece_raw(square, piece);
            }
        }

        self.restore_reversible_state(undo.previous);
    }

    fn make_castling(&mut self, mv: Move, king: Piece) {
        assert_eq!(king.kind(), PieceKind::King);
        let rook = self
            .piece_at(mv.to())
            .expect("castling move must encode the rook origin");
        assert_eq!(
            rook,
            Piece::new(king.color(), PieceKind::Rook),
            "castling destination must contain a friendly rook"
        );

        let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
        let king_destination = side.king_destination(king.color());
        let rook_destination = side.rook_destination(king.color());

        self.remove_piece_raw(mv.from());
        self.remove_piece_raw(mv.to());
        self.add_piece_raw(king_destination, king);
        self.add_piece_raw(rook_destination, rook);
        self.zobrist.toggle_piece(king, mv.from());
        self.zobrist.toggle_piece(king, king_destination);
        self.zobrist.toggle_piece(rook, mv.to());
        self.zobrist.toggle_piece(rook, rook_destination);
        self.clear_castling_color(king.color());
    }

    fn make_non_castling(&mut self, mv: Move, moved: Piece) -> Option<(Square, Piece)> {
        let capture_square = if mv.flag().is_en_passant() {
            let offset = if moved.color() == Color::White { -8 } else { 8 };
            Square::new((i16::from(mv.to().index()) + offset) as u8)
                .expect("en-passant capture square must be on the board")
        } else {
            mv.to()
        };
        let captured = if mv.flag().is_capture() {
            Some((
                capture_square,
                self.remove_piece_at(capture_square)
                    .expect("capture move requires a victim"),
            ))
        } else {
            None
        };

        if let Some(kind) = mv.flag().promotion() {
            self.remove_piece_at(mv.from());
            self.add_piece(mv.to(), Piece::new(moved.color(), kind));
        } else {
            self.relocate_piece(mv.from(), mv.to(), moved);
        }

        match moved.kind() {
            PieceKind::King => self.clear_castling_color(moved.color()),
            PieceKind::Rook => self.clear_castling_rook(moved.color(), mv.from()),
            _ => {}
        }
        if let Some((square, piece)) = captured
            && piece.kind() == PieceKind::Rook
        {
            self.clear_castling_rook(piece.color(), square);
        }
        if mv.flag().is_double_pawn_push() {
            let target = (u16::from(mv.from().index()) + u16::from(mv.to().index())) / 2;
            self.set_en_passant(Square::new(target as u8));
        }

        captured
    }

    fn add_piece(&mut self, square: Square, piece: Piece) {
        let color = piece.color().index();
        let kind = piece.kind().index();
        if piece.kind().is_non_pawn_material() {
            let count = self.pieces[color][kind].count() as u8;
            self.zobrist
                .toggle_material_count(piece.color(), piece.kind(), count);
            self.zobrist
                .toggle_material_count(piece.color(), piece.kind(), count + 1);
        }
        self.add_piece_raw(square, piece);
        self.zobrist.toggle_piece(piece, square);
    }

    fn remove_piece_at(&mut self, square: Square) -> Option<Piece> {
        let piece = self.piece_at(square)?;
        let color = piece.color().index();
        let kind = piece.kind().index();
        if piece.kind().is_non_pawn_material() {
            let count = self.pieces[color][kind].count() as u8;
            self.zobrist
                .toggle_material_count(piece.color(), piece.kind(), count);
            self.zobrist
                .toggle_material_count(piece.color(), piece.kind(), count - 1);
        }
        self.remove_piece_raw(square);
        self.zobrist.toggle_piece(piece, square);
        Some(piece)
    }

    fn relocate_piece(&mut self, from: Square, to: Square, piece: Piece) {
        assert_eq!(
            self.remove_piece_raw(from),
            Some(piece),
            "relocation source must contain the moved piece"
        );
        self.add_piece_raw(to, piece);
        self.zobrist.toggle_piece(piece, from);
        self.zobrist.toggle_piece(piece, to);
    }

    fn add_piece_raw(&mut self, square: Square, piece: Piece) {
        assert!(
            self.piece_at(square).is_none(),
            "cannot add two pieces to one square"
        );
        let color = piece.color().index();
        let kind = piece.kind().index();
        self.board[square.index() as usize] = Some(piece);
        self.pieces[color][kind].set(square);
        self.colors[color].set(square);
    }

    fn remove_piece_raw(&mut self, square: Square) -> Option<Piece> {
        let piece = self.board[square.index() as usize]?;
        let color = piece.color().index();
        let kind = piece.kind().index();
        self.board[square.index() as usize] = None;
        self.pieces[color][kind].clear(square);
        self.colors[color].clear(square);
        Some(piece)
    }

    fn clear_castling_color(&mut self, color: Color) {
        for side in CastlingSide::ALL {
            self.set_castling_rook(color, side, None);
        }
    }

    fn clear_castling_rook(&mut self, color: Color, square: Square) {
        for side in CastlingSide::ALL {
            if self.castling.rook(color, side) == Some(square) {
                self.set_castling_rook(color, side, None);
            }
        }
    }

    fn reversible_state(&self) -> ReversibleState {
        ReversibleState {
            side_to_move: self.side_to_move,
            castling: self.castling,
            en_passant: self.en_passant,
            halfmove_clock: self.halfmove_clock,
            fullmove_number: self.fullmove_number,
            zobrist: self.zobrist,
        }
    }

    fn restore_reversible_state(&mut self, state: ReversibleState) {
        self.side_to_move = state.side_to_move;
        self.castling = state.castling;
        self.en_passant = state.en_passant;
        self.halfmove_clock = state.halfmove_clock;
        self.fullmove_number = state.fullmove_number;
        self.zobrist = state.zobrist;
    }
}

#[inline]
fn square(file: u8, rank: u8) -> Square {
    Square::new(rank * 8 + file).expect("file and rank are in 0..8")
}
