//! Does a time-limited `go` actually use its budget?
//!
//! An analysis GUI sends `go movetime`/`go wtime`, not `go infinite`. If those return
//! instantly at depth 1 the engine is unusable for analysis even though every position
//! parses correctly.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use mf_core::{Position, format_uci_move, generate_legal_moves};

const FEN: &str = "8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - - 0 1";

fn engine_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_manifold"));
    path.set_extension(std::env::consts::EXE_EXTENSION);
    path
}

struct TimedGo {
    wall_elapsed: Duration,
    reported_elapsed: Duration,
    completed_iterations: u32,
    nodes: u64,
    bestmove: String,
}

fn field<T: std::str::FromStr>(line: &str, name: &str) -> Option<T> {
    let mut tokens = line.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == name {
            return tokens.next()?.parse().ok();
        }
    }
    None
}

/// Sends one `go`, keeping stdin open, and reports protocol-observable search conditions.
fn timed_go(go: &str) -> TimedGo {
    let mut child = Command::new(engine_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("engine should start");

    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let mut stdin = child.stdin.take().expect("stdin piped");
    writeln!(stdin, "uci").unwrap();
    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    let ready_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = ready_deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(remaining)
            .expect("engine should answer isready within the watchdog");
        if line == "readyok" {
            break;
        }
    }

    writeln!(stdin, "position fen {FEN}").unwrap();
    let started = Instant::now();
    writeln!(stdin, "{go}").unwrap();
    stdin.flush().unwrap();

    let mut completed_iterations = 0;
    let mut nodes = 0;
    let mut reported_elapsed = Duration::ZERO;
    let mut bestmove = None;
    let mut wall_elapsed = Duration::ZERO;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if line.starts_with("info depth ") && !line.contains(" currmove ") {
                    completed_iterations += 1;
                }
                nodes = nodes.max(field(&line, "nodes").unwrap_or(0));
                if let Some(time) = field::<u64>(&line, "time") {
                    reported_elapsed = Duration::from_millis(time);
                }
                if let Some(rest) = line.strip_prefix("bestmove ") {
                    bestmove = rest.split_whitespace().next().map(str::to_owned);
                    wall_elapsed = started.elapsed();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // stdin is still open here; dropping it now is what ends the session.
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    TimedGo {
        wall_elapsed,
        reported_elapsed,
        completed_iterations,
        nodes,
        bestmove: bestmove.expect("search should return bestmove within the watchdog"),
    }
}

fn assert_legal_bestmove(bestmove: &str) {
    let position = Position::from_fen(FEN, false).expect("test FEN should parse");
    let legal: Vec<_> = generate_legal_moves(&position)
        .as_slice()
        .iter()
        .map(|&mv| format_uci_move(&position, mv, false))
        .collect();
    assert!(
        legal.iter().any(|mv| mv == bestmove),
        "search returned illegal move {bestmove}; legal={legal:?}"
    );
}

#[test]
fn movetime_spends_a_meaningful_share_of_its_budget_and_returns_a_legal_move() {
    let sample = timed_go("go movetime 4000");
    assert!(sample.completed_iterations > 0);
    assert!(sample.nodes > 0);
    assert_legal_bestmove(&sample.bestmove);
    assert!(sample.reported_elapsed >= Duration::from_millis(2990));
    assert!(sample.wall_elapsed < Duration::from_secs(60));
}

#[test]
fn clock_management_spends_a_nontrivial_budget_and_returns_a_legal_move() {
    let sample = timed_go("go wtime 300000 btime 300000 winc 0 binc 0");
    assert!(sample.completed_iterations > 0);
    assert!(sample.nodes > 0);
    assert_legal_bestmove(&sample.bestmove);
    assert!(sample.reported_elapsed >= Duration::from_millis(500));
    assert!(sample.wall_elapsed < Duration::from_secs(60));
}

#[test]
fn fixed_depth_reports_the_requested_completed_iteration() {
    let sample = timed_go("go depth 14");
    assert_eq!(sample.completed_iterations, 14);
    assert!(sample.nodes > 0);
    assert_legal_bestmove(&sample.bestmove);
}
