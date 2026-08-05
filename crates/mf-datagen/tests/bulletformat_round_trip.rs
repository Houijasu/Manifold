//! Differential tests against `bulletformat` itself.
//!
//! `mf-datagen` defines the 32-byte record layout independently so that it carries no
//! runtime dependency, which means nothing in the crate itself can catch a divergence
//! from the format bullet actually reads. These tests close that gap the same way
//! `mf-core`'s perft suite uses `cozy-chess`: the oracle is a dev-dependency, it is
//! never linked into the shipped binary, and it is the only thing that proves the
//! hand-rolled encoder agrees with the trainer.
//!
//! This is the test that backs the validation contract's requirement that "a
//! third-party load with bullet's `DirectSequentialDataLoader` succeeds on the
//! produced file". `DirectSequentialDataLoader` is a `bullet_lib` type whose only
//! requirement of a file is that its length be a whole number of `ChessBoard`s and
//! that each 32-byte window transmute to one — `bulletformat::DataLoader`, used here,
//! reads files by exactly that rule.

use std::io::Write;
use std::str::FromStr;

use bulletformat::{BulletFormat, ChessBoard, DataLoader};
use mf_core::{Color, Position};
use mf_datagen::{Filter, GenerateConfig, Outcome, RECORD_BYTES, Record, generate};

/// A spread of positions covering the structural cases that change the encoding:
/// both sides to move, asymmetric material, promotions pending, castling available,
/// and sparse endgames.
const CASES: &[&str] = &[
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR b KQkq - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R b KQkq - 0 1",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 b - - 0 1",
    "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1",
    "4k3/8/8/8/8/8/4P3/4K3 b - - 0 1",
    "r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
    "2rq1rk1/pb2bppp/np2pn2/3p4/3P4/1P2PN2/PB1NBPPP/R2Q1RK1 w - - 0 1",
    "8/1p3pp1/7p/5P1P/2k3P1/8/2K2P2/8 w - - 0 1",
    "6k1/6p1/7p/P1N5/1r3p2/7P/1b3PP1/3bR1K1 b - - 0 1",
];

fn position(fen: &str) -> Position {
    Position::from_fen(fen, false).expect("test FEN parses")
}

/// Builds the reference record via `bulletformat`'s own text parser, which is the
/// path the ecosystem uses to ingest `<FEN> | <score> | <result>` data.
fn reference(fen: &str, white_score: i16, white_result: f32) -> ChessBoard {
    ChessBoard::from_str(&format!("{fen} | {white_score} | {white_result:.1}"))
        .expect("bulletformat parses the reference line")
}

fn white_relative_outcome(result: f32) -> Outcome {
    match result {
        r if r > 0.75 => Outcome::Win,
        r if r < 0.25 => Outcome::Loss,
        _ => Outcome::Draw,
    }
}

#[test]
fn our_encoder_produces_byte_identical_records_to_bulletformat() {
    for fen in CASES {
        let position = position(fen);
        for (white_score, white_result) in [(0i16, 0.5f32), (137, 1.0), (-849, 0.0), (25, 0.5)] {
            // bulletformat takes white-relative inputs and negates internally; our
            // encoder takes them already side-to-move relative. Converting here is
            // what makes the two directly comparable, and a sign error in either
            // would show up as a mismatched score byte.
            let relative_score = match position.side_to_move() {
                Color::White => white_score,
                Color::Black => -white_score,
            };
            let relative_outcome = Outcome::from_white_relative(
                white_relative_outcome(white_result),
                position.side_to_move(),
            );

            let ours = Record::encode(&position, relative_score, relative_outcome)
                .expect("position encodes");
            let theirs = reference(fen, white_score, white_result);

            let ours_bytes = ours.to_bytes();
            let theirs_bytes = ChessBoard::as_bytes_slice(std::slice::from_ref(&theirs));

            assert_eq!(
                ours_bytes.as_slice(),
                theirs_bytes,
                "record bytes diverge for {fen} at score {white_score} result {white_result}"
            );
        }
    }
}

