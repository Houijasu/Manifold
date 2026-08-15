//! Malformed table files must be rejected at load time, never panic or abort.
//!
//! `SyzygyPath` points the engine at user-supplied binaries parsed on the UCI thread,
//! and the workspace compiles with `panic = "abort"`: a stack overflow or an overflow
//! panic there kills the engine mid-game. These tests craft `.rtbw` files whose headers
//! drive the two historically unguarded paths -- the symbol-length recursion and the
//! section-offset accumulation -- and assert the load fails gracefully (the table is
//! simply unavailable, and probing reports `None`).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use mf_core::Position;
use mf_tb::Tablebases;

/// KQvKR, white to move: white king and queen against black king and rook.
const KQVKR_FEN: &str = "8/8/8/7r/2k5/8/1Q6/K7 w - - 0 1";

/// Little-endian WDL magic, the only header field `load_table` checks before parsing.
const WDL_MAGIC_LE: [u8; 4] = 0x5d23e871u32.to_le_bytes();

/// Writes `data` as `KQvKR.rtbw` in a fresh temp directory and opens the store over it.
fn tables_over(tag: &str, data: &[u8]) -> Tablebases {
    let dir = temp_dir_for(tag);
    std::fs::create_dir_all(&dir).expect("temp directory should create");
    // `find_file` only accepts files whose length is congruent to 16 modulo 64, so the
    // crafted buffer is zero-padded to the next legal length.
    let mut file = data.to_vec();
    while file.len() % 64 != 16 {
        file.push(0);
    }
    std::fs::write(dir.join("KQvKR.rtbw"), file).expect("crafted table should be writable");
    Tablebases::new(dir.to_str().expect("temp path should be UTF-8"))
        .expect("a store over one existing directory should open")
}

fn temp_dir_for(tag: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be past the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "manifold-malformed-tb-{tag}-{}-{unique}",
        std::process::id()
    ))
}

/// Builds a WDL header up to and including the symbol table.
///
/// Layout for a non-pawn table (`load_table` -> `setup_pairs`): a 5-byte file header,
/// the 6-byte piece-configuration block of a 5-man table, one alignment byte, then the
/// pair-compression header whose `h = max_len - min_len + 1` is fixed at 1 here so the
/// symbol table starts at a known offset.
fn wdl_with_symbols(real_num_blocks: u32, symbols: &[[u8; 3]]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&WDL_MAGIC_LE);
    data.push(0); // file header byte: no split
    data.extend_from_slice(&[0; 6]); // piece-configuration block (entry.num + 1 bytes)
    data.push(0); // alignment: 11 -> 12
    data.push(0); // pair flags: not a constant table
    data.push(10); // block_size (<= 30)
    // A large idx_bits keeps the index table one entry wide, so the rejection is
    // driven by the block count below rather than by the index-table size.
    data.push(40); // idx_bits (1..=63)
    data.push(0); // num_blocks high add-on
    data.extend_from_slice(&real_num_blocks.to_le_bytes());
    data.push(1); // max_len
    data.push(1); // min_len
    data.extend_from_slice(&[0; 2]); // base[] table (h = 1 entry)
    data.extend_from_slice(&(symbols.len() as u16).to_le_bytes());
    for symbol in symbols {
        data.extend_from_slice(symbol);
    }
    data
}

/// Encodes one 3-byte symbol-table entry: `s1` in the low 12 bits, `s2` in the high 12.
///
/// A symbol with `s2 == 0x0fff` is a leaf; otherwise both `s1` and `s2` are child
/// symbol indices the length recursion descends into.
fn symbol(s1: u16, s2: u16) -> [u8; 3] {
    [
        (s1 & 0xff) as u8,
        (((s1 >> 8) & 0x0f) | ((s2 & 0x0f) << 4)) as u8,
        ((s2 >> 4) & 0xff) as u8,
    ]
}

/// A symbol chain deeper than any legitimate table must poison the load, not recurse
/// unbounded on the UCI thread.
#[test]
fn an_over_deep_symbol_chain_is_rejected() {
    // 300 symbols, each pointing at the next; the last is a leaf. Honest symbol trees
    // cannot be this deep: a symbol's stored length bounds its subtree height.
    let mut symbols = Vec::new();
    for s in 0..300u16 {
        symbols.push(if s == 299 {
            symbol(0, 0x0fff)
        } else {
            symbol(s + 1, s + 1)
        });
    }
    let tables = tables_over("chain", &wdl_with_symbols(0, &symbols));

    let position = Position::from_fen(KQVKR_FEN, false).expect("test FEN must parse");
    assert_eq!(
        tables.probe_wdl(&position),
        None,
        "a crafted symbol chain must make the table unavailable, not abort the process"
    );
}

/// A block count large enough to wrap unchecked offset arithmetic must be rejected at
/// the first out-of-range step, not after a wrapped pointer sails past the final check.
#[test]
fn a_wraparound_sized_block_count_is_rejected() {
    // Two leaf symbols keep the pair header itself valid; the enormous block count
    // only inflates the section sizes that `load_table` accumulates.
    let tables = tables_over(
        "blocks",
        &wdl_with_symbols(u32::MAX, &[symbol(0, 0x0fff), symbol(0, 0x0fff)]),
    );

    let position = Position::from_fen(KQVKR_FEN, false).expect("test FEN must parse");
    assert_eq!(
        tables.probe_wdl(&position),
        None,
        "a wraparound-sized header must make the table unavailable"
    );
}
