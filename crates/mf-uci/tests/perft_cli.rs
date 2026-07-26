use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_manifold"))
        .args(args)
        .output()
        .expect("manifold binary should start")
}

#[test]
fn perft_subcommand_uses_startpos_by_default() {
    let output = run(&["perft", "3"]);
    assert!(output.status.success());
    assert!(
        std::str::from_utf8(&output.stdout)
            .unwrap()
            .lines()
            .any(|line| line == "Nodes searched: 8902")
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn perft_subcommand_accepts_fen_and_chess960() {
    let output = run(&[
        "perft",
        "1",
        "--chess960",
        "--fen",
        "rk6/8/8/8/8/8/8/RK6 w Aa - 0 1",
    ]);
    assert!(output.status.success());

    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(stdout.lines().any(|line| line == "b1a1: 1"));
    assert!(stdout.lines().any(|line| line == "Nodes searched: 11"));
    assert!(output.stderr.is_empty());
}

#[test]
fn perft_subcommand_rejects_bad_arguments_helpfully() {
    let output = run(&["perft", "nope"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    assert!(stderr.contains("invalid perft depth 'nope'"));
    assert!(stderr.contains("Usage: manifold perft <depth>"));
}
