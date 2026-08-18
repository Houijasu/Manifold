// Derived from Pyrrhic's tbprobe.c (https://github.com/AndyGrant/Pyrrhic).
// See THIRD_PARTY_NOTICES/Pyrrhic.txt for the upstream MIT license notice.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::chess::{
    MAX_MOVES, TbMove, TbPos, calc_key, calc_key_from_pcs, colour_is_white, do_move, gen_captures,
    gen_legal, gen_moves, is_capture, is_check, is_en_passant, is_mate, is_pawn_move,
    pieces_by_type, type_of_piece,
};

const WDL_MAGIC: u32 = 0x5d23_e871;
const DTZ_MAGIC: u32 = 0xa50c_66d7;

const PIECE_ENC: u32 = 0;
const FILE_ENC: u32 = 1;
const RANK_ENC: u32 = 2;

pub(crate) const WDL_TO_DTZ: [i32; 5] = [-1, -101, 0, 101, 1];
const WDL_TO_MAP: [usize; 5] = [1, 3, 0, 2, 0];
const PA_FLAGS: [u8; 5] = [8, 0, 0, 0, 4];

#[rustfmt::skip]
const OFF_DIAG: [i8; 64] = [
    0, -1, -1, -1, -1, -1, -1, -1,
    1,  0, -1, -1, -1, -1, -1, -1,
    1,  1,  0, -1, -1, -1, -1, -1,
    1,  1,  1,  0, -1, -1, -1, -1,
    1,  1,  1,  1,  0, -1, -1, -1,
    1,  1,  1,  1,  1,  0, -1, -1,
    1,  1,  1,  1,  1,  1,  0, -1,
    1,  1,  1,  1,  1,  1,  1,  0,
];

#[rustfmt::skip]
const TRIANGLE: [u64; 64] = [
    6, 0, 1, 2, 2, 1, 0, 6,
    0, 7, 3, 4, 4, 3, 7, 0,
    1, 3, 8, 5, 5, 8, 3, 1,
    2, 4, 5, 9, 9, 5, 4, 2,
    2, 4, 5, 9, 9, 5, 4, 2,
    1, 3, 8, 5, 5, 8, 3, 1,
    0, 7, 3, 4, 4, 3, 7, 0,
    6, 0, 1, 2, 2, 1, 0, 6,
];

#[rustfmt::skip]
const FLIP_DIAG: [usize; 64] = [
    0,  8, 16, 24, 32, 40, 48, 56,
    1,  9, 17, 25, 33, 41, 49, 57,
    2, 10, 18, 26, 34, 42, 50, 58,
    3, 11, 19, 27, 35, 43, 51, 59,
    4, 12, 20, 28, 36, 44, 52, 60,
    5, 13, 21, 29, 37, 45, 53, 61,
    6, 14, 22, 30, 38, 46, 54, 62,
    7, 15, 23, 31, 39, 47, 55, 63,
];

#[rustfmt::skip]
const LOWER: [u64; 64] = [
    28,  0,  1,  2,  3,  4,  5,  6,
     0, 29,  7,  8,  9, 10, 11, 12,
     1,  7, 30, 13, 14, 15, 16, 17,
     2,  8, 13, 31, 18, 19, 20, 21,
     3,  9, 14, 18, 32, 22, 23, 24,
     4, 10, 15, 19, 22, 33, 25, 26,
     5, 11, 16, 20, 23, 25, 34, 27,
     6, 12, 17, 21, 24, 26, 27, 35,
];

#[rustfmt::skip]
const DIAG: [u64; 64] = [
     0,  0,  0,  0,  0,  0,  0,  8,
     0,  1,  0,  0,  0,  0,  9,  0,
     0,  0,  2,  0,  0, 10,  0,  0,
     0,  0,  0,  3, 11,  0,  0,  0,
     0,  0,  0, 12,  4,  0,  0,  0,
     0,  0, 13,  0,  0,  5,  0,  0,
     0, 14,  0,  0,  0,  0,  6,  0,
    15,  0,  0,  0,  0,  0,  0,  7,
];

#[rustfmt::skip]
const FLAP: [[usize; 64]; 2] = [
    [
        0,  0,  0,  0,  0,  0,  0, 0,
        0,  6, 12, 18, 18, 12,  6, 0,
        1,  7, 13, 19, 19, 13,  7, 1,
        2,  8, 14, 20, 20, 14,  8, 2,
        3,  9, 15, 21, 21, 15,  9, 3,
        4, 10, 16, 22, 22, 16, 10, 4,
        5, 11, 17, 23, 23, 17, 11, 5,
        0,  0,  0,  0,  0,  0,  0, 0,
    ],
    [
         0,  0,  0,  0,  0,  0,  0,  0,
         0,  1,  2,  3,  3,  2,  1,  0,
         4,  5,  6,  7,  7,  6,  5,  4,
         8,  9, 10, 11, 11, 10,  9,  8,
        12, 13, 14, 15, 15, 14, 13, 12,
        16, 17, 18, 19, 19, 18, 17, 16,
        20, 21, 22, 23, 23, 22, 21, 20,
         0,  0,  0,  0,  0,  0,  0,  0,
    ],
];

#[rustfmt::skip]
const PAWN_TWIST: [[usize; 64]; 2] = [
    [
         0,  0,  0,  0,  0,  0,  0,  0,
        47, 35, 23, 11, 10, 22, 34, 46,
        45, 33, 21,  9,  8, 20, 32, 44,
        43, 31, 19,  7,  6, 18, 30, 42,
        41, 29, 17,  5,  4, 16, 28, 40,
        39, 27, 15,  3,  2, 14, 26, 38,
        37, 25, 13,  1,  0, 12, 24, 36,
         0,  0,  0,  0,  0,  0,  0,  0,
    ],
    [
         0,  0,  0,  0,  0,  0,  0,  0,
        47, 45, 43, 41, 40, 42, 44, 46,
        39, 37, 35, 33, 32, 34, 36, 38,
        31, 29, 27, 25, 24, 26, 28, 30,
        23, 21, 19, 17, 16, 18, 20, 22,
        15, 13, 11,  9,  8, 10, 12, 14,
         7,  5,  3,  1,  0,  2,  4,  6,
         0,  0,  0,  0,  0,  0,  0,  0,
    ],
];

