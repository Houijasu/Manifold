//! FullThreats feature indexing and active-feature enumeration.
//!
//! Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
//! Eonego's copyright and MIT license notice are reproduced there.

use std::sync::OnceLock;

use mf_core::{
    Bitboard, CastlingSide, Color, Move, Piece, PieceKind, Position, Square, Undo, bishop_attacks,
    king_attacks, knight_attacks, pawn_attacks, queen_attacks, rook_attacks,
};

/// Number of FullThreats input dimensions.
pub const DIMENSIONS: usize = 60_720;
/// Safe fixed capacity for active FullThreats features.
pub const MAX_ACTIVE: usize = 256;
/// Maximum number of physical FullThreats edge changes produced by one move.
pub(crate) const MAX_CHANGED: usize = 128;

const PIECE_NB: usize = 16;
const BOARD_SQUARES: usize = 64;
const INDEX_LUT1_LEN: usize = PIECE_NB * PIECE_NB * 2;
const INDEX_LUT2_LEN: usize = PIECE_NB * BOARD_SQUARES * BOARD_SQUARES;
const OFFSETS_LEN: usize = PIECE_NB * BOARD_SQUARES;
const ALL_PIECES: [u8; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];
const NUM_VALID_TARGETS: [usize; PIECE_NB] = [0, 6, 10, 8, 8, 10, 0, 0, 0, 6, 10, 8, 8, 10, 0, 0];
const THREAT_MAP: [[i8; 6]; 6] = [
    [0, 1, -1, 2, -1, -1],
    [0, 1, 2, 3, 4, -1],
    [0, 1, 2, 3, -1, -1],
    [0, 1, 2, 3, -1, -1],
    [0, 1, 2, 3, 4, -1],
    [-1, -1, -1, -1, -1, -1],
];
const ORIENT_TABLE: [u8; 64] = [
    0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7,
    0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7, 0, 0, 0, 0, 7, 7, 7, 7,
];
const FILE_A: u64 = 0x0101_0101_0101_0101;
const FILE_H: u64 = 0x8080_8080_8080_8080;
const DIRTY_SIGN_BIT: u32 = 1 << 20;
const DIRTY_EDGE_MASK: u32 = DIRTY_SIGN_BIT - 1;

/// A piece in the FullThreats reference encoding: `(color << 3) + piece_type`.
///
/// Piece types are pawn=1 through king=6, matching the Eonego feature format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreatPiece(u8);

impl ThreatPiece {
    #[inline]
    pub const fn new(color: Color, kind: PieceKind) -> Self {
        Self(((color as u8) << 3) + kind as u8 + 1)
    }

    #[inline]
    const fn encoded(self) -> usize {
        self.0 as usize
    }
}

/// Perspective-independent signed FullThreats edge change.
///
/// Bits match Eonego exactly: attacker | attacked << 4 | from << 8 | to << 14,
/// with bit 20 set for additions and clear for removals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirtyThreat(u32);

impl DirtyThreat {
    const EMPTY: Self = Self(0);

    #[inline]
    pub(crate) const fn new(
        attacker: ThreatPiece,
        from: Square,
        to: Square,
        attacked: ThreatPiece,
        sign: i32,
    ) -> Self {
        let edge = attacker.0 as u32
            | ((attacked.0 as u32) << 4)
            | ((from.index() as u32) << 8)
            | ((to.index() as u32) << 14);
        Self(edge | if sign > 0 { DIRTY_SIGN_BIT } else { 0 })
    }

    #[inline]
    #[cfg(test)]
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }

    #[inline]
    pub(crate) const fn physical_bits(self) -> u32 {
        self.0 & DIRTY_EDGE_MASK
    }

    #[inline]
    pub(crate) const fn sign(self) -> i32 {
        if self.0 & DIRTY_SIGN_BIT != 0 { 1 } else { -1 }
    }

    #[inline]
    pub(crate) const fn with_sign(self, sign: i32) -> Self {
        Self(self.physical_bits() | if sign > 0 { DIRTY_SIGN_BIT } else { 0 })
    }

    #[inline]
    pub(crate) const fn attacker(self) -> ThreatPiece {
        ThreatPiece((self.physical_bits() & 0xF) as u8)
    }

    #[inline]
    pub(crate) const fn attacked(self) -> ThreatPiece {
        ThreatPiece(((self.physical_bits() >> 4) & 0xF) as u8)
    }

    #[inline]
    pub(crate) fn from(self) -> Square {
        square(((self.physical_bits() >> 8) & 0x3F) as usize)
    }

    #[inline]
    pub(crate) fn to(self) -> Square {
        square(((self.physical_bits() >> 14) & 0x3F) as usize)
    }
}

/// Fixed-capacity changed-edge buffer. Overflow invalidates the whole delta.
#[derive(Clone)]
pub(crate) struct ChangedThreatBuffer<const CAPACITY: usize> {
    edges: [DirtyThreat; CAPACITY],
    len: usize,
    overflowed: bool,
}

impl<const CAPACITY: usize> ChangedThreatBuffer<CAPACITY> {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            edges: [DirtyThreat::EMPTY; CAPACITY],
            len: 0,
            overflowed: false,
        }
    }

    #[inline]
    #[cfg(any(test, feature = "instrumentation"))]
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub(crate) const fn overflowed(&self) -> bool {
        self.overflowed
    }

    #[inline]
    pub(crate) fn reset(&mut self) {
        self.len = 0;
        self.overflowed = false;
    }

    #[inline]
    pub(crate) fn iter(&self) -> impl Iterator<Item = DirtyThreat> + '_ {
        self.edges[..self.len].iter().copied()
    }

    /// Adds a raw entry without netting. Used by the fixed-buffer contract test.
    #[inline]
    pub(crate) fn push(&mut self, edge: DirtyThreat) -> bool {
        if self.overflowed {
            return false;
        }
        if self.len == CAPACITY {
            self.overflowed = true;
            return false;
        }
        self.edges[self.len] = edge;
        self.len += 1;
        true
    }

    /// Adds one locally discovered edge, removing duplicates and opposite-sign cancellations.
    fn push_netted(&mut self, edge: DirtyThreat) {
        if self.overflowed {
            return;
        }
        if let Some(index) = self.edges[..self.len]
            .iter()
            .position(|candidate| candidate.physical_bits() == edge.physical_bits())
        {
            if self.edges[index].sign() != edge.sign() {
                self.len -= 1;
                self.edges[index] = self.edges[self.len];
            }
            return;
        }
        let _ = self.push(edge);
    }
}

