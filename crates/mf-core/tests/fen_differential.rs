//! Differential fuzz of the FEN parser against cozy-chess.
//!
//! `perft_differential.rs` builds positions programmatically and never exercises
//! `from_fen`, so a parser bug that mangles a pasted position is invisible to it.
//! This drives the same random reachable positions through the FEN text itself.

use cozy_chess::{Board, Color as CozyColor, Move as CozyMove};
use mf_core::{Position, format_uci_move, generate_legal_moves};
use std::collections::BTreeSet;

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

fn cozy_legal_moves(board: &Board) -> Vec<CozyMove> {
    let mut moves = Vec::new();
    board.generate_moves(|piece_moves| {
        moves.extend(piece_moves);
        false
    });
    moves
}

fn random_reachable_position(board: &mut Board, rng: &mut SplitMix64) {
    let plies = 8 + (rng.next() % 72) as usize;
    for _ in 0..plies {
        let moves = cozy_legal_moves(board);
        if moves.is_empty() {
            break;
        }
        let mv = moves[rng.next() as usize % moves.len()];
        board.play_unchecked(mv);
    }
}

/// Renders cozy's move list in the same UCI dialect the engine would emit.
fn cozy_move_set(board: &Board, chess960: bool) -> BTreeSet<String> {
    cozy_legal_moves(board)
        .into_iter()
        .map(|mv| {
            let mut mv = mv;
            if !chess960 {
                // cozy always emits king-takes-rook; convert to king-to-file for
                // standard-dialect comparison.
                let rank = if board.side_to_move() == CozyColor::White {
                    0
                } else {
                    7
                };
                if board.color_on(mv.to) == Some(board.side_to_move())
                    && board.piece_on(mv.from) == Some(cozy_chess::Piece::King)
                    && board.piece_on(mv.to) == Some(cozy_chess::Piece::Rook)
                {
                    let file = if mv.to.file() > mv.from.file() { 6 } else { 2 };
                    mv.to = cozy_chess::Square::index(rank * 8 + file);
                }
            }
            mv.to_string()
        })
        .collect()
}

fn mf_move_set(position: &Position, chess960: bool) -> BTreeSet<String> {
    generate_legal_moves(position)
        .into_iter()
        .map(|mv| format_uci_move(position, *mv, chess960))
        .collect()
}

fn compare_via_fen(board: &Board, chess960: bool) {
    let fen = format!("{board}");
    let position = Position::from_fen(&fen, chess960)
        .unwrap_or_else(|error| panic!("from_fen rejected '{fen}': {error}"));

    let expected = cozy_move_set(board, chess960);
    let actual = mf_move_set(&position, chess960);

    assert_eq!(
        actual,
        expected,
        "\nFEN: {fen}\nchess960: {chess960}\nmf-only: {:?}\ncozy-only: {:?}",
        actual.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&actual).collect::<Vec<_>>(),
    );
}

#[test]
fn standard_positions_survive_a_fen_roundtrip() {
    let mut rng = SplitMix64(0x6665_6e2d_6469_6666);

    for _ in 0..512 {
        let mut board = Board::startpos();
        random_reachable_position(&mut board, &mut rng);
        compare_via_fen(&board, false);
    }
}

#[test]
fn chess960_positions_survive_a_fen_roundtrip() {
    let mut rng = SplitMix64(0x3936_302d_6669_7368);

    for _ in 0..512 {
        let mut board = Board::chess960_startpos((rng.next() % 960) as u32);
        random_reachable_position(&mut board, &mut rng);
        compare_via_fen(&board, true);
    }
}