#[rustfmt::skip]
const KK_IDX: [[i16; 64]; 10] = [
    [ -1, -1, -1,  0,  1,  2,  3,  4,
      -1, -1, -1,  5,  6,  7,  8,  9,
      10, 11, 12, 13, 14, 15, 16, 17,
      18, 19, 20, 21, 22, 23, 24, 25,
      26, 27, 28, 29, 30, 31, 32, 33,
      34, 35, 36, 37, 38, 39, 40, 41,
      42, 43, 44, 45, 46, 47, 48, 49,
      50, 51, 52, 53, 54, 55, 56, 57 ],
    [ 58, -1, -1, -1, 59, 60, 61, 62,
      63, -1, -1, -1, 64, 65, 66, 67,
      68, 69, 70, 71, 72, 73, 74, 75,
      76, 77, 78, 79, 80, 81, 82, 83,
      84, 85, 86, 87, 88, 89, 90, 91,
      92, 93, 94, 95, 96, 97, 98, 99,
     100,101,102,103,104,105,106,107,
     108,109,110,111,112,113,114,115 ],
    [116,117, -1, -1, -1,118,119,120,
     121,122, -1, -1, -1,123,124,125,
     126,127,128,129,130,131,132,133,
     134,135,136,137,138,139,140,141,
     142,143,144,145,146,147,148,149,
     150,151,152,153,154,155,156,157,
     158,159,160,161,162,163,164,165,
     166,167,168,169,170,171,172,173 ],
    [174, -1, -1, -1,175,176,177,178,
     179, -1, -1, -1,180,181,182,183,
     184, -1, -1, -1,185,186,187,188,
     189,190,191,192,193,194,195,196,
     197,198,199,200,201,202,203,204,
     205,206,207,208,209,210,211,212,
     213,214,215,216,217,218,219,220,
     221,222,223,224,225,226,227,228 ],
    [229,230, -1, -1, -1,231,232,233,
     234,235, -1, -1, -1,236,237,238,
     239,240, -1, -1, -1,241,242,243,
     244,245,246,247,248,249,250,251,
     252,253,254,255,256,257,258,259,
     260,261,262,263,264,265,266,267,
     268,269,270,271,272,273,274,275,
     276,277,278,279,280,281,282,283 ],
    [284,285,286,287,288,289,290,291,
     292,293, -1, -1, -1,294,295,296,
     297,298, -1, -1, -1,299,300,301,
     302,303, -1, -1, -1,304,305,306,
     307,308,309,310,311,312,313,314,
     315,316,317,318,319,320,321,322,
     323,324,325,326,327,328,329,330,
     331,332,333,334,335,336,337,338 ],
    [ -1, -1,339,340,341,342,343,344,
      -1, -1,345,346,347,348,349,350,
      -1, -1,441,351,352,353,354,355,
      -1, -1, -1,442,356,357,358,359,
      -1, -1, -1, -1,443,360,361,362,
      -1, -1, -1, -1, -1,444,363,364,
      -1, -1, -1, -1, -1, -1,445,365,
      -1, -1, -1, -1, -1, -1, -1,446 ],
    [ -1, -1, -1,366,367,368,369,370,
      -1, -1, -1,371,372,373,374,375,
      -1, -1, -1,376,377,378,379,380,
      -1, -1, -1,447,381,382,383,384,
      -1, -1, -1, -1,448,385,386,387,
      -1, -1, -1, -1, -1,449,388,389,
      -1, -1, -1, -1, -1, -1,450,390,
      -1, -1, -1, -1, -1, -1, -1,451 ],
    [452,391,392,393,394,395,396,397,
      -1, -1, -1, -1,398,399,400,401,
      -1, -1, -1, -1,402,403,404,405,
      -1, -1, -1, -1,406,407,408,409,
      -1, -1, -1, -1,453,410,411,412,
      -1, -1, -1, -1, -1,454,413,414,
      -1, -1, -1, -1, -1, -1,455,415,
      -1, -1, -1, -1, -1, -1, -1,456 ],
    [457,416,417,418,419,420,421,422,
      -1,458,423,424,425,426,427,428,
      -1, -1, -1, -1, -1,429,430,431,
      -1, -1, -1, -1, -1,432,433,434,
      -1, -1, -1, -1, -1,435,436,437,
      -1, -1, -1, -1, -1,459,438,439,
      -1, -1, -1, -1, -1, -1,460,440,
      -1, -1, -1, -1, -1, -1, -1,461 ],
];

const FILE_TO_FILE: [usize; 8] = [0, 1, 2, 3, 3, 2, 1, 0];

struct Indices {
    binomial: [[u64; 64]; 7],
    pawn_idx: [[[u64; 24]; 6]; 2],
    pawn_factor_file: [[u64; 4]; 6],
    pawn_factor_rank: [[u64; 6]; 6],
}

static INDICES: OnceLock<Indices> = OnceLock::new();

fn indices() -> &'static Indices {
    INDICES.get_or_init(|| {
        let mut binomial = [[0u64; 64]; 7];
        for (i, row) in binomial.iter_mut().enumerate() {
            for (j, slot) in row.iter_mut().enumerate() {
                let mut f = 1u64;
                let mut l = 1u64;
                for k in 0..i {
                    f *= (j as u64).wrapping_sub(k as u64);
                    l *= k as u64 + 1;
                }
                *slot = f / l;
            }
        }

        let mut pawn_idx = [[[0u64; 24]; 6]; 2];
        let mut pawn_factor_file = [[0u64; 4]; 6];
        let mut pawn_factor_rank = [[0u64; 6]; 6];
        for i in 0..6 {
            let mut s = 0u64;
            for j in 0..24 {
                pawn_idx[0][i][j] = s;
                s += binomial[i][PAWN_TWIST[0][(1 + (j % 6)) * 8 + (j / 6)]];
                if (j + 1) % 6 == 0 {
                    pawn_factor_file[i][j / 6] = s;
                    s = 0;
                }
            }
        }
        for i in 0..6 {
            let mut s = 0u64;
            for j in 0..24 {
                pawn_idx[1][i][j] = s;
                s += binomial[i][PAWN_TWIST[1][(1 + (j / 4)) * 8 + (j % 4)]];
                if (j + 1) % 4 == 0 {
                    pawn_factor_rank[i][j / 4] = s;
                    s = 0;
                }
            }
        }

        Indices {
            binomial,
            pawn_idx,
            pawn_factor_file,
            pawn_factor_rank,
        }
    })
}

#[inline]
fn rd_u8(data: &[u8], at: usize) -> u8 {
    data.get(at).copied().unwrap_or(0)
}