#[derive(Debug)]
struct ThreatLuts {
    index_lut1: [u32; INDEX_LUT1_LEN],
    index_lut2: [u8; INDEX_LUT2_LEN],
    offsets: [u16; OFFSETS_LEN],
}

static THREAT_LUTS: OnceLock<ThreatLuts> = OnceLock::new();
static RAY_BEYOND: OnceLock<[u64; BOARD_SQUARES * BOARD_SQUARES]> = OnceLock::new();

#[inline]
const fn piece_type(piece: usize) -> usize {
    piece & 7
}

#[inline]
const fn piece_color(piece: usize) -> usize {
    piece >> 3
}

#[inline]
fn square(index: usize) -> Square {
    Square::new(index as u8).expect("FullThreats square must be on board")
}

#[inline]
fn pawn_push_or_attacks(color: usize, from: usize) -> Bitboard {
    let color = if color == Color::White.index() {
        Color::White
    } else {
        Color::Black
    };
    let from = square(from);
    let push = match color {
        Color::White if from.index() < 56 => 1u64 << (from.index() + 8),
        Color::Black if from.index() >= 8 => 1u64 << (from.index() - 8),
        Color::White | Color::Black => 0,
    };
    Bitboard::new(pawn_attacks(from, color).bits() | push)
}

#[inline]
fn pseudo_attacks(kind: usize, from: usize) -> Bitboard {
    let from = square(from);
    match kind {
        2 => knight_attacks(from),
        3 => bishop_attacks(from, Bitboard::EMPTY),
        4 => rook_attacks(from, Bitboard::EMPTY),
        5 => queen_attacks(from, Bitboard::EMPTY),
        6 => king_attacks(from),
        _ => unreachable!("only non-pawn reference piece types have pseudo-attacks"),
    }
}

#[inline]
fn attack_set(piece: usize, from: usize) -> Bitboard {
    if piece_type(piece) == 1 {
        pawn_push_or_attacks(piece_color(piece), from)
    } else {
        pseudo_attacks(piece_type(piece), from)
    }
}

fn build_luts() -> ThreatLuts {
    let mut luts = ThreatLuts {
        index_lut1: [DIMENSIONS as u32; INDEX_LUT1_LEN],
        index_lut2: [0; INDEX_LUT2_LEN],
        offsets: [0; OFFSETS_LEN],
    };

    for &piece in &ALL_PIECES {
        let piece = usize::from(piece);
        for from in 0..BOARD_SQUARES {
            let attacks = attack_set(piece, from).bits();
            for to in 0..BOARD_SQUARES {
                let below_target = if to == 0 { 0 } else { (1u64 << to) - 1 };
                luts.index_lut2[(piece * BOARD_SQUARES + from) * BOARD_SQUARES + to] =
                    (attacks & below_target).count_ones() as u8;
            }
        }
    }

    let mut cumulative_piece = [0usize; PIECE_NB];
    let mut cumulative_offsets = [0usize; PIECE_NB];
    let mut cumulative_offset = 0;
    for &piece in &ALL_PIECES {
        let piece = usize::from(piece);
        let mut piece_offset = 0;
        let is_pawn = piece_type(piece) == 1;
        for from in 0..BOARD_SQUARES {
            luts.offsets[piece * BOARD_SQUARES + from] = piece_offset as u16;
            if !is_pawn {
                piece_offset += pseudo_attacks(piece_type(piece), from).count() as usize;
            } else if (8..=55).contains(&from) {
                piece_offset += pawn_push_or_attacks(piece_color(piece), from).count() as usize;
            }
        }
        cumulative_piece[piece] = piece_offset;
        cumulative_offsets[piece] = cumulative_offset;
        cumulative_offset += NUM_VALID_TARGETS[piece] * piece_offset;
    }
    debug_assert_eq!(cumulative_offset, DIMENSIONS);

    for &attacker in &ALL_PIECES {
        let attacker = usize::from(attacker);
        for &attacked in &ALL_PIECES {
            let attacked = usize::from(attacked);
            let enemy = attacker ^ attacked == 8;
            let attacker_type = piece_type(attacker);
            let attacked_type = piece_type(attacked);
            let mapped = THREAT_MAP[attacker_type - 1][attacked_type - 1];
            let semi_excluded = attacker_type == attacked_type && (enemy || attacker_type != 1);
            let table_index = (attacker * PIECE_NB + attacked) * 2;

            if mapped >= 0 {
                let feature = cumulative_offsets[attacker]
                    + (piece_color(attacked) * (NUM_VALID_TARGETS[attacker] / 2) + mapped as usize)
                        * cumulative_piece[attacker];
                luts.index_lut1[table_index] = feature as u32;
                if !semi_excluded {
                    luts.index_lut1[table_index + 1] = feature as u32;
                }
            }
        }
    }

    luts
}

#[inline]
fn luts() -> &'static ThreatLuts {
    THREAT_LUTS.get_or_init(build_luts)
}

