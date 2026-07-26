use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

const BOARD_SQUARES: usize = 64;
const ROOK_DIRECTIONS: &[(i8, i8)] = &[(1, 0), (-1, 0), (0, 1), (0, -1)];
const BISHOP_DIRECTIONS: &[(i8, i8)] = &[(1, 1), (1, -1), (-1, 1), (-1, -1)];

#[derive(Clone, Copy)]
struct GeneratedEntry {
    mask: u64,
    magic: u64,
    shift: u32,
    offset: usize,
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn sparse(&mut self) -> u64 {
        self.next() & self.next() & self.next()
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let (rook_entries, rook_pext, rook_magic) = generate_slider(ROOK_DIRECTIONS, 0x726f_6f6b);
    let (bishop_entries, bishop_pext, bishop_magic) =
        generate_slider(BISHOP_DIRECTIONS, 0x6269_7368_6f70);

    let mut generated = String::new();
    emit_entries(&mut generated, "ROOK_ENTRIES", &rook_entries);
    emit_entries(&mut generated, "BISHOP_ENTRIES", &bishop_entries);
    emit_attacks(&mut generated, "ROOK_PEXT_ATTACKS", &rook_pext);
    emit_attacks(&mut generated, "BISHOP_PEXT_ATTACKS", &bishop_pext);
    emit_attacks(&mut generated, "ROOK_MAGIC_ATTACKS", &rook_magic);
    emit_attacks(&mut generated, "BISHOP_MAGIC_ATTACKS", &bishop_magic);

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("sliding_tables.rs");
    fs::write(output, generated).expect("generated sliding tables should be writable");
}

fn generate_slider(
    directions: &[(i8, i8)],
    seed: u64,
) -> (Vec<GeneratedEntry>, Vec<u64>, Vec<u64>) {
    let mut entries = Vec::with_capacity(BOARD_SQUARES);
    let mut pext_attacks = Vec::new();
    let mut magic_attacks = Vec::new();
    let mut rng = SplitMix64(seed);

    for square in 0..BOARD_SQUARES {
        let mask = relevant_mask(square, directions);
        let relevant_bits = mask.count_ones();
        let table_size = 1usize << relevant_bits;
        let shift = 64 - relevant_bits;
        let offset = pext_attacks.len();
        let occupancies: Vec<u64> = subsets(mask).collect();
        let attacks: Vec<u64> = occupancies
            .iter()
            .map(|&occupancy| sliding_attacks(square, occupancy, directions))
            .collect();

        let mut pext_segment = vec![0; table_size];
        for (&occupancy, &attack) in occupancies.iter().zip(&attacks) {
            pext_segment[software_pext(occupancy, mask) as usize] = attack;
        }
        pext_attacks.extend(pext_segment);

        let (magic, magic_segment) =
            find_black_magic(mask, shift, &occupancies, &attacks, &mut rng);
        magic_attacks.extend(magic_segment);
        entries.push(GeneratedEntry {
            mask,
            magic,
            shift,
            offset,
        });
    }

    (entries, pext_attacks, magic_attacks)
}

fn find_black_magic(
    mask: u64,
    shift: u32,
    occupancies: &[u64],
    attacks: &[u64],
    rng: &mut SplitMix64,
) -> (u64, Vec<u64>) {
    let table_size = 1usize << mask.count_ones();
    let black_mask = !mask;
    let mut used = vec![None; table_size];
    let mut touched = Vec::with_capacity(table_size);

    for attempt in 0..100_000_000u32 {
        let magic = rng.sparse();
        if mask.wrapping_mul(magic) & 0xff00_0000_0000_0000 == 0 {
            continue;
        }

        touched.clear();
        let mut collision = false;
        for (&occupancy, &attack) in occupancies.iter().zip(attacks) {
            let index = ((occupancy | black_mask).wrapping_mul(magic) >> shift) as usize;
            match used[index] {
                Some(existing) if existing != attack => {
                    collision = true;
                    break;
                }
                Some(_) => {}
                None => {
                    used[index] = Some(attack);
                    touched.push(index);
                }
            }
        }

        if !collision {
            return (
                magic,
                used.into_iter().map(|attack| attack.unwrap_or(0)).collect(),
            );
        }

        for &index in &touched {
            used[index] = None;
        }

        assert!(
            attempt + 1 < 100_000_000,
            "failed to find a black magic for mask {mask:#018x}"
        );
    }

    unreachable!("the bounded search either returns or panics")
}

fn relevant_mask(square: usize, directions: &[(i8, i8)]) -> u64 {
    let file = (square & 7) as i8;
    let rank = (square >> 3) as i8;
    let mut mask = 0;

    for &(file_step, rank_step) in directions {
        let mut next_file = file + file_step;
        let mut next_rank = rank + rank_step;
        while on_board(next_file, next_rank) {
            let beyond_file = next_file + file_step;
            let beyond_rank = next_rank + rank_step;
            if !on_board(beyond_file, beyond_rank) {
                break;
            }
            mask |= bit(next_file, next_rank);
            next_file = beyond_file;
            next_rank = beyond_rank;
        }
    }

    mask
}

fn sliding_attacks(square: usize, occupancy: u64, directions: &[(i8, i8)]) -> u64 {
    let file = (square & 7) as i8;
    let rank = (square >> 3) as i8;
    let mut attacks = 0;

    for &(file_step, rank_step) in directions {
        let mut next_file = file + file_step;
        let mut next_rank = rank + rank_step;
        while on_board(next_file, next_rank) {
            let target = bit(next_file, next_rank);
            attacks |= target;
            if occupancy & target != 0 {
                break;
            }
            next_file += file_step;
            next_rank += rank_step;
        }
    }

    attacks
}

fn subsets(mask: u64) -> impl Iterator<Item = u64> {
    let mut subset = 0u64;
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let current = subset;
        subset = subset.wrapping_sub(mask) & mask;
        finished = subset == 0;
        Some(current)
    })
}

fn software_pext(value: u64, mut mask: u64) -> u64 {
    let mut result = 0;
    let mut target = 1;
    while mask != 0 {
        let source = mask & mask.wrapping_neg();
        if value & source != 0 {
            result |= target;
        }
        mask &= mask - 1;
        target <<= 1;
    }
    result
}

fn bit(file: i8, rank: i8) -> u64 {
    1u64 << ((rank as u32 * 8) + file as u32)
}

fn on_board(file: i8, rank: i8) -> bool {
    (0..8).contains(&file) && (0..8).contains(&rank)
}

fn emit_entries(output: &mut String, name: &str, entries: &[GeneratedEntry]) {
    writeln!(output, "static {name}: [MagicEntry; {}] = [", entries.len()).unwrap();
    for entry in entries {
        writeln!(
            output,
            "    MagicEntry {{ mask: {:#018x}, magic: {:#018x}, shift: {}, offset: {} }},",
            entry.mask, entry.magic, entry.shift, entry.offset
        )
        .unwrap();
    }
    writeln!(output, "];").unwrap();
}

fn emit_attacks(output: &mut String, name: &str, attacks: &[u64]) {
    writeln!(output, "#[allow(dead_code)]").unwrap();
    writeln!(output, "static {name}: [u64; {}] = [", attacks.len()).unwrap();
    for chunk in attacks.chunks(4) {
        output.push_str("    ");
        for attack in chunk {
            write!(output, "{attack:#018x}, ").unwrap();
        }
        output.push('\n');
    }
    writeln!(output, "];").unwrap();
}