#[inline]
fn rd_le_u16(data: &[u8], at: usize) -> u16 {
    u16::from(rd_u8(data, at)) | (u16::from(rd_u8(data, at + 1)) << 8)
}

#[inline]
fn rd_le_u32(data: &[u8], at: usize) -> u32 {
    match data.get(at..at + 4) {
        Some(bytes) => u32::from_le_bytes(bytes.try_into().unwrap()),
        None => 0,
    }
}

#[inline]
fn rd_be_u32(data: &[u8], at: usize) -> u32 {
    match data.get(at..at + 4) {
        Some(bytes) => u32::from_be_bytes(bytes.try_into().unwrap()),
        None => 0,
    }
}

#[inline]
fn rd_be_u64(data: &[u8], at: usize) -> u64 {
    match data.get(at..at + 8) {
        Some(bytes) => u64::from_be_bytes(bytes.try_into().unwrap()),
        None => 0,
    }
}

#[derive(Clone, Default)]
struct PairsData {
    index_table: usize,
    size_table: usize,
    data: usize,
    offset: usize,
    sym_pat: usize,
    sym_len: Vec<u8>,
    base: Vec<u64>,
    block_size: u8,
    idx_bits: u8,
    min_len: u8,
    const_value: [u8; 2],
}

#[derive(Clone, Default)]
struct EncInfo {
    precomp: Option<PairsData>,
    factor: [u64; 7],
    pieces: [u8; 7],
    norm: [u8; 7],
}

struct Table {
    data: Vec<u8>,
    ei: Vec<EncInfo>,
    dtz_flags: [u8; 4],
    dtz_map: usize,
    dtz_map_idx: [[u16; 4]; 4],
}

struct Entry {
    key: u64,
    num: u8,
    symmetric: bool,
    has_pawns: bool,
    has_dtz: bool,
    kk_enc: bool,
    pawns: [u8; 2],
    wdl_path: PathBuf,
    dtz_path: Option<PathBuf>,
    wdl: OnceLock<Option<Table>>,
    dtz: OnceLock<Option<Table>>,
}

pub(crate) struct Store {
    entries: Vec<Entry>,
    by_key: HashMap<u64, usize>,
    max_pieces: usize,
    num_wdl: usize,
    num_dtz: usize,
}

fn piece_code(letter: char) -> u32 {
    match letter {
        'P' => 1,
        'N' => 2,
        'B' => 3,
        'R' => 4,
        'Q' => 5,
        'K' => 6,
        _ => 0,
    }
}

fn find_file(dirs: &[PathBuf], name: &str, suffix: &str) -> Option<PathBuf> {
    for dir in dirs {
        let candidate = dir.join(format!("{name}{suffix}"));
        if let Ok(metadata) = fs::metadata(&candidate)
            && metadata.is_file()
            && metadata.len() & 63 == 16
        {
            return Some(candidate);
        }
    }
    None
}

#[allow(clippy::needless_range_loop)]
fn material_names() -> Vec<String> {
    const PCHR: [char; 5] = ['Q', 'R', 'B', 'N', 'P'];
    let mut names = Vec::new();
    for i in 0..5 {
        names.push(format!("K{}vK", PCHR[i]));
    }
    for i in 0..5 {
        for j in i..5 {
            names.push(format!("K{}vK{}", PCHR[i], PCHR[j]));
        }
    }
    for i in 0..5 {
        for j in i..5 {
            names.push(format!("K{}{}vK", PCHR[i], PCHR[j]));
        }
    }
    for i in 0..5 {
        for j in i..5 {
            for k in 0..5 {
                names.push(format!("K{}{}vK{}", PCHR[i], PCHR[j], PCHR[k]));
            }
        }
    }
    for i in 0..5 {
        for j in i..5 {
            for k in j..5 {
                names.push(format!("K{}{}{}vK", PCHR[i], PCHR[j], PCHR[k]));
            }
        }
    }
    for i in 0..5 {
        for j in i..5 {
            for k in i..5 {
                let l0 = if i == k { j } else { k };
                for l in l0..5 {
                    names.push(format!("K{}{}vK{}{}", PCHR[i], PCHR[j], PCHR[k], PCHR[l]));
                }
            }
        }
    }
    for i in 0..5 {
        for j in i..5 {
            for k in j..5 {
                for l in 0..5 {
                    names.push(format!("K{}{}{}vK{}", PCHR[i], PCHR[j], PCHR[k], PCHR[l]));
                }
            }
        }
    }
    for i in 0..5 {
        for j in i..5 {
            for k in j..5 {
                for l in k..5 {
                    names.push(format!("K{}{}{}{}vK", PCHR[i], PCHR[j], PCHR[k], PCHR[l]));
                }
            }
        }
    }
    names
}

impl Store {
    pub(crate) fn new(paths: &str) -> Result<Self, String> {
        indices();
        let dirs: Vec<PathBuf> = paths
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(PathBuf::from)
            .collect();
        if dirs.is_empty() {
            return Err("no tablebase directories given".to_string());
        }
        if !dirs.iter().any(|dir| dir.is_dir()) {
            return Err(format!("no tablebase directory in '{paths}' exists"));
        }

        let mut store = Self {
            entries: Vec::new(),
            by_key: HashMap::new(),
            max_pieces: 0,
            num_wdl: 0,
            num_dtz: 0,
        };
        for name in material_names() {
            store.init_entry(&dirs, &name);
        }
        Ok(store)
    }

    fn init_entry(&mut self, dirs: &[PathBuf], name: &str) {
        let Some(wdl_path) = find_file(dirs, name, ".rtbw") else {
            return;
        };

        let mut pcs = [0u32; 16];
        let mut color = 0usize;
        for letter in name.chars() {
            if letter == 'v' {
                color = 8;
            } else {
                let kind = piece_code(letter) as usize;
                if kind != 0 {
                    pcs[kind | color] += 1;
                }
            }
        }

        let key = calc_key_from_pcs(&pcs, false);
        let key2 = calc_key_from_pcs(&pcs, true);
        let has_pawns = pcs[1] != 0 || pcs[9] != 0;
        let num = pcs.iter().sum::<u32>() as u8;

        let mut kk_enc = false;
        let mut pawns = [0u8; 2];
        if has_pawns {
            pawns = [pcs[1] as u8, pcs[9] as u8];
            if pcs[9] != 0 && (pcs[1] == 0 || pcs[1] > pcs[9]) {
                pawns.swap(0, 1);
            }
        } else {
            kk_enc = pcs.iter().filter(|&&count| count == 1).count() == 2;
        }

        let dtz_path = find_file(dirs, name, ".rtbz");
        let has_dtz = dtz_path.is_some();

        self.num_wdl += 1;
        self.num_dtz += usize::from(has_dtz);
        self.max_pieces = self.max_pieces.max(num as usize);

        let index = self.entries.len();
        self.entries.push(Entry {
            key,
            num,
            symmetric: key == key2,
            has_pawns,
            has_dtz,
            kk_enc,
            pawns,
            wdl_path,
            dtz_path,
            wdl: OnceLock::new(),
            dtz: OnceLock::new(),
        });
        self.by_key.insert(key, index);
        if key != key2 {
            self.by_key.insert(key2, index);
        }
    }