fn build_ray_beyond() -> [u64; BOARD_SQUARES * BOARD_SQUARES] {
    let mut table = [0_u64; BOARD_SQUARES * BOARD_SQUARES];
    for from in 0_i8..64 {
        for through in 0_i8..64 {
            if from == through {
                continue;
            }
            let from_file = from % 8;
            let from_rank = from / 8;
            let through_file = through % 8;
            let through_rank = through / 8;
            let file_delta = through_file - from_file;
            let rank_delta = through_rank - from_rank;
            let step = if rank_delta == 0 {
                file_delta.signum()
            } else if file_delta == 0 {
                8 * rank_delta.signum()
            } else if file_delta == rank_delta {
                9 * rank_delta.signum()
            } else if file_delta == -rank_delta {
                7 * rank_delta.signum()
            } else {
                0
            };
            if step == 0 {
                continue;
            }

            let mut target = through + step;
            while (0..64).contains(&target) {
                let target_file = target % 8;
                let target_rank = target / 8;
                let target_file_delta = target_file - from_file;
                let target_rank_delta = target_rank - from_rank;
                let aligned = if rank_delta == 0 {
                    target_rank_delta == 0
                } else if file_delta == 0 {
                    target_file_delta == 0
                } else if file_delta == rank_delta {
                    target_file_delta == target_rank_delta
                        && target_file_delta.signum() == file_delta.signum()
                } else {
                    target_file_delta == -target_rank_delta
                        && target_file_delta.signum() == file_delta.signum()
                };
                if !aligned {
                    break;
                }
                table[from as usize * BOARD_SQUARES + through as usize] |= 1_u64 << target;
                target += step;
            }
        }
    }
    table
}

#[inline]
fn ray_beyond(from: Square, through: Square) -> Bitboard {
    Bitboard::new(
        RAY_BEYOND.get_or_init(build_ray_beyond)
            [usize::from(from.index()) * BOARD_SQUARES + usize::from(through.index())],
    )
}

#[inline]
fn make_index_oriented(
    orientation: u8,
    swap: usize,
    attacker: ThreatPiece,
    from: Square,
    to: Square,
    attacked: ThreatPiece,
) -> usize {
    let luts = luts();
    let from = usize::from(from.index() ^ orientation);
    let to = usize::from(to.index() ^ orientation);
    let attacker = attacker.encoded() ^ swap;
    let attacked = attacked.encoded() ^ swap;

    luts.index_lut1[(attacker * PIECE_NB + attacked) * 2 + usize::from(from < to)] as usize
        + usize::from(luts.offsets[attacker * BOARD_SQUARES + from])
        + usize::from(luts.index_lut2[(attacker * BOARD_SQUARES + from) * BOARD_SQUARES + to])
}

/// Returns whether two king squares produce identical FullThreats indices for every edge.
///
/// [`make_index`] consults the king square only through `ORIENT_TABLE`, which depends solely on
/// the king's file. Two king squares that share an orientation therefore agree on every threat
/// feature index, so a king move between them leaves the whole FullThreats contribution intact.
#[inline]
#[must_use]
pub fn mirrors_alike(left: Square, right: Square) -> bool {
    ORIENT_TABLE[usize::from(left.index())] == ORIENT_TABLE[usize::from(right.index())]
}

/// Returns the FullThreats feature index for one physical threat.
///
/// Excluded threats return an index greater than or equal to [`DIMENSIONS`].
#[inline]
pub fn make_index(
    perspective: Color,
    attacker: ThreatPiece,
    from: Square,
    to: Square,
    attacked: ThreatPiece,
    king_square: Square,
) -> usize {
    let orientation = ORIENT_TABLE[usize::from(king_square.index())] ^ (56 * perspective as u8);
    make_index_oriented(
        orientation,
        8 * perspective.index(),
        attacker,
        from,
        to,
        attacked,
    )
}

/// Appends all active FullThreats features for one perspective into a fixed buffer.
///
/// Own-piece contacts are included as defences. Kings are targets but never attackers.
#[inline]
pub fn append_active_threats(
    perspective: Color,
    position: &Position,
    buffer: &mut [usize; MAX_ACTIVE],
) -> usize {
    let king_square = position.king_square(perspective);
    let orientation = ORIENT_TABLE[usize::from(king_square.index())] ^ (56 * perspective as u8);
    let swap = 8 * perspective.index();
    let occupied = position.occupancy();
    let occupied_bits = occupied.bits();
    let all_pawns = position.pieces(Color::White, PieceKind::Pawn).bits()
        | position.pieces(Color::Black, PieceKind::Pawn).bits();
    let mut count = 0;

    for color in [perspective, !perspective] {
        let attacker = ThreatPiece::new(color, PieceKind::Pawn);
        let color_pawns = position.pieces(color, PieceKind::Pawn).bits();

        match color {
            Color::White => {
                let mut northeast = ((color_pawns & !FILE_H) << 9) & occupied_bits;
                while northeast != 0 {
                    let to = northeast.trailing_zeros() as u8;
                    northeast &= northeast - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to - 9,
                        to,
                    );
                }

                let mut northwest = ((color_pawns & !FILE_A) << 7) & occupied_bits;
                while northwest != 0 {
                    let to = northwest.trailing_zeros() as u8;
                    northwest &= northwest - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to - 7,
                        to,
                    );
                }

                let mut blocked = ((all_pawns >> 8) & color_pawns) << 8;
                while blocked != 0 {
                    let to = blocked.trailing_zeros() as u8;
                    blocked &= blocked - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to - 8,
                        to,
                    );
                }
            }
            Color::Black => {
                let mut southwest = ((color_pawns & !FILE_A) >> 9) & occupied_bits;
                while southwest != 0 {
                    let to = southwest.trailing_zeros() as u8;
                    southwest &= southwest - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to + 9,
                        to,
                    );
                }

                let mut southeast = ((color_pawns & !FILE_H) >> 7) & occupied_bits;
                while southeast != 0 {
                    let to = southeast.trailing_zeros() as u8;
                    southeast &= southeast - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to + 7,
                        to,
                    );
                }

                let mut blocked = ((all_pawns << 8) & color_pawns) >> 8;
                while blocked != 0 {
                    let to = blocked.trailing_zeros() as u8;
                    blocked &= blocked - 1;
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        to + 8,
                        to,
                    );
                }
            }
        }

        for kind in [
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Rook,
            PieceKind::Queen,
        ] {
            let attacker = ThreatPiece::new(color, kind);
            let mut pieces = position.pieces(color, kind);
            while let Some(from) = pieces.pop_first() {
                let mut attacks = match kind {
                    PieceKind::Knight => knight_attacks(from),
                    PieceKind::Bishop => bishop_attacks(from, occupied),
                    PieceKind::Rook => rook_attacks(from, occupied),
                    PieceKind::Queen => queen_attacks(from, occupied),
                    PieceKind::Pawn | PieceKind::King => unreachable!(),
                } & occupied;
                while let Some(to) = attacks.pop_first() {
                    emit(
                        position,
                        buffer,
                        &mut count,
                        orientation,
                        swap,
                        attacker,
                        from.index(),
                        to.index(),
                    );
                }
            }
        }
    }

    count
}

