//! Replays the exact option set Fritz applies from `manifold.uci`.
//!
//! Fritz stores `UCI_Chess960=true` and `Threads=24` and sends them on every start, so
//! that combination -- not the default one -- is what the GUI actually runs.

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

struct Engine {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl Engine {
    fn start() -> Self {
        let mut child = Command::new(engine_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("engine should start");
        let stdout = child.stdout.take().expect("stdout piped");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take().expect("stdin piped");
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").expect("engine should accept input");
        self.stdin.flush().expect("flush");
    }

    /// Collects lines until one satisfies `done`, or the timeout expires.
    fn collect_until<F: Fn(&str) -> bool>(&self, timeout: Duration, done: F) -> Vec<String> {
        let deadline = Instant::now() + timeout;
        let mut collected = Vec::new();
        while Instant::now() < deadline {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    let finished = done(&line);
                    collected.push(line);
                    if finished {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        collected
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .child
                .try_wait()
                .expect("process status should be readable")
                .is_some()
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        if !self.wait_for_exit(Duration::from_secs(5)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The options Fritz sends, read straight from the stored `manifold.uci`.
fn fritz_setup(engine: &mut Engine) {
    engine.send("uci");
    engine.collect_until(Duration::from_secs(10), |line| line == "uciok");
    engine.send("setoption name UCI_Chess960 value true");
    engine.send("setoption name Threads value 24");
    engine.send("isready");
    let ready = engine.collect_until(Duration::from_secs(30), |line| line == "readyok");
    assert!(
        ready.iter().any(|line| line == "readyok"),
        "engine must answer isready after Fritz's options, got {ready:?}"
    );
}

#[test]
fn the_fritz_option_set_stops_after_real_analysis_with_one_legal_bestmove() {
    let mut engine = Engine::start();
    fritz_setup(&mut engine);
    engine.send(&format!("position fen {FEN}"));
    engine.send("go infinite");

    let mut output = engine.collect_until(Duration::from_secs(15), |line| {
        line.starts_with("info depth ") && !line.contains(" currmove ")
    });
    assert!(
        output
            .iter()
            .any(|line| line.starts_with("info depth ") && !line.contains(" currmove ")),
        "Fritz's option set must complete at least one real iteration:\n{}",
        output.join("\n")
    );
    assert!(
        !output.iter().any(|line| line.starts_with("bestmove ")),
        "go infinite must not answer before stop:\n{}",
        output.join("\n")
    );

    engine.send("stop");
    engine.send("isready");
    output.extend(engine.collect_until(Duration::from_secs(15), |line| line == "readyok"));

    let bestmoves: Vec<_> = output
        .iter()
        .filter_map(|line| line.strip_prefix("bestmove "))
        .collect();
    assert_eq!(
        bestmoves.len(),
        1,
        "stop must produce exactly one bestmove:\n{}",
        output.join("\n")
    );
    let bestmove = bestmoves[0]
        .split_whitespace()
        .next()
        .expect("bestmove must carry a move");
    let position = Position::from_fen(FEN, true).expect("Fritz FEN should parse");
    let legal_moves = generate_legal_moves(&position);
    let legal: Vec<_> = legal_moves
        .as_slice()
        .iter()
        .map(|&mv| format_uci_move(&position, mv, true))
        .collect();
    assert!(
        legal.iter().any(|mv| mv == bestmove),
        "Fritz session returned illegal move {bestmove}; legal={legal:?}"
    );
    assert!(output.iter().any(|line| line == "readyok"));

    engine.send("quit");
    assert!(
        engine.wait_for_exit(Duration::from_secs(5)),
        "engine must exit within the quit watchdog"
    );
}

#[test]
fn threads_24_answers_isready_promptly_on_repeated_positions() {
    // Fritz reconfigures and re-sends `position` constantly while you click around.
    let mut engine = Engine::start();
    fritz_setup(&mut engine);

    for round in 0..3 {
        engine.send(&format!("position fen {FEN}"));
        engine.send("go movetime 1500");
        let output = engine.collect_until(Duration::from_secs(20), |line| {
            line.starts_with("bestmove ")
        });
        assert!(
            output.iter().any(|line| line.starts_with("bestmove ")),
            "round {round} produced no bestmove:\n{}",
            output.join("\n")
        );
    }
}