    pub(crate) fn max_pieces(&self) -> usize {
        self.max_pieces
    }

    pub(crate) fn wdl_table_count(&self) -> usize {
        self.num_wdl
    }

    pub(crate) fn dtz_table_count(&self) -> usize {
        self.num_dtz
    }
}

/// Depth bound for the symbol-length recursion in [`calc_sym_len`].
///
/// The symbol graph is file-controlled: without a bound, a crafted table chains up to
/// tens of thousands of nested frames on the UCI thread, and with `panic = "abort"` a
/// stack overflow kills the engine mid-game. The bound cannot reject a legitimate
/// table, because a symbol's stored length dominates the height of its subtree (a leaf
/// stores 0, an internal symbol stores `left + right + 1`), so an honest `u8` length
/// implies a tree at most 255 levels deep.
const MAX_SYMBOL_DEPTH: usize = 256;

fn calc_sym_len(
    data: &[u8],
    sym_pat: usize,
    sym_len: &mut [u8],
    visited: &mut [bool],
    poisoned: &mut [bool],
    depth: usize,
    s: usize,
) {
    if s >= sym_len.len() || visited[s] {
        return;
    }
    visited[s] = true;
    if depth >= MAX_SYMBOL_DEPTH {
        poisoned[s] = true;
        return;
    }
    let w = sym_pat + 3 * s;
    let s2 = ((rd_u8(data, w + 2) as usize) << 4) | ((rd_u8(data, w + 1) as usize) >> 4);
    if s2 == 0x0fff {
        sym_len[s] = 0;
    } else {
        let s1 = (((rd_u8(data, w + 1) as usize) & 0xf) << 8) | rd_u8(data, w) as usize;
        calc_sym_len(data, sym_pat, sym_len, visited, poisoned, depth + 1, s1);
        calc_sym_len(data, sym_pat, sym_len, visited, poisoned, depth + 1, s2);
        let left = sym_len.get(s1).copied().unwrap_or(0);
        let right = sym_len.get(s2).copied().unwrap_or(0);
        sym_len[s] = left.wrapping_add(right).wrapping_add(1);
    }
}

fn setup_pairs(
    data: &[u8],
    ptr: &mut usize,
    tb_size: u64,
    size: &mut [u64; 3],
    flags_out: &mut u8,
    is_wdl: bool,
) -> Option<PairsData> {
    let start = *ptr;
    let flags = rd_u8(data, start);
    *flags_out = flags;
    if flags & 0x80 != 0 {
        let const_value = if is_wdl { rd_u8(data, start + 1) } else { 0 };
        *ptr = start + 2;
        *size = [0, 0, 0];
        return Some(PairsData {
            idx_bits: 0,
            const_value: [const_value, 0],
            ..PairsData::default()
        });
    }

    let block_size = rd_u8(data, start + 1);
    let idx_bits = rd_u8(data, start + 2);
    if idx_bits == 0 || idx_bits > 63 || block_size > 30 {
        return None;
    }
    let real_num_blocks = u64::from(rd_le_u32(data, start + 4));
    let num_blocks = real_num_blocks + u64::from(rd_u8(data, start + 3));
    let max_len = rd_u8(data, start + 8);
    let min_len = rd_u8(data, start + 9);
    if max_len < min_len {
        return None;
    }
    let h = (max_len - min_len + 1) as usize;
    let num_syms = rd_le_u16(data, start + 10 + 2 * h) as usize;
    let offset = start + 10;
    let sym_pat = start + 12 + 2 * h;
    *ptr = sym_pat + 3 * num_syms + (num_syms & 1);
    if *ptr > data.len() {
        return None;
    }

    let num_indices = (tb_size + (1u64 << idx_bits) - 1) >> idx_bits;
    size[0] = 6 * num_indices;
    size[1] = 2 * num_blocks;
    size[2] = real_num_blocks << block_size;

    let mut sym_len = vec![0u8; num_syms];
    let mut visited = vec![false; num_syms];
    let mut poisoned = vec![false; num_syms];
    for s in 0..num_syms {
        calc_sym_len(
            data,
            sym_pat,
            &mut sym_len,
            &mut visited,
            &mut poisoned,
            0,
            s,
        );
    }
    // An over-deep symbol graph cannot come from a legitimate table (see
    // `MAX_SYMBOL_DEPTH`), so this rejects crafted files instead of decoding garbage
    // from them.
    if poisoned.iter().any(|&is_poisoned| is_poisoned) {
        return None;
    }

    let mut base = vec![0u64; h];
    for i in (0..h.saturating_sub(1)).rev() {
        base[i] = base[i + 1]
            .wrapping_add(u64::from(rd_le_u16(data, offset + 2 * i)))
            .wrapping_sub(u64::from(rd_le_u16(data, offset + 2 * (i + 1))))
            / 2;
    }
    for (i, slot) in base.iter_mut().enumerate() {
        let shift = 64 - (min_len as usize + i);
        *slot = if shift < 64 { *slot << shift } else { 0 };
    }

    Some(PairsData {
        index_table: 0,
        size_table: 0,
        data: 0,
        offset,
        sym_pat,
        sym_len,
        base,
        block_size,
        idx_bits,
        min_len,
        const_value: [0, 0],
    })
}