/// Discovers move-local physical FullThreats edge changes.
///
/// The scan is restricted to moved/captured/replaced squares, their direct contacts, and sliders
/// whose ray crosses one of those squares. It never enumerates every threat in both positions.
///
/// Returns the number of slider candidates inspected, which is always zero unless the
/// `instrumentation` feature is enabled: counting them costs a `popcnt` per affected square in
/// the hottest loop in the engine, so the production build must not pay for it.
pub(crate) fn discover_changed_threats<const CAPACITY: usize>(
    parent: &Position,
    child: &Position,
    mv: Move,
    undo: &Undo,
    changed: &mut ChangedThreatBuffer<CAPACITY>,
) -> usize {
    let mut sliders_scanned = 0;
    discover_changed_threats_impl::<CAPACITY, { cfg!(feature = "instrumentation") }>(
        parent,
        child,
        mv,
        undo,
        changed,
        &mut sliders_scanned,
    );
    sliders_scanned
}

/// Returns the squares whose threat edges a move can change, without duplicates.
///
/// These are the moved, captured and replaced squares. Every edge that differs between parent
/// and child either starts or ends on one of them, or is a slider ray crossing one of them.
fn affected_squares(mv: Move, undo: &Undo) -> ([Square; 4], usize) {
    let mut affected = [A1; 4];
    let mut count = 0;
    let mut append_square = |candidate: Square| {
        if !affected[..count].contains(&candidate) {
            affected[count] = candidate;
            count += 1;
        }
    };

    if mv.flag().is_castling() {
        let color = undo.moved().color();
        let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
        append_square(mv.from());
        append_square(side.king_destination(color));
        append_square(mv.to());
        append_square(side.rook_destination(color));
    } else {
        append_square(mv.from());
        append_square(mv.to());
        if let Some((captured_square, _)) = undo.captured() {
            append_square(captured_square);
        }
    }
    (affected, count)
}

fn discover_changed_threats_impl<const CAPACITY: usize, const PROFILE: bool>(
    parent: &Position,
    child: &Position,
    mv: Move,
    undo: &Undo,
    changed: &mut ChangedThreatBuffer<CAPACITY>,
    sliders_scanned: &mut usize,
) {
    let (affected, count) = affected_squares(mv, undo);

    for &affected_square in &affected[..count] {
        gather_changed_square::<CAPACITY, PROFILE>(
            parent,
            child,
            affected_square,
            changed,
            sliders_scanned,
        );
        if changed.overflowed() {
            return;
        }
    }
}

const A1: Square = match Square::new(0) {
    Some(square) => square,
    None => unreachable!(),
};

fn gather_changed_square<const CAPACITY: usize, const PROFILE: bool>(
    parent: &Position,
    child: &Position,
    affected: Square,
    changed: &mut ChangedThreatBuffer<CAPACITY>,
    sliders_scanned: &mut usize,
) {
    for (position, source_is_parent) in [(parent, true), (child, false)] {
        // Whether the affected square holds a piece in *this* position decides which half of
        // the scan can produce an edge at all, and the two halves are complementary.
        //
        // Occupied: the square terminates edges, so outgoing targets, incoming attackers and
        // the `attacker -> affected` slider contacts are all live. But a slider's attacks stop
        // *at* an occupied square, so `ray_beyond(attacker, affected) & slider_attacks(attacker)`
        // is provably empty and the discovered-contact probe is dead work.
        //
        // Empty: no physical edge can terminate here, so every incoming attacker would be
        // discarded by `known_physical_edge` and the outgoing scan has no piece to run from.
        // The only live edges are the discovered contacts that sliders now reach *through* the
        // vacated square, which is exactly the probe the occupied case cannot produce.
        let occupant = position.piece_at(affected);
        let occupied = occupant.is_some();

        if occupied {
            if occupant.is_some_and(|piece| piece.kind() != PieceKind::King) {
                let mut targets = outgoing_targets(position, affected);
                while let Some(target) = targets.pop_first() {
                    record_source_edge(position, source_is_parent, affected, target, changed);
                }
            }

            let mut incoming = non_slider_attackers_to(position, affected);
            while let Some(attacker) = incoming.pop_first() {
                record_source_edge(position, source_is_parent, attacker, affected, changed);
            }
        }

        let occupancy = position.occupancy();
        let rook_queens = position.pieces(Color::White, PieceKind::Rook)
            | position.pieces(Color::Black, PieceKind::Rook)
            | position.pieces(Color::White, PieceKind::Queen)
            | position.pieces(Color::Black, PieceKind::Queen);
        let bishop_queens = position.pieces(Color::White, PieceKind::Bishop)
            | position.pieces(Color::Black, PieceKind::Bishop)
            | position.pieces(Color::White, PieceKind::Queen)
            | position.pieces(Color::Black, PieceKind::Queen);
        let mut sliders = (rook_attacks(affected, occupancy) & rook_queens)
            | (bishop_attacks(affected, occupancy) & bishop_queens);
        if PROFILE {
            *sliders_scanned += sliders.count() as usize;
        }
        while let Some(attacker) = sliders.pop_first() {
            if occupied {
                record_source_edge(position, source_is_parent, attacker, affected, changed);
            } else if let Some(target) = first_blocker_beyond(attacker, affected, occupancy) {
                record_source_edge(position, source_is_parent, attacker, target, changed);
            }
        }
    }
}

