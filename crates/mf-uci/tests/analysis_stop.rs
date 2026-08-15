//! Drives `go infinite` + `stop` the way an analysis GUI does, with stdout drained on a
//! dedicated thread so a full pipe can never be mistaken for an engine hang.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const FEN: &str = "8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - - 0 1";

fn engine_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_manifold"));
    path.set_extension(std::env::consts::EXE_EXTENSION);
    path
}

/// A king-and-queen mate in one (Qf8#). `go infinite` here finds a mate immediately and,
/// per UCI, must not answer before `stop` -- which is exactly the state the user reported
/// the engine could not be pulled out of. The FEN this constant held had the black king
/// already in check with white to move; this is its legal sibling.
const FORCED_MATE: &str = "7k/5Q2/6K1/8/8/8/8/8 w - - 0 1";

/// The ply ceiling the search must never iterate past. Mirrors `mf_search::MAX_SEARCH_PLY`
/// and the NNUE accumulator stack depth; spelled out here so the protocol surface is
/// asserted independently of the library constant.
const MAX_DEPTH: u32 = 128;

/// The deepest `info depth N` in a batch of engine output.
fn deepest_depth(lines: &[String]) -> u32 {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.strip_prefix("info depth ")?;
            rest.split_whitespace().next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// Runs an analysis session: `go infinite`, wait, `stop`, then expect a `bestmove`.
fn analysis_session(setup: &[&str], think: Duration) -> (Option<String>, bool) {
    let (bestmove, exited, _, _) = analysis_session_on(FEN, setup, think);
    (bestmove, exited)
}

/// The full analysis session: returns the bestmove, whether the engine exited on `quit`,
/// how long `stop` took to answer, and every line seen.
fn analysis_session_on(
    fen: &str,
    setup: &[&str],
    think: Duration,
) -> (Option<String>, bool, Duration, Vec<String>) {
    let mut child = Command::new(engine_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("engine should start");

    let stdout = child.stdout.take().expect("stdout piped");
    let (lines_tx, lines_rx) = mpsc::channel();
    // Drain continuously. `go infinite` emits enough info lines to fill the OS pipe
    // buffer in seconds, and a blocked writer looks exactly like a hung engine.
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if lines_tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        for command in setup {
            writeln!(stdin, "{command}").expect("engine should accept setup");
        }
        writeln!(stdin, "position fen {fen}").expect("engine should accept position");
        writeln!(stdin, "go infinite").expect("engine should accept go");
        stdin.flush().expect("flush");
    }

    thread::sleep(think);

    let mut seen = Vec::new();
    while let Ok(line) = lines_rx.try_recv() {
        seen.push(line);
    }
    assert!(
        !seen.iter().any(|line| line.starts_with("bestmove ")),
        "an infinite search answered before `stop`; GUIs discard that bestmove"
    );

    let stop_sent = std::time::Instant::now();
    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "stop").expect("engine should accept stop");
        stdin.flush().expect("flush");
    }

    // A GUI waits for `bestmove` after `stop`; give it a generous but finite window.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut bestmove = None;
    let mut stop_latency = Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match lines_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("bestmove ") {
                    stop_latency = stop_sent.elapsed();
                    bestmove = rest.split_whitespace().next().map(str::to_string);
                    seen.push(line);
                    break;
                }
                seen.push(line);
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

    // Poll for exit rather than blocking forever, so a genuine hang fails the test.
    let exit_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while std::time::Instant::now() < exit_deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            exited = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        let _ = child.kill();
    }
    let _ = child.wait();

    (bestmove, exited, stop_latency, seen)
}

#[test]
fn stop_after_a_long_infinite_analysis_answers_and_exits() {
    // Long enough that the engine has emitted far more output than a pipe buffer holds.
    let (bestmove, exited) = analysis_session(&["uci", "isready"], Duration::from_secs(6));
    assert!(
        bestmove.is_some(),
        "stop must produce a bestmove; a GUI waits forever without one"
    );
    assert!(exited, "engine must exit on quit");
}

