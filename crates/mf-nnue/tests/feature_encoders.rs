use mf_core::{
    Bitboard, Color, Piece, PieceKind, Position, Square, bishop_attacks, generate_legal_moves,
    king_attacks, knight_attacks, pawn_attacks, queen_attacks, rook_attacks,
};
use mf_nnue::{HALF_KA_DIMS, halfka, threats};

const PS_WHITE: [usize; 12] = [0, 128, 256, 384, 512, 640, 64, 192, 320, 448, 576, 640];
const PS_BLACK: [usize; 12] = [64, 192, 320, 448, 576, 640, 0, 128, 256, 384, 512, 640];
const WHITE_KING_BUCKETS: [usize; 64] = [
    19712, 20416, 21120, 21824, 21824, 21120, 20416, 19712, 16896, 17600, 18304, 19008, 19008,
    18304, 17600, 16896, 14080, 14784, 15488, 16192, 16192, 15488, 14784, 14080, 11264, 11968,
    12672, 13376, 13376, 12672, 11968, 11264, 8448, 9152, 9856, 10560, 10560, 9856, 9152, 8448,
    5632, 6336, 7040, 7744, 7744, 7040, 6336, 5632, 2816, 3520, 4224, 4928, 4928, 4224, 3520, 2816,
    0, 704, 1408, 2112, 2112, 1408, 704, 0,
];
const REFERENCE_THREAT_DIMENSIONS: usize = 60_720;
const REFERENCE_ALL_PIECES: [usize; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];
const REFERENCE_NUM_VALID_TARGETS: [usize; 16] =
    [0, 6, 10, 8, 8, 10, 0, 0, 0, 6, 10, 8, 8, 10, 0, 0];
const REFERENCE_THREAT_MAP: [[i8; 6]; 6] = [
    [0, 1, -1, 2, -1, -1],
    [0, 1, 2, 3, 4, -1],
    [0, 1, 2, 3, -1, -1],
    [0, 1, 2, 3, -1, -1],
    [0, 1, 2, 3, 4, -1],
    [-1, -1, -1, -1, -1, -1],
];

struct ReferenceThreatLayout {
    offsets: [[usize; 64]; 16],
    cumulative_piece: [usize; 16],
    cumulative_offsets: [usize; 16],
}

impl ReferenceThreatLayout {
    fn new() -> Self {
        let mut layout = Self {
            offsets: [[0; 64]; 16],
            cumulative_piece: [0; 16],
            cumulative_offsets: [0; 16],
        };
        let mut cumulative_offset = 0;

        for &piece in &REFERENCE_ALL_PIECES {
            let mut piece_offset = 0;
            for from in 0..64 {
                layout.offsets[piece][from] = piece_offset;
                if reference_piece_type(piece) != 1 || (8..=55).contains(&from) {
                    piece_offset += reference_attack_set(piece, from).count_ones() as usize;
                }
            }
            layout.cumulative_piece[piece] = piece_offset;
            layout.cumulative_offsets[piece] = cumulative_offset;
            cumulative_offset += REFERENCE_NUM_VALID_TARGETS[piece] * piece_offset;
        }

        assert_eq!(cumulative_offset, REFERENCE_THREAT_DIMENSIONS);
        layout
    }
}

fn reference_piece(color: Color, kind: PieceKind) -> usize {
    (color.index() << 3) + kind.index() + 1
}

fn reference_piece_type(piece: usize) -> usize {
    piece & 7
}

fn reference_piece_color(piece: usize) -> usize {
    piece >> 3
}

fn reference_attack_set(piece: usize, from: usize) -> u64 {
    let from = square(from as u8);
    match reference_piece_type(piece) {
        1 => {
            let color = if reference_piece_color(piece) == 0 {
                Color::White
            } else {
                Color::Black
            };
            let push = match color {
                Color::White if from.index() < 56 => 1u64 << (from.index() + 8),
                Color::Black if from.index() >= 8 => 1u64 << (from.index() - 8),
                Color::White | Color::Black => 0,
            };
            pawn_attacks(from, color).bits() | push
        }
        2 => knight_attacks(from).bits(),
        3 => bishop_attacks(from, Bitboard::EMPTY).bits(),
        4 => rook_attacks(from, Bitboard::EMPTY).bits(),
        5 => queen_attacks(from, Bitboard::EMPTY).bits(),
        6 => king_attacks(from).bits(),
        _ => unreachable!(),
    }
}

