//! A `go depth N` must actually reach depth N before answering.
//!
//! This exists because a batch-style session (`position` / `go depth 12` / `quit` fed
//! from a file) answers from depth 2-3. That is correct protocol behaviour -- `quit`
//! arrives immediately and legitimately aborts the search -- but it is indistinguishable
//! from a real search-truncation bug when read from a log, and it has already caused one
//! false "the FEN parser is broken" investigation. The harness below keeps stdin open and
//! waits for `bestmove`, which is what a GUI does, so a failure here is the engine's.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// A locked pawn structure with only 8 legal moves. Positions like this reach high depth
/// almost instantly, so a low reported depth cannot be blamed on the search being slow.
const LOCKED: &str = "8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - - 0 1";

struct Engine {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Engine {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("manifold should start");
        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = child.stdout.take().expect("stdout should be piped");
        let (sender, lines) = mpsc::channel();
        // A real draining thread. Without one the engine's stdout pipe fills and the
        // engine blocks mid-search, which looks exactly like a hang.
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").expect("engine should accept commands");
        self.stdin.flush().expect("engine stdin should flush");
    }

    /// Collects output until `bestmove`, returning every line seen.
    fn collect_until_bestmove(&mut self, timeout: Duration) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            match self.lines.recv_timeout(timeout) {
                Ok(line) => {
                    let is_bestmove = line.starts_with("bestmove ");
                    seen.push(line);
                    if is_bestmove {
                        return seen;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("no bestmove within {timeout:?}; saw {seen:#?}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("engine exited before bestmove; saw {seen:#?}")
                }
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

fn deepest_reported(lines: &[String]) -> u32 {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.strip_prefix("info depth ")?;
            rest.split_whitespace().next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
}

fn bestmove_of(lines: &[String]) -> String {
    lines
        .iter()
        .find_map(|line| line.strip_prefix("bestmove "))
        .expect("a bestmove line must be present")
        .split_whitespace()
        .next()
        .expect("bestmove must name a move")
        .to_string()
}

#[test]
fn go_depth_reaches_the_requested_depth_from_a_pasted_fen() {
    let mut engine = Engine::spawn();
    engine.send(&format!("position fen {LOCKED}"));
    engine.send("go depth 12");
    let lines = engine.collect_until_bestmove(Duration::from_secs(60));

    assert_eq!(
        deepest_reported(&lines),
        12,
        "a search left to finish must reach the depth it was asked for"
    );
}

/// The eight legal moves in `LOCKED`, verified against the perft CLI.
#[test]
fn a_pasted_fen_yields_a_move_that_is_legal_in_that_position() {
    const LEGAL: [&str; 8] = [
        "b2c4", "b2d3", "d1b3", "d1c2", "d1e2", "d1f3", "g1f1", "g1h1",
    ];

    let mut engine = Engine::spawn();
    engine.send(&format!("position fen {LOCKED}"));
    engine.send("go depth 10");
    let lines = engine.collect_until_bestmove(Duration::from_secs(60));
    let bestmove = bestmove_of(&lines);

    assert!(
        LEGAL.contains(&bestmove.as_str()),
        "bestmove {bestmove} is not legal in the pasted position; \
         the engine may be searching a stale board"
    );
}

/// Switching positions must not leave the engine on the previous board.
#[test]
fn a_second_position_command_replaces_the_first() {
    let mut engine = Engine::spawn();
    engine.send("position startpos");
    engine.send("go depth 8");
    let first = engine.collect_until_bestmove(Duration::from_secs(60));
    assert!(
        bestmove_of(&first).starts_with(|c: char| c.is_ascii_lowercase()),
        "startpos should produce a normal move"
    );

    engine.send(&format!("position fen {LOCKED}"));
    engine.send("go depth 10");
    let second = engine.collect_until_bestmove(Duration::from_secs(60));
    const LEGAL: [&str; 8] = [
        "b2c4", "b2d3", "d1b3", "d1c2", "d1e2", "d1f3", "g1f1", "g1h1",
    ];
    assert!(
        LEGAL.contains(&bestmove_of(&second).as_str()),
        "after switching positions the engine answered with a move from the old board"
    );
}
