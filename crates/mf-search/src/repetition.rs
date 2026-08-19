use std::sync::OnceLock;

use mf_core::{
    Bitboard, Color, Move, MoveFlag, Piece, PieceKind, Position, Square, bishop_attacks,
    king_attacks, knight_attacks, queen_attacks, reversible_move_delta, rook_attacks,
};

const CUCKOO_SIZE: usize = 8_192;
const REVERSIBLE_MOVE_COUNT: usize = 3_668;

struct CuckooTable {
    keys: Box<[u64; CUCKOO_SIZE]>,
    moves: Box<[u16; CUCKOO_SIZE]>,
    len: usize,
}

impl CuckooTable {
    fn build() -> Self {
        let mut table = Self {
            keys: Box::new([0; CUCKOO_SIZE]),
            moves: Box::new([0; CUCKOO_SIZE]),
            len: 0,
        };

        for color in Color::ALL {
            for kind in [
                PieceKind::Knight,
                PieceKind::Bishop,
                PieceKind::Rook,
                PieceKind::Queen,
                PieceKind::King,
            ] {
                let piece = Piece::new(color, kind);
                for from_index in 0..64 {
                    let from = Square::new(from_index).unwrap();
                    for to in reversible_targets(kind, from) {
                        if from >= to {
                            continue;
                        }
                        let key = reversible_move_delta(piece, from, to);
                        let mv = Move::new(from, to, MoveFlag::QUIET);
                        table.insert(key, mv);
                    }
                }
            }
        }

        assert_eq!(
            table.len, REVERSIBLE_MOVE_COUNT,
            "cuckoo table must contain every reversible non-pawn move"
        );
        table
    }

    fn insert(&mut self, mut key: u64, mut mv: Move) {
        assert_ne!(key, 0, "zero is reserved as the empty cuckoo key");
        let mut slot = cuckoo_h1(key);
        for _ in 0..CUCKOO_SIZE * 2 {
            core::mem::swap(&mut key, &mut self.keys[slot]);
            let mut move_raw = mv.raw();
            core::mem::swap(&mut move_raw, &mut self.moves[slot]);
            if key == 0 {
                self.len += 1;
                return;
            }
            mv = Move::from_raw(move_raw).expect("displaced cuckoo move must remain valid");
            slot = if slot == cuckoo_h1(key) {
                cuckoo_h2(key)
            } else {
                cuckoo_h1(key)
            };
        }
        panic!("cuckoo insertion cycle exceeded the fixed displacement bound");
    }

    fn lookup(&self, key: u64) -> Option<Move> {
        for slot in [cuckoo_h1(key), cuckoo_h2(key)] {
            if self.keys[slot] == key {
                return Move::from_raw(self.moves[slot]);
            }
        }
        None
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }
}

fn cuckoo_table() -> &'static CuckooTable {
    static TABLE: OnceLock<CuckooTable> = OnceLock::new();
    TABLE.get_or_init(CuckooTable::build)
}

#[inline]
fn cuckoo_h1(key: u64) -> usize {
    key as usize & (CUCKOO_SIZE - 1)
}

#[inline]
fn cuckoo_h2(key: u64) -> usize {
    (key >> 16) as usize & (CUCKOO_SIZE - 1)
}

fn reversible_targets(kind: PieceKind, from: Square) -> Bitboard {
    match kind {
        PieceKind::Knight => knight_attacks(from),
        PieceKind::Bishop => bishop_attacks(from, Bitboard::EMPTY),
        PieceKind::Rook => rook_attacks(from, Bitboard::EMPTY),
        PieceKind::Queen => queen_attacks(from, Bitboard::EMPTY),
        PieceKind::King => king_attacks(from),
        PieceKind::Pawn => Bitboard::EMPTY,
    }
}

#[derive(Clone, Copy)]
struct HistoryEntry {
    key: Option<u64>,
    repetition: i16,
}

pub(crate) struct RepetitionHistory {
    entries: Vec<HistoryEntry>,
}