#[allow(clippy::too_many_arguments)]
fn reference_threat_index(
    layout: &ReferenceThreatLayout,
    perspective: Color,
    attacker_color: Color,
    attacker_kind: PieceKind,
    from: Square,
    to: Square,
    attacked_color: Color,
    attacked_kind: PieceKind,
    king_square: Square,
) -> usize {
    let orientation = if king_square.file() < 4 { 0 } else { 7 }
        ^ if perspective == Color::Black { 56 } else { 0 };
    let swap = perspective.index() << 3;
    let from = usize::from(from.index() ^ orientation);
    let to = usize::from(to.index() ^ orientation);
    let attacker = reference_piece(attacker_color, attacker_kind) ^ swap;
    let attacked = reference_piece(attacked_color, attacked_kind) ^ swap;
    let attacker_type = reference_piece_type(attacker);
    let attacked_type = reference_piece_type(attacked);
    let mapped = REFERENCE_THREAT_MAP[attacker_type - 1][attacked_type - 1];
    let enemy = attacker ^ attacked == 8;
    let semi_excluded =
        attacker_type == attacked_type && (enemy || attacker_type != PieceKind::Pawn.index() + 1);
    let excluded = mapped < 0 || (from < to && semi_excluded);

    let base = if excluded {
        REFERENCE_THREAT_DIMENSIONS
    } else {
        layout.cumulative_offsets[attacker]
            + (reference_piece_color(attacked) * (REFERENCE_NUM_VALID_TARGETS[attacker] / 2)
                + mapped as usize)
                * layout.cumulative_piece[attacker]
    };
    let below_target = if to == 0 { 0 } else { (1u64 << to) - 1 };
    base + layout.offsets[attacker][from]
        + (reference_attack_set(attacker, from) & below_target).count_ones() as usize
}

fn square(index: u8) -> Square {
    Square::new(index).unwrap()
}

fn reference_halfka_index(
    perspective: Color,
    piece: Piece,
    piece_square: Square,
    king_square: Square,
) -> usize {
    let orient = if king_square.file() < 4 { 7 } else { 0 }
        ^ if perspective == Color::Black { 56 } else { 0 };
    let bucket_square = if perspective == Color::White {
        king_square.index()
    } else {
        king_square.index() ^ 56
    };
    let piece_square = piece_square.index() ^ orient;
    let piece_offset = if perspective == Color::White {
        PS_WHITE[piece.index()]
    } else {
        PS_BLACK[piece.index()]
    };
    piece_square as usize + piece_offset + WHITE_KING_BUCKETS[bucket_square as usize]
}

#[test]
fn halfka_known_values_match_eonego() {
    assert_eq!(
        halfka::make_index(
            Color::White,
            Piece::new(Color::White, PieceKind::Pawn),
            square(8),
            square(4),
        ),
        21_832
    );
    assert_eq!(
        halfka::make_index(
            Color::White,
            Piece::new(Color::Black, PieceKind::Pawn),
            square(48),
            square(4),
        ),
        21_936
    );
    assert_eq!(
        halfka::make_index(
            Color::Black,
            Piece::new(Color::White, PieceKind::Pawn),
            square(8),
            square(60),
        ),
        21_936
    );
}

#[test]
fn halfka_indices_match_exact_tables_and_stay_in_range() {
    for perspective in Color::ALL {
        for piece_color in Color::ALL {
            for kind in PieceKind::ALL {
                let piece = Piece::new(piece_color, kind);
                for king in 0..64 {
                    for piece_square in 0..64 {
                        let actual = halfka::make_index(
                            perspective,
                            piece,
                            square(piece_square),
                            square(king),
                        );
                        assert_eq!(
                            actual,
                            reference_halfka_index(
                                perspective,
                                piece,
                                square(piece_square),
                                square(king),
                            )
                        );
                        assert!(actual < HALF_KA_DIMS);
                    }
                }
            }
        }
    }
}

#[test]
fn halfka_color_and_perspective_symmetry_is_exact() {
    for piece_color in Color::ALL {
        for kind in PieceKind::ALL {
            let piece = Piece::new(piece_color, kind);
            let mirrored_piece = Piece::new(!piece_color, kind);
            for king in 0..64u8 {
                for piece_square in 0..64u8 {
                    assert_eq!(
                        halfka::make_index(Color::White, piece, square(piece_square), square(king),),
                        halfka::make_index(
                            Color::Black,
                            mirrored_piece,
                            square(piece_square ^ 56),
                            square(king ^ 56),
                        )
                    );
                }
            }
        }
    }
}

