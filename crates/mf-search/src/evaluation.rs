mod weights;

use mf_core::{
    Bitboard, Color, PieceKind, Position, Square, bishop_attacks, king_attacks, knight_attacks,
    material_value, pawn_attacks, queen_attacks, rook_attacks,
};

pub use weights::{DEFAULT_PARAMETERS, EvaluationParameters, TaperedScore};

pub(crate) const EVALUATION_LIMIT: i32 = 10_000;
const TOTAL_PHASE: i32 = 24;
const PHASE_WEIGHTS: [i32; 6] = [0, 1, 1, 2, 4, 0];
const PASSED_PAWN_MASKS: [[Bitboard; 64]; 2] = build_passed_pawn_masks();

pub fn evaluate(position: &Position) -> i32 {
    evaluate_with_parameters(position, &DEFAULT_PARAMETERS)
}

pub fn evaluate_with_parameters(position: &Position, parameters: &EvaluationParameters) -> i32 {
    let mut score = TaperedScore::ZERO;
    let mut phase = 0;
    let occupancy = position.occupancy();

    for color in Color::ALL {
        let sign = color_sign(color);
        let available = !position.color_occupancy(color);
        for kind in PieceKind::ALL {
            let pieces = position.pieces(color, kind);
            phase += PHASE_WEIGHTS[kind.index()] * pieces.count() as i32;
            for square in pieces {
                add(&mut score, parameters.material[kind.index()], sign);
                add(
                    &mut score,
                    piece_square_score(parameters, kind, color, square),
                    sign,
                );
                if matches!(
                    kind,
                    PieceKind::Knight | PieceKind::Bishop | PieceKind::Rook | PieceKind::Queen
                ) {
                    let attacks = piece_attacks(kind, square, color, occupancy);
                    add(
                        &mut score,
                        parameters.mobility[kind.index()],
                        sign * (attacks & available).count() as i32,
                    );
                }
            }
        }

        add_pawn_structure(position, parameters, color, sign, &mut score);
        add_king_safety(position, parameters, color, sign, &mut score);
        if position.pieces(color, PieceKind::Bishop).count() >= 2 {
            add(&mut score, parameters.bishop_pair, sign);
        }
    }

    add_basic_endgame_knowledge(position, parameters, &mut score);

    let phase = phase.min(TOTAL_PHASE);
    let white_perspective =
        (score.middle_game * phase + score.end_game * (TOTAL_PHASE - phase)) / TOTAL_PHASE;
    (white_perspective * color_sign(position.side_to_move()))
        .clamp(-EVALUATION_LIMIT, EVALUATION_LIMIT)
}

pub(crate) fn piece_square_value(kind: PieceKind, color: Color, square: Square) -> i32 {
    piece_square_score(&DEFAULT_PARAMETERS, kind, color, square).middle_game
}

fn add_pawn_structure(
    position: &Position,
    parameters: &EvaluationParameters,
    color: Color,
    sign: i32,
    score: &mut TaperedScore,
) {
    let pawns = position.pieces(color, PieceKind::Pawn);
    let enemy_pawns = position.pieces(!color, PieceKind::Pawn);

    for file in 0..8 {
        let count = (pawns & file_mask(file)).count() as i32;
        for _ in 1..count {
            add(score, parameters.doubled_pawn, sign);
        }
    }

    for square in pawns {
        let adjacent_files = adjacent_file_mask(square.file());
        if (pawns & adjacent_files).is_empty() {
            add(score, parameters.isolated_pawn, sign);
        }
        if !(pawn_attacks(square, !color) & pawns).is_empty() {
            add(score, parameters.defended_pawn, sign);
        }
        if is_passed_pawn(square, color, enemy_pawns) {
            add(
                score,
                parameters.passed_pawn[relative_rank(color, square) as usize],
                sign,
            );
        }
    }
}