/// Returns the first occupied square a slider on `attacker` reaches *past* the empty `through`.
///
/// The caller guarantees `attacker` attacks `through` and that `through` is empty, so the ray is
/// clear all the way to the first blocker beyond it. That blocker is the occupied square on
/// `ray_beyond` nearest `through`, which is the lowest set bit when the ray runs toward higher
/// square indices and the highest set bit when it runs toward lower ones. Selecting it by
/// direction avoids regenerating the attacker's whole magic attack set, which is the single most
/// expensive operation the old formulation performed per slider candidate.
#[inline]
fn first_blocker_beyond(attacker: Square, through: Square, occupancy: Bitboard) -> Option<Square> {
    let blockers = (ray_beyond(attacker, through) & occupancy).bits();
    if blockers == 0 {
        return None;
    }
    let nearest = if through.index() > attacker.index() {
        blockers.trailing_zeros()
    } else {
        63 - blockers.leading_zeros()
    };
    Square::new(nearest as u8)
}

fn record_source_edge<const CAPACITY: usize>(
    source: &Position,
    source_is_parent: bool,
    attacker: Square,
    target: Square,
    changed: &mut ChangedThreatBuffer<CAPACITY>,
) {
    // The parent and child scans emit opposite signs, so unchanged physical edges cancel here
    // without regenerating the attack set in the other position.
    if let Some(edge) = known_physical_edge(source, attacker, target) {
        changed.push_netted(edge.with_sign(if source_is_parent { -1 } else { 1 }));
    }
}

fn known_physical_edge(position: &Position, from: Square, to: Square) -> Option<DirtyThreat> {
    // Callers obtained this pair from an attack set, so re-generating attacks would duplicate
    // the most expensive part of changed-edge discovery.
    let attacker = position.piece_at(from)?;
    let attacked = position.piece_at(to)?;
    if attacker.kind() == PieceKind::King {
        return None;
    }
    Some(DirtyThreat::new(
        ThreatPiece::from(attacker),
        from,
        to,
        ThreatPiece::from(attacked),
        1,
    ))
}

fn outgoing_targets(position: &Position, from: Square) -> Bitboard {
    let Some(attacker) = position.piece_at(from) else {
        return Bitboard::EMPTY;
    };
    let occupied = position.occupancy();
    match attacker.kind() {
        PieceKind::Pawn => {
            let push = match attacker.color() {
                Color::White if from.index() < 56 => 1_u64 << (from.index() + 8),
                Color::Black if from.index() >= 8 => 1_u64 << (from.index() - 8),
                Color::White | Color::Black => 0,
            };
            (pawn_attacks(from, attacker.color()) & occupied)
                | (Bitboard::new(push)
                    & (position.pieces(Color::White, PieceKind::Pawn)
                        | position.pieces(Color::Black, PieceKind::Pawn)))
        }
        PieceKind::Knight => knight_attacks(from) & occupied,
        PieceKind::Bishop => bishop_attacks(from, occupied) & occupied,
        PieceKind::Rook => rook_attacks(from, occupied) & occupied,
        PieceKind::Queen => queen_attacks(from, occupied) & occupied,
        PieceKind::King => Bitboard::EMPTY,
    }
}

fn non_slider_attackers_to(position: &Position, target: Square) -> Bitboard {
    let mut attackers = Bitboard::EMPTY;
    for color in Color::ALL {
        attackers |= pawn_attacks(target, !color) & position.pieces(color, PieceKind::Pawn);
    }
    attackers |= knight_attacks(target)
        & (position.pieces(Color::White, PieceKind::Knight)
            | position.pieces(Color::Black, PieceKind::Knight));

    if position
        .piece_at(target)
        .is_some_and(|piece| piece.kind() == PieceKind::Pawn)
    {
        if target.index() >= 8 {
            let from = square(usize::from(target.index() - 8));
            if position.piece_at(from) == Some(Piece::new(Color::White, PieceKind::Pawn)) {
                attackers |= Bitboard::new(1_u64 << from.index());
            }
        }
        if target.index() < 56 {
            let from = square(usize::from(target.index() + 8));
            if position.piece_at(from) == Some(Piece::new(Color::Black, PieceKind::Pawn)) {
                attackers |= Bitboard::new(1_u64 << from.index());
            }
        }
    }
    attackers
}

