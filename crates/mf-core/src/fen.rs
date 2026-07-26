use core::fmt;

use crate::{CastlingSide, Color, Piece, PieceKind, Position, Square};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FenError {
    message: String,
}

impl FenError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FenError {}

impl Position {
    /// Parses a six-field FEN. In Chess960 mode, rook-file castling rights are accepted.
    pub fn from_fen(fen: &str, chess960: bool) -> Result<Self, FenError> {
        let fields: Vec<_> = fen.split_whitespace().collect();
        if fields.len() != 6 {
            return Err(FenError::new(format!(
                "expected 6 FEN fields, found {}",
                fields.len()
            )));
        }

        let side_to_move = match fields[1] {
            "w" => Color::White,
            "b" => Color::Black,
            other => return Err(FenError::new(format!("invalid side to move '{other}'"))),
        };
        let mut position = Position::empty(side_to_move);
        parse_board(&mut position, fields[0])?;
        validate_kings(&position)?;
        parse_castling(&mut position, fields[2], chess960)?;
        position.set_en_passant(parse_en_passant(&position, fields[3])?);
        position.set_halfmove_clock(
            fields[4]
                .parse()
                .map_err(|_| FenError::new(format!("invalid halfmove clock '{}'", fields[4])))?,
        );
        let fullmove_number = fields[5]
            .parse()
            .map_err(|_| FenError::new(format!("invalid fullmove number '{}'", fields[5])))?;
        if fullmove_number == 0 {
            return Err(FenError::new("fullmove number must be at least 1"));
        }
        position.set_fullmove_number(fullmove_number);
        Ok(position)
    }
}

fn parse_board(position: &mut Position, board: &str) -> Result<(), FenError> {
    let ranks: Vec<_> = board.split('/').collect();
    if ranks.len() != 8 {
        return Err(FenError::new(format!(
            "expected 8 board ranks, found {}",
            ranks.len()
        )));
    }

    for (fen_rank, rank) in ranks.into_iter().enumerate() {
        let board_rank = 7 - fen_rank as u8;
        let mut file = 0u8;
        for symbol in rank.chars() {
            if let Some(empty) = symbol.to_digit(10) {
                if !(1..=8).contains(&empty) {
                    return Err(FenError::new(format!(
                        "invalid empty-square run '{symbol}'"
                    )));
                }
                file = file
                    .checked_add(empty as u8)
                    .ok_or_else(|| FenError::new("rank contains too many squares"))?;
            } else {
                if file >= 8 {
                    return Err(FenError::new("rank contains too many squares"));
                }
                let piece = parse_piece(symbol)
                    .ok_or_else(|| FenError::new(format!("invalid piece '{symbol}'")))?;
                position.place_piece(square(file, board_rank), piece);
                file += 1;
            }
        }
        if file != 8 {
            return Err(FenError::new(format!(
                "rank {} contains {file} squares instead of 8",
                8 - fen_rank
            )));
        }
    }
    Ok(())
}

fn parse_piece(symbol: char) -> Option<Piece> {
    let color = if symbol.is_ascii_uppercase() {
        Color::White
    } else {
        Color::Black
    };
    let kind = match symbol.to_ascii_lowercase() {
        'p' => PieceKind::Pawn,
        'n' => PieceKind::Knight,
        'b' => PieceKind::Bishop,
        'r' => PieceKind::Rook,
        'q' => PieceKind::Queen,
        'k' => PieceKind::King,
        _ => return None,
    };
    Some(Piece::new(color, kind))
}

fn validate_kings(position: &Position) -> Result<(), FenError> {
    for color in Color::ALL {
        let count = position.pieces(color, PieceKind::King).count();
        if count != 1 {
            return Err(FenError::new(format!(
                "{color:?} must have exactly one king, found {count}"
            )));
        }
    }
    Ok(())
}