fn append_reference_threats(
    perspective: Color,
    position: &Position,
    buffer: &mut [usize; threats::MAX_ACTIVE],
) -> usize {
    let king_square = position.king_square(perspective);
    let occupancy = position.occupancy();
    let mut count = 0;

    let mut emit = |attacker: Piece, from: Square, to: Square| {
        let attacked = position
            .piece_at(to)
            .expect("reference threats only target occupied squares");
        let index = threats::make_index(
            perspective,
            threats::ThreatPiece::new(attacker.color(), attacker.kind()),
            from,
            to,
            threats::ThreatPiece::new(attacked.color(), attacked.kind()),
            king_square,
        );
        if index < threats::DIMENSIONS {
            buffer[count] = index;
            count += 1;
        }
    };

    for color in [perspective, !perspective] {
        let pawn = Piece::new(color, PieceKind::Pawn);
        for from in position.pieces(color, PieceKind::Pawn) {
            for to in mf_core::pawn_attacks(from, color) & occupancy {
                emit(pawn, from, to);
            }

            let target_index = match color {
                Color::White if from.index() < 56 => Some(from.index() + 8),
                Color::Black if from.index() >= 8 => Some(from.index() - 8),
                _ => None,
            };
            if let Some(to) = target_index.and_then(Square::new)
                && position
                    .piece_at(to)
                    .is_some_and(|piece| piece.kind() == PieceKind::Pawn)
            {
                emit(pawn, from, to);
            }
        }

        for kind in [
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Rook,
            PieceKind::Queen,
        ] {
            let attacker = Piece::new(color, kind);
            for from in position.pieces(color, kind) {
                let attacks = match kind {
                    PieceKind::Knight => knight_attacks(from),
                    PieceKind::Bishop => bishop_attacks(from, occupancy),
                    PieceKind::Rook => rook_attacks(from, occupancy),
                    PieceKind::Queen => queen_attacks(from, occupancy),
                    PieceKind::Pawn | PieceKind::King => unreachable!(),
                } & occupancy;
                for to in attacks {
                    emit(attacker, from, to);
                }
            }
        }
    }

    count
}

fn active_indices(perspective: Color, position: &Position) -> Vec<usize> {
    let mut buffer = [0; threats::MAX_ACTIVE];
    let count = threats::append_active_threats(perspective, position, &mut buffer);
    buffer[..count].to_vec()
}

#[test]
fn full_threats_known_values_use_the_opposite_file_orientation() {
    let white_knight = threats::ThreatPiece::new(Color::White, PieceKind::Knight);
    let white_pawn = threats::ThreatPiece::new(Color::White, PieceKind::Pawn);
    assert_eq!(
        threats::make_index(
            Color::White,
            white_knight,
            square(1),
            square(16),
            white_pawn,
            square(3),
        ),
        795
    );
    assert_eq!(
        threats::make_index(
            Color::White,
            white_knight,
            square(1),
            square(16),
            white_pawn,
            square(4),
        ),
        815
    );
    assert_eq!(
        threats::make_index(
            Color::Black,
            threats::ThreatPiece::new(Color::Black, PieceKind::Knight),
            square(57),
            square(40),
            threats::ThreatPiece::new(Color::Black, PieceKind::Pawn),
            square(59),
        ),
        795
    );
}

#[test]
fn full_threats_luts_match_an_independent_eonego_reference() {
    let layout = ReferenceThreatLayout::new();
    let from = square(32);
    let to = square(16);
    let king_square = square(3);

    for attacker_kind in PieceKind::ALL {
        for attacked_kind in PieceKind::ALL {
            for attacked_color in Color::ALL {
                let expected = reference_threat_index(
                    &layout,
                    Color::White,
                    Color::White,
                    attacker_kind,
                    from,
                    to,
                    attacked_color,
                    attacked_kind,
                    king_square,
                );
                let actual = threats::make_index(
                    Color::White,
                    threats::ThreatPiece::new(Color::White, attacker_kind),
                    from,
                    to,
                    threats::ThreatPiece::new(attacked_color, attacked_kind),
                    king_square,
                );

                assert_eq!(
                    actual, expected,
                    "attacker={attacker_kind:?}, attacked={attacked_color:?} {attacked_kind:?}"
                );
                if REFERENCE_THREAT_MAP[attacker_kind.index()][attacked_kind.index()] < 0 {
                    assert!(actual >= REFERENCE_THREAT_DIMENSIONS);
                }
            }
        }
    }
}

