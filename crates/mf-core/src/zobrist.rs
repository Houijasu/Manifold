use crate::{CastlingSide, Color, Piece, PieceKind, Square};

const PIECE_COUNT: usize = 12;
const BOARD_SQUARES: usize = 64;
const MATERIAL_KINDS: usize = 4;
/// The largest count of one non-pawn kind the material key can represent.
///
/// `Position::from_fen` rejects anything above this so that untrusted input cannot
/// index past `ZobristTables::material` and panic inside the parser.
pub(crate) const MAX_MATERIAL_COUNT: usize = 16;

/// Incremental hashes used by the transposition table and correction histories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZobristKeys {
    main: u64,
    pawn: u64,
    minor: u64,
    major: u64,
    non_pawn_material: u64,
}

impl ZobristKeys {
    #[inline]
    pub const fn main(self) -> u64 {
        self.main
    }

    #[inline]
    pub const fn pawn(self) -> u64 {
        self.pawn
    }

    #[inline]
    pub const fn minor(self) -> u64 {
        self.minor
    }

    #[inline]
    pub const fn major(self) -> u64 {
        self.major
    }

    #[inline]
    pub const fn non_pawn_material(self) -> u64 {
        self.non_pawn_material
    }

    #[inline]
    pub(crate) fn toggle_piece(&mut self, piece: Piece, square: Square) {
        let key = piece_square_key(piece, square);
        self.main ^= key;
        match piece.kind() {
            PieceKind::Pawn => self.pawn ^= key,
            PieceKind::Knight | PieceKind::Bishop => self.minor ^= key,
            PieceKind::Rook | PieceKind::Queen => self.major ^= key,
            PieceKind::King => {}
        }
    }

    #[inline]
    pub(crate) fn toggle_side(&mut self) {
        self.main ^= TABLES.side;
    }

    #[inline]
    pub(crate) fn toggle_en_passant(&mut self, square: Square) {
        self.main ^= TABLES.en_passant[square.index() as usize];
    }

    #[inline]
    pub(crate) fn toggle_castling(&mut self, color: Color, side: CastlingSide, rook: Square) {
        self.main ^= TABLES.castling[color.index()][side.index()][rook.index() as usize];
    }

    #[inline]
    pub(crate) fn toggle_material_count(&mut self, color: Color, kind: PieceKind, count: u8) {
        let kind = material_index(kind).expect("kings and pawns are not material-key pieces");
        self.non_pawn_material ^= TABLES.material[color.index()][kind][usize::from(count)];
    }

    pub(crate) fn with_empty_material_counts() -> Self {
        let mut keys = Self {
            main: 0,
            pawn: 0,
            minor: 0,
            major: 0,
            non_pawn_material: 0,
        };
        for color in Color::ALL {
            for kind in PieceKind::NON_PAWN_MATERIAL {
                keys.toggle_material_count(color, kind, 0);
            }
        }
        keys
    }
}

#[inline]
fn piece_square_key(piece: Piece, square: Square) -> u64 {
    TABLES.piece_square[piece.index()][square.index() as usize]
}

/// Returns the main-key XOR delta for a quiet, reversible move.
///
/// The caller is responsible for using this only when no capture, promotion,
/// castling-right, or en-passant component changes.
#[inline]
pub fn reversible_move_delta(piece: Piece, from: Square, to: Square) -> u64 {
    TABLES.side ^ piece_square_key(piece, from) ^ piece_square_key(piece, to)
}

#[inline]
const fn material_index(kind: PieceKind) -> Option<usize> {
    match kind {
        PieceKind::Knight => Some(0),
        PieceKind::Bishop => Some(1),
        PieceKind::Rook => Some(2),
        PieceKind::Queen => Some(3),
        PieceKind::Pawn | PieceKind::King => None,
    }
}

struct ZobristTables {
    piece_square: [[u64; BOARD_SQUARES]; PIECE_COUNT],
    castling: [[[u64; BOARD_SQUARES]; 2]; 2],
    en_passant: [u64; BOARD_SQUARES],
    material: [[[u64; MAX_MATERIAL_COUNT + 1]; MATERIAL_KINDS]; 2],
    side: u64,
}

impl ZobristTables {
    const fn new() -> Self {
        let mut state = 0x4d61_6e69_666f_6c64;
        let mut piece_square = [[0; BOARD_SQUARES]; PIECE_COUNT];
        let mut piece = 0;
        while piece < PIECE_COUNT {
            let mut square = 0;
            while square < BOARD_SQUARES {
                piece_square[piece][square] = splitmix64(&mut state);
                square += 1;
            }
            piece += 1;
        }

        let mut castling = [[[0; BOARD_SQUARES]; 2]; 2];
        let mut color = 0;
        while color < 2 {
            let mut side = 0;
            while side < 2 {
                let mut square = 0;
                while square < BOARD_SQUARES {
                    castling[color][side][square] = splitmix64(&mut state);
                    square += 1;
                }
                side += 1;
            }
            color += 1;
        }

        let mut en_passant = [0; BOARD_SQUARES];
        let mut square = 0;
        while square < BOARD_SQUARES {
            en_passant[square] = splitmix64(&mut state);
            square += 1;
        }

        let mut material = [[[0; MAX_MATERIAL_COUNT + 1]; MATERIAL_KINDS]; 2];
        color = 0;
        while color < 2 {
            let mut kind = 0;
            while kind < MATERIAL_KINDS {
                let mut count = 0;
                while count <= MAX_MATERIAL_COUNT {
                    material[color][kind][count] = splitmix64(&mut state);
                    count += 1;
                }
                kind += 1;
            }
            color += 1;
        }

        Self {
            piece_square,
            castling,
            en_passant,
            material,
            side: splitmix64(&mut state),
        }
    }
}

const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

static TABLES: ZobristTables = ZobristTables::new();
