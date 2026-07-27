use mf_core::{Color, PieceKind, Position, Square};

pub(crate) const EVALUATION_LIMIT: i32 = 10_000;

pub(crate) fn evaluate(position: &Position) -> i32 {
    let mut white = 0;
    let mut black = 0;
    for kind in PieceKind::ALL {
        let value = piece_value(kind);
        for square in position.pieces(Color::White, kind) {
            white += value + piece_square_value(kind, Color::White, square);
        }
        for square in position.pieces(Color::Black, kind) {
            black += value + piece_square_value(kind, Color::Black, square);
        }
    }

    let score = match position.side_to_move() {
        Color::White => white - black + 10,
        Color::Black => black - white + 10,
    };
    score.clamp(-EVALUATION_LIMIT, EVALUATION_LIMIT)
}

pub(crate) fn piece_square_value(kind: PieceKind, color: Color, square: Square) -> i32 {
    let file = square.file() as i32;
    let rank = match color {
        Color::White => square.rank() as i32,
        Color::Black => 7 - square.rank() as i32,
    };
    let file_center = 3 - (file - 3).abs().min((file - 4).abs());
    let rank_center = 3 - (rank - 3).abs().min((rank - 4).abs());
    let center = file_center + rank_center;

    match kind {
        PieceKind::Pawn => rank * 8 + center * 2,
        PieceKind::Knight => center * 10,
        PieceKind::Bishop => center * 6,
        PieceKind::Rook => rank * 2,
        PieceKind::Queen => center * 2,
        PieceKind::King => -center * 4,
    }
}

pub(crate) const fn piece_value(kind: PieceKind) -> i32 {
    match kind {
        PieceKind::Pawn => 100,
        PieceKind::Knight => 320,
        PieceKind::Bishop => 330,
        PieceKind::Rook => 500,
        PieceKind::Queen => 900,
        PieceKind::King => 0,
    }
}