fn init_enc_info(
    ei: &mut EncInfo,
    entry: &Entry,
    data: &[u8],
    ptr: usize,
    shift: u32,
    t: usize,
    enc: u32,
) -> u64 {
    let idx = indices();
    let more_pawns = enc != PIECE_ENC && entry.pawns[1] > 0;
    let num = entry.num as usize;

    for i in 0..num {
        ei.pieces[i] = (rd_u8(data, ptr + i + 1 + usize::from(more_pawns)) >> shift) & 0x0f;
        ei.norm[i] = 0;
    }

    let order = (rd_u8(data, ptr) >> shift) & 0x0f;
    let order2 = if more_pawns {
        (rd_u8(data, ptr + 1) >> shift) & 0x0f
    } else {
        0x0f
    };

    let mut k = if enc != PIECE_ENC {
        entry.pawns[0] as usize
    } else if entry.kk_enc {
        2
    } else {
        3
    };
    ei.norm[0] = k as u8;

    if more_pawns {
        ei.norm[k] = entry.pawns[1];
        k += ei.norm[k] as usize;
    }

    let mut i = k;
    while i < num {
        let mut j = i;
        while j < num && ei.pieces[j] == ei.pieces[i] {
            ei.norm[i] += 1;
            j += 1;
        }
        i += ei.norm[i].max(1) as usize;
    }

    let mut n = 64 - k as u64;
    let mut f = 1u64;
    let mut i = 0u32;
    while k < num || i == u32::from(order) || i == u32::from(order2) {
        if i == u32::from(order) {
            ei.factor[0] = f;
            f *= match enc {
                FILE_ENC => idx.pawn_factor_file[ei.norm[0] as usize - 1][t],
                RANK_ENC => idx.pawn_factor_rank[ei.norm[0] as usize - 1][t],
                _ => {
                    if entry.kk_enc {
                        462
                    } else {
                        31332
                    }
                }
            };
        } else if i == u32::from(order2) {
            ei.factor[ei.norm[0] as usize] = f;
            f *= subfactor(
                u64::from(ei.norm[ei.norm[0] as usize]),
                48 - u64::from(ei.norm[0]),
            );
        } else {
            ei.factor[k] = f;
            f *= subfactor(u64::from(ei.norm[k]), n);
            n -= u64::from(ei.norm[k]);
            k += ei.norm[k].max(1) as usize;
        }
        i += 1;
    }

    f
}

fn subfactor(k: u64, n: u64) -> u64 {
    let mut f = n;
    let mut l = 1u64;
    for i in 1..k {
        f = f.wrapping_mul(n.wrapping_sub(i));
        l = l.wrapping_mul(i + 1);
    }
    f.checked_div(l).unwrap_or(0)
}

fn num_tables(entry: &Entry) -> usize {
    if entry.has_pawns { 4 } else { 1 }
}

/// Adds `step` to `ptr`, yielding `None` for the caller to reject the table when the
/// addition would wrap or leave the `data.len() + 0x3f` window that `load_table` ends
/// within.
///
/// Section sizes are header-derived and untrusted: they used to be summed first and
/// checked once at the end, which a crafted header could wrap past in release builds
/// (overflow checks off). Every offset advance in `load_table` goes through here, so a
/// malformed size is caught at the first step instead.
fn advance_offset(data: &[u8], ptr: &mut usize, step: usize) -> Option<()> {
    *ptr = ptr
        .checked_add(step)
        .filter(|advanced| *advanced <= data.len() + 0x3f)?;
    Some(())
}

/// Rounds `ptr` up to the next 64-byte boundary under the same rejection rules as
/// [`advance_offset`].
fn align_offset(data: &[u8], ptr: &mut usize) -> Option<()> {
    let aligned = ptr.checked_add(0x3f)? & !0x3f;
    if aligned > data.len() + 0x3f {
        return None;
    }
    *ptr = aligned;
    Some(())
}

fn load_table(entry: &Entry, path: &Path, is_dtz: bool) -> Option<Table> {
    let data = fs::read(path).ok()?;
    let magic = if is_dtz { DTZ_MAGIC } else { WDL_MAGIC };
    if data.len() < 5 || rd_le_u32(&data, 0) != magic {
        return None;
    }

    let split = !is_dtz && (data[4] & 0x01) != 0;
    let num = num_tables(entry);
    let enc = if entry.has_pawns { FILE_ENC } else { PIECE_ENC };

    let mut ptr = 5usize;
    let mut ei = vec![EncInfo::default(); num * 2];
    let mut tb_size = [[0u64; 2]; 4];
    for t in 0..num {
        tb_size[t][0] = init_enc_info(&mut ei[t], entry, &data, ptr, 0, t, enc);
        if split {
            tb_size[t][1] = init_enc_info(&mut ei[num + t], entry, &data, ptr, 4, t, enc);
        }
        advance_offset(
            &data,
            &mut ptr,
            entry.num as usize + 1 + usize::from(entry.has_pawns && entry.pawns[1] != 0),
        )?;
    }
    let pad = ptr & 1;
    advance_offset(&data, &mut ptr, pad)?;

    let mut size = [[[0u64; 3]; 2]; 4];
    let mut dtz_flags = [0u8; 4];
    for t in 0..num {
        let mut flags = 0u8;
        let pairs = setup_pairs(
            &data,
            &mut ptr,
            tb_size[t][0],
            &mut size[t][0],
            &mut flags,
            !is_dtz,
        )?;
        ei[t].precomp = Some(pairs);
        if is_dtz {
            dtz_flags[t] = flags;
        }
        if split {
            let mut side_flags = 0u8;
            let pairs = setup_pairs(
                &data,
                &mut ptr,
                tb_size[t][1],
                &mut size[t][1],
                &mut side_flags,
                !is_dtz,
            )?;
            ei[num + t].precomp = Some(pairs);
        }
    }

    let mut dtz_map = 0usize;
    let mut dtz_map_idx = [[0u16; 4]; 4];
    if is_dtz {
        dtz_map = ptr;
        for t in 0..num {
            if dtz_flags[t] & 2 != 0 {
                if dtz_flags[t] & 16 == 0 {
                    for slot in dtz_map_idx[t].iter_mut() {
                        *slot = (ptr + 1 - dtz_map) as u16;
                        let step = 1 + rd_u8(&data, ptr) as usize;
                        advance_offset(&data, &mut ptr, step)?;
                    }
                } else {
                    let pad = ptr & 1;
                    advance_offset(&data, &mut ptr, pad)?;
                    for slot in dtz_map_idx[t].iter_mut() {
                        *slot = ((ptr - dtz_map) / 2 + 1) as u16;
                        let step = 2 + 2 * rd_le_u16(&data, ptr) as usize;
                        advance_offset(&data, &mut ptr, step)?;
                    }
                }
            }
        }
        let pad = ptr & 1;
        advance_offset(&data, &mut ptr, pad)?;
    }

    for t in 0..num {
        if let Some(pairs) = ei[t].precomp.as_mut() {
            pairs.index_table = ptr;
        }
        advance_offset(&data, &mut ptr, size[t][0][0] as usize)?;
        if split {
            if let Some(pairs) = ei[num + t].precomp.as_mut() {
                pairs.index_table = ptr;
            }
            advance_offset(&data, &mut ptr, size[t][1][0] as usize)?;
        }
    }
    for t in 0..num {
        if let Some(pairs) = ei[t].precomp.as_mut() {
            pairs.size_table = ptr;
        }
        advance_offset(&data, &mut ptr, size[t][0][1] as usize)?;
        if split {
            if let Some(pairs) = ei[num + t].precomp.as_mut() {
                pairs.size_table = ptr;
            }
            advance_offset(&data, &mut ptr, size[t][1][1] as usize)?;
        }
    }
    for t in 0..num {
        align_offset(&data, &mut ptr)?;
        if let Some(pairs) = ei[t].precomp.as_mut() {
            pairs.data = ptr;
        }
        advance_offset(&data, &mut ptr, size[t][0][2] as usize)?;
        if split {
            align_offset(&data, &mut ptr)?;
            if let Some(pairs) = ei[num + t].precomp.as_mut() {
                pairs.data = ptr;
            }
            advance_offset(&data, &mut ptr, size[t][1][2] as usize)?;
        }
    }
    // Belt-and-braces: unreachable now that every advance above is checked against the
    // same window, but it documents the invariant the checks enforce and guards any
    // future edit that adds a bare advance.
    if ptr > data.len() + 0x3f {
        return None;
    }

    Some(Table {
        data,
        ei,
        dtz_flags,
        dtz_map,
        dtz_map_idx,
    })
}