#[test]
fn bulletformat_decodes_our_records_to_the_same_pieces_scores_and_results() {
    for fen in CASES {
        let position = position(fen);
        let relative_score = 321i16;
        let ours = Record::encode(&position, relative_score, Outcome::Win).expect("encodes");

        let theirs = ChessBoard::from_bytes(ours.to_bytes());

        assert_eq!(theirs.score(), relative_score, "score survives for {fen}");
        assert_eq!(
            theirs.result_idx(),
            Outcome::Win as usize,
            "result for {fen}"
        );
        assert_eq!(theirs.occ(), ours.occupancy(), "occupancy for {fen}");
        assert_eq!(theirs.our_ksq(), ours.king_square(), "stm king for {fen}");
        assert_eq!(
            theirs.opp_ksq(),
            ours.opponent_king_square(),
            "nstm king for {fen}"
        );

        let ours_pieces: Vec<(u8, u8)> = ours.iter_pieces().collect();
        let theirs_pieces: Vec<(u8, u8)> = theirs.into_iter().collect();
        assert_eq!(
            ours_pieces, theirs_pieces,
            "piece iteration order and codes for {fen}"
        );
    }
}

/// Extension trait so the test can build a `ChessBoard` from our bytes without
/// `bulletformat` exposing a constructor. The transmute is exactly what
/// `DirectSequentialDataLoader` does to every 32-byte window of a data file, so
/// exercising it here is exercising the real load path.
trait FromRecordBytes {
    fn from_bytes(bytes: [u8; RECORD_BYTES]) -> Self;
}

impl FromRecordBytes for ChessBoard {
    fn from_bytes(bytes: [u8; RECORD_BYTES]) -> Self {
        assert_eq!(size_of::<ChessBoard>(), RECORD_BYTES);
        // SAFETY: `ChessBoard` is `#[repr(C)]` and composed entirely of integer fields
        // with no invalid bit patterns and no padding (its own const assertion pins
        // the size at 32 bytes), so any 32 bytes are a valid value. This is precisely
        // the guarantee bullet's `CanBeDirectlySequentiallyLoaded` marker asserts, and
        // the reinterpretation its data loader performs on every record it reads.
        unsafe { std::mem::transmute::<[u8; RECORD_BYTES], ChessBoard>(bytes) }
    }
}

#[test]
fn a_generated_file_loads_through_bulletformats_data_loader_with_a_matching_count() {
    // Self-play evaluates with NNUE, so this round trip needs the (gitignored) network.
    let network_path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        std::path::PathBuf::from,
    );
    if !network_path.is_file() {
        eprintln!(
            "SKIPPED: bulletformat round trip needs {}",
            network_path.display()
        );
        return;
    }
    let network = mf_nnue::Network::load(&network_path).expect("network loads");

    let config = GenerateConfig {
        games: 12,
        nodes: 1_500,
        threads: 4,
        seed: 20_260_731,
        filter: Filter::default(),
    };

    let mut path = std::env::temp_dir();
    path.push(format!("mf-datagen-loader-{}.bullet", std::process::id()));

    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).expect("temp file"));
    let stats = generate(config, &network, |batch| {
        for record in batch {
            file.write_all(&record.to_bytes())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })
    .expect("generation succeeds");
    file.flush().expect("flush");
    drop(file);

    assert!(stats.positions > 0, "the run must produce records");

    let length = std::fs::metadata(&path).expect("metadata").len();
    assert_eq!(
        length % RECORD_BYTES as u64,
        0,
        "file length must be a whole number of records"
    );
    assert_eq!(
        length / RECORD_BYTES as u64,
        stats.positions,
        "the file's true record count must equal the reported position count"
    );

    // This is the third-party load the validation contract asks for: bullet's own
    // reader, reading the file we produced, with no conversion step.
    let loader = DataLoader::<ChessBoard>::new(&path, 4).expect("bullet opens the file");
    assert_eq!(loader.len() as u64, stats.positions);

    let mut loaded = 0u64;
    let mut kings_seen_by_bullet = 0u64;
    loader.map_positions(|board| {
        loaded += 1;
        // Re-run bullet's own validity rules, which is what `bullet-utils validate`
        // does: exactly one king per side, correct king-square fields, no pawn on a
        // back rank, and a sane piece count.
        let mut counts = [0u32; 12];
        for (piece, square) in board.into_iter() {
            let kind = usize::from(piece & 7);
            let colour = usize::from(piece >> 3);
            counts[6 * colour + kind] += 1;
            if kind == 5 {
                kings_seen_by_bullet += 1;
                if colour == 0 {
                    assert_eq!(
                        board.our_ksq(),
                        square,
                        "stm king square must match occupancy"
                    );
                } else {
                    assert_eq!(
                        board.opp_ksq(),
                        square ^ 56,
                        "nstm king square must match occupancy"
                    );
                }
            } else if kind == 0 {
                assert!(
                    !matches!(square / 8, 0 | 7),
                    "no pawn may sit on the 1st or 8th rank"
                );
            }
        }
        assert_eq!(counts[5], 1, "exactly one stm king");
        assert_eq!(counts[11], 1, "exactly one nstm king");
        let total: u32 = counts.iter().sum();
        assert!(total > 2, "board must hold more than the two kings");
        assert!(total <= 32, "board must hold at most 32 pieces");
        assert!(board.result <= 2, "result must be a valid WDL label");
        assert!(
            board.score().abs() as i32 <= mf_datagen::DEFAULT_SCORE_BOUND,
            "every score must respect the filter bound"
        );
    });

    assert_eq!(loaded, stats.positions, "bullet reads every record");
    assert_eq!(kings_seen_by_bullet, 2 * stats.positions);

    let _ = std::fs::remove_file(&path);
}