impl RepetitionHistory {
    pub(crate) fn new(position: &Position, history: &[u64]) -> Self {
        let root_key = position.repetition_key();
        let keys = if history.is_empty() {
            vec![root_key]
        } else {
            assert_eq!(
                history.last().copied(),
                Some(root_key),
                "search history must end at the root position"
            );
            history.to_vec()
        };
        let mut entries = Vec::with_capacity(keys.len() + 128);
        let reversible_positions = usize::from(position.halfmove_clock()) + 1;
        let start = keys.len().saturating_sub(reversible_positions);
        for key in keys.into_iter().skip(start) {
            let repetition = repetition_distance(&entries, key, entries.len());
            entries.push(HistoryEntry {
                key: Some(key),
                repetition,
            });
        }
        Self { entries }
    }

    pub(crate) fn push_position(&mut self, position: &Position) {
        let key = position.repetition_key();
        let reversible_plies = usize::from(position.halfmove_clock());
        let repetition = repetition_distance(&self.entries, key, reversible_plies);
        self.entries.push(HistoryEntry {
            key: Some(key),
            repetition,
        });
    }

    pub(crate) fn push_null(&mut self) {
        self.entries.push(HistoryEntry {
            key: None,
            repetition: 0,
        });
    }

    pub(crate) fn pop(&mut self) {
        self.entries
            .pop()
            .expect("repetition history must contain the root");
    }

    pub(crate) fn current_repetition(&self) -> i16 {
        self.entries.last().map_or(0, |entry| entry.repetition)
    }

    pub(crate) fn is_repetition(&self, ply: usize) -> bool {
        let repetition = self.current_repetition();
        repetition != 0 && repetition < i16::try_from(ply).unwrap_or(i16::MAX)
    }

    pub(crate) fn upcoming_repetition(&self, position: &Position, ply: usize) -> bool {
        let current_index = self.entries.len().saturating_sub(1);
        // The halfmove clock and the history length are free to read; a repetition
        // needs three reversible plies, so either being short rules one out before
        // the backward `plies_since_null` scan runs.
        let end = usize::from(position.halfmove_clock()).min(current_index);
        if end < 3 {
            return false;
        }
        let end = end.min(self.plies_since_null());
        if end < 3 {
            return false;
        }

        let original_key = position.repetition_key();
        for distance in (3..=end).step_by(2) {
            let ancestor = self.entries[current_index - distance];
            let Some(ancestor_key) = ancestor.key else {
                break;
            };
            let move_key = original_key ^ ancestor_key;
            let Some(stored_move) = cuckoo_table().lookup(move_key) else {
                continue;
            };
            if reversible_move_from_position(position, stored_move).is_none() {
                continue;
            }
            if ply > distance || ancestor.repetition != 0 {
                return true;
            }
        }
        false
    }

    pub(crate) fn plies_since_null(&self) -> usize {
        let current_index = self.entries.len().saturating_sub(1);
        self.entries
            .iter()
            .rposition(|entry| entry.key.is_none())
            .map_or(current_index, |null_index| current_index - null_index)
    }
}

fn repetition_distance(entries: &[HistoryEntry], key: u64, reversible_plies: usize) -> i16 {
    let current = entries.len();
    let available = reversible_plies.min(current);
    for distance in (2..=available).step_by(2) {
        let previous = entries[current - distance];
        if previous.key == Some(key) {
            let distance = i16::try_from(distance).unwrap_or(i16::MAX);
            return if previous.repetition != 0 {
                -distance
            } else {
                distance
            };
        }
    }
    0
}