fn decompress_pairs(table: &Table, d: &PairsData, idx: u64) -> (u8, u8) {
    if d.idx_bits == 0 {
        return (d.const_value[0], d.const_value[1]);
    }
    let data = &table.data;

    let main_idx = (idx >> d.idx_bits) as usize;
    let mut lit_idx = (idx & ((1u64 << d.idx_bits) - 1)) as i64 - (1i64 << (d.idx_bits - 1));
    let mut block = rd_le_u32(data, d.index_table + 6 * main_idx) as usize;
    lit_idx += i64::from(rd_le_u16(data, d.index_table + 6 * main_idx + 4));

    while lit_idx < 0 {
        if block == 0 {
            return (0, 0);
        }
        block -= 1;
        lit_idx += i64::from(rd_le_u16(data, d.size_table + 2 * block)) + 1;
    }
    while lit_idx > i64::from(rd_le_u16(data, d.size_table + 2 * block)) {
        lit_idx -= i64::from(rd_le_u16(data, d.size_table + 2 * block)) + 1;
        block += 1;
    }

    let mut ptr = d.data + (block << d.block_size);
    let m = d.min_len as usize;

    let mut code = rd_be_u64(data, ptr);
    ptr += 8;
    let mut bit_cnt = 0u32;
    let mut sym;
    loop {
        let mut l = m;
        while l - m < d.base.len() && code < d.base[l - m] {
            l += 1;
        }
        if l - m >= d.base.len() {
            l = m + d.base.len() - 1;
        }
        sym = u32::from(rd_le_u16(data, d.offset + 2 * (l - m)));
        sym += ((code.wrapping_sub(d.base[l - m])) >> (64 - l)) as u32;
        let len = d.sym_len.get(sym as usize).copied().unwrap_or(0);
        if lit_idx < i64::from(len) + 1 {
            break;
        }
        lit_idx -= i64::from(len) + 1;
        code <<= l;
        bit_cnt += l as u32;
        if bit_cnt >= 32 {
            bit_cnt -= 32;
            code |= u64::from(rd_be_u32(data, ptr)) << bit_cnt;
            ptr += 4;
        }
    }

    let sym_pat = d.sym_pat;
    let mut sym = sym as usize;
    while d.sym_len.get(sym).copied().unwrap_or(0) != 0 {
        let w = sym_pat + 3 * sym;
        let s1 = (((rd_u8(data, w + 1) as usize) & 0xf) << 8) | rd_u8(data, w) as usize;
        let len1 = d.sym_len.get(s1).copied().unwrap_or(0);
        if lit_idx < i64::from(len1) + 1 {
            sym = s1;
        } else {
            lit_idx -= i64::from(len1) + 1;
            sym = ((rd_u8(data, w + 2) as usize) << 4) | ((rd_u8(data, w + 1) as usize) >> 4);
        }
    }

    let w = sym_pat + 3 * sym;
    (rd_u8(data, w), rd_u8(data, w + 1))
}

fn fill_squares(
    pos: &TbPos,
    pieces: &[u8; 7],
    flip: bool,
    mirror: usize,
    p: &mut [usize; 7],
    mut i: usize,
) -> usize {
    let mut white = colour_is_white(pieces[i]);
    if flip {
        white = !white;
    }
    let mut bb = pieces_by_type(pos, white, type_of_piece(pieces[i]));
    while bb != 0 {
        let sq = bb.trailing_zeros() as usize;
        bb &= bb - 1;
        if i < 7 {
            p[i] = sq ^ mirror;
            i += 1;
        }
    }
    i
}

fn leading_pawn(p: &mut [usize; 7], entry: &Entry, enc: u32) -> usize {
    for i in 1..entry.pawns[0] as usize {
        if FLAP[enc as usize - 1][p[0]] > FLAP[enc as usize - 1][p[i]] {
            p.swap(0, i);
        }
    }
    if enc == FILE_ENC {
        FILE_TO_FILE[p[0] & 7]
    } else {
        (p[0] - 8) >> 3
    }
}

