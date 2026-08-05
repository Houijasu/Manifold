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

const FEN: &str = "8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - - 0 1";

fn engine_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_manifold"));
    path.set_extension(std::env::consts::EXE_EXTENSION);
    path
}

/// Sends one `go`, keeping stdin open, and reports (elapsed, max depth reported).
fn timed_go(go: &str) -> (Duration, u32) {
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
    writeln!(stdin, "position fen {FEN}").unwrap();
    let started = Instant::now();
    writeln!(stdin, "{go}").unwrap();
    stdin.flush().unwrap();

    let mut max_depth = 0;
    let mut elapsed = Duration::ZERO;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("info depth ")
                    && let Some(depth) = rest.split_whitespace().next()
                    && let Ok(depth) = depth.parse::<u32>()
                {
                    max_depth = max_depth.max(depth);
                }
                if line.starts_with("bestmove ") {
                    elapsed = started.elapsed();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // stdin is still open here; dropping it now is what ends the session.
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    (elapsed, max_depth)
}

#[test]
fn movetime_spends_close_to_its_budget() {
    let (elapsed, depth) = timed_go("go movetime 4000");
    assert!(
        elapsed >= Duration::from_millis(3000),
        "go movetime 4000 returned after only {elapsed:?} at depth {depth}"
    );
    assert!(depth >= 10, "only reached depth {depth} in {elapsed:?}");
}

#[test]
fn a_clock_based_go_spends_a_sensible_share_of_it() {
    // 300s on the clock should buy seconds of thought, not milliseconds.
    let (elapsed, depth) = timed_go("go wtime 300000 btime 300000 winc 0 binc 0");
    assert!(
        elapsed >= Duration::from_millis(1000),
        "a 300s clock only bought {elapsed:?} at depth {depth}"
    );
    assert!(depth >= 10, "only reached depth {depth} in {elapsed:?}");
}

#[test]
fn a_fixed_depth_go_actually_reaches_that_depth() {
    let (elapsed, depth) = timed_go("go depth 14");
    assert_eq!(
        depth, 14,
        "go depth 14 stopped at depth {depth} ({elapsed:?})"
    );
}