fn parse_castling(position: &mut Position, castling: &str, chess960: bool) -> Result<(), FenError> {
    if castling == "-" {
        return Ok(());
    }
    if castling.is_empty() {
        return Err(FenError::new("castling field cannot be empty"));
    }

    for symbol in castling.chars() {
        let (color, file) = match symbol {
            'K' => (
                Color::White,
                castling_rook_file(position, Color::White, CastlingSide::KingSide, chess960)?,
            ),
            'Q' => (
                Color::White,
                castling_rook_file(position, Color::White, CastlingSide::QueenSide, chess960)?,
            ),
            'k' => (
                Color::Black,
                castling_rook_file(position, Color::Black, CastlingSide::KingSide, chess960)?,
            ),
            'q' => (
                Color::Black,
                castling_rook_file(position, Color::Black, CastlingSide::QueenSide, chess960)?,
            ),
            'A'..='H' if chess960 => (Color::White, symbol as u8 - b'A'),
            'a'..='h' if chess960 => (Color::Black, symbol as u8 - b'a'),
            _ => {
                return Err(FenError::new(format!("invalid castling right '{symbol}'")));
            }
        };
        let rook = square(file, color.back_rank());
        let king = position
            .pieces(color, PieceKind::King)
            .first()
            .expect("king count was validated");
        if king.rank() != color.back_rank() {
            return Err(FenError::new(format!(
                "{color:?} castling king is not on its back rank"
            )));
        }
        if position.piece_at(rook) != Some(Piece::new(color, PieceKind::Rook)) {
            return Err(FenError::new(format!(
                "castling right '{symbol}' has no rook on {rook:?}"
            )));
        }
        if rook == king {
            return Err(FenError::new("castling rook cannot share the king square"));
        }
        let side = CastlingSide::from_rook_origin(king, rook);
        if position.castling_rook(color, side).is_some() {
            return Err(FenError::new(format!(
                "duplicate {color:?} {side:?} castling right"
            )));
        }
        position.set_castling_rook(color, side, Some(rook));
    }
    Ok(())
}

fn castling_rook_file(
    position: &Position,
    color: Color,
    side: CastlingSide,
    chess960: bool,
) -> Result<u8, FenError> {
    if !chess960 {
        return Ok(match side {
            CastlingSide::KingSide => 7,
            CastlingSide::QueenSide => 0,
        });
    }

    let king = position
        .pieces(color, PieceKind::King)
        .first()
        .expect("king count was validated");
    let rooks = position.pieces(color, PieceKind::Rook);
    let file = match side {
        CastlingSide::KingSide => rooks
            .into_iter()
            .filter(|rook| rook.rank() == color.back_rank() && rook.file() > king.file())
            .map(Square::file)
            .max(),
        CastlingSide::QueenSide => rooks
            .into_iter()
            .filter(|rook| rook.rank() == color.back_rank() && rook.file() < king.file())
            .map(Square::file)
            .min(),
    };
    file.ok_or_else(|| FenError::new(format!("no {color:?} {side:?} castling rook")))
}

fn parse_en_passant(position: &Position, field: &str) -> Result<Option<Square>, FenError> {
    if field == "-" {
        return Ok(None);
    }
    let bytes = field.as_bytes();
    if bytes.len() != 2 || !(b'a'..=b'h').contains(&bytes[0]) || !matches!(bytes[1], b'3' | b'6') {
        return Err(FenError::new(format!(
            "invalid en-passant square '{field}'"
        )));
    }
    let target = square(bytes[0] - b'a', bytes[1] - b'1');
    let side_to_move = position.side_to_move();
    let expected_rank = match side_to_move {
        Color::White => 5,
        Color::Black => 2,
    };
    if target.rank() != expected_rank {
        return Err(FenError::new(format!(
            "en-passant square '{field}' is inconsistent with the side to move"
        )));
    }
    if position.piece_at(target).is_some() {
        return Err(FenError::new(format!(
            "en-passant square '{field}' is occupied"
        )));
    }

    let captured_index = match side_to_move {
        Color::White => target.index() - 8,
        Color::Black => target.index() + 8,
    };
    let captured = Square::new(captured_index).expect("validated en-passant rank is on board");
    if position.piece_at(captured) != Some(Piece::new(!side_to_move, PieceKind::Pawn)) {
        return Err(FenError::new(format!(
            "en-passant square '{field}' has no capturable pawn"
        )));
    }

    Ok(Some(target))
}

#[inline]
fn square(file: u8, rank: u8) -> Square {
    Square::new(rank * 8 + file).expect("file and rank are in 0..8")
}