fn add_king_safety(
    position: &Position,
    parameters: &EvaluationParameters,
    color: Color,
    sign: i32,
    score: &mut TaperedScore,
) {
    let Some(king) = position.pieces(color, PieceKind::King).first() else {
        return;
    };
    let pawns = position.pieces(color, PieceKind::Pawn);
    let direction = if color == Color::White { 1 } else { -1 };
    let first_shield = shield_count(king, direction, pawns);
    let second_shield = shield_count(king, direction * 2, pawns);
    add(
        score,
        parameters.king_shield_first_rank,
        sign * first_shield,
    );
    add(
        score,
        parameters.king_shield_second_rank,
        sign * second_shield,
    );
    if (pawns & file_mask(king.file())).is_empty() {
        add(score, parameters.king_open_file, sign);
    }
}

fn add_basic_endgame_knowledge(
    position: &Position,
    parameters: &EvaluationParameters,
    score: &mut TaperedScore,
) {
    if !position.pieces(Color::White, PieceKind::Queen).is_empty()
        || !position.pieces(Color::Black, PieceKind::Queen).is_empty()
    {
        return;
    }

    let white_material = endgame_material(position, Color::White);
    let black_material = endgame_material(position, Color::Black);
    let advantage = white_material - black_material;
    if advantage.abs() < material_value(PieceKind::Rook) {
        return;
    }

    let stronger = if advantage > 0 {
        Color::White
    } else {
        Color::Black
    };
    let Some(stronger_king) = position.pieces(stronger, PieceKind::King).first() else {
        return;
    };
    let Some(weaker_king) = position.pieces(!stronger, PieceKind::King).first() else {
        return;
    };
    let sign = color_sign(stronger);
    let edge = 3 - edge_distance(weaker_king);
    let proximity = 14 - manhattan_distance(stronger_king, weaker_king);
    add(score, parameters.mop_up_edge, sign * edge);
    add(score, parameters.mop_up_king_proximity, sign * proximity);
}

fn piece_square_score(
    parameters: &EvaluationParameters,
    kind: PieceKind,
    color: Color,
    square: Square,
) -> TaperedScore {
    let index = match color {
        Color::White => square.index(),
        Color::Black => (7 - square.rank()) * 8 + square.file(),
    };
    parameters.piece_square_tables[kind.index()][index as usize]
}

fn piece_attacks(kind: PieceKind, square: Square, color: Color, occupancy: Bitboard) -> Bitboard {
    match kind {
        PieceKind::Pawn => pawn_attacks(square, color),
        PieceKind::Knight => knight_attacks(square),
        PieceKind::Bishop => bishop_attacks(square, occupancy),
        PieceKind::Rook => rook_attacks(square, occupancy),
        PieceKind::Queen => queen_attacks(square, occupancy),
        PieceKind::King => king_attacks(square),
    }
}

fn is_passed_pawn(square: Square, color: Color, enemy_pawns: Bitboard) -> bool {
    (enemy_pawns & PASSED_PAWN_MASKS[color.index()][square.index() as usize]).is_empty()
}

fn shield_count(king: Square, rank_delta: i8, pawns: Bitboard) -> i32 {
    let rank = king.rank() as i8 + rank_delta;
    if !(0..8).contains(&rank) {
        return 0;
    }
    let mut count = 0;
    for file_delta in -1..=1 {
        let file = king.file() as i8 + file_delta;
        if (0..8).contains(&file) {
            let square =
                Square::new(rank as u8 * 8 + file as u8).expect("shield square stays on board");
            count += i32::from(pawns.contains(square));
        }
    }
    count
}

fn endgame_material(position: &Position, color: Color) -> i32 {
    PieceKind::NON_PAWN_MATERIAL
        .into_iter()
        .chain([PieceKind::Pawn])
        .map(|kind| position.pieces(color, kind).count() as i32 * material_value(kind))
        .sum()
}

fn relative_rank(color: Color, square: Square) -> u8 {
    match color {
        Color::White => square.rank(),
        Color::Black => 7 - square.rank(),
    }
}

fn file_mask(file: u8) -> Bitboard {
    Bitboard::new(0x0101_0101_0101_0101u64 << file)
}