/// The reported defect: `go infinite` on a forced mate iterated to depth 3546 and could
/// not be stopped. Nothing above the ply ceiling can produce a new line, and the info
/// lines those iterations emitted are what buried `stop` under a backlog.
#[test]
fn an_infinite_search_on_a_forced_mate_saturates_at_the_depth_ceiling() {
    let (bestmove, exited, stop_latency, lines) =
        analysis_session_on(FORCED_MATE, &["uci", "isready"], Duration::from_secs(6));

    let deepest = deepest_depth(&lines);
    assert!(
        deepest <= MAX_DEPTH,
        "infinite analysis reported depth {deepest}, past the {MAX_DEPTH}-ply ceiling"
    );
    assert!(
        bestmove.is_some(),
        "stop must produce a bestmove even once the search has saturated"
    );
    assert!(
        stop_latency < Duration::from_millis(500),
        "stop took {stop_latency:?} to answer from the saturated idle state"
    );
    assert!(exited, "engine must exit on quit");
}

/// `stop` during ordinary deep analysis answers promptly. 500 ms is a generous CI bound;
/// the reported defect took longer than a GUI will ever wait.
#[test]
fn stop_answers_within_half_a_second_during_deep_analysis() {
    let (bestmove, exited, stop_latency, _) =
        analysis_session_on(FEN, &["uci", "isready"], Duration::from_secs(5));

    assert!(bestmove.is_some(), "stop must produce a bestmove");
    assert!(
        stop_latency < Duration::from_millis(500),
        "stop took {stop_latency:?} to answer a running analysis"
    );
    assert!(exited, "engine must exit on quit");
}

/// `go depth N` above the ceiling searches to the ceiling instead of chasing N.
///
/// Driven from a position the search exhausts in milliseconds, so the assertion is about
/// the reported depth rather than about how long 128 plies of a real middlegame take.
#[test]
fn a_go_depth_above_the_ceiling_clamps_to_it() {
    let mut child = Command::new(engine_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("engine should start");

    let stdout = child.stdout.take().expect("stdout piped");
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "uci").unwrap();
        writeln!(stdin, "isready").unwrap();
        writeln!(stdin, "position fen {FORCED_MATE}").unwrap();
        writeln!(stdin, "go depth 200").unwrap();
        stdin.flush().unwrap();
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut lines = Vec::new();
    let mut answered = false;
    while std::time::Instant::now() < deadline {
        match lines_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let is_bestmove = line.starts_with("bestmove ");
                lines.push(line);
                if is_bestmove {
                    answered = true;
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
    let _ = child.kill();
    let _ = child.wait();

    assert!(answered, "go depth 200 never produced a bestmove");
    let deepest = deepest_depth(&lines);
    assert!(
        deepest <= MAX_DEPTH,
        "go depth 200 reported depth {deepest}, past the {MAX_DEPTH}-ply ceiling"
    );
}

#[test]
fn stop_answers_with_multiple_threads() {
    let (bestmove, exited) = analysis_session(
        &["uci", "setoption name Threads value 4", "isready"],
        Duration::from_secs(5),
    );
    assert!(
        bestmove.is_some(),
        "stop must produce a bestmove at Threads=4"
    );
    assert!(exited, "engine must exit on quit");
}

#[test]
fn a_second_analysis_in_the_same_session_still_answers() {
    // GUIs reuse one process across many positions; state left behind by the first
    // stopped search is what would break the second.
    let mut child = Command::new(engine_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("engine should start");

    let stdout = child.stdout.take().expect("stdout piped");
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });

    let mut count = 0;
    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "uci").unwrap();
        writeln!(stdin, "isready").unwrap();
        for _ in 0..2 {
            writeln!(stdin, "position fen {FEN}").unwrap();
            writeln!(stdin, "go infinite").unwrap();
            stdin.flush().unwrap();
            thread::sleep(Duration::from_secs(3));
            writeln!(stdin, "stop").unwrap();
            stdin.flush().unwrap();

            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut got = false;
            while std::time::Instant::now() < deadline {
                match lines_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(line) if line.starts_with("bestmove ") => {
                        got = true;
                        break;
                    }
                    Ok(_) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if got {
                count += 1;
            }
        }
        let _ = writeln!(stdin, "quit");
        let _ = stdin.flush();
    }
    let _ = child.wait();

    assert_eq!(count, 2, "both analyses must answer their stop");
}
