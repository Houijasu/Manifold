//! Syzygy endgame tablebase probing (WDL and DTZ, up to 6 men).
//!
//! The probing core is a pure-Rust port of Pyrrhic (see
//! `THIRD_PARTY_NOTICES/Pyrrhic.txt`); this module adapts it to
//! [`mf_core::Position`] behind a small engine-facing API.

mod chess;
mod probe;

use std::error::Error;
use std::fmt;

use mf_core::{CastlingSide, Color, Move, PieceKind, Position, generate_legal_moves};

use crate::chess::{TbMove, TbPos, move_from, move_promotes, move_to};
use crate::probe::{Store, dtz_to_wdl};

/// A win/draw/loss verdict from the probing side's point of view, ordered from
/// worst to best so that `max` picks the better outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Wdl {
    Loss,
    BlessedLoss,
    Draw,
    CursedWin,
    Win,
}

impl Wdl {
    fn from_value(value: i32) -> Self {
        match value {
            -2 => Self::Loss,
            -1 => Self::BlessedLoss,
            0 => Self::Draw,
            1 => Self::CursedWin,
            _ => Self::Win,
        }
    }
}

/// An error raised while opening a tablebase directory set.
#[derive(Debug)]
pub struct TbError(String);

impl fmt::Display for TbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl Error for TbError {}

/// One root move with its tablebase verdict for the side to move at the root.
#[derive(Clone, Copy, Debug)]
pub struct RootMove {
    /// The move in the engine's own encoding.
    pub mv: Move,
    /// The verdict after playing the move, from the root mover's point of view.
    pub wdl: Wdl,
    /// A DTZ-style distance: positive means the root side is winning, and
    /// smaller positive values reach a zeroing move sooner.
    pub dtz: i32,
}

/// The result of a DTZ probe at the root: every legal move ranked by verdict.
#[derive(Clone, Debug)]
pub struct RootProbe {
    /// The DTZ-optimal move.
    pub best_move: Move,
    /// The root position's verdict for the side to move.
    pub wdl: Wdl,
    moves: Vec<RootMove>,
}

impl RootProbe {
    /// Returns every ranked root move.
    pub fn moves(&self) -> &[RootMove] {
        &self.moves
    }

    /// Returns the moves that preserve the root verdict.
    pub fn preserving_moves(&self) -> impl Iterator<Item = &RootMove> {
        self.moves.iter().filter(|entry| entry.wdl == self.wdl)
    }
}

/// A set of Syzygy tables discovered from `';'`-separated directories.
///
/// Table files are loaded lazily on first probe and shared across threads.
/// `probe_wdl` reports the position's true verdict only when the halfmove
/// clock is zero (standard Syzygy WDL semantics); callers must gate on
/// `halfmove_clock() == 0` or accept the value as an approximation.
pub struct Tablebases {
    store: Store,
}

impl Tablebases {
    /// Discovers tables under the given `';'`-separated directories.
    ///
    /// Fails when no listed directory exists; unreadable or corrupt individual
    /// files are skipped at load time instead of failing the whole set.
    pub fn new(paths: &str) -> Result<Self, TbError> {
        let store = Store::new(paths).map_err(TbError)?;
        Ok(Self { store })
    }

    /// Returns the largest piece count covered by a discovered WDL table.
    pub fn max_pieces(&self) -> usize {
        self.store.max_pieces()
    }

    /// Returns how many WDL table files were discovered.
    pub fn wdl_table_count(&self) -> usize {
        self.store.wdl_table_count()
    }

    /// Returns how many DTZ table files were discovered.
    pub fn dtz_table_count(&self) -> usize {
        self.store.dtz_table_count()
    }