fn adjacent_file_mask(file: u8) -> Bitboard {
    let mut mask = Bitboard::EMPTY;
    if file > 0 {
        mask |= file_mask(file - 1);
    }
    if file < 7 {
        mask |= file_mask(file + 1);
    }
    mask
}

fn edge_distance(square: Square) -> i32 {
    let file = square.file() as i32;
    let rank = square.rank() as i32;
    file.min(7 - file).min(rank.min(7 - rank))
}

fn manhattan_distance(left: Square, right: Square) -> i32 {
    (left.file() as i32 - right.file() as i32).abs()
        + (left.rank() as i32 - right.rank() as i32).abs()
}

fn color_sign(color: Color) -> i32 {
    match color {
        Color::White => 1,
        Color::Black => -1,
    }
}

fn add(total: &mut TaperedScore, value: TaperedScore, scale: i32) {
    total.middle_game += value.middle_game * scale;
    total.end_game += value.end_game * scale;
}

const fn build_passed_pawn_masks() -> [[Bitboard; 64]; 2] {
    let mut masks = [[Bitboard::EMPTY; 64]; 2];
    let mut color = 0;
    while color < Color::ALL.len() {
        let mut square = 0;
        while square < 64 {
            let file = (square & 7) as i8;
            let mut rank = (square >> 3) as i8;
            let direction = if color == Color::White as usize {
                1
            } else {
                -1
            };
            let mut bits = 0u64;
            loop {
                rank += direction;
                if rank < 0 || rank >= 8 {
                    break;
                }
                let mut file_delta = -1;
                while file_delta <= 1 {
                    let target_file = file + file_delta;
                    if target_file >= 0 && target_file < 8 {
                        bits |= 1u64 << (rank * 8 + target_file);
                    }
                    file_delta += 1;
                }
            }
            masks[color][square] = Bitboard::new(bits);
            square += 1;
        }
        color += 1;
    }
    masks
}

#[cfg(test)]
mod tests {
    use mf_core::{Piece, Position, generate_legal_moves};

    use super::*;

    fn position(fen: &str) -> Position {
        Position::from_fen(fen, false).expect("evaluation test FEN should parse")
    }

