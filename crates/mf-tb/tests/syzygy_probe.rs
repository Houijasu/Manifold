use mf_core::{PieceKind, Position, generate_legal_moves};
use mf_tb::{Tablebases, Wdl};

fn open_tables() -> Option<Tablebases> {
    let paths = std::env::var("MF_SYZYGY_PATH").ok()?;
    let tables = Tablebases::new(&paths).expect("MF_SYZYGY_PATH must name existing directories");
    Some(tables)
}

fn probe(tables: &Tablebases, fen: &str) -> Option<Wdl> {
    let position = Position::from_fen(fen, false).expect("test FEN must parse");
    tables.probe_wdl(&position)
}

#[test]
fn discovery_finds_the_complete_three_to_six_man_sets() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(tables.max_pieces(), 6);
    assert_eq!(tables.wdl_table_count(), 145 + 365);
    assert!(
        tables.dtz_table_count() >= 145 + 365 - 1,
        "expected at least 509 DTZ tables (the local KRBPvKR.rtbz fails the \
         Syzygy size invariant and is skipped), found {}",
        tables.dtz_table_count()
    );
}

#[test]
fn kqvk_white_to_move_is_a_win() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(
        probe(&tables, "8/8/8/8/8/2k5/8/KQ6 w - - 0 1"),
        Some(Wdl::Win)
    );
}

#[test]
fn kqvk_defender_to_move_is_a_loss() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(
        probe(&tables, "8/8/8/8/8/2k5/8/K2Q4 b - - 0 1"),
        Some(Wdl::Loss)
    );
}

#[test]
fn kbvk_is_a_draw() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(
        probe(&tables, "8/8/8/8/8/2k5/8/KB6 w - - 0 1"),
        Some(Wdl::Draw)
    );
}

#[test]
fn kpvk_fortress_is_a_draw() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(
        probe(&tables, "6k1/8/8/3P4/4K3/8/8/8 b - - 0 1"),
        Some(Wdl::Draw)
    );
}

#[test]
fn kpvk_advanced_pawn_is_a_win() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(
        probe(&tables, "6k1/8/8/3P4/4K3/8/8/8 w - - 0 1"),
        Some(Wdl::Win)
    );
}

#[test]
fn knnvkp_position_probes_as_cursed_win() {
    let Some(tables) = open_tables() else {
        return;
    };
    assert_eq!(
        probe(&tables, "8/p3NN2/8/2K5/8/8/5k2/8 w - - 0 1"),
        Some(Wdl::CursedWin)
    );
}

#[test]
fn positions_with_castling_rights_are_rejected() {
    let Some(tables) = open_tables() else {
        return;
    };
    let position = Position::from_fen("4k3/8/8/8/8/8/8/4K2R w K - 0 1", false).unwrap();
    assert_eq!(tables.probe_wdl(&position), None);
}

#[test]
fn positions_beyond_the_table_range_are_rejected() {
    let Some(tables) = open_tables() else {
        return;
    };
    let position = Position::startpos();
    assert_eq!(tables.probe_wdl(&position), None);
}

#[test]
fn en_passant_capture_changes_the_verdict() {
    let Some(tables) = open_tables() else {
        return;
    };
    let with_ep = Position::from_fen("8/8/8/8/3KpP2/k7/8/8 b - f3 0 1", false).unwrap();
    let without_ep = Position::from_fen("8/8/8/8/3KpP2/k7/8/8 b - - 0 1", false).unwrap();
    assert!(with_ep.en_passant().is_some());
    assert_eq!(tables.probe_wdl(&with_ep), Some(Wdl::Draw));
    assert_eq!(tables.probe_wdl(&without_ep), Some(Wdl::Loss));
}

#[test]
fn root_probe_on_a_five_man_win_returns_a_win_preserving_move() {
    let Some(tables) = open_tables() else {
        return;
    };
    let mut position = Position::from_fen("8/8/8/8/8/2k5/8/KQ1RN3 w - - 0 1", false).unwrap();
    let root = tables.probe_root(&position).expect("5-man DTZ probe");
    assert_eq!(root.wdl, Wdl::Win);
    assert!(root.preserving_moves().count() >= 1);

    let best = root.best_move;
    assert!(generate_legal_moves(&position).as_slice().contains(&best));
    let undo = position.make_move(best);
    let child_wdl = tables
        .probe_wdl(&position)
        .expect("child of a 5-man position stays in range");
    position.unmake_move(best, undo);
    assert_eq!(child_wdl, Wdl::Loss);
}