fn encode(p: &mut [usize; 7], ei: &EncInfo, entry: &Entry, enc: u32) -> u64 {
    let idx_tables = indices();
    let n = entry.num as usize;
    let mut idx: u64;
    let mut k;

    if p[0] & 0x04 != 0 {
        for square in p.iter_mut().take(n) {
            *square ^= 0x07;
        }
    }

    if enc == PIECE_ENC {
        if p[0] & 0x20 != 0 {
            for square in p.iter_mut().take(n) {
                *square ^= 0x38;
            }
        }

        for i in 0..n {
            if OFF_DIAG[p[i]] != 0 {
                if OFF_DIAG[p[i]] > 0 && i < if entry.kk_enc { 2 } else { 3 } {
                    for square in p.iter_mut().take(n) {
                        *square = FLIP_DIAG[*square];
                    }
                }
                break;
            }
        }

        if entry.kk_enc {
            idx = KK_IDX[TRIANGLE[p[0]] as usize][p[1]] as u64;
            k = 2;
        } else {
            let s1 = u64::from(p[1] > p[0]);
            let s2 = u64::from(p[2] > p[0]) + u64::from(p[2] > p[1]);

            if OFF_DIAG[p[0]] != 0 {
                idx = TRIANGLE[p[0]] * 63 * 62 + (p[1] as u64 - s1) * 62 + (p[2] as u64 - s2);
            } else if OFF_DIAG[p[1]] != 0 {
                idx = 6 * 63 * 62 + DIAG[p[0]] * 28 * 62 + LOWER[p[1]] * 62 + p[2] as u64 - s2;
            } else if OFF_DIAG[p[2]] != 0 {
                idx = 6 * 63 * 62
                    + 4 * 28 * 62
                    + DIAG[p[0]] * 7 * 28
                    + (DIAG[p[1]] - s1) * 28
                    + LOWER[p[2]];
            } else {
                idx = 6 * 63 * 62
                    + 4 * 28 * 62
                    + 4 * 7 * 28
                    + DIAG[p[0]] * 7 * 6
                    + (DIAG[p[1]] - s1) * 6
                    + (DIAG[p[2]] - s2);
            }
            k = 3;
        }
        idx = idx.wrapping_mul(ei.factor[0]);
    } else {
        let pawns0 = entry.pawns[0] as usize;
        for i in 1..pawns0 {
            for j in i + 1..pawns0 {
                if PAWN_TWIST[enc as usize - 1][p[i]] < PAWN_TWIST[enc as usize - 1][p[j]] {
                    p.swap(i, j);
                }
            }
        }
        k = pawns0;
        idx = idx_tables.pawn_idx[enc as usize - 1][k - 1][FLAP[enc as usize - 1][p[0]]];
        for (i, &square) in p.iter().enumerate().take(k).skip(1) {
            idx += idx_tables.binomial[k - i][PAWN_TWIST[enc as usize - 1][square]];
        }
        idx = idx.wrapping_mul(ei.factor[0]);

        if entry.pawns[1] != 0 {
            let t = k + entry.pawns[1] as usize;
            for i in k..t {
                for j in i + 1..t {
                    if p[i] > p[j] {
                        p.swap(i, j);
                    }
                }
            }
            let mut s = 0u64;
            for i in k..t {
                let sq = p[i];
                let mut skips = 0usize;
                for &other in p.iter().take(k) {
                    skips += usize::from(sq > other);
                }
                s += idx_tables.binomial[i - k + 1][sq - skips - 8];
            }
            idx = idx.wrapping_add(s.wrapping_mul(ei.factor[k]));
            k = t;
        }
    }

    while k < n {
        let t = k + ei.norm[k] as usize;
        for i in k..t {
            for j in i + 1..t {
                if p[i] > p[j] {
                    p.swap(i, j);
                }
            }
        }
        let mut s = 0u64;
        for i in k..t {
            let sq = p[i];
            let mut skips = 0usize;
            for &other in p.iter().take(k) {
                skips += usize::from(sq > other);
            }
            s += idx_tables.binomial[i - k + 1][sq - skips];
        }
        idx = idx.wrapping_add(s.wrapping_mul(ei.factor[k]));
        k = t;
    }

    idx
}

impl Store {
    fn probe_table(&self, pos: &TbPos, s: i32, success: &mut i32, is_dtz: bool) -> i32 {
        let key = calc_key(pos, false);
        if !is_dtz && key == 0 {
            return 0;
        }

        let Some(&entry_index) = self.by_key.get(&key) else {
            *success = 0;
            return 0;
        };
        let entry = &self.entries[entry_index];
        if is_dtz && !entry.has_dtz {
            *success = 0;
            return 0;
        }

        let cell = if is_dtz { &entry.dtz } else { &entry.wdl };
        let table = cell.get_or_init(|| {
            let path = if is_dtz {
                entry.dtz_path.as_deref()?
            } else {
                &entry.wdl_path
            };
            load_table(entry, path, is_dtz)
        });
        let Some(table) = table.as_ref() else {
            *success = 0;
            return 0;
        };

        let (flip, bside) = if !entry.symmetric {
            let flip = key != entry.key;
            (flip, (pos.turn == crate::chess::WHITE) == flip)
        } else {
            (pos.turn != crate::chess::WHITE, false)
        };

        let num = num_tables(entry);
        let mut p = [0usize; 7];
        let idx;
        let mut t = 0usize;
        let mut flags = 0u8;
        let ei;

        if !entry.has_pawns {
            if is_dtz {
                flags = table.dtz_flags[0];
                if (flags & 1) != u8::from(bside) && !entry.symmetric {
                    *success = -1;
                    return 0;
                }
            }
            ei = if is_dtz {
                &table.ei[0]
            } else {
                &table.ei[usize::from(bside)]
            };
            let mut i = 0usize;
            while i < entry.num as usize {
                i = fill_squares(pos, &ei.pieces, flip, 0, &mut p, i);
            }
            idx = encode(&mut p, ei, entry, PIECE_ENC);
        } else {
            let mirror = if flip { 0x38 } else { 0 };
            let mut i = fill_squares(pos, &table.ei[0].pieces, flip, mirror, &mut p, 0);
            t = leading_pawn(&mut p, entry, FILE_ENC);
            if is_dtz {
                flags = table.dtz_flags[t];
                if (flags & 1) != u8::from(bside) && !entry.symmetric {
                    *success = -1;
                    return 0;
                }
            }
            ei = if is_dtz {
                &table.ei[t]
            } else {
                &table.ei[t + num * usize::from(bside)]
            };
            while i < entry.num as usize {
                i = fill_squares(pos, &ei.pieces, flip, mirror, &mut p, i);
            }
            idx = encode(&mut p, ei, entry, FILE_ENC);
        }

        let Some(pairs) = ei.precomp.as_ref() else {
            *success = 0;
            return 0;
        };
        let (w0, w1) = decompress_pairs(table, pairs, idx);

        if !is_dtz {
            return i32::from(w0) - 2;
        }

        let mut v = i32::from(w0) + ((i32::from(w1) & 0x0f) << 8);
        if flags & 2 != 0 {
            let m = WDL_TO_MAP[(s + 2) as usize];
            let map_index = usize::from(table.dtz_map_idx[t][m]) + v as usize;
            if flags & 16 == 0 {
                v = i32::from(rd_u8(&table.data, table.dtz_map + map_index));
            } else {
                v = i32::from(rd_le_u16(&table.data, table.dtz_map + 2 * map_index));
            }
        }
        if flags & PA_FLAGS[(s + 2) as usize] == 0 || (s & 1) != 0 {
            v *= 2;
        }
        v
    }

