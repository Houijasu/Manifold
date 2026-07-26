use crate::{CastlingSide, Move, PieceKind, Position, generate_legal_moves};

/// Formats a legal move in UCI notation.
pub fn format_uci_move(position: &Position, mv: Move, chess960: bool) -> String {
    let mut destination = mv.to();
    if mv.flag().is_castling() && !chess960 {
        let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
        destination = side.king_destination(position.side_to_move());
    }

    let mut notation = format!("{:?}{:?}", mv.from(), destination);
    if let Some(promotion) = mv.flag().promotion() {
        notation.push(match promotion {
            PieceKind::Knight => 'n',
            PieceKind::Bishop => 'b',
            PieceKind::Rook => 'r',
            PieceKind::Queen => 'q',
            PieceKind::Pawn | PieceKind::King => unreachable!(),
        });
    }
    notation
}

/// Resolves UCI notation against the current legal move list.
pub fn parse_uci_move(position: &Position, notation: &str, chess960: bool) -> Option<Move> {
    generate_legal_moves(position)
        .iter()
        .copied()
        .find(|&mv| format_uci_move(position, mv, chess960) == notation)
}