/// Converts physical changed edges into perspective-dependent add/sub feature rows.
pub(crate) fn append_changed_threat_indices<const CAPACITY: usize>(
    perspective: Color,
    position: &Position,
    changed: &ChangedThreatBuffer<CAPACITY>,
    additions: &mut [u32],
    removals: &mut [u32],
) -> (usize, usize) {
    let king_square = position.king_square(perspective);
    let mut added = 0;
    let mut removed = 0;
    for edge in changed.iter() {
        let index = make_index(
            perspective,
            edge.attacker(),
            edge.from(),
            edge.to(),
            edge.attacked(),
            king_square,
        );
        if index >= DIMENSIONS {
            continue;
        }
        if edge.sign() > 0 {
            additions[added] = index as u32;
            added += 1;
        } else {
            removals[removed] = index as u32;
            removed += 1;
        }
    }
    (added, removed)
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn emit(
    position: &Position,
    buffer: &mut [usize; MAX_ACTIVE],
    count: &mut usize,
    orientation: u8,
    swap: usize,
    attacker: ThreatPiece,
    from: u8,
    to: u8,
) {
    let to_square = square(usize::from(to));
    let attacked = ThreatPiece::from(
        position
            .piece_at(to_square)
            .expect("FullThreats targets must be occupied"),
    );
    let index = make_index_oriented(
        orientation,
        swap,
        attacker,
        square(usize::from(from)),
        to_square,
        attacked,
    );
    if index < DIMENSIONS && *count < MAX_ACTIVE {
        buffer[*count] = index;
        *count += 1;
    }
}

impl From<Piece> for ThreatPiece {
    #[inline]
    fn from(piece: Piece) -> Self {
        Self::new(piece.color(), piece.kind())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mf_core::{
        Move, PieceKind, Position, Square, Undo, bishop_attacks, generate_legal_moves,
        knight_attacks, parse_uci_move, pawn_attacks, queen_attacks, rook_attacks,
    };

    use super::{
        ChangedThreatBuffer, DirtyThreat, ThreatPiece, append_changed_threat_indices,
        discover_changed_threats,
    };

    fn parse(position: &Position, notation: &str, chess960: bool) -> Move {
        parse_uci_move(position, notation, chess960)
            .unwrap_or_else(|| panic!("{notation} should be legal in {position:?}"))
    }

    fn square(index: u8) -> Square {
        Square::new(index).expect("test square should be valid")
    }

    fn physical_edges(position: &Position) -> Vec<DirtyThreat> {
        let occupied = position.occupancy();
        let all_pawns = position.pieces(super::Color::White, PieceKind::Pawn)
            | position.pieces(super::Color::Black, PieceKind::Pawn);
        let mut edges = Vec::new();

        for color in super::Color::ALL {
            let mut pawns = position.pieces(color, PieceKind::Pawn);
            while let Some(from) = pawns.pop_first() {
                let push = match color {
                    super::Color::White if from.index() < 56 => 1_u64 << (from.index() + 8),
                    super::Color::Black if from.index() >= 8 => 1_u64 << (from.index() - 8),
                    super::Color::White | super::Color::Black => 0,
                };
                let mut targets = pawn_attacks(from, color) & occupied;
                targets |= super::Bitboard::new(push) & all_pawns;
                while let Some(to) = targets.pop_first() {
                    edges.push(DirtyThreat::new(
                        ThreatPiece::new(color, PieceKind::Pawn),
                        from,
                        to,
                        ThreatPiece::from(
                            position.piece_at(to).expect("physical target is occupied"),
                        ),
                        1,
                    ));
                }
            }

            for kind in [
                PieceKind::Knight,
                PieceKind::Bishop,
                PieceKind::Rook,
                PieceKind::Queen,
            ] {
                let mut pieces = position.pieces(color, kind);
                while let Some(from) = pieces.pop_first() {
                    let mut targets = match kind {
                        PieceKind::Knight => knight_attacks(from),
                        PieceKind::Bishop => bishop_attacks(from, occupied),
                        PieceKind::Rook => rook_attacks(from, occupied),
                        PieceKind::Queen => queen_attacks(from, occupied),
                        PieceKind::Pawn | PieceKind::King => unreachable!(),
                    } & occupied;
                    while let Some(to) = targets.pop_first() {
                        edges.push(DirtyThreat::new(
                            ThreatPiece::new(color, kind),
                            from,
                            to,
                            ThreatPiece::from(
                                position.piece_at(to).expect("physical target is occupied"),
                            ),
                            1,
                        ));
                    }
                }
            }
        }
        edges
    }

    fn normalized_signed(edges: impl IntoIterator<Item = DirtyThreat>) -> BTreeMap<u32, i32> {
        let mut counts = BTreeMap::new();
        for edge in edges {
            *counts.entry(edge.physical_bits()).or_default() += edge.sign();
        }
        counts.retain(|_, count| *count != 0);
        counts
    }

    fn active_indices(position: &Position, perspective: super::Color) -> Vec<usize> {
        let mut indices = [0_usize; super::MAX_ACTIVE];
        let count = super::append_active_threats(perspective, position, &mut indices);
        indices[..count].to_vec()
    }

    fn normalized_indices(entries: impl IntoIterator<Item = (usize, i32)>) -> BTreeMap<usize, i32> {
        let mut counts = BTreeMap::new();
        for (index, sign) in entries {
            *counts.entry(index).or_default() += sign;
        }
        counts.retain(|_, count| *count != 0);
        counts
    }

    fn assert_changed_edges_match_full_diff(
        fen: &str,
        notation: &str,
        chess960: bool,
    ) -> (Position, Position, Move, Undo, ChangedThreatBuffer<128>) {
        let parent = Position::from_fen(fen, chess960).expect("targeted FEN should parse");
        let mv = parse(&parent, notation, chess960);
        let mut child = parent.clone();
        let undo = child.make_move(mv);
        let mut changed = ChangedThreatBuffer::<128>::new();
        discover_changed_threats(&parent, &child, mv, &undo, &mut changed);
        assert!(!changed.overflowed(), "{fen}, {notation}");

        let expected = normalized_signed(
            physical_edges(&parent)
                .into_iter()
                .map(|edge| edge.with_sign(-1))
                .chain(physical_edges(&child)),
        );
        let actual = normalized_signed(changed.iter());
        assert_eq!(actual, expected, "{fen}, {notation}");

        for perspective in super::Color::ALL {
            if parent.king_square(perspective) != child.king_square(perspective) {
                continue;
            }
            let expected_indices = normalized_indices(
                active_indices(&parent, perspective)
                    .into_iter()
                    .map(|index| (index, -1))
                    .chain(
                        active_indices(&child, perspective)
                            .into_iter()
                            .map(|index| (index, 1)),
                    ),
            );
            let mut additions = [0_u32; 128];
            let mut removals = [0_u32; 128];
            let (added, removed) = append_changed_threat_indices(
                perspective,
                &child,
                &changed,
                &mut additions,
                &mut removals,
            );
            let actual_indices = normalized_indices(
                removals[..removed]
                    .iter()
                    .copied()
                    .map(|index| (index as usize, -1))
                    .chain(
                        additions[..added]
                            .iter()
                            .copied()
                            .map(|index| (index as usize, 1)),
                    ),
            );
            assert_eq!(
                actual_indices, expected_indices,
                "{fen}, {notation}, {perspective:?}"
            );
        }
        (parent, child, mv, undo, changed)
    }

    #[test]
    fn dirty_threat_pack_unpack_and_sign_are_byte_exact() {
        let edge = DirtyThreat::new(
            ThreatPiece::new(super::Color::Black, PieceKind::Queen),
            square(63),
            square(42),
            ThreatPiece::new(super::Color::White, PieceKind::King),
            1,
        );
        assert_eq!(
            edge.raw(),
            13_u32 | (6_u32 << 4) | (63_u32 << 8) | (42_u32 << 14) | (1_u32 << 20)
        );
        assert_eq!(
            edge.attacker(),
            ThreatPiece::new(super::Color::Black, PieceKind::Queen)
        );
        assert_eq!(
            edge.attacked(),
            ThreatPiece::new(super::Color::White, PieceKind::King)
        );
        assert_eq!(edge.from(), square(63));
        assert_eq!(edge.to(), square(42));
        assert_eq!(edge.sign(), 1);

        let removed = edge.with_sign(-1);
        assert_eq!(removed.raw(), edge.physical_bits());
        assert_eq!(removed.sign(), -1);
        assert_eq!(removed.with_sign(1), edge);
    }

    #[test]
    fn changed_threat_buffer_overflow_sets_flag_without_partial_append() {
        let edge = DirtyThreat::new(
            ThreatPiece::new(super::Color::White, PieceKind::Pawn),
            square(8),
            square(16),
            ThreatPiece::new(super::Color::Black, PieceKind::Pawn),
            1,
        );
        let mut buffer = ChangedThreatBuffer::<2>::new();
        assert!(buffer.push(edge));
        assert!(buffer.push(edge.with_sign(-1)));
        assert!(!buffer.push(edge));
        assert!(buffer.overflowed());
        assert_eq!(buffer.len(), 2);
        assert_eq!(
            buffer.iter().collect::<Vec<_>>(),
            vec![edge, edge.with_sign(-1)]
        );
    }

    #[test]
    fn changed_threat_buffer_reset_reuses_storage_without_stale_entries() {
        let edge = DirtyThreat::new(
            ThreatPiece::new(super::Color::White, PieceKind::Knight),
            square(1),
            square(18),
            ThreatPiece::new(super::Color::Black, PieceKind::Pawn),
            1,
        );
        let mut buffer = ChangedThreatBuffer::<2>::new();
        assert!(buffer.push(edge));

        buffer.reset();

        assert_eq!(buffer.len(), 0);
        assert!(!buffer.overflowed());
        assert_eq!(buffer.iter().count(), 0);
    }

    #[test]
    fn changed_edges_cover_quiets_captures_en_passant_and_promotions() {
        for (fen, notation) in [
            (
                "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
                "g1f3",
            ),
            ("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "e4d5"),
            ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"),
            ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8q"),
            ("1r2k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7b8q"),
        ] {
            assert_changed_edges_match_full_diff(fen, notation, false);
        }
    }

    #[test]
    fn changed_edges_cover_standard_and_chess960_castling_relocations() {
        for (fen, notation, chess960) in [
            ("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1g1", false),
            ("4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1", "c1f1", true),
            ("4k3/8/8/8/8/8/8/6KR w H - 0 1", "g1h1", true),
            ("4k3/8/8/8/8/8/8/4KR2 w F - 0 1", "e1f1", true),
        ] {
            assert_changed_edges_match_full_diff(fen, notation, chess960);
        }
    }

    #[test]
    fn changed_edges_cover_pawn_blocks_and_discovered_slider_contacts() {
        for (fen, notation) in [
            ("4k3/8/8/8/4p3/8/4p3/4K3 b - - 0 1", "e4e3"),
            ("4k3/8/8/3p4/4P3/4P3/8/4K3 w - - 0 1", "e4d5"),
            ("4k3/8/8/3r4/2N5/8/8/B3K3 w - - 0 1", "c4b6"),
            ("4k3/r7/8/N7/8/8/8/R3K3 w - - 0 1", "a5b7"),
            ("4k3/7r/8/5N2/8/3Q4/8/4K3 w - - 0 1", "f5h4"),
        ] {
            assert_changed_edges_match_full_diff(fen, notation, false);
        }
    }

    #[test]
    fn changed_edges_cover_defences_same_type_rules_and_king_targets() {
        let (_, child, _, _, changed) =
            assert_changed_edges_match_full_diff("4k3/8/8/8/8/8/P7/R3K3 w - - 0 1", "a2a3", false);
        let mut additions = [0_u32; 128];
        let mut removals = [0_u32; 128];
        for perspective in super::Color::ALL {
            let (added, removed) = append_changed_threat_indices(
                perspective,
                &child,
                &changed,
                &mut additions,
                &mut removals,
            );
            assert!(added + removed > 0);
        }

        assert_changed_edges_match_full_diff("4k3/8/8/8/8/2N5/4N3/4K3 w - - 0 1", "c3b5", false);

        // This case formerly gave check from e2 in the parent position (illegal with
        // white to move). The rook now swings onto the e-file instead, so the added
        // king-target edge is exactly the one a legal move creates.
        let (_, _, _, _, king_target_changes) =
            assert_changed_edges_match_full_diff("4k3/8/8/8/8/8/R7/4K3 w - - 0 1", "a2e2", false);
        assert!(king_target_changes.iter().any(|edge| {
            edge.attacked() == ThreatPiece::new(super::Color::Black, PieceKind::King)
        }));
        assert!(king_target_changes.iter().all(|edge| {
            edge.attacker() != ThreatPiece::new(super::Color::White, PieceKind::King)
                && edge.attacker() != ThreatPiece::new(super::Color::Black, PieceKind::King)
        }));
    }

    #[test]
    fn move_local_discovery_matches_full_physical_diff_on_random_walk() {
        let mut position = Position::startpos();
        let mut random = 0xD1B5_4A32_D192_ED03_u64;
        for ply in 0..4_000 {
            let moves = generate_legal_moves(&position);
            if moves.is_empty() {
                position = Position::startpos();
                continue;
            }
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let mv = moves[random as usize % moves.len()];
            let parent = position.clone();
            let undo = position.make_move(mv);
            let mut changed = ChangedThreatBuffer::<128>::new();
            discover_changed_threats(&parent, &position, mv, &undo, &mut changed);
            let expected = normalized_signed(
                physical_edges(&parent)
                    .into_iter()
                    .map(|edge| edge.with_sign(-1))
                    .chain(physical_edges(&position)),
            );
            assert_eq!(
                normalized_signed(changed.iter()),
                expected,
                "ply {ply}, move {mv:?}, parent {parent:?}"
            );
        }
    }

    /// The precondition the empty-square skip in [`gather_changed_square`] relies on.
    ///
    /// A physical threat edge always terminates on an occupied square, so when an affected
    /// square is empty in one of the two positions, every non-slider attacker of it is
    /// discarded by `known_physical_edge`. Enumerating those attackers is therefore pure waste,
    /// and the scan skips it. This test pins the premise rather than the consequence: if a
    /// future feature ever lets an empty square carry an edge, this fails loudly instead of the
    /// skip silently dropping real edges.
    #[test]
    fn empty_squares_never_terminate_a_physical_threat_edge() {
        let mut position = Position::startpos();
        let mut random = 0x9E37_79B9_7F4A_7C15_u64;
        let mut empty_squares_checked = 0_usize;
        let mut attackers_confirmed_dead = 0_usize;

        for _ in 0..600 {
            let moves = generate_legal_moves(&position);
            if moves.is_empty() {
                position = Position::startpos();
                continue;
            }
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            position.make_move(moves[random as usize % moves.len()]);

            for index in 0..64_u8 {
                let target = square(index);
                if position.piece_at(target).is_some() {
                    continue;
                }
                empty_squares_checked += 1;

                let mut incoming = super::non_slider_attackers_to(&position, target);
                while let Some(attacker) = incoming.pop_first() {
                    assert!(
                        super::known_physical_edge(&position, attacker, target).is_none(),
                        "empty {target:?} must not carry an edge from {attacker:?} in {position:?}"
                    );
                    attackers_confirmed_dead += 1;
                }

                // The whole-position oracle must agree: no edge anywhere targets this square.
                assert!(
                    physical_edges(&position)
                        .iter()
                        .all(|edge| edge.to() != target),
                    "oracle produced an edge into empty {target:?} in {position:?}"
                );
            }
        }

        // Guards against the walk degenerating into a test that checks nothing.
        assert!(empty_squares_checked > 10_000, "{empty_squares_checked}");
        assert!(attackers_confirmed_dead > 500, "{attackers_confirmed_dead}");
    }

    /// Broadens the random walk past the startpos-only version above.
    ///
    /// The empty-square skip fires on 39.6% of all square scans, so the parity net has to cover
    /// positions where affected squares are empty far more often than the opening does: open
    /// middlegames, sparse endgames, and Chess960 castling relocations (which contribute four
    /// affected squares, several of them empty on one side of the move).
    #[test]
    fn move_local_discovery_matches_full_physical_diff_across_open_and_chess960_walks() {
        let roots = [
            // Sparse endgames: most affected squares are empty in at least one position.
            ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", false),
            ("8/8/4k3/8/2q5/8/4K3/6R1 w - - 0 1", false),
            ("8/3k4/8/8/3B4/2N5/4K3/8 w - - 0 1", false),
            // Open middlegames with long slider rays crossing vacated squares.
            (
                "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
                false,
            ),
            (
                "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
                false,
            ),
            // Chess960 castling: four affected squares per castle, king-takes-rook notation.
            ("1rk1r3/pppppppp/8/8/8/8/PPPPPPPP/1RK1R3 w EBeb - 0 1", true),
            ("rk2r3/pppppppp/8/8/8/8/PPPPPPPP/RK2R3 w EAea - 0 1", true),
        ];

        let mut verifications = 0_usize;
        for (root_index, (fen, chess960)) in roots.into_iter().enumerate() {
            let root = Position::from_fen(fen, chess960).expect("walk FEN should parse");
            let mut position = root.clone();
            let mut random = 0x2545_F491_4F6C_DD1D_u64 ^ (root_index as u64).wrapping_mul(0x9E37);

            for ply in 0..900 {
                let moves = generate_legal_moves(&position);
                if moves.is_empty() {
                    position = root.clone();
                    continue;
                }
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let mv = moves[random as usize % moves.len()];
                let parent = position.clone();
                let undo = position.make_move(mv);

                let mut changed = ChangedThreatBuffer::<128>::new();
                discover_changed_threats(&parent, &position, mv, &undo, &mut changed);
                assert!(!changed.overflowed(), "{fen} ply {ply}");

                let expected = normalized_signed(
                    physical_edges(&parent)
                        .into_iter()
                        .map(|edge| edge.with_sign(-1))
                        .chain(physical_edges(&position)),
                );
                assert_eq!(
                    normalized_signed(changed.iter()),
                    expected,
                    "{fen} ply {ply}, move {mv:?}, parent {parent:?}"
                );
                verifications += 1;
            }
        }

        assert!(verifications > 6_000, "{verifications}");
    }
}