    fn color_flip(position: &Position) -> Position {
        let mut flipped = Position::empty(position.side_to_move());
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                for square in position.pieces(color, kind) {
                    let mirrored = Square::new((7 - square.rank()) * 8 + square.file())
                        .expect("mirrored square stays on the board");
                    flipped.place_piece(mirrored, Piece::new(!color, kind));
                }
            }
        }
        flipped
    }

    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn zero_parameters() -> EvaluationParameters {
        EvaluationParameters {
            material: [TaperedScore::ZERO; 6],
            piece_square_tables: [[TaperedScore::ZERO; 64]; 6],
            doubled_pawn: TaperedScore::ZERO,
            isolated_pawn: TaperedScore::ZERO,
            defended_pawn: TaperedScore::ZERO,
            passed_pawn: [TaperedScore::ZERO; 8],
            mobility: [TaperedScore::ZERO; 6],
            bishop_pair: TaperedScore::ZERO,
            king_shield_first_rank: TaperedScore::ZERO,
            king_shield_second_rank: TaperedScore::ZERO,
            king_open_file: TaperedScore::ZERO,
            mop_up_edge: TaperedScore::ZERO,
            mop_up_king_proximity: TaperedScore::ZERO,
        }
    }

    #[test]
    fn start_position_is_exactly_balanced() {
        assert_eq!(evaluate(&Position::startpos()), 0);
    }

    #[test]
    fn color_flip_negates_the_score_exactly() {
        let original = position("6k1/5ppp/8/3n4/4P3/2N5/PP3PPP/6K1 w - - 0 1");
        let flipped = color_flip(&original);

        assert_eq!(evaluate(&flipped), -evaluate(&original));
    }

    #[test]
    fn color_flip_antisymmetry_holds_on_random_reachable_positions() {
        let mut position = Position::startpos();
        let mut state = 0xC0FFEEu64;

        for sample in 0..256 {
            let flipped = color_flip(&position);
            assert_eq!(
                evaluate(&flipped),
                -evaluate(&position),
                "antisymmetry failed at sample {sample}"
            );

            let moves = generate_legal_moves(&position);
            if moves.is_empty() {
                position = Position::startpos();
                continue;
            }
            let index = (next_random(&mut state) as usize) % moves.len();
            position.make_move(moves[index]);
        }
    }

    #[test]
    fn material_imbalance_has_the_expected_direction_and_scale() {
        let white_queen = position("4k3/8/8/8/8/8/8/3QK3 w - - 0 1");
        let black_queen = position("3qk3/8/8/8/8/8/8/4K3 w - - 0 1");

        assert!((800..=1_100).contains(&evaluate(&white_queen)));
        assert!((-1_100..=-800).contains(&evaluate(&black_queen)));
    }

    #[test]
    fn pawn_shield_is_preferred_to_exposed_king() {
        let shielded = position("3q2k1/8/8/8/8/8/5PPP/3Q2K1 w - - 0 1");
        let exposed = position("3q2k1/8/8/8/8/8/PPP5/3Q2K1 w - - 0 1");
        let mut parameters = zero_parameters();
        parameters.king_shield_first_rank = DEFAULT_PARAMETERS.king_shield_first_rank;
        parameters.king_shield_second_rank = DEFAULT_PARAMETERS.king_shield_second_rank;

        assert!(
            evaluate_with_parameters(&shielded, &parameters)
                >= evaluate_with_parameters(&exposed, &parameters) + 15,
            "shielded={} exposed={}",
            evaluate_with_parameters(&shielded, &parameters),
            evaluate_with_parameters(&exposed, &parameters)
        );
    }

    #[test]
    fn mobile_rook_is_preferred_to_cornered_rook() {
        let mobile = position("7k/8/8/8/3R4/8/8/K7 w - - 0 1");
        let cornered = position("7k/8/8/8/8/8/8/RK6 w - - 0 1");
        let mut parameters = zero_parameters();
        parameters.mobility = DEFAULT_PARAMETERS.mobility;

        assert!(
            evaluate_with_parameters(&mobile, &parameters)
                >= evaluate_with_parameters(&cornered, &parameters) + 20,
            "mobile={} cornered={}",
            evaluate_with_parameters(&mobile, &parameters),
            evaluate_with_parameters(&cornered, &parameters)
        );
    }

    #[test]
    fn passed_pawn_is_preferred_to_blocked_pawn() {
        let passed = position("7k/8/p7/4P3/8/8/8/K7 w - - 0 1");
        let blocked = position("7k/8/4p3/4P3/8/8/8/K7 w - - 0 1");
        let mut parameters = zero_parameters();
        parameters.passed_pawn = DEFAULT_PARAMETERS.passed_pawn;

        assert!(
            evaluate_with_parameters(&passed, &parameters)
                >= evaluate_with_parameters(&blocked, &parameters) + 20,
            "passed={} blocked={}",
            evaluate_with_parameters(&passed, &parameters),
            evaluate_with_parameters(&blocked, &parameters)
        );
    }

    #[test]
    fn king_centralization_is_rewarded_in_basic_endgames() {
        let central = position("7k/8/8/8/3K4/8/P7/8 w - - 0 1");
        let corner = position("7k/8/8/8/8/8/P7/K7 w - - 0 1");

        assert!(
            evaluate(&central) >= evaluate(&corner) + 20,
            "central={} corner={}",
            evaluate(&central),
            evaluate(&corner)
        );
    }

    #[test]
    fn mop_up_term_rewards_bringing_the_stronger_king_closer() {
        let central = position("7k/8/8/8/3K4/8/8/R7 w - - 0 1");
        let distant = position("7k/8/8/8/8/8/8/RK6 w - - 0 1");
        let mut parameters = zero_parameters();
        parameters.mop_up_king_proximity = TaperedScore::new(0, 20);

        assert!(
            evaluate_with_parameters(&central, &parameters)
                >= evaluate_with_parameters(&distant, &parameters) + 60
        );
    }
}
