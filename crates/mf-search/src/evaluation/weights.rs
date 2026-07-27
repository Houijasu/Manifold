use mf_core::{PieceKind, material_value};

/// A pair of middle-game and endgame centipawn weights.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaperedScore {
    pub middle_game: i32,
    pub end_game: i32,
}

impl TaperedScore {
    pub const ZERO: Self = Self::new(0, 0);

    pub const fn new(middle_game: i32, end_game: i32) -> Self {
        Self {
            middle_game,
            end_game,
        }
    }
}

/// Linear HCE coefficients collected in one structure for Texel tuning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationParameters {
    pub material: [TaperedScore; 6],
    pub piece_square_tables: [[TaperedScore; 64]; 6],
    pub doubled_pawn: TaperedScore,
    pub isolated_pawn: TaperedScore,
    pub defended_pawn: TaperedScore,
    pub passed_pawn: [TaperedScore; 8],
    pub mobility: [TaperedScore; 6],
    pub bishop_pair: TaperedScore,
    pub king_shield_first_rank: TaperedScore,
    pub king_shield_second_rank: TaperedScore,
    pub king_open_file: TaperedScore,
    pub mop_up_edge: TaperedScore,
    pub mop_up_king_proximity: TaperedScore,
}

pub const DEFAULT_PARAMETERS: EvaluationParameters = EvaluationParameters {
    material: [
        TaperedScore::new(material_value(PieceKind::Pawn), 120),
        TaperedScore::new(material_value(PieceKind::Knight), 300),
        TaperedScore::new(material_value(PieceKind::Bishop), 320),
        TaperedScore::new(material_value(PieceKind::Rook), 520),
        TaperedScore::new(material_value(PieceKind::Queen), 900),
        TaperedScore::ZERO,
    ],
    piece_square_tables: build_piece_square_tables(),
    doubled_pawn: TaperedScore::new(-12, -20),
    isolated_pawn: TaperedScore::new(-10, -12),
    defended_pawn: TaperedScore::new(6, 12),
    passed_pawn: [
        TaperedScore::ZERO,
        TaperedScore::new(5, 8),
        TaperedScore::new(10, 16),
        TaperedScore::new(20, 32),
        TaperedScore::new(35, 60),
        TaperedScore::new(60, 100),
        TaperedScore::new(100, 180),
        TaperedScore::ZERO,
    ],
    mobility: [
        TaperedScore::ZERO,
        TaperedScore::new(4, 4),
        TaperedScore::new(4, 5),
        TaperedScore::new(2, 4),
        TaperedScore::new(1, 2),
        TaperedScore::ZERO,
    ],
    bishop_pair: TaperedScore::new(28, 45),
    king_shield_first_rank: TaperedScore::new(24, 0),
    king_shield_second_rank: TaperedScore::new(10, 0),
    king_open_file: TaperedScore::new(-14, -2),
    mop_up_edge: TaperedScore::new(0, 10),
    mop_up_king_proximity: TaperedScore::new(0, 4),
};

const fn build_piece_square_tables() -> [[TaperedScore; 64]; 6] {
    let mut tables = [[TaperedScore::ZERO; 64]; 6];
    let mut kind = 0;
    while kind < PieceKind::ALL.len() {
        let mut square = 0;
        while square < 64 {
            let file = (square & 7) as i32;
            let rank = (square >> 3) as i32;
            let file_center = center_distance(file);
            let rank_center = center_distance(rank);
            let center = file_center + rank_center;
            tables[kind][square] = match PieceKind::ALL[kind] {
                PieceKind::Pawn => {
                    TaperedScore::new(rank * 8 + file_center * 2, rank * 12 + file_center)
                }
                PieceKind::Knight => TaperedScore::new(center * 12, center * 8),
                PieceKind::Bishop => TaperedScore::new(center * 7 + rank * 2, center * 6 + rank),
                PieceKind::Rook => {
                    TaperedScore::new(rank * 2 + file_center, rank * 4 + file_center)
                }
                PieceKind::Queen => TaperedScore::new(center * 3, center * 5),
                PieceKind::King => TaperedScore::new(-center * 8, center * 12),
            };
            square += 1;
        }
        kind += 1;
    }
    tables
}

const fn center_distance(coordinate: i32) -> i32 {
    let from_three = abs(coordinate - 3);
    let from_four = abs(coordinate - 4);
    3 - min(from_three, from_four)
}

const fn abs(value: i32) -> i32 {
    if value < 0 { -value } else { value }
}

const fn min(left: i32, right: i32) -> i32 {
    if left < right { left } else { right }
}
