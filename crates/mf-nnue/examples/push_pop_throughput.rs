#[cfg(not(feature = "instrumentation"))]
fn main() {
    panic!("run with --features instrumentation");
}

#[cfg(feature = "instrumentation")]
fn main() {
    instrumented::run();
}

#[cfg(feature = "instrumentation")]
mod instrumented {
    use std::hint::black_box;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use mf_core::{Position, Undo, generate_legal_moves};
    use mf_nnue::{AccumulatorStack, Network, UpdateProfile, production_forward_mode};

    const DEFAULT_ITERATIONS: usize = 100_000;
    const WARMUP_ITERATIONS: usize = 10_000;
    const BUSY_FEN: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";

    pub(super) fn run() {
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

        println!(
            "backend={:?} iterations={} net={}",
            production_forward_mode().backend(),
            iterations,
            path.display()
        );
        measure_case("quiet", Position::startpos(), &network, iterations);
        measure_case(
            "busy",
            Position::from_fen(BUSY_FEN, false).expect("benchmark FEN should parse"),
            &network,
            iterations,
        );
    }

    fn measure_case(label: &str, parent: Position, network: &Network, iterations: usize) {
        let children = child_positions(&parent);
        let profiles = children
            .iter()
            .map(|(child, mv, undo)| AccumulatorStack::profile_real_update(child, *mv, undo))
            .collect::<Vec<_>>();
        let mut stack = AccumulatorStack::new_production(network, &parent);

        for index in 0..WARMUP_ITERATIONS {
            let (child, mv, undo) = &children[index % children.len()];
            stack
                .push_real(black_box(child), *mv, black_box(undo))
                .expect("warmup push should fit");
            black_box(stack.evaluate_internal(child));
            stack.pop().expect("warmup pop should return to root");
        }

        let push = measure_pushes(&mut stack, &children, iterations);
        let (eval, eval_checksum) = measure_evaluations(network, &parent, &children, iterations);
        let (combined, combined_checksum) = measure_combined(&mut stack, &children, iterations);

        println!(
            "case={label} moves={} push_ns={:.2} eval_ns={:.2} combined_ns={:.2} \
             eval_checksum={eval_checksum} combined_checksum={combined_checksum}",
            children.len(),
            nanos_per_iteration(push, iterations),
            nanos_per_iteration(eval, iterations),
            nanos_per_iteration(combined, iterations),
        );
        println!(
            "case={label} halfka_removed={} halfka_added={} changed_edges={} sliders_scanned={}",
            summarize(&profiles, |profile| profile.halfka_removals),
            summarize(&profiles, |profile| profile.halfka_additions),
            summarize(&profiles, |profile| profile.changed_threat_edges),
            summarize(&profiles, |profile| profile.sliders_scanned),
        );
    }

    fn child_positions(parent: &Position) -> Vec<(Position, mf_core::Move, Undo)> {
        generate_legal_moves(parent)
            .iter()
            .map(|&mv| {
                let mut child = parent.clone();
                let undo = child.make_move(mv);
                (child, mv, undo)
            })
            .collect()
    }

    fn measure_pushes(
        stack: &mut AccumulatorStack<'_>,
        children: &[(Position, mf_core::Move, Undo)],
        iterations: usize,
    ) -> Duration {
        let started = Instant::now();
        for index in 0..iterations {
            let (child, mv, undo) = &children[index % children.len()];
            stack
                .push_real(black_box(child), *mv, black_box(undo))
                .expect("benchmark push should fit");
            black_box(stack.current());
            stack.pop().expect("benchmark pop should return to root");
        }
        started.elapsed()
    }

    fn measure_evaluations(
        network: &Network,
        parent: &Position,
        children: &[(Position, mf_core::Move, Undo)],
        iterations: usize,
    ) -> (Duration, i32) {
        let (child, mv, undo) = &children[0];
        let mut stack = AccumulatorStack::new_production(network, parent);
        stack
            .push_real(child, *mv, undo)
            .expect("evaluation state should fit");

        let started = Instant::now();
        let mut checksum = 0_i32;
        for _ in 0..iterations {
            checksum = checksum.wrapping_add(black_box(stack.evaluate_internal(black_box(child))));
        }
        (started.elapsed(), checksum)
    }

    fn measure_combined(
        stack: &mut AccumulatorStack<'_>,
        children: &[(Position, mf_core::Move, Undo)],
        iterations: usize,
    ) -> (Duration, i32) {
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
        (started.elapsed(), checksum)
    }

    fn nanos_per_iteration(elapsed: Duration, iterations: usize) -> f64 {
        elapsed.as_secs_f64() * 1e9 / iterations as f64
    }

    fn summarize(profiles: &[UpdateProfile], value: impl Fn(&UpdateProfile) -> usize) -> String {
        let mut values = profiles.iter().map(value).collect::<Vec<_>>();
        values.sort_unstable();
        let total = values.iter().sum::<usize>();
        format!(
            "{}/{:.1}/{}/{}",
            values[0],
            total as f64 / values.len() as f64,
            values[values.len() / 2],
            values[values.len() - 1],
        )
    }
}