#[test]
fn full_threats_same_type_dedup_matches_the_reference_in_both_directions() {
    let layout = ReferenceThreatLayout::new();
    let king_square = square(3);
    let cases = [
        (PieceKind::Pawn, Color::White, false),
        (PieceKind::Pawn, Color::Black, true),
        (PieceKind::Knight, Color::White, true),
        (PieceKind::Knight, Color::Black, true),
    ];

    for (kind, attacked_color, forward_is_excluded) in cases {
        for (from, to, should_be_excluded) in [
            (square(1), square(16), forward_is_excluded),
            (square(16), square(1), false),
        ] {
            let expected = reference_threat_index(
                &layout,
                Color::White,
                Color::White,
                kind,
                from,
                to,
                attacked_color,
                kind,
                king_square,
            );
            let actual = threats::make_index(
                Color::White,
                threats::ThreatPiece::new(Color::White, kind),
                from,
                to,
                threats::ThreatPiece::new(attacked_color, kind),
                king_square,
            );

            assert_eq!(
                actual, expected,
                "kind={kind:?}, attacked_color={attacked_color:?}, from={from:?}, to={to:?}"
            );
            assert_eq!(
                actual >= REFERENCE_THREAT_DIMENSIONS,
                should_be_excluded,
                "kind={kind:?}, attacked_color={attacked_color:?}, from={from:?}, to={to:?}"
            );
        }
    }
}

#[test]
fn full_threats_includes_own_piece_defence() {
    let position = Position::from_fen("7k/8/8/8/8/P7/8/1N1K4 w - - 0 1", false).unwrap();
    assert_eq!(active_indices(Color::White, &position), vec![795]);
}

#[test]
fn full_threats_never_uses_kings_as_attackers() {
    let position = Position::from_fen("7k/8/8/8/8/8/3P4/3K4 w - - 0 1", false).unwrap();
    assert!(active_indices(Color::White, &position).is_empty());
    assert!(active_indices(Color::Black, &position).is_empty());
}

#[test]
fn full_threats_emits_pawn_blocks_only_for_pawns() {
    let pawn_block = Position::from_fen("7k/8/8/8/8/p7/P7/3K4 w - - 0 1", false).unwrap();
    assert_eq!(active_indices(Color::White, &pawn_block).len(), 1);

    let non_pawn_block = Position::from_fen("7k/8/8/8/8/n7/P7/3K4 w - - 0 1", false).unwrap();
    assert!(active_indices(Color::White, &non_pawn_block).is_empty());
}

#[test]
fn full_threats_matches_a_direct_position_enumeration() {
    let positions = [
        Position::startpos(),
        Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/2pP4/1p2P3/2N2N2/PPPBBPPP/R2Q1RK1 w kq - 0 1",
            false,
        )
        .unwrap(),
        Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1", true).unwrap(),
    ];

    for position in positions {
        for perspective in Color::ALL {
            let mut actual = [0; threats::MAX_ACTIVE];
            let actual_count = threats::append_active_threats(perspective, &position, &mut actual);
            let mut expected = [0; threats::MAX_ACTIVE];
            let expected_count = append_reference_threats(perspective, &position, &mut expected);

            actual[..actual_count].sort_unstable();
            expected[..expected_count].sort_unstable();
            assert_eq!(
                &actual[..actual_count],
                &expected[..expected_count],
                "perspective {perspective:?}, position {position:?}"
            );
        }
    }
}

#[test]
fn full_threats_indices_and_active_counts_are_bounded_on_reachable_positions() {
    let mut position = Position::startpos();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;

    for _ in 0..256 {
        for perspective in Color::ALL {
            let mut buffer = [0; threats::MAX_ACTIVE];
            let count = threats::append_active_threats(perspective, &position, &mut buffer);
            assert!(count <= 128, "count {count} in {position:?}");
            assert!(
                buffer[..count]
                    .iter()
                    .all(|&index| index < threats::DIMENSIONS)
            );
        }

        let moves = generate_legal_moves(&position);
        if moves.is_empty() {
            position = Position::startpos();
            continue;
        }
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mv = moves[state as usize % moves.len()];
        position.make_move(mv);
    }
}

#[test]
fn full_threats_ignores_chess960_castling_rights() {
    let with_rights = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1", true).unwrap();
    let without_rights = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w - - 0 1", true).unwrap();

    for perspective in Color::ALL {
        let mut with = active_indices(perspective, &with_rights);
        let mut without = active_indices(perspective, &without_rights);
        with.sort_unstable();
        without.sort_unstable();
        assert_eq!(with, without);
    }
}
