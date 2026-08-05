//! `currmove` progress reporting.
//!
//! A GUI renders a line carrying `currmove` as a transient "now searching X (n/m)" status
//! rather than an analysis row. That makes two things load-bearing: the line must carry
//! only the three progress fields, and it must not appear during short searches, where a
//! root move resolves faster than the GUI can draw it.
//!
//! These tests drive the engine over a live pipe with stdin held open, because end-of-file
//! aborts a running search and would truncate it before the reporting threshold.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// The opening position: the most root moves available, so `currmovenumber` has room to
/// climb and every reported move is easy to recognise.
const STARTPOS: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

struct Engine {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Engine {
    fn spawn(threads: usize) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("manifold should start");
        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = child.stdout.take().expect("stdout should be piped");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut engine = Self {
            child,
            stdin,
            lines,
        };
        engine.send(&format!("setoption name Threads value {threads}"));
        engine
    }

    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").expect("engine should accept commands");
        self.stdin.flush().expect("engine stdin should flush");
    }

    fn collect_until_bestmove(&mut self, timeout: Duration) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            match self.lines.recv_timeout(timeout) {
                Ok(line) => {
                    let done = line.starts_with("bestmove ");
                    seen.push(line);
                    if done {
                        return seen;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("no bestmove within {timeout:?}"),
                Err(RecvTimeoutError::Disconnected) => panic!("engine exited before bestmove"),
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

fn currmove_lines(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|line| line.contains(" currmove "))
        .collect()
}

fn token_after<'a>(line: &'a str, name: &str) -> &'a str {
    let tokens: Vec<_> = line.split_whitespace().collect();
    let index = tokens
        .iter()
        .position(|token| *token == name)
        .unwrap_or_else(|| panic!("'{line}' has no '{name}' field"));
    tokens
        .get(index + 1)
        .unwrap_or_else(|| panic!("'{line}' has '{name}' with no value"))
}

#[test]
fn a_long_search_reports_the_root_move_it_is_searching() {
    let mut engine = Engine::spawn(1);
    engine.send(&format!("position fen {STARTPOS}"));
    engine.send("go movetime 8000");
    let lines = engine.collect_until_bestmove(Duration::from_secs(60));
    let progress = currmove_lines(&lines);

    assert!(
        !progress.is_empty(),
        "an 8s search must report currmove progress"
    );
    for line in progress {
        assert_eq!(
            line.split_whitespace().next(),
            Some("info"),
            "'{line}' must be an info line"
        );
        // Only the three progress fields. A score or pv here would land in the GUI's
        // analysis pane as a half-finished evaluation.
        for forbidden in ["score", "pv", "nodes", "seldepth", "multipv"] {
            assert!(
                !line.split_whitespace().any(|token| token == forbidden),
                "'{line}' must not carry '{forbidden}'"
            );
        }
        assert!(
            token_after(line, "depth").parse::<u32>().is_ok(),
            "'{line}' must report a numeric depth"
        );
        let number: usize = token_after(line, "currmovenumber")
            .parse()
            .unwrap_or_else(|_| panic!("'{line}' must report a numeric currmovenumber"));
        assert!(number >= 1, "currmovenumber is 1-based, got {number}");
        let mv = token_after(line, "currmove");
        assert!(
            mv.len() >= 4 && mv.is_char_boundary(2),
            "'{line}' must report a UCI move"
        );
    }
}

/// Short searches report too. The reference engine withholds `currmove` for the first few
/// seconds; we do not, so a GUI driving fast searches still sees progress.
#[test]
fn a_short_search_also_reports_progress() {
    let mut engine = Engine::spawn(1);
    engine.send(&format!("position fen {STARTPOS}"));
    engine.send("go movetime 300");
    let lines = engine.collect_until_bestmove(Duration::from_secs(60));

    assert!(
        !currmove_lines(&lines).is_empty(),
        "a 300ms search must still emit currmove"
    );
}

/// The very first root move of the very first depth is reported: there is no warm-up
/// period during which progress is withheld.
#[test]
fn reporting_starts_at_the_first_move_of_the_first_depth() {
    let mut engine = Engine::spawn(1);
    engine.send(&format!("position fen {STARTPOS}"));
    engine.send("go movetime 2000");
    let lines = engine.collect_until_bestmove(Duration::from_secs(60));
    let progress = currmove_lines(&lines);
    let first = progress.first().expect("search must report currmove");

    assert_eq!(
        token_after(first, "depth"),
        "1",
        "first progress line should come from depth 1, got '{first}'"
    );
    assert_eq!(
        token_after(first, "currmovenumber"),
        "1",
        "first progress line should be move 1, got '{first}'"
    );
}

/// Within one depth, `currmovenumber` counts up, and the only way it may go backwards is
/// by restarting cleanly at 1.
///
/// A depth can legitimately be searched more than once: when an aspiration window fails,
/// the root move list is re-searched from the top, so `... 20` followed by `1 2 3 ...` is
/// correct. What must never happen is a jump *back into the middle* of the count
/// (`... 20, 7, 8 ...`), which is the signature of a second worker interleaving its own
/// pass -- helpers search the same root moves in a different order.
#[test]
fn only_one_worker_reports_so_numbering_stays_consistent_per_depth() {
    let mut engine = Engine::spawn(8);
    engine.send(&format!("position fen {STARTPOS}"));
    engine.send("go movetime 8000");
    let lines = engine.collect_until_bestmove(Duration::from_secs(60));
    let progress = currmove_lines(&lines);
    assert!(
        !progress.is_empty(),
        "an 8s search must report currmove progress"
    );

    let mut current_depth = None;
    let mut previous_number = 0usize;
    for line in progress {
        let depth: u32 = token_after(line, "depth").parse().expect("numeric depth");
        let number: usize = token_after(line, "currmovenumber")
            .parse()
            .expect("numeric currmovenumber");
        if current_depth != Some(depth) {
            current_depth = Some(depth);
            previous_number = 0;
        }
        assert!(
            number > previous_number || number == 1,
            "within depth {depth}, currmovenumber went {previous_number} -> {number}; \
             a re-search must restart at 1, so jumping back into the middle of the count \
             means a second worker is reporting too"
        );
        previous_number = number;
    }
}