#[test]
fn six_man_position_probes_and_root_probe_works() {
    let Some(tables) = open_tables() else {
        return;
    };
    let position = Position::from_fen("8/8/8/8/3k4/8/2R5/KQ1n1n2 w - - 0 1", false).unwrap();
    let wdl = tables
        .probe_wdl(&position)
        .expect("6-man probe must decode the large table");
    assert_eq!(wdl, Wdl::Win);

    let root = tables
        .probe_root(&position)
        .expect("6-man DTZ root probe must succeed");
    assert_eq!(root.wdl, Wdl::Win);
    assert!(root.preserving_moves().count() >= 1);
}

#[test]
fn wdl_recursion_is_self_consistent_on_random_five_man_positions() {
    let Some(tables) = open_tables() else {
        return;
    };
    let seeds = [
        "8/8/8/3k4/8/3P4/3K4/5R2 w - - 0 1",
        "8/3q4/8/3k4/8/8/3P4/3K4 b - - 0 1",
        "8/8/2n5/3k4/8/8/3PK3/8 w - - 0 1",
        "8/2p5/8/3k4/8/8/2QK4/8 b - - 0 1",
        "8/8/8/2bk4/8/8/2NK4/8 w - - 0 1",
    ];

    let mut checked = 0usize;
    let mut rng_state = 0x9e3779b97f4a7c15u64;
    let mut next_random = move |bound: usize| {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        (rng_state % bound as u64) as usize
    };

    for seed in seeds {
        let mut position = Position::from_fen(seed, false).unwrap();
        for _ in 0..24 {
            if position.occupancy().count() as usize <= tables.max_pieces()
                && verify_zeroing_consistency(&tables, &mut position)
            {
                checked += 1;
            }
            let moves = generate_legal_moves(&position);
            if moves.as_slice().is_empty() {
                break;
            }
            let mv = moves.as_slice()[next_random(moves.as_slice().len())];
            position.make_move(mv);
            if position.occupancy().count() < 3 {
                break;
            }
        }
    }
    assert!(checked >= 40, "only {checked} positions were checked");
}

/// Syzygy WDL recursion: with the halfmove clock forced to zero, the parent's
/// verdict must be at least the best negated child verdict over zeroing moves,
/// and equal to it whenever some zeroing move is optimal.
fn verify_zeroing_consistency(tables: &Tablebases, position: &mut Position) -> bool {
    let mut probe_position = position.clone();
    probe_position.set_halfmove_clock(0);
    let Some(parent) = tables.probe_wdl(&probe_position) else {
        return false;
    };

    let moves = generate_legal_moves(&probe_position);
    let mut best_zeroing: Option<Wdl> = None;
    for &mv in moves.as_slice() {
        let is_zeroing = mv.flag().is_capture()
            || probe_position
                .piece_at(mv.from())
                .is_some_and(|piece| piece.kind() == PieceKind::Pawn);
        if !is_zeroing {
            continue;
        }
        let undo = probe_position.make_move(mv);
        let child = tables.probe_wdl(&probe_position);
        probe_position.unmake_move(mv, undo);
        let Some(child) = child else {
            continue;
        };
        let negated = match child {
            Wdl::Loss => Wdl::Win,
            Wdl::BlessedLoss => Wdl::CursedWin,
            Wdl::Draw => Wdl::Draw,
            Wdl::CursedWin => Wdl::BlessedLoss,
            Wdl::Win => Wdl::Loss,
        };
        best_zeroing = Some(best_zeroing.map_or(negated, |best| best.max(negated)));
    }

    if let Some(best) = best_zeroing {
        assert!(
            parent >= best,
            "parent {parent:?} worse than best zeroing move {best:?}"
        );
    }
    true
}
