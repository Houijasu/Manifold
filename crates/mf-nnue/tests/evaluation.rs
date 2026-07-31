use std::path::PathBuf;

use mf_core::{Color, Position};
use mf_nnue::{L1, Network};

fn local_network() -> Option<Network> {
    let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
    let path = explicit_path.clone().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    if !path.is_file() {
        assert!(
            explicit_path.is_none(),
            "MF_NNUE_TEST_NET requires an existing network file: {}",
            path.display()
        );
        eprintln!(
            "SKIPPED: local NNUE evaluation test is missing {}",
            path.display()
        );
        return None;
    }
    Some(
        Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}

#[test]
fn eonego_golden_internal_evaluations_match() {
    let Some(network) = local_network() else {
        return;
    };
    let cases = [
        (
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            -12,
        ),
        (
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            -317,
        ),
        (
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R b KQkq - 0 1",
            962,
        ),
        ("2r3k1/5ppp/8/8/8/8/5PPP/2R3K1 w - - 0 1", 17),
        ("8/8/8/3k4/8/3K4/4P3/8 w - - 0 1", 321),
        (
            "1nbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQk - 0 1",
            1_785,
        ),
    ];

    for (fen, expected) in cases {
        let position = Position::from_fen(fen, false).expect("test FEN should parse");
        assert_eq!(network.evaluate_internal(&position), expected, "{fen}");
    }
}

#[test]
fn startpos_color_mirror_and_dump_metadata_are_consistent() {
    let Some(network) = local_network() else {
        return;
    };
    let white = Position::from_fen(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w - - 0 1",
        false,
    )
    .expect("white start position should parse");
    let black = Position::from_fen(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR b - - 0 1",
        false,
    )
    .expect("black mirrored start position should parse");
    let mut white_ft = [0; L1];
    let mut black_ft = [0; L1];

    let white_dump = network.dump_features(&white, &mut white_ft);
    let black_dump = network.dump_features(&black, &mut black_ft);

    assert_eq!(white_dump.bucket, 7);
    assert_eq!(white_dump.stm, Color::White);
    assert_eq!(black_dump.stm, Color::Black);
    assert_eq!(white_dump.psqt_internal, black_dump.psqt_internal);
    assert_eq!(white_dump.eval_internal, black_dump.eval_internal);
    assert_eq!(white_ft, black_ft);
    assert_eq!(network.evaluate(&white), network.evaluate(&black));
}
