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

/// Runs an analysis session: `go infinite`, wait, `stop`, then expect a `bestmove`.
fn analysis_session(setup: &[&str], think: Duration) -> (Option<String>, bool) {
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
        writeln!(stdin, "position fen {FEN}").expect("engine should accept position");
        writeln!(stdin, "go infinite").expect("engine should accept go");
        stdin.flush().expect("flush");
    }

    thread::sleep(think);

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "stop").expect("engine should accept stop");
        stdin.flush().expect("flush");
    }

    // A GUI waits for `bestmove` after `stop`; give it a generous but finite window.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut bestmove = None;
    while std::time::Instant::now() < deadline {
        match lines_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("bestmove ") {
                    bestmove = rest.split_whitespace().next().map(str::to_string);
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

    (bestmove, exited)
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
