use crate::{CastlingSide, Move, PieceKind, Position, Square, generate_legal_moves};

/// Formats a legal move in UCI notation.
///
/// Castling is spelled king-to-file (`e1g1`) in standard mode and king-takes-rook
/// (`e1h1`) in Chess960 mode, matching the reference engine. The one exception is a
/// standard-mode spelling that would not name this move uniquely: in some Chess960
/// layouts the king's castling destination is one step away, so `b1c1` is both the
/// castle and a legal quiet king move, and a king already standing on its destination
/// spells the castle as the non-move `g1g1`. Standard chess can never reach either case
/// -- the king always travels two squares from `e1` -- so this only fires on a 960
/// geometry loaded while the engine is in standard mode. There the unambiguous
/// king-takes-rook spelling is the only correct output.
pub fn format_uci_move(position: &Position, mv: Move, chess960: bool) -> String {
    let mut destination = mv.to();
    if mv.flag().is_castling() && !chess960 {
        let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
        let king_destination = side.king_destination(position.side_to_move());
        if !standard_castling_spelling_is_ambiguous(position, mv, king_destination) {
            destination = king_destination;
        }
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

/// Whether `from -> king_destination` names something other than this castle.
///
/// Two ways it can: the "move" is a null move because the king already stands on its
/// castling destination, or another legal move shares the same origin and destination.
fn standard_castling_spelling_is_ambiguous(
    position: &Position,
    castle: Move,
    king_destination: Square,
) -> bool {
    if castle.from() == king_destination {
        return true;
    }
    generate_legal_moves(position).iter().any(|&other| {
        other != castle && other.from() == castle.from() && other.to() == king_destination
    })
}

/// Resolves UCI notation against the current legal move list.
///
/// Matching is case-insensitive, and a castling move is accepted in either the
/// king-to-file (`e1g1`) or king-takes-rook (`e1h1`) spelling regardless of which one
/// this engine would emit. GUIs disagree on both points -- uppercase promotion suffixes
/// (`b7b8Q`) are common, and a GUI's castling dialect does not always follow the
/// `UCI_Chess960` option -- and a rejected move is not a harmless no-op: it fails the
/// whole `position` command, leaving the engine on its previous board so it analyses
/// the wrong position and replies with a move that is illegal in the GUI's.
///
/// The configured dialect is still tried first and on its own. Only if nothing matches
/// exactly are castling moves reconsidered in the other spelling, which preserves the
/// Chess960 case where a king-to-rook notation like `f1g1` could otherwise be read as
/// both a quiet king move and a castle.
pub fn parse_uci_move(position: &Position, notation: &str, chess960: bool) -> Option<Move> {
    let moves = generate_legal_moves(position);
    let exact = moves
        .iter()
        .copied()
        .find(|&mv| format_uci_move(position, mv, chess960).eq_ignore_ascii_case(notation));
    exact.or_else(|| {
        moves.iter().copied().find(|&mv| {
            mv.flag().is_castling()
                && format_uci_move(position, mv, !chess960).eq_ignore_ascii_case(notation)
        })
    })
}
