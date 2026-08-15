// Derived from Pyrrhic's tbchess.c (https://github.com/AndyGrant/Pyrrhic).
// See THIRD_PARTY_NOTICES/Pyrrhic.txt for the upstream MIT license notice.

use mf_core::{
    Bitboard, Color, Square, bishop_attacks, king_attacks, knight_attacks, pawn_attacks,
    rook_attacks,
};

pub(crate) const WHITE: bool = true;

pub(crate) const PAWN: u8 = 1;
pub(crate) const KNIGHT: u8 = 2;
pub(crate) const BISHOP: u8 = 3;
pub(crate) const ROOK: u8 = 4;
pub(crate) const QUEEN: u8 = 5;
pub(crate) const KING: u8 = 6;

const PROMO_SQUARES: u64 = 0xff00_0000_0000_00ff;

const PRIME_WPAWN: u64 = 17008651141875982339;
const PRIME_WKNIGHT: u64 = 15202887380319082783;
const PRIME_WBISHOP: u64 = 12311744257139811149;
const PRIME_WROOK: u64 = 10979190538029446137;
const PRIME_WQUEEN: u64 = 11811845319353239651;
const PRIME_BPAWN: u64 = 11695583624105689831;
const PRIME_BKNIGHT: u64 = 13469005675588064321;
const PRIME_BBISHOP: u64 = 15394650811035483107;
const PRIME_BROOK: u64 = 18264461213049635989;
const PRIME_BQUEEN: u64 = 15484752644942473553;

/// Per-piece-code material primes indexed by the Pyrrhic piece codes: white
/// pieces occupy 1..=6 and black pieces 9..=14, with kings contributing zero.
pub(crate) const PRIMES: [u64; 16] = [
    0,
    PRIME_WPAWN,
    PRIME_WKNIGHT,
    PRIME_WBISHOP,
    PRIME_WROOK,
    PRIME_WQUEEN,
    0,
    0,
    0,
    PRIME_BPAWN,
    PRIME_BKNIGHT,
    PRIME_BBISHOP,
    PRIME_BROOK,
    PRIME_BQUEEN,
    0,
    0,
];

pub(crate) type TbMove = u16;

const FLAG_ENPASS: u32 = 8;

pub(crate) const MAX_MOVES: usize = 256;

/// A bitboard-only probing position in Pyrrhic's layout: piece-kind boards are
/// shared between the colors and `turn` is `true` when white is to move.
#[derive(Clone, Copy, Default)]
pub(crate) struct TbPos {
    pub(crate) white: u64,
    pub(crate) black: u64,
    pub(crate) kings: u64,
    pub(crate) queens: u64,
    pub(crate) rooks: u64,
    pub(crate) bishops: u64,
    pub(crate) knights: u64,
    pub(crate) pawns: u64,
    pub(crate) rule50: u8,
    pub(crate) ep: u8,
    pub(crate) turn: bool,
}

pub(crate) struct TbMoveList {
    moves: [TbMove; MAX_MOVES],
    len: usize,
}

impl TbMoveList {
    pub(crate) const fn new() -> Self {
        Self {
            moves: [0; MAX_MOVES],
            len: 0,
        }
    }

    pub(crate) fn as_slice(&self) -> &[TbMove] {
        &self.moves[..self.len]
    }

    fn push(&mut self, mv: TbMove) {
        if self.len < MAX_MOVES {
            self.moves[self.len] = mv;
            self.len += 1;
        }
    }

    fn add(&mut self, promotes: bool, enpass: bool, from: u32, to: u32) {
        if enpass {
            self.push(build_move(FLAG_ENPASS, from, to));
        } else if promotes {
            self.push(build_move(1, from, to));
            self.push(build_move(2, from, to));
            self.push(build_move(3, from, to));
            self.push(build_move(4, from, to));
        } else {
            self.push(build_move(0, from, to));
        }
    }
}

#[inline]
fn square(index: u32) -> Square {
    Square::new((index & 63) as u8).unwrap()
}

#[inline]
fn king_att(from: u32) -> u64 {
    king_attacks(square(from)).bits()
}

