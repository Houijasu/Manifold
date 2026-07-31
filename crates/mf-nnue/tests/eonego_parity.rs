use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use mf_core::{Color, Position};
use mf_nnue::{AccumulatorStack, ForwardMode, L1, Network, SimdBackend};

const RECORDS: usize = 10_000;
const RECORD_BYTES: usize = 2 + 4 + 4 + L1;
const DUMP_BYTES: usize = RECORDS * RECORD_BYTES;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn read_fens() -> Vec<String> {
    let path = fixture_path("eonego_refset_10k.fen");
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture {}: {error}", path.display()));
    contents.lines().map(str::to_owned).collect()
}

fn read_dump() -> Vec<u8> {
    let path = fixture_path("eonego_refdump_10k.bin");
    fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture {}: {error}", path.display()))
}

fn resolve_network_path(explicit_path: Option<OsString>) -> (PathBuf, bool) {
    let is_explicit = explicit_path.is_some();
    let path = explicit_path.map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    (path, is_explicit)
}

fn load_network(path: &Path, is_explicit: bool, gate: &str) -> Option<Network> {
    if !path.is_file() {
        assert!(
            !is_explicit,
            "MF_NNUE_TEST_NET requires an existing network file: {}",
            path.display()
        );
        eprintln!("SKIPPED: {gate} is missing {}", path.display());
        return None;
    }
    Some(
        Network::load(path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}

fn explicit_network(gate: &str) -> Option<Network> {
    let path = std::env::var_os("MF_NNUE_TEST_NET").map(PathBuf::from)?;
    load_network(&path, true, gate)
}

fn default_network(gate: &str) -> Option<Network> {
    let (path, is_explicit) = resolve_network_path(None);
    load_network(&path, is_explicit, gate)
}

fn local_network(gate: &str) -> Option<Network> {
    explicit_network(gate).or_else(|| default_network(gate))
}

fn chess960_fen(fen: &str) -> bool {
    fen.split_whitespace().nth(2).is_some_and(|rights| {
        rights
            .bytes()
            .any(|symbol| matches!(symbol, b'A'..=b'H' | b'a'..=b'h'))
    })
}

fn parse_position(fen: &str) -> Position {
    Position::from_fen(fen, chess960_fen(fen))
        .unwrap_or_else(|error| panic!("fixture FEN should parse: {fen}: {error}"))
}

fn stm_byte(color: Color) -> u8 {
    match color {
        Color::White => 0,
        Color::Black => 1,
    }
}

fn read_i32(record: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(
        record[offset..offset + 4]
            .try_into()
            .expect("record field has four bytes"),
    )
}

#[test]
fn eonego_ref_fixture_has_expected_schema() {
    let fens = read_fens();
    assert_eq!(fens.len(), RECORDS, "fixture FEN line count");
    assert!(
        fens.iter().all(|fen| !fen.trim().is_empty()),
        "fixture must not contain blank FEN lines"
    );

    let dump = read_dump();
    assert_eq!(dump.len(), DUMP_BYTES, "fixture dump byte size");

    for (index, (fen, record)) in fens.iter().zip(dump.chunks_exact(RECORD_BYTES)).enumerate() {
        let position = parse_position(fen);
        let bucket = record[0];
        let stm = record[1];
        assert!(
            bucket < 8,
            "record {index} has out-of-range bucket {bucket}: {fen}"
        );
        assert!(stm < 2, "record {index} has out-of-range stm {stm}: {fen}");
        assert_eq!(
            stm,
            stm_byte(position.side_to_move()),
            "record {index} stm does not match FEN: {fen}"
        );
    }
}

#[test]
fn network_path_resolution_marks_only_environment_paths_as_explicit() {
    let explicit = PathBuf::from("explicit-test.nnue");
    assert_eq!(
        resolve_network_path(Some(explicit.clone().into_os_string())),
        (explicit, true)
    );

    let (default, is_explicit) = resolve_network_path(None);
    assert_eq!(
        default,
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue")
    );
    assert!(!is_explicit);
}

#[test]
#[should_panic(expected = "failed to load NNUE network")]
fn existing_invalid_network_is_rejected_by_loader() {
    let path = fixture_path("README.md");
    let _ = load_network(&path, true, "invalid-network helper test");
}

#[test]
#[ignore = "large local 10k-position parity gate"]
fn manifold_matches_eonego_on_10k_reference_positions() {
    let fens = read_fens();
    assert_eq!(fens.len(), RECORDS, "fixture FEN line count");
    let dump = read_dump();
    assert_eq!(dump.len(), DUMP_BYTES, "fixture dump byte size");

    let Some(network) = local_network("10k Eonego parity gate") else {
        return;
    };

    let mut transformed = [0_u8; L1];
    for backend in [
        SimdBackend::Scalar,
        SimdBackend::Avx2,
        SimdBackend::Avx2Vnni,
    ] {
        if !backend.is_supported() {
            eprintln!("10k Eonego parity gate: skipping unsupported {backend:?}");
            continue;
        }
        let sparse_choices: &[bool] = if backend == SimdBackend::Scalar {
            &[false]
        } else {
            &[false, true]
        };
        for &sparse_fc0 in sparse_choices {
            let mode = ForwardMode::new(backend, sparse_fc0).expect("backend is supported");
            let started = Instant::now();
            for (index, (fen, record)) in
                fens.iter().zip(dump.chunks_exact(RECORD_BYTES)).enumerate()
            {
                let position = parse_position(fen);
                let actual = network.dump_features_with_mode(&position, &mut transformed, mode);
                let expected_bucket = usize::from(record[0]);
                let expected_stm = record[1];
                let expected_psqt = read_i32(record, 2);
                let expected_eval = read_i32(record, 6);
                let expected_ft = &record[10..];

                assert_eq!(
                    actual.bucket, expected_bucket,
                    "{mode:?} record {index} bucket mismatch: {fen}"
                );
                assert_eq!(
                    expected_stm,
                    stm_byte(actual.stm),
                    "{mode:?} record {index} stm mismatch: {fen}"
                );
                assert_eq!(
                    actual.psqt_internal, expected_psqt,
                    "{mode:?} record {index} psqtInternal mismatch: {fen}"
                );
                assert_eq!(
                    actual.eval_internal, expected_eval,
                    "{mode:?} record {index} evalInternal mismatch: {fen}"
                );
                if let Some((lane, (&manifold, &eonego))) = transformed
                    .iter()
                    .zip(expected_ft)
                    .enumerate()
                    .find(|(_, (manifold, eonego))| manifold != eonego)
                {
                    panic!(
                        "{mode:?} record {index} FT lane {lane} mismatch: \
                         Manifold={manifold}, Eonego={eonego}: {fen}"
                    );
                }
            }

            eprintln!(
                "10k Eonego parity gate [{mode:?}]: \
                 {RECORDS}/{RECORDS} records byte-exact in {:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
    }
}

#[test]
fn root_accumulator_stack_matches_eonego_on_10k_reference_positions() {
    let explicit_network = explicit_network("10k Eonego root-stack parity gate");

    if cfg!(debug_assertions) {
        eprintln!("SKIPPED: 10k Eonego root-stack parity gate runs only in release builds");
        return;
    }

    let fens = read_fens();
    assert_eq!(fens.len(), RECORDS, "fixture FEN line count");
    let dump = read_dump();
    assert_eq!(dump.len(), DUMP_BYTES, "fixture dump byte size");

    let network = match explicit_network {
        Some(network) => network,
        None => {
            let Some(network) = default_network("10k Eonego root-stack parity gate") else {
                return;
            };
            network
        }
    };

    let started = Instant::now();
    let mut transformed = [0_u8; L1];
    for (index, (fen, record)) in fens.iter().zip(dump.chunks_exact(RECORD_BYTES)).enumerate() {
        let position = parse_position(fen);
        let stack = AccumulatorStack::new(&network, &position);
        let actual = stack.dump_features(&position, &mut transformed);
        let expected_bucket = usize::from(record[0]);
        let expected_stm = record[1];
        let expected_psqt = read_i32(record, 2);
        let expected_eval = read_i32(record, 6);
        let expected_ft = &record[10..];

        assert_eq!(
            actual.bucket, expected_bucket,
            "record {index} bucket mismatch: {fen}"
        );
        assert_eq!(
            expected_stm,
            stm_byte(actual.stm),
            "record {index} stm mismatch: {fen}"
        );
        assert_eq!(
            actual.psqt_internal, expected_psqt,
            "record {index} psqtInternal mismatch: {fen}"
        );
        assert_eq!(
            actual.eval_internal, expected_eval,
            "record {index} evalInternal mismatch: {fen}"
        );
        if let Some((lane, (&manifold, &eonego))) = transformed
            .iter()
            .zip(expected_ft)
            .enumerate()
            .find(|(_, (manifold, eonego))| manifold != eonego)
        {
            panic!(
                "record {index} FT lane {lane} mismatch: \
                 Manifold={manifold}, Eonego={eonego}: {fen}"
            );
        }
    }

    eprintln!(
        "10k Eonego root-stack parity gate: {RECORDS}/{RECORDS} records byte-exact in {:.3}s",
        started.elapsed().as_secs_f64()
    );
}
