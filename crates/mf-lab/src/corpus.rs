use mf_core::Position;

use crate::reservoir::SplitMix64;

#[derive(Clone, Debug)]
pub struct CorpusRoot {
    pub fen: String,
    pub position: Position,
    pub key: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    Train,
    Test,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedRoot {
    pub index: usize,
    pub split: Split,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootSelection {
    pub warmup: Vec<usize>,
    pub measured: Vec<SelectedRoot>,
}

pub fn parse_epd(input: &str) -> Result<Vec<CorpusRoot>, String> {
    let mut roots = Vec::new();
    for (line_index, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fen = line.split_once(';').map_or(line, |(fen, _)| fen).trim();
        let position = Position::from_fen(fen, false)
            .map_err(|error| format!("line {}: invalid FEN: {error}", line_index + 1))?;
        roots.push(CorpusRoot {
            fen: fen.to_owned(),
            key: position.repetition_key(),
            position,
        });
    }
    if roots.is_empty() {
        return Err("EPD corpus contains no positions".to_owned());
    }
    Ok(roots)
}

pub fn select_roots(
    roots: &[CorpusRoot],
    warm_roots: usize,
    measured_roots: usize,
    seed: u64,
) -> Result<RootSelection, String> {
    let total = warm_roots
        .checked_add(measured_roots)
        .ok_or_else(|| "root count overflow".to_owned())?;
    if total > roots.len() {
        return Err(format!(
            "requested {total} roots but corpus contains {}",
            roots.len()
        ));
    }

    let mut indices = (0..roots.len()).collect::<Vec<_>>();
    let mut rng = SplitMix64::new(seed);
    for index in 0..total {
        let other = index + rng.index((roots.len() - index) as u64) as usize;
        indices.swap(index, other);
    }
    let warmup = indices[..warm_roots].to_vec();
    let measured = indices[warm_roots..total]
        .iter()
        .copied()
        .map(|index| SelectedRoot {
            index,
            split: split_for_root(roots[index].key, seed),
        })
        .collect();
    Ok(RootSelection { warmup, measured })
}

pub fn split_for_root(key: u64, seed: u64) -> Split {
    let mut rng = SplitMix64::new(key ^ seed.rotate_left(17) ^ 0xD1B5_4A32_D192_ED03);
    if rng.next_u64().is_multiple_of(5) {
        Split::Test
    } else {
        Split::Train
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTPOS: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";

    #[test]
    fn parser_accepts_plain_fen_and_semicolon_epd_and_skips_blank_comments() {
        let roots = parse_epd(&format!(
            "\n# comment\n{STARTPOS}\n{KIWIPETE}; id \"kiwipete\";\n"
        ))
        .expect("valid corpus");

        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].fen, STARTPOS);
        assert_eq!(roots[1].fen, KIWIPETE);
    }

    #[test]
    fn parser_reports_invalid_fen_with_its_line_number() {
        let error = parse_epd("not a FEN").expect_err("invalid FEN must fail");
        assert!(error.contains("line 1"));
        assert!(error.contains("invalid FEN"));
    }

    #[test]
    fn deterministic_selection_and_split_keep_each_root_in_one_partition() {
        let roots = parse_epd(&format!(
            "{STARTPOS}\n{KIWIPETE}\n8/8/8/8/8/8/6k1/4K3 w - - 0 1\n\
             8/8/8/8/8/6k1/8/4K3 w - - 0 1\n8/8/8/8/6k1/8/8/4K3 w - - 0 1\n"
        ))
        .expect("valid corpus");

        let first = select_roots(&roots, 1, 4, 7).expect("selection fits");
        let second = select_roots(&roots, 1, 4, 7).expect("selection fits");
        assert_eq!(first, second);
        assert_eq!(first.warmup.len(), 1);
        assert_eq!(first.measured.len(), 4);

        let train = first
            .measured
            .iter()
            .filter(|root| root.split == Split::Train)
            .map(|root| root.index)
            .collect::<std::collections::BTreeSet<_>>();
        let test = first
            .measured
            .iter()
            .filter(|root| root.split == Split::Test)
            .map(|root| root.index)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(train.is_disjoint(&test));
        assert_eq!(train.len() + test.len(), 4);
    }
}