#[inline]
fn knight_att(from: u32) -> u64 {
    knight_attacks(square(from)).bits()
}

#[inline]
fn rook_att(from: u32, occupancy: u64) -> u64 {
    rook_attacks(square(from), Bitboard::new(occupancy)).bits()
}

#[inline]
fn bishop_att(from: u32, occupancy: u64) -> u64 {
    bishop_attacks(square(from), Bitboard::new(occupancy)).bits()
}

#[inline]
fn pawn_att(from: u32, white: bool) -> u64 {
    let color = if white { Color::White } else { Color::Black };
    pawn_attacks(square(from), color).bits()
}

#[inline]
fn poplsb(bb: &mut u64) -> u32 {
    let lsb = bb.trailing_zeros();
    *bb &= bb.wrapping_sub(1);
    lsb
}

#[inline]
fn test_bit(bb: u64, sq: u32) -> bool {
    (bb >> sq) & 1 != 0
}

#[inline]
fn disable_bit(bb: &mut u64, sq: u32) {
    *bb &= !(1u64 << sq);
}

#[inline]
fn enable_bit(bb: &mut u64, sq: u32) {
    *bb |= 1u64 << sq;
}

#[inline]
fn promo_square(sq: u32) -> bool {
    test_bit(PROMO_SQUARES, sq)
}

#[inline]
fn pawn_start_square(white: bool, sq: u32) -> bool {
    sq >> 3 == if white { 1 } else { 6 }
}

#[inline]
const fn build_move(flags: u32, from: u32, to: u32) -> TbMove {
    ((to & 0x3f) | ((from & 0x3f) << 6) | ((flags & 0x0f) << 12)) as TbMove
}

#[inline]
pub(crate) const fn move_to(mv: TbMove) -> u32 {
    (mv as u32) & 0x3f
}

#[inline]
pub(crate) const fn move_from(mv: TbMove) -> u32 {
    ((mv as u32) >> 6) & 0x3f
}

#[inline]
pub(crate) const fn move_promotes(mv: TbMove) -> u32 {
    ((mv as u32) >> 12) & 0x07
}

#[inline]
pub(crate) const fn colour_is_white(piece: u8) -> bool {
    piece >> 3 == 0
}

#[inline]
pub(crate) const fn type_of_piece(piece: u8) -> u8 {
    piece & 0x7
}

pub(crate) fn pieces_by_type(pos: &TbPos, white: bool, piece: u8) -> u64 {
    let side = if white { pos.white } else { pos.black };
    match piece {
        PAWN => pos.pawns & side,
        KNIGHT => pos.knights & side,
        BISHOP => pos.bishops & side,
        ROOK => pos.rooks & side,
        QUEEN => pos.queens & side,
        KING => pos.kings & side,
        _ => 0,
    }
}

pub(crate) fn calc_key(pos: &TbPos, mirror: bool) -> u64 {
    let white = if mirror { pos.black } else { pos.white };
    let black = if mirror { pos.white } else { pos.black };
    u64::from((white & pos.queens).count_ones())
        .wrapping_mul(PRIME_WQUEEN)
        .wrapping_add(u64::from((white & pos.rooks).count_ones()).wrapping_mul(PRIME_WROOK))
        .wrapping_add(u64::from((white & pos.bishops).count_ones()).wrapping_mul(PRIME_WBISHOP))
        .wrapping_add(u64::from((white & pos.knights).count_ones()).wrapping_mul(PRIME_WKNIGHT))
        .wrapping_add(u64::from((white & pos.pawns).count_ones()).wrapping_mul(PRIME_WPAWN))
        .wrapping_add(u64::from((black & pos.queens).count_ones()).wrapping_mul(PRIME_BQUEEN))
        .wrapping_add(u64::from((black & pos.rooks).count_ones()).wrapping_mul(PRIME_BROOK))
        .wrapping_add(u64::from((black & pos.bishops).count_ones()).wrapping_mul(PRIME_BBISHOP))
        .wrapping_add(u64::from((black & pos.knights).count_ones()).wrapping_mul(PRIME_BKNIGHT))
        .wrapping_add(u64::from((black & pos.pawns).count_ones()).wrapping_mul(PRIME_BPAWN))
}