    fn probe_wdl_table(&self, pos: &TbPos, success: &mut i32) -> i32 {
        self.probe_table(pos, 0, success, false)
    }

    fn probe_dtz_table(&self, pos: &TbPos, wdl: i32, success: &mut i32) -> i32 {
        self.probe_table(pos, wdl, success, true)
    }

    fn probe_ab(&self, pos: &TbPos, mut alpha: i32, beta: i32, success: &mut i32) -> i32 {
        let captures = gen_captures(pos);
        for &mv in captures.as_slice() {
            if !is_capture(pos, mv) {
                continue;
            }
            let Some(child) = do_move(pos, mv) else {
                continue;
            };
            let v = -self.probe_ab(&child, -beta, -alpha, success);
            if *success == 0 {
                return 0;
            }
            if v > alpha {
                if v >= beta {
                    return v;
                }
                alpha = v;
            }
        }

        let v = self.probe_wdl_table(pos, success);
        alpha.max(v)
    }

    pub(crate) fn probe_wdl(&self, pos: &TbPos, success: &mut i32) -> i32 {
        *success = 1;

        let captures = gen_captures(pos);
        let mut best_cap = -3i32;
        let mut best_ep = -3i32;

        for &mv in captures.as_slice() {
            if !is_capture(pos, mv) {
                continue;
            }
            let Some(child) = do_move(pos, mv) else {
                continue;
            };
            let v = -self.probe_ab(&child, -2, -best_cap, success);
            if *success == 0 {
                return 0;
            }
            if v > best_cap {
                if v == 2 {
                    *success = 2;
                    return 2;
                }
                if !is_en_passant(pos, mv) {
                    best_cap = v;
                } else if v > best_ep {
                    best_ep = v;
                }
            }
        }

        let v = self.probe_wdl_table(pos, success);
        if *success == 0 {
            return 0;
        }

        if best_ep > best_cap {
            if best_ep > v {
                *success = 2;
                return best_ep;
            }
            best_cap = best_ep;
        }

        if best_cap >= v {
            *success = 1 + i32::from(best_cap > 0);
            return best_cap;
        }

        if best_ep > -3 && v == 0 {
            let moves = gen_moves(pos);
            let mut has_non_ep = false;
            for &mv in moves.as_slice() {
                if !is_en_passant(pos, mv) && crate::chess::legal_move(pos, mv) {
                    has_non_ep = true;
                    break;
                }
            }
            if !has_non_ep && !is_check(pos) {
                *success = 2;
                return best_ep;
            }
        }

        v
    }

    pub(crate) fn probe_dtz(&self, pos: &TbPos, success: &mut i32) -> i32 {
        let wdl = self.probe_wdl(pos, success);
        if *success == 0 {
            return 0;
        }
        if wdl == 0 {
            return 0;
        }
        if *success == 2 {
            return WDL_TO_DTZ[(wdl + 2) as usize];
        }

        if wdl > 0 {
            let moves = gen_legal(pos);
            for &mv in moves.as_slice() {
                if !is_pawn_move(pos, mv) || is_capture(pos, mv) {
                    continue;
                }
                let Some(child) = do_move(pos, mv) else {
                    continue;
                };
                let v = -self.probe_wdl(&child, success);
                if *success == 0 {
                    return 0;
                }
                if v == wdl {
                    return WDL_TO_DTZ[(wdl + 2) as usize];
                }
            }
        }

        let dtz = self.probe_dtz_table(pos, wdl, success);
        if *success >= 0 {
            return WDL_TO_DTZ[(wdl + 2) as usize] + if wdl > 0 { dtz } else { -dtz };
        }

        let mut best;
        let move_list;
        if wdl > 0 {
            best = i32::MAX;
            move_list = gen_moves(pos);
        } else {
            best = WDL_TO_DTZ[(wdl + 2) as usize];
            move_list = gen_moves(pos);
        }

        for &mv in move_list.as_slice() {
            if is_capture(pos, mv) || is_pawn_move(pos, mv) {
                continue;
            }
            let Some(child) = do_move(pos, mv) else {
                continue;
            };
            let v = -self.probe_dtz(&child, success);
            if v == 1 && is_mate(&child) {
                best = 1;
            } else if wdl > 0 {
                if v > 0 && v + 1 < best {
                    best = v + 1;
                }
            } else if v - 1 < best {
                best = v - 1;
            }
            if *success == 0 {
                return 0;
            }
        }
        best
    }

    /// Ranks every legal root move by its DTZ-style value from the root
    /// mover's perspective. Returns `(root_dtz, per-move values)`; values use
    /// the Fathom convention where positive means the root side wins.
    pub(crate) fn probe_root(&self, pos: &TbPos) -> Option<(i32, Vec<(TbMove, i32)>)> {
        let mut success = 1i32;
        let dtz = self.probe_dtz(pos, &mut success);
        if success == 0 {
            return None;
        }

        let moves = gen_moves(pos);
        let mut scored = Vec::with_capacity(MAX_MOVES);
        for &mv in moves.as_slice() {
            let Some(child) = do_move(pos, mv) else {
                continue;
            };
            let v = if dtz > 0 && is_mate(&child) {
                1
            } else if child.rule50 != 0 {
                let mut v = -self.probe_dtz(&child, &mut success);
                if v > 0 {
                    v += 1;
                } else if v < 0 {
                    v -= 1;
                }
                v
            } else {
                let v = -self.probe_wdl(&child, &mut success);
                WDL_TO_DTZ[(v + 2) as usize]
            };
            if success == 0 {
                return None;
            }
            scored.push((mv, v));
        }
        Some((dtz, scored))
    }
}

pub(crate) fn dtz_to_wdl(cnt50: i32, dtz: i32) -> i32 {
    if dtz > 0 {
        if dtz + cnt50 <= 100 { 2 } else { 1 }
    } else if dtz < 0 {
        if -dtz + cnt50 <= 100 { -2 } else { -1 }
    } else {
        0
    }
}