fn reversible_move_from_position(position: &Position, stored_move: Move) -> Option<Move> {
    let side_to_move = position.side_to_move();
    for (from, to) in [
        (stored_move.from(), stored_move.to()),
        (stored_move.to(), stored_move.from()),
    ] {
        let Some(piece) = position.piece_at(from) else {
            continue;
        };
        if piece.color() != side_to_move || position.piece_at(to).is_some() {
            continue;
        }
        let reachable = match piece.kind() {
            PieceKind::Knight => knight_attacks(from).contains(to),
            PieceKind::Bishop => bishop_attacks(from, position.occupancy()).contains(to),
            PieceKind::Rook => rook_attacks(from, position.occupancy()).contains(to),
            PieceKind::Queen => queen_attacks(from, position.occupancy()).contains(to),
            PieceKind::King => king_attacks(from).contains(to),
            PieceKind::Pawn => false,
        };
        if reachable {
            return Some(Move::new(from, to, MoveFlag::QUIET));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use mf_core::{
        Color, Move, MoveFlag, Piece, PieceKind, Square, parse_uci_move, reversible_move_delta,
    };

    use super::*;

    fn play(position: &mut Position, history: &mut RepetitionHistory, uci: &str) {
        let mv = parse_uci_move(position, uci, false).expect("test move must parse");
        position.make_move(mv);
        history.push_position(position);
    }

    #[test]
    fn signed_repetition_distance_distinguishes_twofold_and_threefold_cycles() {
        let mut position = Position::from_fen("1q5k/8/8/8/8/8/8/R5K1 w - - 0 1", false).unwrap();
        let mut history = RepetitionHistory::new(&position, &[position.repetition_key()]);
        let moves = [
            "a1a2", "b8b7", "a2a1", "b7b8", "a1a2", "b8b7", "a2a1", "b7b8",
        ];

        for (ply, uci) in moves.into_iter().enumerate() {
            play(&mut position, &mut history, uci);
            if ply == 3 {
                assert_eq!(history.current_repetition(), 4);
                assert!(!history.is_repetition(0));
                assert!(history.is_repetition(5));
            }
        }

        assert_eq!(history.current_repetition(), -4);
        assert!(history.is_repetition(0));
    }

    #[test]
    fn null_move_is_a_hard_boundary_for_repetition_state() {
        let position = Position::from_fen(
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 8 1",
            false,
        )
        .unwrap();
        let key = position.repetition_key();
        let other = key ^ 1;
        let mut history = RepetitionHistory::new(&position, &[key, other, key, other, key]);

        assert!(history.is_repetition(0));
        history.push_null();
        assert!(!history.is_repetition(20));
        history.pop();
        assert!(history.is_repetition(0));
    }

    #[test]
    fn irreversible_move_clock_limits_supplied_game_history() {
        let position = Position::startpos();
        let key = position.repetition_key();
        let history = RepetitionHistory::new(&position, &[key, key ^ 1, key]);

        assert_eq!(history.current_repetition(), 0);
        assert!(!history.is_repetition(20));
    }

    #[test]
    fn cuckoo_table_contains_all_reversible_non_pawn_move_deltas() {
        let table = cuckoo_table();
        assert_eq!(table.len(), 3_668);

        let piece = Piece::new(Color::White, PieceKind::Knight);
        let from = Square::new(6).unwrap();
        let to = Square::new(21).unwrap();
        let key = reversible_move_delta(piece, from, to);
        let stored = table.lookup(key).expect("g1-f3 delta must be present");
        assert!(
            stored == Move::new(from, to, MoveFlag::QUIET)
                || stored == Move::new(to, from, MoveFlag::QUIET)
        );
    }

    #[test]
    fn upcoming_repetition_detects_a_cycle_available_inside_the_search_tree() {
        let mut position = Position::from_fen("1q5k/8/8/8/8/8/8/R5K1 w - - 0 1", false).unwrap();
        for uci in ["a1a2", "b8b7"] {
            let mv = parse_uci_move(&position, uci, false).unwrap();
            position.make_move(mv);
        }
        let mut history = RepetitionHistory::new(&position, &[position.repetition_key()]);
        for uci in ["a2a1", "b7b8", "a1a2", "b8b7"] {
            play(&mut position, &mut history, uci);
        }

        assert!(history.upcoming_repetition(&position, 4));
        history.push_null();
        assert!(!history.upcoming_repetition(&position, 5));
    }
}