pub(crate) fn calc_key_from_pcs(pcs: &[u32; 16], mirror: bool) -> u64 {
    let flip = if mirror { 8usize } else { 0 };
    let mut key = 0u64;
    for (code, prime) in PRIMES.iter().enumerate() {
        key = key.wrapping_add(u64::from(pcs[code ^ flip]).wrapping_mul(*prime));
    }
    key
}

#[inline]
fn do_bb_move(bb: u64, from: u32, to: u32) -> u64 {
    (((bb >> from) & 0x1) << to) | (bb & (!(1u64 << from) & !(1u64 << to)))
}

pub(crate) fn gen_captures(pos: &TbPos) -> TbMoveList {
    let mut list = TbMoveList::new();
    let us = if pos.turn { pos.white } else { pos.black };
    let them = if pos.turn { pos.black } else { pos.white };
    let occupancy = us | them;

    let mut b = us & pos.kings;
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = king_att(from) & them;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & (pos.rooks | pos.queens);
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = rook_att(from, occupancy) & them;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & (pos.bishops | pos.queens);
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = bishop_att(from, occupancy) & them;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & pos.knights;
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = knight_att(from) & them;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & pos.pawns;
    while b != 0 {
        let from = poplsb(&mut b);
        let attacks = pawn_att(from, pos.turn);
        if pos.ep != 0 && test_bit(attacks, u32::from(pos.ep)) {
            list.add(false, true, from, u32::from(pos.ep));
        }
        let mut att = attacks & them;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(promo_square(to), false, from, to);
        }
    }
    list
}

pub(crate) fn gen_moves(pos: &TbPos) -> TbMoveList {
    let mut list = TbMoveList::new();
    let forward: i32 = if pos.turn { 8 } else { -8 };
    let us = if pos.turn { pos.white } else { pos.black };
    let them = if pos.turn { pos.black } else { pos.white };
    let occupancy = us | them;

    let mut b = us & pos.kings;
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = king_att(from) & !us;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & (pos.rooks | pos.queens);
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = rook_att(from, occupancy) & !us;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & (pos.bishops | pos.queens);
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = bishop_att(from, occupancy) & !us;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & pos.knights;
    while b != 0 {
        let from = poplsb(&mut b);
        let mut att = knight_att(from) & !us;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(false, false, from, to);
        }
    }
    let mut b = us & pos.pawns;
    while b != 0 {
        let from = poplsb(&mut b);
        let attacks = pawn_att(from, pos.turn);
        if pos.ep != 0 && test_bit(attacks, u32::from(pos.ep)) {
            list.add(false, true, from, u32::from(pos.ep));
        }
        let push = (from as i32 + forward) as u32;
        if !test_bit(occupancy, push) {
            list.add(promo_square(push), false, from, push);
        }
        let double_push = (from as i32 + 2 * forward) as u32;
        if pawn_start_square(pos.turn, from)
            && !test_bit(occupancy, push)
            && !test_bit(occupancy, double_push)
        {
            list.add(false, false, from, double_push);
        }
        let mut att = attacks & them;
        while att != 0 {
            let to = poplsb(&mut att);
            list.add(promo_square(to), false, from, to);
        }
    }
    list
}

pub(crate) fn gen_legal(pos: &TbPos) -> TbMoveList {
    let pseudo = gen_moves(pos);
    let mut list = TbMoveList::new();
    for &mv in pseudo.as_slice() {
        if legal_move(pos, mv) {
            list.push(mv);
        }
    }
    list
}

pub(crate) fn is_pawn_move(pos: &TbPos, mv: TbMove) -> bool {
    let us = if pos.turn { pos.white } else { pos.black };
    test_bit(us & pos.pawns, move_from(mv))
}

pub(crate) fn is_en_passant(pos: &TbPos, mv: TbMove) -> bool {
    is_pawn_move(pos, mv) && move_to(mv) == u32::from(pos.ep) && pos.ep != 0
}

