use std::process::{Command, Output};

const BENCH_NODES: &str = "Nodes searched: 10746536";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_manifold"))
        .args(args)
        .output()
        .expect("manifold binary should start")
}

fn metric<'a>(output: &'a str, prefix: &str) -> &'a str {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .expect("benchmark metric should be present")
}

#[test]
fn bench_reports_deterministic_nodes_time_and_nps() {
    let first = run(&["bench"]);
    let second = run(&["bench"]);
    assert!(first.status.success());
    assert!(second.status.success());
    assert!(first.stderr.is_empty());
    assert!(second.stderr.is_empty());

    for output in [&first, &second] {
        let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
        assert!(stdout.lines().any(|line| line == "Positions: 6"));
        assert!(stdout.lines().any(|line| line == BENCH_NODES));
        metric(stdout, "Time (ms): ")
            .parse::<u128>()
            .expect("time should be an integer");
        assert!(
            metric(stdout, "NPS: ")
                .parse::<u64>()
                .expect("NPS should be an integer")
                > 0
        );
    }
}

#[test]
fn bench_rejects_arguments_helpfully() {
    let output = run(&["bench", "extra"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("bench does not accept arguments"));
    assert!(stderr.contains("Usage: manifold bench"));
}