    /// Probes the WDL verdict for the side to move.
    ///
    /// Returns `None` when the position is out of table range, any castling
    /// rights remain, or the required table file is absent.
    pub fn probe_wdl(&self, position: &Position) -> Option<Wdl> {
        let pos = to_tb_pos(position)?;
        if (pos.white | pos.black).count_ones() as usize > self.max_pieces() {
            return None;
        }
        let mut success = 0i32;
        let value = self.store.probe_wdl(&pos, &mut success);
        if success == 0 {
            return None;
        }
        Some(Wdl::from_value(value))
    }

    /// Probes DTZ at the root and ranks every legal move.
    ///
    /// Returns `None` under the same conditions as [`Self::probe_wdl`], or
    /// when a required DTZ table is missing.
    pub fn probe_root(&self, position: &Position) -> Option<RootProbe> {
        let pos = to_tb_pos(position)?;
        if (pos.white | pos.black).count_ones() as usize > self.max_pieces() {
            return None;
        }
        let (root_dtz, scored) = self.store.probe_root(&pos)?;
        let cnt50 = i32::from(position.halfmove_clock());
        let root_wdl = Wdl::from_value(dtz_to_wdl(cnt50, root_dtz));

        let legal = generate_legal_moves(position);
        let mut moves = Vec::with_capacity(scored.len());
        for (tb_move, value) in scored {
            let Some(mv) = match_engine_move(legal.as_slice(), tb_move) else {
                continue;
            };
            moves.push(RootMove {
                mv,
                wdl: Wdl::from_value(dtz_to_wdl(cnt50, value)),
                dtz: value,
            });
        }
        if moves.is_empty() {
            return None;
        }

        let best = moves
            .iter()
            .copied()
            .max_by_key(|entry| {
                (
                    entry.wdl,
                    match entry.wdl {
                        Wdl::Win | Wdl::CursedWin => -entry.dtz,
                        Wdl::Loss | Wdl::BlessedLoss => -entry.dtz,
                        Wdl::Draw => 0,
                    },
                )
            })
            .expect("moves is non-empty");
        Some(RootProbe {
            best_move: best.mv,
            wdl: root_wdl,
            moves,
        })
    }
}

fn to_tb_pos(position: &Position) -> Option<TbPos> {
    for color in Color::ALL {
        for side in CastlingSide::ALL {
            if position.castling_rook(color, side).is_some() {
                return None;
            }
        }
    }

    let mut pos = TbPos {
        white: position.color_occupancy(Color::White).bits(),
        black: position.color_occupancy(Color::Black).bits(),
        kings: 0,
        queens: 0,
        rooks: 0,
        bishops: 0,
        knights: 0,
        pawns: 0,
        rule50: position.halfmove_clock().min(255) as u8,
        ep: 0,
        turn: position.side_to_move() == Color::White,
    };
    for color in Color::ALL {
        pos.pawns |= position.pieces(color, PieceKind::Pawn).bits();
        pos.knights |= position.pieces(color, PieceKind::Knight).bits();
        pos.bishops |= position.pieces(color, PieceKind::Bishop).bits();
        pos.rooks |= position.pieces(color, PieceKind::Rook).bits();
        pos.queens |= position.pieces(color, PieceKind::Queen).bits();
        pos.kings |= position.pieces(color, PieceKind::King).bits();
    }
    if let Some(square) = position.en_passant() {
        pos.ep = square.index();
    }
    Some(pos)
}

fn match_engine_move(legal: &[Move], tb_move: TbMove) -> Option<Move> {
    let from = move_from(tb_move) as u8;
    let to = move_to(tb_move) as u8;
    let promotion = match move_promotes(tb_move) {
        1 => Some(PieceKind::Queen),
        2 => Some(PieceKind::Rook),
        3 => Some(PieceKind::Bishop),
        4 => Some(PieceKind::Knight),
        _ => None,
    };
    legal.iter().copied().find(|mv| {
        mv.from().index() == from
            && mv.to().index() == to
            && mv.flag().promotion() == promotion
            && !mv.flag().is_castling()
    })
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Tablebases>()
};