pub(crate) fn is_capture(pos: &TbPos, mv: TbMove) -> bool {
    let them = if pos.turn { pos.black } else { pos.white };
    test_bit(them, move_to(mv)) || is_en_passant(pos, mv)
}

fn is_legal(pos: &TbPos) -> bool {
    let us = if pos.turn { pos.black } else { pos.white };
    let them = if pos.turn { pos.white } else { pos.black };
    let king = (pos.kings & us).trailing_zeros();
    if king >= 64 {
        return false;
    }
    king_att(king) & pos.kings & them == 0
        && rook_att(king, us | them) & (pos.rooks | pos.queens) & them == 0
        && bishop_att(king, us | them) & (pos.bishops | pos.queens) & them == 0
        && knight_att(king) & pos.knights & them == 0
        && pawn_att(king, !pos.turn) & pos.pawns & them == 0
}

pub(crate) fn is_check(pos: &TbPos) -> bool {
    let us = if pos.turn { pos.white } else { pos.black };
    let them = if pos.turn { pos.black } else { pos.white };
    let king = (pos.kings & us).trailing_zeros();
    if king >= 64 {
        return false;
    }
    rook_att(king, us | them) & (pos.rooks | pos.queens) & them != 0
        || bishop_att(king, us | them) & (pos.bishops | pos.queens) & them != 0
        || knight_att(king) & pos.knights & them != 0
        || pawn_att(king, pos.turn) & pos.pawns & them != 0
}

pub(crate) fn is_mate(pos: &TbPos) -> bool {
    if !is_check(pos) {
        return false;
    }
    let moves = gen_moves(pos);
    for &mv in moves.as_slice() {
        if do_move(pos, mv).is_some() {
            return false;
        }
    }
    true
}

pub(crate) fn do_move(pos0: &TbPos, mv: TbMove) -> Option<TbPos> {
    let from = move_from(mv);
    let to = move_to(mv);
    let promotes = move_promotes(mv);
    let mut pos = TbPos {
        turn: !pos0.turn,
        white: do_bb_move(pos0.white, from, to),
        black: do_bb_move(pos0.black, from, to),
        kings: do_bb_move(pos0.kings, from, to),
        queens: do_bb_move(pos0.queens, from, to),
        rooks: do_bb_move(pos0.rooks, from, to),
        bishops: do_bb_move(pos0.bishops, from, to),
        knights: do_bb_move(pos0.knights, from, to),
        pawns: do_bb_move(pos0.pawns, from, to),
        rule50: 0,
        ep: 0,
    };

    if promotes != 0 {
        disable_bit(&mut pos.pawns, to);
        match promotes {
            1 => enable_bit(&mut pos.queens, to),
            2 => enable_bit(&mut pos.rooks, to),
            3 => enable_bit(&mut pos.bishops, to),
            4 => enable_bit(&mut pos.knights, to),
            _ => {}
        }
    } else if test_bit(pos0.pawns, from) {
        if from ^ to == 16
            && pos0.turn == WHITE
            && pawn_att(from + 8, true) & pos0.pawns & pos0.black != 0
        {
            pos.ep = (from + 8) as u8;
        }
        if from ^ to == 16
            && pos0.turn != WHITE
            && pawn_att(from.wrapping_sub(8), false) & pos0.pawns & pos0.white != 0
        {
            pos.ep = from.wrapping_sub(8) as u8;
        } else if pos0.ep != 0 && to == u32::from(pos0.ep) {
            let captured = if pos0.turn {
                to.wrapping_sub(8)
            } else {
                to + 8
            };
            disable_bit(&mut pos.white, captured);
            disable_bit(&mut pos.black, captured);
            disable_bit(&mut pos.pawns, captured);
        }
    } else if test_bit(pos0.white | pos0.black, to) {
        pos.rule50 = 0;
    } else {
        pos.rule50 = pos0.rule50.saturating_add(1);
    }

    if is_legal(&pos) { Some(pos) } else { None }
}

pub(crate) fn legal_move(pos: &TbPos, mv: TbMove) -> bool {
    do_move(pos, mv).is_some()
}
