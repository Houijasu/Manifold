use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use mf_core::{Position, generate_legal_moves};
use mf_nnue::{AccumulatorStack, Network, production_forward_mode};

const DEFAULT_ITERATIONS: usize = 100_000;

fn main() {
    let iterations = std::env::args().nth(1).map_or(DEFAULT_ITERATIONS, |value| {
        value
            .parse::<usize>()
            .ok()
            .filter(|&count| count > 0)
            .expect("iterations must be a positive integer")
    });
    assert!(
        std::env::args().nth(2).is_none(),
        "usage: push_pop_throughput [iterations]"
    );

    let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    let network = Network::load(&path)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", path.display()));
    let parent = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .expect("benchmark FEN should parse");
    let moves = generate_legal_moves(&parent);
    let mut children = Vec::with_capacity(moves.len());
    for &mv in &moves {
        let mut child = parent.clone();
        let undo = child.make_move(mv);
        children.push((child, mv, undo));
    }
    let mut stack = AccumulatorStack::new_production(&network, &parent);

    for index in 0..10_000 {
        let (child, mv, undo) = &children[index % children.len()];
        stack
            .push_real(black_box(child), *mv, black_box(undo))
            .expect("warmup push should fit");
        black_box(stack.current());
        stack.pop().expect("warmup pop should return to root");
    }

    let started = Instant::now();
    let mut checksum = 0_i32;
    for index in 0..iterations {
        let (child, mv, undo) = &children[index % children.len()];
        stack
            .push_real(black_box(child), *mv, black_box(undo))
            .expect("benchmark push should fit");
        checksum = checksum.wrapping_add(black_box(stack.evaluate_internal(child)));
        stack.pop().expect("benchmark pop should return to root");
    }
    let elapsed = started.elapsed();
    println!(
        "backend={:?} iterations={} elapsed={:.6}s pushes_per_second={:.0} checksum={} net={}",
        production_forward_mode().backend(),
        iterations,
        elapsed.as_secs_f64(),
        iterations as f64 / elapsed.as_secs_f64(),
        checksum,
        path.display(),
    );
}
