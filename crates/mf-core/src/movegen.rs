use core::ops::Deref;

use crate::attacks::{
    is_in_check, is_square_attacked_with_occupancy, king_attacks, knight_attacks, offset,
};
use crate::{
    Bitboard, CastlingSide, Color, Move, MoveFlag, Piece, PieceKind, Position, Square,
    bishop_attacks, queen_attacks, rook_attacks,
};

const MAX_LEGAL_MOVES: usize = 256;

/// Fixed-capacity legal move collection. Chess has at most 218 legal moves.
///
/// Storage is deliberately left uninitialized: one of these is built per move-picker
/// construction in the search, so filling the array on `new` is a per-node memset.
/// Only the first `len` slots are ever written, and `as_slice` exposes exactly that
/// prefix, so the uninitialized tail is never read.
#[derive(Clone)]
pub struct MoveList {
    moves: [core::mem::MaybeUninit<Move>; MAX_LEGAL_MOVES],
    len: usize,
}

impl MoveList {
    #[inline]
    pub const fn new() -> Self {
        Self {
            moves: [core::mem::MaybeUninit::uninit(); MAX_LEGAL_MOVES],
            len: 0,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn as_slice(&self) -> &[Move] {
        // SAFETY: `push` initializes every slot below `len` before advancing it, and
        // nothing ever lowers a slot back to uninitialized.
        unsafe { core::slice::from_raw_parts(self.moves.as_ptr().cast::<Move>(), self.len) }
    }

    /// Appends a move. Panics if the list is already at capacity.
    #[inline]
    pub fn push(&mut self, mv: Move) {
        assert!(self.len < MAX_LEGAL_MOVES, "legal move list overflow");
        self.moves[self.len] = core::mem::MaybeUninit::new(mv);
        self.len += 1;
    }
}

impl Default for MoveList {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for MoveList {
    type Target = [Move];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = core::slice::Iter<'a, Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// Generates all legal moves for the side to move.
pub fn generate_legal_moves(position: &Position) -> MoveList {
    let mut pseudo = MoveList::new();
    generate_pseudo_legal_moves_into(position, &mut pseudo);

    let mover = position.side_to_move();
    let mut scratch = position.clone();
    let mut legal = MoveList::new();
    for &mv in &pseudo {
        let undo = scratch.make_move(mv);
        if !is_in_check(&scratch, mover) {
            legal.push(mv);
        }
        scratch.unmake_move(mv, undo);
    }
    legal
}

/// Returns whether the side to move has at least one legal move.
pub fn has_legal_move(position: &Position) -> bool {
    has_legal_move_in_place(&mut position.clone())
}

/// `has_legal_move` without the position clone: candidates are made and unmade
/// directly on the caller's position, which `make_move`/`unmake_move` restore
/// bit-for-bit (the make/unmake tests pin this). For callers that already hold a
/// mutable position, such as the search, this skips copying the whole position.
///
/// Captures are probed before quiets. The answer cannot depend on family order (any
/// legal move suffices), but the hot caller is the quiescence search's stalemate
/// probe, where a legal capture usually exists and is found within the first few
/// candidates; the quiet family is only generated in barren positions where
/// stalemate is actually plausible. Every candidate is still legality-verified by
/// make/unmake -- a pseudo-legal-only shortcut would mis-answer on pinned pieces
/// and illegal en passant.
pub fn has_legal_move_in_place(position: &mut Position) -> bool {
    let mover = position.side_to_move();
    for family in [MoveFamily::Captures, MoveFamily::Quiets] {
        let mut pseudo = MoveList::new();
        generate_moves_family(position, &mut pseudo, family);
        for &mv in &pseudo {
            let undo = position.make_move(mv);
            let legal = !is_in_check(position, mover);
            position.unmake_move(mv, undo);
            if legal {
                return true;
            }
        }
    }
    false
}

/// Generates moves that obey piece movement but may leave the moving king in check.
pub fn generate_pseudo_legal_moves(position: &Position) -> MoveList {
    let mut moves = MoveList::new();
    generate_pseudo_legal_moves_into(position, &mut moves);
    moves
}

/// Generates pseudo-legal captures and promotions only, in full-generation order.
///
/// A quiet promotion belongs to this family, not to the quiet one: the search scores
/// and gates it as a capture-equivalent everywhere.
pub fn generate_pseudo_legal_captures(position: &Position) -> MoveList {
    let mut moves = MoveList::new();
    generate_moves_family(position, &mut moves, MoveFamily::Captures);
    moves
}

/// Generates pseudo-legal quiet moves only, in full-generation order.
pub fn generate_pseudo_legal_quiets(position: &Position) -> MoveList {
    let mut moves = MoveList::new();
    generate_moves_family(position, &mut moves, MoveFamily::Quiets);
    moves
}

/// Whether `mv` is a move the pseudo-legal generator would emit for this position.
///
/// This is the containment test `generate_pseudo_legal_moves(position).contains(&mv)`
/// without generating anything, so a staged move picker can validate a transposition
/// -table move before deciding whether to generate at all. The equivalence is pinned
/// exhaustively over every encodable `Move` in the tests below; any divergence would
/// let a corrupt TT entry inject an ungeneratable move into the search.
pub fn is_pseudo_legal(position: &Position, mv: Move) -> bool {
    let us = position.side_to_move();
    let Some(piece) = position.piece_at(mv.from()) else {
        return false;
    };
    if piece.color() != us {
        return false;
    }
    let from = mv.from();
    let to = mv.to();
    let flag = mv.flag();
    let enemies = position.color_occupancy(!us);
    let occupancy = position.occupancy();

    if piece.kind() == PieceKind::Pawn {
        let rank_step: i8 = match us {
            Color::White => 1,
            Color::Black => -1,
        };
        let start_rank = match us {
            Color::White => 1,
            Color::Black => 6,
        };
        let promotion_rank = match us {
            Color::White => 7,
            Color::Black => 0,
        };
        let single_push = offset(from, 0, rank_step);
        let is_diagonal = [-1, 1]
            .into_iter()
            .any(|file_step| offset(from, file_step, rank_step) == Some(to));
        let on_promotion_rank = to.rank() == promotion_rank;
        return if flag.is_en_passant() {
            is_diagonal
                && position.en_passant() == Some(to)
                && offset(to, 0, -rank_step).is_some_and(|square| {
                    position.piece_at(square) == Some(Piece::new(!us, PieceKind::Pawn))
                })
        } else if flag.is_double_pawn_push() {
            from.rank() == start_rank
                && single_push.is_some_and(|mid| {
                    position.piece_at(mid).is_none()
                        && offset(from, 0, rank_step * 2) == Some(to)
                        && position.piece_at(to).is_none()
                })
        } else if flag.promotion().is_some() {
            on_promotion_rank
                && if flag.is_capture() {
                    is_diagonal && enemies.contains(to)
                } else {
                    single_push == Some(to) && position.piece_at(to).is_none()
                }
        } else if flag == MoveFlag::QUIET {
            !on_promotion_rank && single_push == Some(to) && position.piece_at(to).is_none()
        } else if flag == MoveFlag::CAPTURE {
            !on_promotion_rank && is_diagonal && enemies.contains(to)
        } else {
            false
        };
    }

    if flag.is_castling() {
        if piece.kind() != PieceKind::King {
            return false;
        }
        let mut castling = MoveList::new();
        generate_castling(position, &mut castling);
        return castling.contains(&mv);
    }
    if flag != MoveFlag::QUIET && flag != MoveFlag::CAPTURE {
        return false;
    }
    let attacks = match piece.kind() {
        PieceKind::Knight => knight_attacks(from),
        PieceKind::Bishop => bishop_attacks(from, occupancy),
        PieceKind::Rook => rook_attacks(from, occupancy),
        PieceKind::Queen => bishop_attacks(from, occupancy) | rook_attacks(from, occupancy),
        PieceKind::King => king_attacks(from),
        PieceKind::Pawn => unreachable!(),
    };
    if !attacks.contains(to) {
        return false;
    }
    if flag == MoveFlag::CAPTURE {
        enemies.contains(to)
    } else {
        position.piece_at(to).is_none()
    }
}

/// Whether `mv` is a legal move for the side to move.
///
/// This is `generate_legal_moves(position).contains(&mv)` without generating anything,
/// for callers -- such as the transposition-table cutoff verification -- that need to
/// ask about one move instead of the whole list. A move is legal exactly when it is
/// pseudo-legal and leaves the mover's king unattacked.
///
/// The classical fast path answers immediately when the mover is not in check and the
/// moving piece is neither the king, an en passant capture, nor the first piece on any
/// slider ray from its own king: those are the only shapes that can create or expose a
/// check (a capture lands on the captured square, so it never vacates a blocking
/// square; a piece behind another blocker cannot open a line by moving). Everything
/// else -- king moves, en passant, in-check positions, and potential pins -- falls
/// back to make/`is_in_check`/unmake on a scratch copy, and castling legality is
/// already decided by the pseudo-legal generator's Chess960-aware path checks.
pub fn is_legal(position: &Position, mv: Move) -> bool {
    if !is_pseudo_legal(position, mv) {
        return false;
    }
    let us = position.side_to_move();
    let king_square = position.king_square(us);
    let from = mv.from();
    let off_king_ray = !queen_attacks(king_square, position.occupancy()).contains(from);
    if from != king_square
        && !mv.flag().is_en_passant()
        && !is_in_check(position, us)
        && off_king_ray
    {
        return true;
    }
    let mut scratch = position.clone();
    scratch.make_move(mv);
    !is_in_check(&scratch, us)
}

/// Which slice of the full pseudo-legal list a family generation emits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MoveFamily {
    All,
    Captures,
    Quiets,
}

fn generate_pseudo_legal_moves_into(position: &Position, moves: &mut MoveList) {
    generate_moves_family(position, moves, MoveFamily::All);
}

fn generate_moves_family(position: &Position, moves: &mut MoveList, family: MoveFamily) {
    let us = position.side_to_move();
    let them = !us;
    let friends = position.color_occupancy(us);
    let enemies = position.color_occupancy(them);
    let occupancy = friends | enemies;
    // The family IS a target mask: captures land on enemies, quiets on empty squares,
    // and the full list on either. Filtering by mask preserves the full generation's
    // order within each family, which the search's staged move picker depends on.
    let targets = match family {
        MoveFamily::All => !friends,
        MoveFamily::Captures => enemies,
        MoveFamily::Quiets => !occupancy,
    };

    generate_pawns(position, moves, us, enemies, family);
    generate_piece_moves(
        moves,
        position.pieces(us, PieceKind::Knight),
        targets,
        enemies,
        knight_attacks,
    );
    generate_sliders(
        moves,
        position.pieces(us, PieceKind::Bishop),
        targets,
        enemies,
        occupancy,
        bishop_attacks,
    );
    generate_sliders(
        moves,
        position.pieces(us, PieceKind::Rook),
        targets,
        enemies,
        occupancy,
        rook_attacks,
    );
    generate_sliders(
        moves,
        position.pieces(us, PieceKind::Queen),
        targets,
        enemies,
        occupancy,
        |square, occupied| bishop_attacks(square, occupied) | rook_attacks(square, occupied),
    );
    generate_piece_moves(
        moves,
        position.pieces(us, PieceKind::King),
        targets,
        enemies,
        king_attacks,
    );
    if family != MoveFamily::Captures {
        generate_castling(position, moves);
    }
}

fn generate_pawns(
    position: &Position,
    moves: &mut MoveList,
    color: Color,
    enemies: Bitboard,
    family: MoveFamily,
) {
    let rank_step = match color {
        Color::White => 1,
        Color::Black => -1,
    };
    let start_rank = match color {
        Color::White => 1,
        Color::Black => 6,
    };
    let promotion_rank = match color {
        Color::White => 7,
        Color::Black => 0,
    };

    for from in position.pieces(color, PieceKind::Pawn) {
        if let Some(to) = offset(from, 0, rank_step)
            && position.piece_at(to).is_none()
        {
            if to.rank() == promotion_rank {
                if family != MoveFamily::Quiets {
                    push_promotions(moves, from, to, false);
                }
            } else if family != MoveFamily::Captures {
                moves.push(Move::new(from, to, MoveFlag::QUIET));
                if from.rank() == start_rank
                    && let Some(double_to) = offset(from, 0, rank_step * 2)
                    && position.piece_at(double_to).is_none()
                {
                    moves.push(Move::new(from, double_to, MoveFlag::DOUBLE_PAWN_PUSH));
                }
            }
        }

        if family == MoveFamily::Quiets {
            continue;
        }
        for file_step in [-1, 1] {
            let Some(to) = offset(from, file_step, rank_step) else {
                continue;
            };
            if enemies.contains(to) {
                if to.rank() == promotion_rank {
                    push_promotions(moves, from, to, true);
                } else {
                    moves.push(Move::new(from, to, MoveFlag::CAPTURE));
                }
            } else if position.en_passant() == Some(to) {
                let captured = offset(to, 0, -rank_step);
                if captured.is_some_and(|square| {
                    position.piece_at(square) == Some(Piece::new(!color, PieceKind::Pawn))
                }) {
                    moves.push(Move::new(from, to, MoveFlag::EN_PASSANT));
                }
            }
        }
    }
}

fn push_promotions(moves: &mut MoveList, from: Square, to: Square, capture: bool) {
    let flags = if capture {
        [
            MoveFlag::KNIGHT_PROMOTION_CAPTURE,
            MoveFlag::BISHOP_PROMOTION_CAPTURE,
            MoveFlag::ROOK_PROMOTION_CAPTURE,
            MoveFlag::QUEEN_PROMOTION_CAPTURE,
        ]
    } else {
        [
            MoveFlag::KNIGHT_PROMOTION,
            MoveFlag::BISHOP_PROMOTION,
            MoveFlag::ROOK_PROMOTION,
            MoveFlag::QUEEN_PROMOTION,
        ]
    };
    for flag in flags {
        moves.push(Move::new(from, to, flag));
    }
}

fn generate_piece_moves(
    moves: &mut MoveList,
    pieces: Bitboard,
    targets: Bitboard,
    enemies: Bitboard,
    attacks: fn(Square) -> Bitboard,
) {
    for from in pieces {
        push_targets(moves, from, attacks(from) & targets, enemies);
    }
}

fn generate_sliders(
    moves: &mut MoveList,
    pieces: Bitboard,
    targets: Bitboard,
    enemies: Bitboard,
    occupancy: Bitboard,
    attacks: fn(Square, Bitboard) -> Bitboard,
) {
    for from in pieces {
        push_targets(moves, from, attacks(from, occupancy) & targets, enemies);
    }
}

fn push_targets(moves: &mut MoveList, from: Square, targets: Bitboard, enemies: Bitboard) {
    for to in targets {
        let flag = if enemies.contains(to) {
            MoveFlag::CAPTURE
        } else {
            MoveFlag::QUIET
        };
        moves.push(Move::new(from, to, flag));
    }
}

fn generate_castling(position: &Position, moves: &mut MoveList) {
    let color = position.side_to_move();
    let Some(king) = position.pieces(color, PieceKind::King).first() else {
        return;
    };
    if king.rank() != color.back_rank() || is_in_check(position, color) {
        return;
    }

    for side in CastlingSide::ALL {
        let Some(rook) = position.castling_rook(color, side) else {
            continue;
        };
        if position.piece_at(rook) != Some(Piece::new(color, PieceKind::Rook))
            || rook.rank() != color.back_rank()
        {
            continue;
        }

        let king_destination = side.king_destination(color);
        let rook_destination = side.rook_destination(color);
        if !path_is_clear(position, king, king_destination, rook)
            || !path_is_clear(position, rook, rook_destination, king)
            || king_path_is_attacked(position, king, king_destination, rook)
        {
            continue;
        }
        moves.push(Move::new(king, rook, MoveFlag::CASTLING));
    }
}

fn path_is_clear(
    position: &Position,
    from: Square,
    destination: Square,
    allowed_occupant: Square,
) -> bool {
    let step = (destination.file() as i8 - from.file() as i8).signum();
    let mut file = from.file() as i8 + step;
    while file != destination.file() as i8 + step {
        let square = Square::new(from.rank() * 8 + file as u8).unwrap();
        if square != allowed_occupant && position.piece_at(square).is_some() {
            return false;
        }
        file += step;
    }
    true
}

fn king_path_is_attacked(
    position: &Position,
    from: Square,
    destination: Square,
    rook: Square,
) -> bool {
    let color = position.side_to_move();
    let mut occupancy = position.occupancy();
    occupancy.clear(from);
    occupancy.clear(rook);
    let step = (destination.file() as i8 - from.file() as i8).signum();
    let mut file = from.file() as i8 + step;
    while file != destination.file() as i8 + step {
        let square = Square::new(from.rank() * 8 + file as u8).unwrap();
        if is_square_attacked_with_occupancy(position, square, !color, occupancy) {
            return true;
        }
        file += step;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positions exercising every generation branch the family split must preserve:
    /// castling both dialects, en passant, promotions of both kinds, and a random walk
    /// so the equivalence is not tested only on hand-picked boards.
    fn battery() -> Vec<Position> {
        let mut positions: Vec<_> = [
            (
                "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
                false,
            ),
            (
                "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
                false,
            ),
            ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", false),
            ("4k3/1P6/8/8/8/8/6p1/4K3 w - - 0 1", false),
            (
                "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3",
                false,
            ),
            (
                "bqbnrbkr/pppppppp/8/8/8/8/PPPPPPPP/BQBNRBKR w EHeh - 0 1",
                true,
            ),
        ]
        .into_iter()
        .map(|(fen, chess960)| Position::from_fen(fen, chess960).expect("test FEN should parse"))
        .collect();

        let mut walk = Position::startpos();
        for sample in 0..48 {
            positions.push(walk.clone());
            let moves = generate_legal_moves(&walk);
            if moves.is_empty() {
                walk = Position::startpos();
            } else {
                walk.make_move(moves[(sample * 13 + 5) % moves.len()]);
            }
        }
        positions
    }

    /// The capture family must be exactly "captures and promotions from the full
    /// list, in the same order", and the quiet family its exact complement. The search
    /// consumes these two generators as a lazier full generation; any move either one
    /// drops, duplicates, or reorders changes the search tree.
    #[test]
    fn capture_and_quiet_families_partition_full_generation_in_order() {
        for position in battery() {
            let full = generate_pseudo_legal_moves(&position);
            let captures = generate_pseudo_legal_captures(&position);
            let quiets = generate_pseudo_legal_quiets(&position);

            let expected_captures: Vec<_> = full
                .iter()
                .copied()
                .filter(|mv| mv.flag().is_capture() || mv.flag().promotion().is_some())
                .collect();
            let expected_quiets: Vec<_> = full
                .iter()
                .copied()
                .filter(|mv| !mv.flag().is_capture() && mv.flag().promotion().is_none())
                .collect();

            assert_eq!(
                captures.as_slice(),
                expected_captures.as_slice(),
                "capture family diverges from filtered full generation: {position:?}"
            );
            assert_eq!(
                quiets.as_slice(),
                expected_quiets.as_slice(),
                "quiet family diverges from filtered full generation: {position:?}"
            );
        }
    }

    /// `has_legal_move_in_place` probes the capture family before the quiet family,
    /// which must not change its answer: it is the quiescence search's stalemate
    /// probe, and a wrong answer would let a stalemate score reach the transposition
    /// table. The three hand-built positions pin each branch -- only a capture is
    /// legal (pseudo-legal quiets all illegal), only quiets are legal (the one
    /// pseudo-legal capture is pin-illegal), and no legal move at all (stalemate) --
    /// and the battery checks agreement with full generation everywhere else.
    #[test]
    fn captures_first_legal_move_probe_agrees_with_full_generation() {
        let branch_positions = [
            // Black's only legal move is Kxg7; the quiet king moves are all illegal.
            ("7k/6Q1/8/8/8/8/8/K7 b - - 0 1", true),
            // White's only pseudo-legal capture Nxg3 is pin-illegal (rook e8 vs king
            // e1); the probe must fall through to the quiet family and answer true.
            ("4r3/8/8/8/4N2k/6p1/8/4K3 w - - 0 1", true),
            // Stalemate: no legal move in either family.
            ("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", false),
        ];
        for (fen, expected) in branch_positions {
            let mut position = Position::from_fen(fen, false).expect("test FEN should parse");
            assert_eq!(has_legal_move_in_place(&mut position), expected, "{fen}");
            assert_eq!(
                !generate_legal_moves(&position).is_empty(),
                expected,
                "{fen}"
            );
        }
        for position in battery() {
            let mut probed = position.clone();
            assert_eq!(
                has_legal_move_in_place(&mut probed),
                !generate_legal_moves(&position).is_empty(),
                "{position:?}"
            );
        }
    }

    /// `is_pseudo_legal` must agree with generated-list containment for EVERY encodable
    /// move, not just for moves a well-behaved search would produce. The staged move
    /// picker trusts this function to validate a transposition-table move without
    /// generating anything, and a TT entry can decode to any 16-bit pattern with a
    /// valid flag, so the equivalence is checked exhaustively over the whole encoding
    /// space on every battery position.
    #[test]
    fn is_pseudo_legal_agrees_with_generated_list_containment_for_every_encodable_move() {
        for position in battery() {
            let generated: std::collections::HashSet<u16> = generate_pseudo_legal_moves(&position)
                .iter()
                .map(|mv| mv.raw())
                .collect();
            for raw in 0..=u16::MAX {
                let Some(mv) = Move::from_raw(raw) else {
                    continue;
                };
                assert_eq!(
                    is_pseudo_legal(&position, mv),
                    generated.contains(&raw),
                    "{position:?} {mv:?}"
                );
            }
        }
    }

    /// `is_legal` must agree with legal-list containment for EVERY encodable move, not
    /// just for moves a well-behaved search would produce: the transposition-table
    /// cutoff trusts this predicate to vet a stored move whose encoding can be any
    /// 16-bit pattern with a valid flag. The battery covers castling in both dialects,
    /// en passant, promotions, and a long random walk, so the pin, check, king-move,
    /// and en-passant branches of the fast path are all exercised.
    #[test]
    fn is_legal_agrees_with_generated_legal_list_for_every_encodable_move() {
        for position in battery() {
            let generated: std::collections::HashSet<u16> = generate_legal_moves(&position)
                .iter()
                .map(|mv| mv.raw())
                .collect();
            for raw in 0..=u16::MAX {
                let Some(mv) = Move::from_raw(raw) else {
                    continue;
                };
                assert_eq!(
                    is_legal(&position, mv),
                    generated.contains(&raw),
                    "{position:?} {mv:?}"
                );
            }
        }
    }
}