/// Real `lichess_db_eval.jsonl` lines, trimmed to two principal variations each.
///
/// The second is a mate announcement, the third is black to move with a negative
/// white-relative score, so the file exercises the mate saturation and the sign flip
/// as well as the plain path.
const SOURCE_LINES: &str = concat!(
    r#"{"fen":"7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -","evals":[{"pvs":[{"cp":69,"line":"f7g7 e6e2 h8d8 e2d2"},{"cp":163,"line":"h8d8 d1e1 a6a5 a2a3"}],"knodes":4189972,"depth":46}]}"#,
    "\n",
    r#"{"fen":"6k1/6p1/8/4K3/4NN2/8/8/8 w - -","evals":[{"pvs":[{"mate":15,"line":"e5e6 g8f8 e4d6"}],"knodes":589893,"depth":95},{"pvs":[{"mate":20,"line":"e4g5 g8f8 f4g6"}],"knodes":74318,"depth":34}]}"#,
    "\n",
    r#"{"fen":"1k4r1/1bp5/p2p2pp/Nq2r3/1P5P/2Q5/5PP1/3R1RK1 b - -","evals":[{"pvs":[{"cp":-104,"line":"g6g5 d1c1 g8g7"}],"knodes":198324,"depth":33}]}"#,
    "\n",
    r#"{"fen":"r1b2rk1/1p2bppp/p1nppn2/q7/2P1P3/N1N5/PP2BPPP/R1BQ1RK1 w - -","evals":[{"pvs":[{"cp":21,"line":"c1e3 f8d8 d1c2"}],"knodes":1000,"depth":40}]}"#,
    "\n",
);

#[test]
fn a_converted_lichess_file_loads_through_bulletformats_data_loader_unchanged() {
    let mut path = std::env::temp_dir();
    path.push(format!("mf-datagen-jsonl-{}.bullet", std::process::id()));

    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).expect("temp file"));
    let stats = mf_datagen::convert(
        SOURCE_LINES.as_bytes(),
        mf_datagen::ConvertConfig::default(),
        |bytes| {
            file.write_all(bytes).map_err(|error| error.to_string())?;
            Ok(())
        },
        |_| Ok(()),
    )
    .expect("conversion succeeds");
    file.flush().expect("flush");
    drop(file);

    assert_eq!(stats.positions, 4, "every source line converts");
    assert_eq!(stats.mate_converted, 1);

    // The record count is re-derived from the byte length, never trusted from a counter.
    let length = std::fs::metadata(&path).expect("file exists").len();
    assert_eq!(length % RECORD_BYTES as u64, 0);
    assert_eq!(length / RECORD_BYTES as u64, stats.positions);

    let loader = DataLoader::<ChessBoard>::new(&path, 4).expect("bullet opens the file");
    assert_eq!(loader.len() as u64, stats.positions);

    let mut loaded = 0u64;
    let mut scores = Vec::new();
    loader.map_positions(|board| {
        loaded += 1;
        scores.push(board.score());
        assert_eq!(
            board.result_idx(),
            Outcome::Draw as usize,
            "the source carries no game result, so every record is the neutral placeholder"
        );
        assert!(
            board.score().abs() as i32 <= mf_datagen::DEFAULT_SCORE_BOUND,
            "a saturated mate must still respect the score bound"
        );
    });
    assert_eq!(
        loaded, stats.positions,
        "bullet reads every converted record"
    );

    // Black to move with white-relative +69 must land at -69, and a white mate at the
    // saturation value. Both sign conventions are load-bearing and neither is inferable
    // from the file alone, so they are pinned through bullet's own reader.
    assert!(scores.contains(&-69), "{scores:?}");
    assert!(scores.contains(&10_000), "{scores:?}");
    assert!(
        scores.contains(&104),
        "black to move with white-relative -104 must read as +104\n{scores:?}"
    );

    let _ = std::fs::remove_file(&path);
}
