use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use mf_core::Position;
use mf_nnue::{AccumulatorState, ForwardMode, Network, SimdBackend};

const DEFAULT_ITERATIONS: usize = 100_000;
const DEFAULT_SAMPLES: usize = 9;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let iterations = arguments.next().map_or(DEFAULT_ITERATIONS, |value| {
        parse_count("iterations", &value)
    });
    let samples = arguments
        .next()
        .map_or(DEFAULT_SAMPLES, |value| parse_count("samples", &value));
    assert!(
        arguments.next().is_none(),
        "usage: forward_throughput [iterations] [samples]"
    );

    let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    let network = Network::load(&path)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", path.display()));
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R b KQkq - 0 1",
        false,
    )
    .expect("benchmark FEN should parse");
    let state = AccumulatorState::from_position(&network, &position);

    println!(
        "supplied-state forward: {iterations} iterations x {samples} samples, {}",
        path.display()
    );
    for mode in supported_modes() {
        for _ in 0..10_000 {
            black_box(network.evaluate_from_state_with_mode(
                black_box(&position),
                black_box(&state),
                mode,
            ));
        }

        let mut timings = Vec::with_capacity(samples);
        let mut checksum = 0_i32;
        for _ in 0..samples {
            let started = Instant::now();
            for _ in 0..iterations {
                checksum = checksum.wrapping_add(black_box(network.evaluate_from_state_with_mode(
                    black_box(&position),
                    black_box(&state),
                    mode,
                )));
            }
            timings.push(started.elapsed().as_secs_f64() * 1e9 / iterations as f64);
        }
        timings.sort_by(f64::total_cmp);
        let median_ns = timings[timings.len() / 2];
        println!(
            "{mode:?}: median {median_ns:.2} ns/eval, {:.0} eval/s, checksum={checksum}",
            1e9 / median_ns
        );
    }
}

fn supported_modes() -> Vec<ForwardMode> {
    let mut modes = vec![ForwardMode::scalar()];
    for backend in [SimdBackend::Avx2, SimdBackend::Avx2Vnni] {
        if backend.is_supported() {
            modes.push(ForwardMode::new(backend, false).expect("backend is supported"));
            modes.push(ForwardMode::new(backend, true).expect("backend is supported"));
        }
    }
    modes
}

fn parse_count(name: &str, value: &str) -> usize {
    value
        .parse::<usize>()
        .ok()
        .filter(|&count| count > 0)
        .unwrap_or_else(|| panic!("{name} must be a positive integer"))
}
