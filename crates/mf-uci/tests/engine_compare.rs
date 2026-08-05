//! Side-by-side `go depth 10` comparison against a reference engine.
//!
//! Run with output shown:
//!   cargo test -p mf-uci --release --test engine_compare -- --nocapture
//!
//! The reference engine path comes from `MF_COMPARE_ENGINE`; the test reports only on
//! Manifold when it is unset, so CI stays hermetic.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const FEN: &str = "8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - - 0 1";

#[derive(Debug)]
struct Session {
    elapsed: Duration,
    max_depth: u32,
    bestmove: Option<String>,
    last_info: Option<String>,
    diagnostics: Vec<String>,
}

/// Runs one `go` against one engine, keeping stdin open and draining stdout throughout.
fn run(exe: &str, go: &str, options: &[&str], timeout: Duration) -> Session {
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("could not start '{exe}': {error}"));

    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let stderr = child.stderr.take().expect("stderr piped");
    let (etx, erx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if etx.send(line).is_err() {
                return;
            }
        }
    });

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "uci").unwrap();
        for option in options {
            writeln!(stdin, "{option}").unwrap();
        }
        writeln!(stdin, "isready").unwrap();
        writeln!(stdin, "position fen {FEN}").unwrap();
        stdin.flush().unwrap();
    }

    let started = Instant::now();
    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "{go}").unwrap();
        stdin.flush().unwrap();
    }

    let mut max_depth = 0;
    let mut bestmove = None;
    let mut last_info = None;
    let mut diagnostics = Vec::new();
    let mut elapsed = timeout;
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("info depth ") {
                    if let Some(Ok(depth)) = rest.split_whitespace().next().map(str::parse::<u32>) {
                        max_depth = max_depth.max(depth);
                    }
                    last_info = Some(line.clone());
                }
                if line.contains("info string") {
                    diagnostics.push(line.clone());
                }
                if let Some(rest) = line.strip_prefix("bestmove ") {
                    bestmove = rest.split_whitespace().next().map(str::to_string);
                    elapsed = started.elapsed();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        let _ = writeln!(stdin, "quit");
        let _ = stdin.flush();
    }
    thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    while let Ok(line) = erx.try_recv() {
        diagnostics.push(format!("stderr: {line}"));
    }

    Session {
        elapsed,
        max_depth,
        bestmove,
        last_info,
        diagnostics,
    }
}

fn report(label: &str, session: &Session) {
    println!("======== {label} ========");
    println!("  elapsed  : {:?}", session.elapsed);
    println!("  maxdepth : {}", session.max_depth);
    println!(
        "  bestmove : {}",
        session.bestmove.as_deref().unwrap_or("*** NONE ***")
    );
    println!(
        "  lastinfo : {}",
        session.last_info.as_deref().unwrap_or("(none)")
    );
    for diagnostic in &session.diagnostics {
        println!("  diag     : {diagnostic}");
    }
    println!();
}

fn manifold() -> String {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_manifold"));
    path.set_extension(std::env::consts::EXE_EXTENSION);
    path.display().to_string()
}

#[test]
fn go_depth_10_side_by_side() {
    let timeout = Duration::from_secs(60);
    let mine = run(&manifold(), "go depth 10", &[], timeout);
    report("MANIFOLD  go depth 10", &mine);

    if let Ok(reference) = std::env::var("MF_COMPARE_ENGINE") {
        let theirs = run(&reference, "go depth 10", &[], timeout);
        report(&format!("REFERENCE ({reference})"), &theirs);

        println!("---- comparison ----");
        println!(
            "  depth   : manifold {} vs reference {}",
            mine.max_depth, theirs.max_depth
        );
        println!(
            "  bestmove: manifold {} vs reference {}",
            mine.bestmove.as_deref().unwrap_or("NONE"),
            theirs.bestmove.as_deref().unwrap_or("NONE")
        );
    } else {
        println!("(set MF_COMPARE_ENGINE to also run a reference engine)");
    }

    assert_eq!(
        mine.max_depth, 10,
        "manifold should report exactly depth 10 for `go depth 10`"
    );
    assert!(mine.bestmove.is_some(), "manifold must answer with a move");
}
