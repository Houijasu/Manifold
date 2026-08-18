use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread;
use std::time::{Duration, Instant};

use mf_core::{Position, format_uci_move, generate_legal_moves};

/// Scales a watchdog deadline to the build, the way `bench_cli`'s session deadline is.
///
/// These deadlines answer "is the engine stuck?", not "was the engine fast?". An
/// unoptimised build is roughly an order of magnitude slower, and under a parallel
/// `cargo test --workspace` it also competes with every other test binary, so a flat
/// one-second handshake watchdog starts reporting the optimiser and the scheduler as
/// engine defects. The tests that assert a real time BUDGET -- the 50 ms clock samples
/// and the `movetime`/`movestogo` comparisons -- deliberately do NOT use this: their
/// bounds are the property under test and must not stretch.
fn watchdog(timeout: Duration) -> Duration {
    if cfg!(debug_assertions) {
        timeout * 10
    } else {
        timeout
    }
}

/// Serialises the one test that asserts a time BUDGET against every other engine in this file.
///
/// `perft.rs` uses a plain mutex for the same purpose, but a mutex held by one test would
/// exclude nothing here: the contention is the other forty-one tests in this binary, which
/// cargo runs in parallel and which spawn engines of their own. So the exclusion runs the other
/// way — an ordinary engine takes a READ guard and shares the machine freely, while
/// `movetime_and_clock_go_forms_honor_bounded_budgets` takes the WRITE guard and gets the
/// machine to itself for the length of its session.
///
/// This is deliberately not a looser bound. The failing arm was the engine genuinely spending
/// 338 ms on a budget it usually meets in ~137 ms, so the search really did overshoot under
/// load, and reading the engine's own reported `time` field instead of a wall clock does not
/// help — that was implemented and measured first, and still failed 2 of 15 full runs with an
/// identical signature (405 ms vs 338 ms). The only honest options were to weaken what the test
/// asserts or to give it the machine; this takes the second.
static ENGINE_LOAD: RwLock<()> = RwLock::new(());

/// Whether an engine shares the machine with its sibling tests or has it to itself.
enum MachineShare {
    #[expect(dead_code, reason = "held for its lifetime, never read")]
    Shared(RwLockReadGuard<'static, ()>),
    #[expect(dead_code, reason = "held for its lifetime, never read")]
    Exclusive(RwLockWriteGuard<'static, ()>),
}

struct InteractiveUci {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    /// Dropped with the session, so the guard spans every search the session runs.
    _machine: MachineShare,
}

impl InteractiveUci {
    fn spawn() -> Self {
        Self::spawn_with(MachineShare::Shared(
            ENGINE_LOAD
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ))
    }

    /// Spawns an engine that has the machine to itself for as long as the session lives.
    fn spawn_exclusive() -> Self {
        Self::spawn_with(MachineShare::Exclusive(
            ENGINE_LOAD
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ))
    }

    fn spawn_with(machine: MachineShare) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("manifold binary should start");
        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = child.stdout.take().expect("stdout should be piped");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            _machine: machine,
        }
    }

    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").expect("command should be written");
        self.stdin.flush().expect("command should be flushed");
    }

    fn receive_until(
        &self,
        timeout: Duration,
        mut predicate: impl FnMut(&str) -> bool,
    ) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(remaining).ok()?;
            if predicate(&line) {
                return Some(line);
            }
        }
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

impl Drop for InteractiveUci {
    fn drop(&mut self) {
        if !self.child.try_wait().is_ok_and(|status| status.is_some()) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn run_uci(commands: &[&str]) -> Output {
    // Shares the machine with the other tests but yields it to the exclusive timing test.
    let _machine = ENGINE_LOAD
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("manifold binary should start");
    let mut stdin = child.stdin.take().expect("stdin should be piped");
    let stdout = child.stdout.take().expect("stdout should be piped");
    let mut stderr = child.stderr.take().expect("stderr should be piped");
    let (sender, receiver) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut output = Vec::new();
        stderr
            .read_to_end(&mut output)
            .expect("stderr should be readable");
        output
    });
    let mut lines = Vec::new();

    for command in commands {
        writeln!(stdin, "{command}").expect("command should be written");
        stdin.flush().expect("command should be flushed");
        let Some(response) = expected_response(command) else {
            continue;
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(line) => {
                    let matched = response.matches(&line);
                    lines.push(line);
                    if matched {
                        break;
                    }
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("timed out waiting for {response:?} after '{command}': {error}");
                }
            }
        }
    }
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("process status should be readable") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break child.wait().expect("killed process should be waitable");
        }
        thread::sleep(Duration::from_millis(5));
    };
    stdout_reader.join().expect("stdout reader should finish");
    lines.extend(receiver.try_iter());
    let stderr = stderr_reader.join().expect("stderr reader should finish");
    let mut stdout = lines.join("\n").into_bytes();
    if !stdout.is_empty() {
        stdout.push(b'\n');
    }
    Output {
        status,
        stdout,
        stderr,
    }
}

#[derive(Clone, Copy, Debug)]
enum ExpectedResponse {
    Exact(&'static str),
    Prefix(&'static str),
}

impl ExpectedResponse {
    fn matches(self, line: &str) -> bool {
        match self {
            Self::Exact(expected) => line == expected,
            Self::Prefix(expected) => line.starts_with(expected),
        }
    }
}

fn expected_response(command: &str) -> Option<ExpectedResponse> {
    let tokens: Vec<_> = command.split_whitespace().collect();
    let keyword = tokens.first()?;
    if tokens.len() == 1 && keyword.eq_ignore_ascii_case("uci") {
        return Some(ExpectedResponse::Exact("uciok"));
    }
    if tokens.len() == 1 && keyword.eq_ignore_ascii_case("isready") {
        return Some(ExpectedResponse::Exact("readyok"));
    }
    if !keyword.eq_ignore_ascii_case("go") {
        return None;
    }
    if tokens.len() == 3
        && tokens[1].eq_ignore_ascii_case("perft")
        && tokens[2].parse::<u32>().is_ok()
    {
        return Some(ExpectedResponse::Prefix("Nodes searched: "));
    }
    tokens
        .get(1)
        .is_some_and(|parameter| {
            // `searchmoves` and `mate` are only here for the bounded forms this
            // helper drives; a bare `go searchmoves ...` is infinite and must use
            // `InteractiveUci` instead.
            [
                "depth",
                "nodes",
                "movetime",
                "wtime",
                "btime",
                "searchmoves",
                "mate",
            ]
            .iter()
            .any(|known| parameter.eq_ignore_ascii_case(known))
        })
        .then_some(ExpectedResponse::Prefix("bestmove "))
}

fn stdout_lines(output: &Output) -> Vec<&str> {
    std::str::from_utf8(&output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .collect()
}

fn perft_rows(output: &Output) -> Vec<(&str, u64)> {
    stdout_lines(output)
        .into_iter()
        .filter_map(|line| {
            let (mv, nodes) = line.split_once(": ")?;
            let valid_move = matches!(mv.len(), 4 | 5)
                && mv.as_bytes()[0].is_ascii_lowercase()
                && mv.as_bytes()[1].is_ascii_digit();
            valid_move.then(|| (mv, nodes.parse::<u64>().unwrap()))
        })
        .collect()
}

/// The move of every `bestmove` line, without the optional `ponder <move>` suggestion.
fn bestmoves(output: &Output) -> Vec<&str> {
    stdout_lines(output)
        .into_iter()
        .filter_map(|line| line.strip_prefix("bestmove "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect()
}

fn search_info_lines(output: &Output) -> Vec<&str> {
    stdout_lines(output)
        .into_iter()
        .filter(|line| line.starts_with("info depth "))
        .collect()
}

fn canonical_search_lines(output: &Output) -> Vec<String> {
    stdout_lines(output)
        .into_iter()
        .filter(|line| line.starts_with("info "))
        .map(|line| {
            let tokens: Vec<_> = line.split_whitespace().collect();
            let mut canonical = Vec::with_capacity(tokens.len());
            let mut index = 0;
            while index < tokens.len() {
                if matches!(tokens[index], "time" | "nps") {
                    index += 2;
                } else {
                    canonical.push(tokens[index]);
                    index += 1;
                }
            }
            canonical.join(" ")
        })
        .collect()
}

/// Pins the keyword order of a search `info` line against the reference engine's.
///
/// UCI lets a GUI parse `info` by keyword rather than position, but GUI parsers are
/// written against the order every major engine emits, and some quietly drop a line whose
/// fields arrive out of sequence. Stockfish emits
/// `depth seldepth multipv score ... nodes nps hashfull tbhits time pv`; this engine
/// emits the same sequence, with `tbhits 0` whenever no tablebases are loaded.
#[test]
fn search_info_fields_follow_the_reference_engine_keyword_order() {
    const EXPECTED: [&str; 9] = [
        "depth", "seldepth", "multipv", "score", "nodes", "nps", "hashfull", "tbhits", "time",
    ];

    let output = run_uci(&["position startpos", "go depth 6", "quit"]);
    assert!(output.status.success());
    let lines = search_info_lines(&output);
    assert!(!lines.is_empty(), "a depth-6 search must report iterations");

    for line in lines {
        let tokens: Vec<_> = line.split_whitespace().collect();
        let order: Vec<_> = EXPECTED
            .iter()
            .filter_map(|field| tokens.iter().position(|token| token == field))
            .collect();
        assert_eq!(
            order.len(),
            EXPECTED.len(),
            "'{line}' is missing one of {EXPECTED:?}"
        );
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "'{line}' does not follow the reference keyword order {EXPECTED:?}"
        );
        let pv = tokens
            .iter()
            .position(|token| *token == "pv")
            .expect("a search line must carry a pv");
        assert!(
            order.iter().all(|index| *index < pv),
            "'{line}' places a field after `pv`, which swallows the rest of the line"
        );
    }
}

fn field(line: &str, name: &str) -> u64 {
    optional_field(line, name)
        .unwrap_or_else(|| panic!("missing or non-numeric field '{name}' in '{line}'"))
}

fn optional_field(line: &str, name: &str) -> Option<u64> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    let index = tokens.iter().position(|token| *token == name)?;
    tokens.get(index + 1)?.parse().ok()
}

fn score_value(line: &str) -> i64 {
    let tokens: Vec<_> = line.split_whitespace().collect();
    let score = tokens
        .iter()
        .position(|token| *token == "score")
        .expect("info line should carry a score");
    let kind = tokens.get(score + 1).expect("score should carry a kind");
    let value = tokens
        .get(score + 2)
        .expect("score should carry a value")
        .parse::<i64>()
        .expect("score value should be numeric");
    match *kind {
        "cp" => value,
        "mate" if value > 0 => 1_000_000 - value,
        "mate" => -1_000_000 - value,
        _ => panic!("unknown score kind '{kind}'"),
    }
}

fn pv_first_move(line: &str) -> &str {
    line.split(" pv ")
        .nth(1)
        .and_then(|pv| pv.split_whitespace().next())
        .expect("info line should carry a PV move")
}

#[test]
fn uci_handshake_is_ordered_and_well_formed() {
    let output = run_uci(&["uci", "quit"]);
    assert!(output.status.success());

    let lines = stdout_lines(&output);
    let name = lines
        .iter()
        .position(|line| *line == "id name Manifold")
        .expect("engine name should be advertised");
    let author = lines
        .iter()
        .position(|line| {
            line.strip_prefix("id author ")
                .is_some_and(|author| !author.trim().is_empty())
        })
        .expect("non-empty author should be advertised");
    let uciok = lines
        .iter()
        .position(|line| *line == "uciok")
        .expect("handshake should terminate");

    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("id name "))
            .count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("id author "))
            .count(),
        1
    );
    assert!(name < uciok);
    assert!(author < uciok);
    assert!(
        lines[..uciok]
            .iter()
            .any(|line| line.starts_with("option name Hash type spin default "))
    );
    assert!(
        lines[..uciok]
            .iter()
            .any(|line| line.starts_with("option name Threads type spin default "))
    );
    assert!(lines[..uciok].contains(&"option name MultiPV type spin default 1 min 1 max 256"));
    assert!(lines[..uciok].contains(&"option name UCI_Chess960 type check default false"));
    assert!(lines[..uciok].contains(&"option name UseNMP type check default true"));
    assert!(lines[..uciok].contains(&"option name UseRFP type check default true"));
    assert!(lines[..uciok].contains(&"option name UseRazoring type check default true"));
    assert!(lines[..uciok].contains(&"option name UseLMR type check default true"));
    assert!(lines[..uciok].contains(&"option name UseLMP type check default true"));
    assert!(lines[..uciok].contains(&"option name UseFutility type check default true"));
    assert!(lines[..uciok].contains(&"option name UseSEEPruning type check default true"));
    assert!(lines[..uciok].contains(&"option name UseCaptureLMR type check default false"));
    assert!(lines[..uciok].contains(&"option name UseSingularExt type check default true"));
    assert!(lines[..uciok].contains(&"option name UseCheckExt type check default true"));
    assert!(lines[..uciok].contains(&"option name UseMultiCut type check default true"));
    assert!(lines[..uciok].contains(&"option name UseIIR type check default true"));
    assert!(lines[..uciok].contains(&"option name UseProbCut type check default true"));
    assert!(
        lines[..uciok]
            .contains(&"option name UseInterpolatedTimeManagement type check default false")
    );
    assert!(lines[..uciok].contains(&"option name UseSearchAgainDepth type check default false"));
    assert!(lines[..uciok].contains(&"option name EvalFile type string default <empty>"));
    // There is no evaluator to switch to, so the engine must not advertise a toggle.
    assert!(
        !lines[..uciok]
            .iter()
            .any(|line| line.starts_with("option name UseNnue"))
    );
    assert_eq!(lines.iter().filter(|line| **line == "uciok").count(), 1);
    assert!(
        !lines[uciok + 1..]
            .iter()
            .any(|line| line.starts_with("id ") || line.starts_with("option "))
    );
}

/// Every tunable parameter is advertised with the range a tuner will sample inside.
///
/// The handshake is the tuner's only source of truth for what a parameter is called and
/// what it may be set to, so the line and the compiled spec must agree exactly. Written
/// against `SEARCH_PARAMETERS` rather than a hard-coded list because a list maintained
/// in two places is a list that eventually disagrees with itself.
#[test]
fn every_tunable_search_parameter_is_advertised_with_its_default_and_range() {
    let output = run_uci(&["uci", "quit"]);
    assert!(output.status.success());

    let lines = stdout_lines(&output);
    let uciok = lines
        .iter()
        .position(|line| *line == "uciok")
        .expect("handshake should terminate");

    for spec in mf_search::SEARCH_PARAMETERS {
        let expected = format!(
            "option name {} type spin default {} min {} max {}",
            spec.name, spec.default, spec.min, spec.max
        );
        assert!(
            lines[..uciok].iter().any(|line| *line == expected),
            "handshake is missing {expected}"
        );
        assert_eq!(
            lines[..uciok]
                .iter()
                .filter(|line| line.starts_with(&format!("option name {} ", spec.name)))
                .count(),
            1,
            "{} is advertised more than once",
            spec.name
        );
    }
}

/// A tunable spin write reaches the search options it names.
///
/// The engine has no way to report a parameter's current value over UCI, so this drives
/// the same `handle_setoption` path a GUI does and reads the state back through the
/// crate's own test surface in `lib.rs`. The end-to-end proof that a changed value
/// reaches the TREE is `bench_cli::changing_a_tunable_parameter_changes_the_bench_signature`.
#[test]
fn a_tunable_spin_write_is_accepted_and_an_unparseable_one_is_reported() {
    let output = run_uci(&[
        "uci",
        "setoption name LmrCoefficient value 2000",
        "setoption name LmrCoefficient value banana",
        "isready",
        "quit",
    ]);
    assert!(output.status.success());

    let lines = stdout_lines(&output);
    assert!(lines.contains(&"readyok"));
    assert!(
        lines.contains(&"info string invalid LmrCoefficient value 'banana'"),
        "an unparseable spin write must be reported, not swallowed"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("info string invalid LmrCoefficient"))
            .count(),
        1,
        "the valid write must not be reported as invalid"
    );
}

#[test]
fn readiness_unknown_commands_and_quit_are_safe() {
    let output = run_uci(&[
        "frobnicate xyzzy",
        "",
        "   ",
        "go banana",
        "setoption name Nonexistent value 7",
        "stop",
        "isready",
        "isready",
        "quit",
    ]);

    assert!(output.status.success());
    let lines = stdout_lines(&output);
    assert_eq!(lines, ["readyok", "readyok"]);
    assert!(output.stderr.is_empty());
}

#[test]
fn malformed_no_argument_commands_are_ignored() {
    let output = run_uci(&[
        "uci trailing",
        "isready trailing",
        "position startpos moves e2e4 e7e5 g1f3",
        "ucinewgame trailing",
        "go perft 1",
        "quit trailing",
        "isready",
        "quit",
    ]);

    assert!(output.status.success());
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"Nodes searched: 29"));
    assert_eq!(lines.iter().filter(|line| **line == "readyok").count(), 1);
    assert!(!lines.contains(&"uciok"));
}

#[test]
fn malformed_position_inputs_do_not_crash_or_poison_loop() {
    let malformed = [
        "position fen this is not a fen",
        "position fen",
        "position fen 8/8/8/8/8/8/8/8 w - - 0 1",
        "position fen rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR",
        "position fen rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR x KQkq - 0 1",
        "position fen 9/8/8/8/8/8/8/RNBQKBNR w KQkq - 0 1",
        "position fen rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq zz 0 1",
        "position fen 4k3/3pP3/4n3/8/8/8/8/4K3 b - e6 0 1",
        "position startpos moves e2e5",
        "position startpos moves zzzz",
    ];

    for command in malformed {
        let output = run_uci(&[
            "uci",
            command,
            "isready",
            "position startpos",
            "go depth 4",
            "isready",
            "quit",
        ]);

        assert!(
            output.status.success(),
            "command should not crash the engine: {command}"
        );
        let lines = stdout_lines(&output);
        assert_eq!(
            lines.iter().filter(|line| **line == "readyok").count(),
            2,
            "engine should remain responsive after: {command}"
        );
        let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8");
        assert!(
            !stderr.contains("panicked at")
                && !stderr.contains("RUST_BACKTRACE")
                && !stderr.contains("index out of bounds"),
            "command emitted a panic: {command}\n{stderr}"
        );
    }
}

#[test]
fn go_perft_emits_machine_parseable_divide_output() {
    let output = run_uci(&[
        "position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "go perft 3",
        "quit",
    ]);
    assert!(output.status.success());

    let rows = perft_rows(&output);
    assert_eq!(rows.len(), 48);
    assert_eq!(rows.iter().map(|(_, nodes)| nodes).sum::<u64>(), 97_862);
    assert!(stdout_lines(&output).contains(&"Nodes searched: 97862"));
}

#[test]
fn go_perft_honors_chess960_castling_encoding() {
    let output = run_uci(&[
        "setoption name UCI_Chess960 value true",
        "position fen rk6/8/8/8/8/8/8/RK6 w Aa - 0 1",
        "go perft 1",
        "quit",
    ]);
    assert!(output.status.success());

    let rows = perft_rows(&output);
    assert_eq!(rows.len(), 11);
    assert!(rows.iter().any(|(mv, _)| *mv == "b1a1"));
    assert!(rows.iter().any(|(mv, _)| *mv == "b1c1"));
    assert!(stdout_lines(&output).contains(&"Nodes searched: 11"));
}

#[test]
fn castling_notation_switches_and_round_trips_in_both_modes() {
    let standard_fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
    let standard = run_uci(&[
        "setoption name UCI_Chess960 value false",
        &format!("position fen {standard_fen}"),
        "go perft 1",
        "quit",
    ]);
    assert!(standard.status.success());
    let standard_moves: Vec<_> = perft_rows(&standard)
        .into_iter()
        .map(|(mv, _)| mv.to_string())
        .collect();
    assert!(standard_moves.contains(&"e1g1".to_string()));
    assert!(standard_moves.contains(&"e1c1".to_string()));
    assert!(!standard_moves.contains(&"e1h1".to_string()));
    assert!(!standard_moves.contains(&"e1a1".to_string()));

    for mv in ["e1g1", "e1c1"] {
        let replay = run_uci(&[
            "setoption name UCI_Chess960 value false",
            &format!("position fen {standard_fen} moves {mv}"),
            "go perft 1",
            "quit",
        ]);
        assert!(replay.status.success());
        assert!(stdout_lines(&replay).contains(&"Nodes searched: 23"));
    }

    let chess960_fen = "r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1";
    let chess960 = run_uci(&[
        "setoption name UCI_Chess960 value true",
        &format!("position fen {chess960_fen}"),
        "go perft 1",
        "quit",
    ]);
    assert!(chess960.status.success());
    let chess960_moves: Vec<_> = perft_rows(&chess960)
        .into_iter()
        .map(|(mv, _)| mv.to_string())
        .collect();
    assert!(chess960_moves.contains(&"e1h1".to_string()));
    assert!(chess960_moves.contains(&"e1a1".to_string()));
    assert!(!chess960_moves.contains(&"e1g1".to_string()));
    assert!(!chess960_moves.contains(&"e1c1".to_string()));

    for mv in ["e1h1", "e1a1"] {
        let replay = run_uci(&[
            "setoption name UCI_Chess960 value true",
            &format!("position fen {chess960_fen} moves {mv}"),
            "go perft 1",
            "quit",
        ]);
        assert!(replay.status.success());
        assert!(stdout_lines(&replay).contains(&"Nodes searched: 23"));
    }
}

#[test]
fn position_startpos_and_fen_move_suffixes_load_exact_positions() {
    let startpos = run_uci(&[
        "position startpos moves e2e4 e7e5 g1f3",
        "go perft 2",
        "quit",
    ]);
    assert!(startpos.status.success());
    assert!(stdout_lines(&startpos).contains(&"Nodes searched: 779"));

    let kiwipete = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    for (mv, expected) in [("e1g1", 2_059), ("e5d7", 2_124)] {
        let output = run_uci(&[
            &format!("position fen {kiwipete} moves {mv}"),
            "go perft 2",
            "quit",
        ]);
        assert!(output.status.success());
        assert!(
            stdout_lines(&output).contains(&format!("Nodes searched: {expected}").as_str()),
            "{mv} should produce the reference perft anchor"
        );
    }
}

#[test]
fn position_fen_accepts_omitted_move_counters() {
    let board = "8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - -";
    for suffix in ["", " 0", " 0 1"] {
        let output = run_uci(&[
            &format!("position fen {board}{suffix}"),
            "go perft 1",
            "quit",
        ]);
        assert!(output.status.success());
        assert!(
            stdout_lines(&output).contains(&"Nodes searched: 8"),
            "FEN with counters '{suffix}' should load the given position"
        );
    }
}

#[test]
fn position_fen_without_move_counters_still_applies_move_suffix() {
    let output = run_uci(&[
        "position fen 8/1p1q1k2/1Pp5/p1Pp4/P2Pp1p1/4PpPp/1N3P1P/3B2K1 w - - moves d1c2",
        "go perft 1",
        "quit",
    ]);
    assert!(output.status.success());
    assert!(stdout_lines(&output).contains(&"Nodes searched: 16"));
}

#[test]
fn gui_move_dialects_are_accepted_rather_than_stranding_the_engine() {
    // Uppercase promotion suffixes and the opposite castling dialect are both common
    // in the wild. Rejecting one used to fail the whole `position` command, leaving
    // the engine on its previous board.
    for (setup, expected) in [
        (
            "position fen 4k3/1P6/8/8/8/8/8/4K3 w - - 0 1 moves b7b8Q",
            3,
        ),
        (
            "position fen r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1 moves e1h1",
            23,
        ),
        ("position startpos moves E2E4", 20),
    ] {
        let output = run_uci(&[setup, "go perft 1", "quit"]);
        assert!(output.status.success());
        assert!(
            stdout_lines(&output).contains(&format!("Nodes searched: {expected}").as_str()),
            "'{setup}' should apply the move, got {:?}",
            stdout_lines(&output)
        );
    }
}

#[test]
fn a_rejected_move_keeps_the_prefix_that_did_parse() {
    // The engine must never silently fall back to an older position: analysing a board
    // the GUI is not showing is what produces a bestmove that is illegal there.
    let output = run_uci(&[
        "position startpos moves e2e4 e7e5 g1f3 not-a-move",
        "go perft 1",
        "quit",
    ]);
    assert!(output.status.success());
    let lines = stdout_lines(&output);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("info string invalid position command:")),
        "the rejected move must still be reported"
    );
    // After 1.e4 e5 2.Nf3 black has 29 legal moves; startpos would report 20.
    assert!(
        lines.contains(&"Nodes searched: 29"),
        "the parsed prefix must be kept, got {lines:?}"
    );
}

#[test]
fn position_reports_a_rejected_fen_instead_of_searching_a_stale_board() {
    let output = run_uci(&["position fen not-a-fen w - -", "go perft 1", "quit"]);
    assert!(output.status.success());
    let lines = stdout_lines(&output);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("info string invalid position command:")),
        "a malformed FEN must report an error, got {lines:?}"
    );
}

#[test]
fn setoption_name_and_boolean_value_are_case_insensitive() {
    let standard_fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
    let output = run_uci(&[
        "SeToPtIoN NaMe uci_chess960 VaLuE TrUe",
        "position fen r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1",
        "go perft 1",
        "SETOPTION NAME UCI_CHESS960 VALUE FALSE",
        &format!("position fen {standard_fen}"),
        "go perft 1",
        "quit",
    ]);
    assert!(output.status.success());

    let rows = perft_rows(&output);
    assert!(rows.iter().any(|(mv, _)| *mv == "e1h1"));
    assert!(rows.iter().any(|(mv, _)| *mv == "e1a1"));
    assert!(rows.iter().any(|(mv, _)| *mv == "e1g1"));
    assert!(rows.iter().any(|(mv, _)| *mv == "e1c1"));
}

/// A SyzygyPath that names no existing directory degrades gracefully: the engine
/// reports the failure as an `info string` and keeps serving commands without
/// tablebases rather than dying on a configuration mistake.
#[test]
fn invalid_syzygy_path_reports_an_error_and_keeps_the_engine_alive() {
    let output = run_uci(&[
        r"setoption name SyzygyPath value C:\NoSuchDir",
        "isready",
        "position startpos",
        "go depth 2",
        "quit",
    ]);

    assert!(output.status.success());
    let lines = stdout_lines(&output);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("info string unable to load SyzygyPath")),
        "a bad path must be reported: {lines:?}"
    );
    assert!(lines.contains(&"readyok"));
    assert_eq!(bestmoves(&output).len(), 1);
}

/// With real tables on disk, `SyzygyPath` reports how many were discovered.
///
/// Skips silently when `MF_SYZYGY_PATH` is unset, matching the repository's
/// skip-if-absent pattern for large local test data.
#[test]
fn syzygy_path_reports_discovered_tables() {
    let Ok(paths) = std::env::var("MF_SYZYGY_PATH") else {
        return;
    };
    let output = run_uci(&[
        &format!("setoption name SyzygyPath value {paths}"),
        "isready",
        "quit",
    ]);

    assert!(output.status.success());
    let lines = stdout_lines(&output);
    let loaded = lines
        .iter()
        .find(|line| line.starts_with("info string Syzygy tablebases loaded: "))
        .unwrap_or_else(|| panic!("tables under MF_SYZYGY_PATH must be reported: {lines:?}"));
    let wdl_count = loaded
        .strip_prefix("info string Syzygy tablebases loaded: ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|count| count.parse::<u64>().ok())
        .expect("the WDL table count must be numeric");
    assert!(wdl_count > 0, "a real table set must contain WDL tables");
}

/// Every search must announce an NNUE evaluator and the source it came from.
///
/// This is the line that would have made the Fritz strength regression obvious: the
/// engine had quietly fallen back to a hand-crafted evaluation because it could not find
/// `nets/main.nnue` from the GUI's working directory. There is no fallback left, so the
/// assertion is now unconditional.
#[test]
fn every_search_reports_an_nnue_evaluator_and_its_source() {
    let network = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
    if !network.is_file() {
        eprintln!(
            "SKIPPED: evaluator protocol test is missing {}",
            network.display()
        );
        return;
    }
    let eval_file = format!("setoption name EvalFile value {}", network.display());
    let output = run_uci(&[
        "position fen 4k3/8/8/8/8/8/8/3QK3 w - - 0 1",
        // First with whatever automatic resolution found, then with an explicit path.
        "go depth 1",
        &eval_file,
        "go depth 1",
        "quit",
    ]);

    assert!(output.status.success());
    let lines = stdout_lines(&output);
    let diagnostics: Vec<_> = lines
        .iter()
        .filter(|line| line.starts_with("info string evaluation "))
        .collect();
    assert_eq!(diagnostics.len(), 2, "one diagnostic per search");
    assert!(
        diagnostics
            .iter()
            .all(|line| line.starts_with("info string evaluation NNUE from ")),
        "the engine has no non-NNUE evaluator: {diagnostics:?}"
    );
    assert!(
        diagnostics[1].starts_with("info string evaluation NNUE from explicit path "),
        "an explicit EvalFile must be reported as such: {}",
        diagnostics[1]
    );
    assert_eq!(bestmoves(&output).len(), 2);
}

/// An oversize request is not covered here on purpose: it now genuinely allocates the
/// advertised maximum, which is gigabytes, and this suite runs its cases in parallel.
/// The clamp is pinned by `an_oversize_hash_request_clamps_to_the_maximum` in the
/// library tests, which exercises the same function at a size a test can afford.
#[test]
fn hash_option_resizes_case_insensitively_and_rejects_invalid_values_without_crashing() {
    let output = run_uci(&[
        "SeToPtIoN NaMe hAsH VaLuE 3",
        "setoption name Hash value 0",
        "setoption name Hash value -5",
        "setoption name Hash value banana",
        "isready",
        "position startpos",
        "go depth 2",
        "quit",
    ]);
    assert!(output.status.success());

    let lines = stdout_lines(&output);
    assert!(lines.contains(&"info string hash resized to 3 MB"));
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("info string invalid Hash value"))
    );
    assert!(lines.contains(&"readyok"));
    assert_eq!(bestmoves(&output).len(), 1);
    assert!(output.stderr.is_empty());
}

/// The advertised maximum has to be a size the engine will actually accept.
///
/// The engine used to advertise `max 1048576` and refuse everything past 4096, so a GUI
/// that offered the advertised range produced a diagnostic and kept the old table. This
/// reads the number out of the handshake and hands it straight back, which is the only
/// check that catches the two drifting apart again.
#[test]
fn the_advertised_hash_maximum_is_accepted_rather_than_refused() {
    let handshake = run_uci(&["uci", "quit"]);
    let advertised = stdout_lines(&handshake)
        .into_iter()
        .find_map(|line| {
            line.strip_prefix("option name Hash type spin")
                .and_then(|rest| rest.split(" max ").nth(1))
                .and_then(|maximum| maximum.trim().parse::<u64>().ok())
        })
        .expect("the handshake should advertise a Hash maximum");

    let output = run_uci(&[
        &format!("setoption name Hash value {advertised}"),
        "isready",
        "quit",
    ]);
    assert!(output.status.success());

    let lines = stdout_lines(&output);
    assert!(
        lines.contains(&format!("info string hash resized to {advertised} MB").as_str()),
        "the advertised maximum must resize, not fail: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("info string unable to allocate Hash")),
        "the engine must never refuse the size it advertises: {lines:?}"
    );
    assert!(lines.contains(&"readyok"));
}

#[allow(non_snake_case)]
mod Threads {
    use super::*;

    #[test]
    fn four_worker_pool_stops_and_quits_cleanly() {
        let mut engine = InteractiveUci::spawn();
        engine.send("setoption name Threads value 4");
        assert_eq!(
            engine.receive_until(watchdog(Duration::from_secs(2)), |line| {
                line == "info string threads set to 4"
            }),
            Some("info string threads set to 4".to_string())
        );
        engine.send("position startpos");
        engine.send("go infinite");
        assert!(
            engine
                .receive_until(watchdog(Duration::from_secs(2)), |line| {
                    line.starts_with("info depth 2 ")
                })
                .is_some(),
            "four-worker search should complete real iterations"
        );

        engine.send("stop");
        let bestmove = engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("bestmove ")
            })
            .expect("stop should return a bestmove");
        assert_ne!(bestmove, "bestmove 0000");

        engine.send("quit");
        assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
    }

    #[test]
    fn invalid_resize_preserves_the_existing_pool() {
        let output = run_uci(&[
            "setoption name Threads value 4",
            "setoption name Threads value banana",
            "position startpos",
            "go depth 3",
            "quit",
        ]);

        assert!(output.status.success());
        let lines = stdout_lines(&output);
        assert!(lines.contains(&"info string threads set to 4"));
        assert!(lines.contains(&"info string invalid Threads value 'banana'"));
        assert_eq!(search_info_lines(&output).len(), 3);
        assert_eq!(bestmoves(&output).len(), 1);
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn one_worker_emits_exactly_one_line_per_completed_depth() {
        let output = run_uci(&[
            "setoption name Threads value 1",
            "position startpos",
            "go depth 6",
            "quit",
        ]);

        assert!(output.status.success());
        let infos = search_info_lines(&output);
        assert_eq!(infos.len(), 6);
        assert_eq!(
            infos
                .iter()
                .map(|line| field(line, "depth"))
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert_eq!(bestmoves(&output).len(), 1);
    }

    #[test]
    fn four_worker_multipv_emits_only_one_ordered_set_per_depth() {
        let output = run_uci(&[
            "setoption name Threads value 4",
            "setoption name MultiPV value 3",
            "position startpos",
            "go nodes 10000",
            "quit",
        ]);

        assert!(output.status.success());
        let infos: Vec<_> = search_info_lines(&output)
            .into_iter()
            .filter(|line| line.contains(" multipv "))
            .collect();
        let deepest = infos
            .iter()
            .map(|line| field(line, "depth"))
            .max()
            .expect("bounded SMP search should report progress");
        let mut complete_depths = 0;
        for depth in 1..=deepest {
            let lines: Vec<_> = infos
                .iter()
                .copied()
                .filter(|line| field(line, "depth") == depth)
                .collect();
            assert!(!lines.is_empty(), "completed depths must not be skipped");
            assert!(lines.len() <= 3, "helpers duplicated depth {depth} output");
            assert_eq!(
                lines
                    .iter()
                    .map(|line| field(line, "multipv"))
                    .collect::<Vec<_>>(),
                (1..=lines.len() as u64).collect::<Vec<_>>(),
                "depth {depth} must contain one worker-0 line per index"
            );
            if lines.len() == 3 {
                complete_depths += 1;
            }
            assert!(
                lines
                    .windows(2)
                    .all(|pair| score_value(pair[0]) >= score_value(pair[1]))
            );
            let first_moves: Vec<_> = lines.iter().map(|line| pv_first_move(line)).collect();
            assert!(
                first_moves
                    .iter()
                    .enumerate()
                    .all(|(index, mv)| !first_moves[..index].contains(mv))
            );
        }
        assert!(
            complete_depths > 0,
            "the node budget must complete at least one full MultiPV set"
        );

        let final_line_one = infos
            .iter()
            .copied()
            .find(|line| field(line, "depth") == deepest && field(line, "multipv") == 1)
            .expect("deepest reported iteration should include line one");
        assert_eq!(bestmoves(&output), [pv_first_move(final_line_one)]);
    }

    #[test]
    fn four_worker_fixed_depth_is_deterministic_across_fresh_processes() {
        let commands = [
            "setoption name Threads value 4",
            "position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "go depth 5",
            "quit",
        ];
        let first = run_uci(&commands);
        let second = run_uci(&commands);

        assert!(first.status.success());
        assert!(second.status.success());
        assert_eq!(
            canonical_search_lines(&first),
            canonical_search_lines(&second)
        );
        assert_eq!(bestmoves(&first), bestmoves(&second));
        assert_eq!(search_info_lines(&first).len(), 5);
    }

    /// Fixed-depth search output must not depend on `Threads` when the helpers never
    /// leave their park.
    ///
    /// The kiwipete depth-10 case is the one that matters. Every hash-keyed shared
    /// table must size its bucket count INDEPENDENTLY of the thread count: when the
    /// mask differs between 1 and 8 threads, a corrhist collision that happens at the
    /// small size and not at the large one changes the residual applied to a static
    /// eval, which changes the tree. That is deterministic — table sizing, not a race
    /// — and it made this position diverge on the M2 baseline binary while the shallow
    /// startpos case still passed. A shallow anchor alone cannot see the defect.
    #[test]
    fn fixed_depth_output_is_identical_at_every_thread_count() {
        // The `info string threads set to N` acknowledgement necessarily differs;
        // everything the search itself reports must not.
        let run = |threads: usize, position: &str, depth: usize| {
            let owned = [
                format!("setoption name Threads value {threads}"),
                format!("position {position}"),
                format!("go depth {depth}"),
                "quit".to_string(),
            ];
            let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
            let output = run_uci(&borrowed);
            assert!(output.status.success());
            let lines = canonical_search_lines(&output)
                .into_iter()
                .filter(|line| !line.starts_with("info string "))
                .collect::<Vec<_>>();
            let bestmove = bestmoves(&output)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            (lines, bestmove)
        };

        let cases = [
            ("startpos moves e2e4 e7e5 g1f3", 8usize),
            (
                "fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
                10usize,
            ),
        ];

        for (position, depth) in cases {
            let (expected_lines, expected_bestmove) = run(1, position, depth);
            assert_eq!(expected_lines.len(), depth);

            for threads in [2, 8] {
                let (lines, bestmove) = run(threads, position, depth);
                assert_eq!(
                    lines, expected_lines,
                    "go depth {depth} output on `{position}` must not depend on Threads \
                     (helpers must stay parked and every shared table must be sized \
                     independently of the thread count)"
                );
                assert_eq!(bestmove, expected_bestmove);
            }
        }
    }

    #[test]
    fn immediate_stop_keeps_publishing_until_pool_dispatch_finishes() {
        let mut engine = InteractiveUci::spawn();
        engine.send("setoption name Threads value 4");
        assert!(
            engine
                .receive_until(watchdog(Duration::from_secs(2)), |line| {
                    line == "info string threads set to 4"
                })
                .is_some()
        );

        for _ in 0..20 {
            engine.send("position startpos");
            engine.send("go movetime 3000");
            engine.send("stop");
            assert!(
                engine
                    .receive_until(watchdog(Duration::from_secs(2)), |line| {
                        line.starts_with("bestmove ")
                    })
                    .is_some(),
                "immediate stop must not lose the stop signal during pool dispatch"
            );
        }

        engine.send("quit");
        assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
    }

    #[test]
    fn immediate_quit_does_not_hang_during_pool_dispatch() {
        for _ in 0..10 {
            let mut engine = InteractiveUci::spawn();
            engine.send("setoption name Threads value 4");
            assert!(
                engine
                    .receive_until(watchdog(Duration::from_secs(2)), |line| {
                        line == "info string threads set to 4"
                    })
                    .is_some()
            );
            engine.send("position startpos");
            engine.send("go infinite");
            engine.send("quit");
            assert!(
                engine.wait_for_exit(watchdog(Duration::from_secs(2))),
                "immediate quit must not lose the stop signal during pool dispatch"
            );
        }
    }
}

#[test]
fn ucinewgame_resets_position_but_preserves_options() {
    let output = run_uci(&[
        "setoption name UCI_Chess960 value TRUE",
        "position startpos moves e2e4 e7e5 g1f3",
        "ucinewgame",
        "go perft 1",
        "position fen r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1",
        "go perft 1",
        "quit",
    ]);
    assert!(output.status.success());

    let lines = stdout_lines(&output);
    assert_eq!(
        lines
            .iter()
            .filter(|line| **line == "Nodes searched: 20")
            .count(),
        1
    );
    assert!(perft_rows(&output).iter().any(|(mv, _)| *mv == "e1h1"));
}

#[test]
fn fixed_depth_and_node_budget_go_forms_emit_deterministic_legal_bestmoves() {
    let commands = [
        "position startpos",
        "go depth 4",
        "ucinewgame",
        "go nodes 1000",
        "quit",
    ];
    let first = run_uci(&commands);
    let second = run_uci(&commands);
    assert!(first.status.success());
    assert!(second.status.success());

    let legal_opening_moves = [
        "a2a3", "a2a4", "b2b3", "b2b4", "c2c3", "c2c4", "d2d3", "d2d4", "e2e3", "e2e4", "f2f3",
        "f2f4", "g2g3", "g2g4", "h2h3", "h2h4", "b1a3", "b1c3", "g1f3", "g1h3",
    ];
    let first_moves = bestmoves(&first);
    assert_eq!(first_moves.len(), 2);
    assert!(
        first_moves
            .iter()
            .all(|mv| legal_opening_moves.contains(mv))
    );
    assert_eq!(first_moves, bestmoves(&second));

    let first_lines = stdout_lines(&first);
    let infos: Vec<_> = first_lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("info depth "))
        .collect();
    assert!(infos.iter().any(|line| field(line, "depth") == 4));
    assert_eq!(
        first_lines
            .iter()
            .filter_map(|line| optional_field(line, "nodes"))
            .next_back(),
        Some(1000)
    );
}

#[test]
fn iterative_search_emits_well_formed_monotone_info_and_legal_pv() {
    let output = run_uci(&["position startpos", "go depth 6", "quit"]);
    assert!(output.status.success());

    let infos = search_info_lines(&output);
    assert_eq!(infos.len(), 6);
    let mut previous_nodes = 0;
    for (index, line) in infos.iter().enumerate() {
        assert_eq!(field(line, "depth"), index as u64 + 1);
        assert!(field(line, "seldepth") >= field(line, "depth"));
        assert!(field(line, "nodes") > previous_nodes);
        previous_nodes = field(line, "nodes");
        for required in ["score", "nodes", "nps", "hashfull", "time", "pv"] {
            assert!(
                line.split_whitespace().any(|token| token == required),
                "missing '{required}' in '{line}'"
            );
        }
        assert!(field(line, "hashfull") <= 1_000);
    }

    let last = infos.last().unwrap();
    let pv = last.split(" pv ").nth(1).expect("PV field should exist");
    let pv_moves: Vec<_> = pv.split_whitespace().collect();
    assert!(pv_moves.len() >= 2);
    assert_eq!(bestmoves(&output), [pv_moves[0]]);

    for prefix_len in 1..=pv_moves.len() {
        let command = format!(
            "position startpos moves {}",
            pv_moves[..prefix_len].join(" ")
        );
        let replay = run_uci(&[&command, "go perft 1", "quit"]);
        assert!(
            replay.status.success(),
            "PV prefix should replay: {command}"
        );
        assert!(
            stdout_lines(&replay)
                .iter()
                .any(|line| { line.starts_with("Nodes searched: ") })
        );
    }
}

#[test]
fn hashfull_is_monotone_and_reported_in_per_mille() {
    let output = run_uci(&[
        "setoption name Hash value 1",
        "ucinewgame",
        "position startpos",
        "go depth 7",
        "quit",
    ]);
    assert!(output.status.success());

    let hashfull: Vec<_> = search_info_lines(&output)
        .iter()
        .map(|line| field(line, "hashfull"))
        .collect();
    assert!(hashfull.len() >= 7);
    assert!(hashfull.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(hashfull.iter().all(|value| *value <= 1_000));
    assert!(
        hashfull.last().copied().unwrap_or(0) > 0,
        "a depth-7 search with Hash=1 should occupy sampled TT entries"
    );
}

#[test]
fn node_limited_search_is_repeatable_at_exact_budget() {
    // No clock tokens here, deliberately: a clock sent alongside a node budget now
    // installs a hard safety deadline (see the node-limited clock test below), so
    // pinning the exact-budget contract requires the pure node-limited form.
    let commands = [
        "setoption name Threads value 1",
        "position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "go nodes 20000",
        "go nodes 20000",
        "quit",
    ];
    let output = run_uci(&commands);

    assert!(output.status.success());
    let moves = bestmoves(&output);
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0], moves[1]);
    let exact_budget_lines: Vec<_> = stdout_lines(&output)
        .into_iter()
        .filter(|line| optional_field(line, "nodes") == Some(20_000))
        .collect();
    assert_eq!(exact_budget_lines.len(), 2);
}

#[test]
fn movetime_and_clock_go_forms_honor_bounded_budgets() {
    // The `movetime` arm is timed over a LIVE session rather than a whole process, and
    // that distinction is load-bearing. `run_uci` measures spawn-to-exit, which now
    // includes loading and quantising the ~106 MiB network at startup. That startup is
    // unrelated to whether the engine honours an 80 ms budget, and under a parallel
    // `cargo test` it dominates: the same search measures ~85 ms spawn-to-exit when run
    // alone and ~640 ms when 47 sibling tests are competing for the machine. Timing the
    // search itself keeps the assertion about time management instead of about process
    // startup under load.
    //
    // It is also the ONE session in this file that takes the machine exclusively. Timing the
    // search rather than the process removed the startup cost but not the load itself: the
    // engine really does overshoot a 137 ms budget to 338 ms when forty siblings are searching
    // at the same time. See `ENGINE_LOAD`.
    let mut engine = InteractiveUci::spawn_exclusive();
    // `isready` is answered only after the network is loaded, so waiting for `readyok`
    // moves that one-off cost outside the timed region.
    engine.send("isready");
    assert!(
        engine
            .receive_until(Duration::from_secs(30), |line| line == "readyok")
            .is_some(),
        "engine should become ready"
    );
    let movetime_elapsed = time_search(&mut engine, "go movetime 80");
    assert!(movetime_elapsed >= Duration::from_millis(40));
    assert!(
        movetime_elapsed <= Duration::from_millis(500),
        "go movetime 80 took {movetime_elapsed:?}"
    );
    engine.send("setoption name UseInterpolatedTimeManagement value true");
    let interpolated_movetime_elapsed = time_search(&mut engine, "go movetime 80");
    assert!(
        interpolated_movetime_elapsed >= Duration::from_millis(40),
        "the interpolated clock governor shortened exact movetime to \
         {interpolated_movetime_elapsed:?}"
    );
    assert!(
        interpolated_movetime_elapsed <= Duration::from_millis(500),
        "go movetime 80 with the interpolated toggle took {interpolated_movetime_elapsed:?}"
    );
    engine.send("setoption name UseInterpolatedTimeManagement value false");
    engine.send("setoption name UseSearchAgainDepth value true");
    let search_again_movetime_elapsed = time_search(&mut engine, "go movetime 80");
    assert!(
        search_again_movetime_elapsed >= Duration::from_millis(40),
        "search-again depth shortened exact movetime to {search_again_movetime_elapsed:?}"
    );
    assert!(
        search_again_movetime_elapsed <= Duration::from_millis(500),
        "go movetime 80 with search-again depth took {search_again_movetime_elapsed:?}"
    );
    engine.send("setoption name UseSearchAgainDepth value false");

    // Same reasoning for the clock arm. The signal here is a ~100 ms budget difference
    // between two `movestogo` values, and spawn-to-exit noise under a parallel run is
    // several times that, so process timing can report the slower budget as the faster
    // one. Both searches are timed inside one already-warm session instead.
    let fast_elapsed = time_search(
        &mut engine,
        "go wtime 1000 btime 1000 winc 80 binc 80 movestogo 40",
    );
    let slow_elapsed = time_search(
        &mut engine,
        "go wtime 1000 btime 1000 winc 80 binc 80 movestogo 2",
    );

    engine.send("quit");
    assert!(engine.wait_for_exit(Duration::from_secs(30)));

    assert!(
        slow_elapsed > fast_elapsed + Duration::from_millis(100),
        "movestogo=2 ({slow_elapsed:?}) must budget more than movestogo=40 ({fast_elapsed:?})"
    );
}

/// Runs one time-limited search and reports how long the ENGINE says it took.
///
/// The duration comes from the `time` field of the last info line rather than a wall clock
/// around the exchange, so the search is not charged for writing `go` into a pipe, the reply
/// travelling back, and a reader thread being scheduled to hand it over. Under load those cost
/// 32-40 ms per arm, which is a third of the margin this test asserts.
///
/// It is NOT what fixed the flake — the exclusive machine guard is. This was tried alone first
/// and still failed 2 of 15 full runs (405 ms vs 338 ms), which is how the overshoot was
/// established as real rather than as measurement error. It is kept because reading the
/// engine's own clock is the right instrument for an assertion about the engine's own clock.
fn time_search(engine: &mut InteractiveUci, go: &str) -> Duration {
    engine.send("position startpos");
    engine.send(go);
    let mut reported = None;
    let bestmove = engine.receive_until(Duration::from_secs(30), |line| {
        if let Some(time) = optional_field(line, "time") {
            reported = Some(time);
        }
        line.starts_with("bestmove ")
    });
    assert!(bestmove.is_some(), "{go} produced no move");
    Duration::from_millis(reported.unwrap_or_else(|| panic!("{go} reported no search time")))
}

/// A clock sent alongside `go depth` is now a safety deadline, not decoration.
///
/// This used to assert the opposite -- that an explicit depth ran to completion no
/// matter how small the clock was -- which is exactly the zero-time-safety defect:
/// a huge node or depth budget on a slow node rate flags the engine. The depth budget
/// stays the primary stop, but a 1 ms clock must cap the run well short of depth 14,
/// which needs orders of magnitude more than 1 ms in every build profile.
#[test]
fn explicit_depth_with_a_tiny_clock_is_capped_by_the_hard_deadline() {
    let output = run_uci(&["position startpos", "go depth 14 wtime 1 btime 1", "quit"]);

    assert!(output.status.success());
    let bestmove = bestmoves(&output);
    assert_eq!(bestmove.len(), 1, "a capped search must still answer");
    let deepest = search_info_lines(&output)
        .iter()
        .map(|line| field(line, "depth"))
        .max()
        .expect("the capped search must still complete at least one iteration");
    assert!(
        deepest < 14,
        "a 1 ms clock must cap a depth-14 request; it reached {deepest}"
    );
}

/// `go nodes` with a realistic clock: the node budget is the primary stop and the
/// hard deadline is the safety net, so a `bestmove` must arrive well inside the
/// 1 s the sender still has on its clock.
#[test]
fn node_limited_go_with_clock_tokens_stays_inside_the_clock() {
    let output = run_uci(&[
        "position startpos",
        "go nodes 50000 wtime 1000 btime 1000 movestogo 40",
        "quit",
    ]);

    assert!(output.status.success());
    assert_eq!(bestmoves(&output).len(), 1);
    let reported = stdout_lines(&output)
        .into_iter()
        .filter_map(|line| optional_field(line, "time"))
        .max()
        .expect("the search must report its elapsed time");
    assert!(
        reported < 1000,
        "a node-limited go with a 1 s clock spent {reported} ms"
    );
}

#[test]
fn fifty_millisecond_clock_returns_legal_moves_without_overshoot() {
    let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    let position = Position::from_fen(fen, false).expect("test FEN should parse");
    let legal_moves: Vec<_> = generate_legal_moves(&position)
        .into_iter()
        .map(|mv| format_uci_move(&position, *mv, false))
        .collect();
    let mut engine = InteractiveUci::spawn();
    engine.send("uci");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(1)), |line| line == "uciok")
            .is_some()
    );
    // `uciok` does not imply the network is loaded; `readyok` does. Without this the
    // first sample pays the one-off ~106 MiB load and is scored as a clock overshoot.
    engine.send("isready");
    assert!(
        engine
            .receive_until(Duration::from_secs(30), |line| line == "readyok")
            .is_some(),
        "engine should become ready"
    );

    for sample in 0..50 {
        engine.send(&format!("position fen {fen}"));
        engine.send("go wtime 50 btime 50 winc 0 binc 0");
        let mut reported = None;
        let bestmove = engine
            .receive_until(Duration::from_secs(10), |line| {
                if let Some(time) = optional_field(line, "time") {
                    reported = Some(time);
                }
                line.starts_with("bestmove ")
            })
            .unwrap_or_else(|| panic!("sample {sample} never answered the 50 ms clock"));
        let reported = reported.unwrap_or_else(|| panic!("sample {sample} reported no time"));
        let mv = bestmove
            .strip_prefix("bestmove ")
            .expect("bestmove prefix should exist")
            .split_whitespace()
            .next()
            .expect("bestmove carries a move");
        assert!(
            legal_moves.iter().any(|legal| legal == mv),
            "sample {sample} returned illegal move {mv}"
        );
        assert!(reported < 80, "sample {sample} reported {reported} ms");
    }

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(1))));
}

#[test]
fn infinite_search_waits_for_stop_and_then_returns_promptly() {
    let mut engine = InteractiveUci::spawn();
    engine.send("uci");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(1)), |line| line == "uciok")
            .is_some()
    );
    engine.send("position startpos");
    engine.send("go infinite");

    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("info depth 2 ")
            })
            .is_some(),
        "infinite search should complete real iterations"
    );
    let quiet_deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < quiet_deadline {
        let remaining = quiet_deadline.saturating_duration_since(Instant::now());
        match engine.lines.recv_timeout(remaining) {
            Ok(line) => assert!(
                !line.starts_with("bestmove "),
                "infinite search terminated before stop: {line}"
            ),
            Err(_) => break,
        }
    }
    engine.send("stop trailing");
    assert!(
        engine
            .receive_until(Duration::from_millis(150), |line| {
                line.starts_with("bestmove ")
            })
            .is_none(),
        "malformed stop must not stop the search"
    );
    engine.send("go banana");
    assert!(
        engine
            .receive_until(Duration::from_millis(150), |line| {
                line.starts_with("bestmove ")
            })
            .is_none(),
        "malformed go must not stop the search"
    );

    let stopped = Instant::now();
    engine.send("stop");
    let bestmove = engine
        .receive_until(watchdog(Duration::from_secs(1)), |line| {
            line.starts_with("bestmove ")
        })
        .expect("stop should produce bestmove");
    assert!(stopped.elapsed() <= watchdog(Duration::from_secs(1)));
    assert_ne!(bestmove, "bestmove 0000");

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(1))));
    assert_eq!(
        engine
            .child
            .try_wait()
            .expect("exit status should be readable")
            .expect("process should have exited")
            .code(),
        Some(0)
    );
}

#[test]
fn every_go_form_a_gui_sends_eventually_answers_with_bestmove() {
    // UCI requires a `bestmove` for every `go`. A form that parses to nothing is not
    // merely ignored -- the GUI blocks forever waiting for a reply that never comes.
    for go in [
        "go",
        "go ponder",
        "go searchmoves d2d4 e2e4",
        "go ponder searchmoves d2d4",
        "go searchmoves d2d4 depth 4",
        // A mate search with no mate on the board must still answer once stopped.
        "go mate 3",
        // Unknown tokens alongside a recognized one are ignored per the UCI spec,
        // not treated as fatal. (A `go` whose arguments are *entirely* unrecognized
        // stays malformed and is covered by the readiness test.)
        "go depth 4 foo bar",
        "go infinite tinkerbell 7",
        // A malformed value drops that one argument, not the command.
        "go depth abc",
    ] {
        let mut engine = InteractiveUci::spawn();
        engine.send("position startpos");
        engine.send(go);
        assert!(
            engine
                .receive_until(watchdog(Duration::from_secs(2)), |line| {
                    line.starts_with("info depth 1 ")
                })
                .is_some(),
            "'{go}' should start a real search"
        );
        engine.send("stop");
        assert!(
            engine
                .receive_until(watchdog(Duration::from_secs(2)), |line| {
                    line.starts_with("bestmove ")
                })
                .is_some(),
            "'{go}' must answer with a bestmove instead of hanging the GUI"
        );
        engine.send("quit");
        assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
    }
}

#[test]
fn a_ponder_search_holds_its_answer_until_stop() {
    // Timing-sensitive on the "no bestmove yet" side: an engine sharing the machine
    // could legitimately be slow, but it must never be TALKATIVE, so the session gets
    // the machine to itself to keep the info stream deterministic.
    let mut engine = InteractiveUci::spawn_exclusive();
    engine.send("position startpos moves e2e4");
    engine.send("go ponder wtime 60000 btime 60000");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("info depth 1 ")
            })
            .is_some(),
        "go ponder must search the predicted position"
    );

    // The ~300 ms window is NOT watchdog-scaled: it asserts silence, and silence does
    // not get harder to produce in a debug build.
    assert_eq!(
        engine.receive_until(Duration::from_millis(300), |line| {
            line.starts_with("bestmove ")
        }),
        None,
        "a pondering engine must not answer before ponderhit or stop"
    );

    engine.send("stop");
    let bestmove = engine
        .receive_until(watchdog(Duration::from_secs(2)), |line| {
            line.starts_with("bestmove ")
        })
        .expect("stop during ponder is a ponder miss and must produce the deferred bestmove");
    assert!(!bestmove.contains("0000"));

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn ponderhit_converts_the_ponder_search_into_a_timed_one() {
    let mut engine = InteractiveUci::spawn_exclusive();
    engine.send("position startpos moves e2e4");
    // 3000 ms on the clock: soft ~97 ms, hard ~388 ms, so the converted search must
    // answer well inside the receive deadline without any `stop`.
    engine.send("go ponder wtime 3000 btime 3000");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("info depth 1 ")
            })
            .is_some(),
        "go ponder must search the predicted position"
    );

    engine.send("ponderhit");
    let bestmove = engine
        .receive_until(watchdog(Duration::from_secs(5)), |line| {
            line.starts_with("bestmove ")
        })
        .expect("ponderhit must convert to a timed search that answers on its own");
    assert!(!bestmove.contains("0000"));

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn a_clocked_ponder_that_reaches_the_analysis_ceiling_spends_the_converted_clock() {
    let mut engine = InteractiveUci::spawn_exclusive();
    engine.send("position fen 7k/8/6QK/8/8/8/8/8 w - - 0 1");
    engine.send("go ponder wtime 3000 btime 3000");

    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(10)), |line| {
                is_completed_iteration(line) && field(line, "depth") == 128
            })
            .is_some(),
        "the forced-mate ponder must reach the bounded analysis ceiling"
    );
    assert!(
        engine
            .receive_until(Duration::from_millis(200), |line| {
                line.starts_with("bestmove ")
            })
            .is_none(),
        "reaching the ceiling must not answer before ponderhit or stop"
    );

    engine.send("ponderhit");
    let mut rebased_time = None;
    let mut post_hit_info_lines = 0;
    let mut post_hit_currmoves = 0;
    let mut post_hit_ceiling_iterations = 0;
    let bestmove = engine.receive_until(watchdog(Duration::from_secs(5)), |line| {
        if line.starts_with("info ") {
            post_hit_info_lines += 1;
        }
        if line.contains(" currmove ") {
            post_hit_currmoves += 1;
        }
        if is_completed_iteration(line)
            && field(line, "depth") == 128
            && let Some(time) = optional_field(line, "time")
        {
            post_hit_ceiling_iterations += 1;
            rebased_time = Some(time);
        }
        line.starts_with("bestmove ")
    });

    assert!(
        bestmove.is_some(),
        "ponderhit must eventually release a bestmove"
    );
    assert!(
        rebased_time.is_some_and(|time| (40..1000).contains(&time)),
        "the post-hit ceiling iteration must spend the rebased budget, got {rebased_time:?}"
    );
    assert_eq!(
        post_hit_ceiling_iterations, 1,
        "the converted search must publish exactly one refreshed ceiling iteration"
    );
    assert_eq!(
        post_hit_currmoves, 0,
        "the converted maximum-depth repeats must suppress currmove output"
    );
    assert_eq!(
        post_hit_info_lines, 1,
        "the converted maximum-depth repeats must emit one bounded info line"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn a_finite_ponder_without_the_side_to_move_clock_cannot_busy_loop_after_ponderhit() {
    let mut engine = InteractiveUci::spawn_exclusive();
    engine.send("position fen 7k/8/6QK/8/8/8/8/8 w - - 0 1");
    engine.send("go ponder btime 3000");

    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(10)), |line| {
                is_completed_iteration(line) && field(line, "depth") == 128
            })
            .is_some(),
        "the finite ponder must reach the bounded analysis ceiling"
    );
    assert!(
        engine
            .receive_until(Duration::from_millis(200), |line| {
                line.starts_with("bestmove ")
            })
            .is_none(),
        "the parked result must wait for ponderhit or stop"
    );

    engine.send("ponderhit");
    assert!(
        engine
            .receive_until(Duration::from_secs(1), |line| line.starts_with("bestmove "))
            .is_some(),
        "without a side-to-move clock, ponderhit must release the parked result instead of repeating forever"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn a_stray_ponderhit_without_an_active_search_is_ignored() {
    let mut engine = InteractiveUci::spawn();
    engine.send("ponderhit");
    engine.send("isready");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(30)), |line| line == "readyok")
            .is_some(),
        "a stray ponderhit must not wedge the engine"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn a_new_go_during_ponder_joins_the_old_search_and_answers_each_go_once() {
    // The GUI shortcut path: on a ponder miss some GUIs skip `stop` and send the
    // corrected `position`/`go` directly. The engine must join the old search (its
    // deferred bestmove is printed and discarded by the GUI) and answer the new one --
    // exactly one bestmove per go.
    let mut engine = InteractiveUci::spawn_exclusive();
    engine.send("position startpos moves e2e4");
    engine.send("go ponder wtime 60000 btime 60000");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("info depth 1 ")
            })
            .is_some(),
        "go ponder must search the predicted position"
    );

    engine.send("position startpos moves d2d4");
    engine.send("go depth 4");
    let first = engine
        .receive_until(watchdog(Duration::from_secs(2)), |line| {
            line.starts_with("bestmove ")
        })
        .expect("joining the ponder search must release its deferred bestmove");
    assert!(!first.contains("0000"));
    let second = engine
        .receive_until(watchdog(Duration::from_secs(5)), |line| {
            line.starts_with("bestmove ")
        })
        .expect("the new go must produce its own bestmove");
    assert!(!second.contains("0000"));
    // No third bestmove: two `go` commands, exactly two answers.
    assert_eq!(
        engine.receive_until(Duration::from_millis(300), |line| {
            line.starts_with("bestmove ")
        }),
        None,
        "two go commands must produce exactly two bestmoves"
    );

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn searchmoves_restricts_the_answer_to_the_listed_moves() {
    let output = run_uci(&["position startpos", "go searchmoves e2e4 depth 4", "quit"]);
    assert!(output.status.success());
    assert_eq!(bestmoves(&output), ["e2e4"]);

    // An illegal or unknown entry is skipped rather than failing the command, and
    // whatever legal remainder there is still restricts the root.
    let output = run_uci(&[
        "position startpos",
        "go searchmoves e2e5 zzzz a2a3 depth 4",
        "quit",
    ]);
    assert!(output.status.success());
    assert_eq!(bestmoves(&output), ["a2a3"]);
}

#[test]
fn multipv_reports_ordered_distinct_lines_and_keeps_line_one_as_bestmove() {
    let output = run_uci(&[
        "setoption name MultiPV value 3",
        "position startpos",
        "go depth 6",
        "quit",
    ]);
    assert!(output.status.success());

    let infos = search_info_lines(&output);
    assert_eq!(infos.len(), 18);
    for depth in 1..=6 {
        let lines: Vec<_> = infos
            .iter()
            .copied()
            .filter(|line| field(line, "depth") == depth)
            .collect();
        assert_eq!(lines.len(), 3, "depth {depth} should report three lines");
        assert_eq!(
            lines
                .iter()
                .map(|line| field(line, "multipv"))
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(
            lines
                .windows(2)
                .all(|pair| score_value(pair[0]) >= score_value(pair[1])),
            "depth {depth} scores should be non-increasing: {lines:?}"
        );
        let first_moves: Vec<_> = lines.iter().map(|line| pv_first_move(line)).collect();
        assert!(
            first_moves
                .iter()
                .enumerate()
                .all(|(index, mv)| !first_moves[..index].contains(mv)),
            "depth {depth} first moves should be distinct: {first_moves:?}"
        );
    }

    let final_line_one = infos
        .iter()
        .copied()
        .find(|line| field(line, "depth") == 6 && field(line, "multipv") == 1)
        .expect("depth 6 should report line one");
    assert_eq!(bestmoves(&output), [pv_first_move(final_line_one)]);
}

#[test]
fn multipv_composes_with_searchmoves() {
    let output = run_uci(&[
        "setoption name MultiPV value 2",
        "position startpos",
        "go depth 6 searchmoves e2e4 d2d4",
        "quit",
    ]);
    assert!(output.status.success());

    let infos = search_info_lines(&output);
    assert_eq!(infos.len(), 12);
    for depth in 1..=6 {
        let lines: Vec<_> = infos
            .iter()
            .copied()
            .filter(|line| field(line, "depth") == depth)
            .collect();
        assert_eq!(lines.len(), 2, "depth {depth} should report two lines");
        assert_eq!(
            lines
                .iter()
                .map(|line| field(line, "multipv"))
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let first_moves: Vec<_> = lines.iter().map(|line| pv_first_move(line)).collect();
        assert!(
            first_moves.iter().all(|mv| ["e2e4", "d2d4"].contains(mv)),
            "searchmoves should restrict every line: {first_moves:?}"
        );
        assert_ne!(first_moves[0], first_moves[1]);
    }
}

#[test]
fn multipv_clamps_to_the_number_of_legal_root_moves() {
    let output = run_uci(&[
        "setoption name MultiPV value 5",
        "position fen 8/7p/8/8/8/2k5/8/K7 w - - 0 1",
        "go depth 4",
        "quit",
    ]);
    assert!(output.status.success());

    let infos = search_info_lines(&output);
    assert_eq!(infos.len(), 8);
    for depth in 1..=4 {
        let lines: Vec<_> = infos
            .iter()
            .copied()
            .filter(|line| field(line, "depth") == depth)
            .collect();
        assert_eq!(lines.len(), 2, "depth {depth} should clamp to two lines");
        assert_eq!(
            lines
                .iter()
                .map(|line| field(line, "multipv"))
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
    assert_eq!(bestmoves(&output).len(), 1);
    assert!(output.stderr.is_empty());
}

#[test]
fn a_searchmoves_list_with_no_legal_entries_searches_normally() {
    let output = run_uci(&[
        "position startpos",
        "go searchmoves e2e5 zzzz depth 4",
        "quit",
    ]);
    assert!(output.status.success());
    let moves = bestmoves(&output);
    assert_eq!(moves.len(), 1);
    assert_ne!(moves[0], "0000", "no restriction means a normal search");
}

#[test]
fn go_mate_returns_promptly_with_a_mate_score_when_one_exists() {
    let output = run_uci(&[
        "position fen k7/8/KQ6/8/8/8/8/8 w - - 0 1",
        "go mate 1",
        "quit",
    ]);
    assert!(output.status.success());
    let infos = search_info_lines(&output);
    assert!(
        infos
            .last()
            .is_some_and(|line| line.contains(" score mate 1 ")),
        "a mate-in-1 must be reported as mate 1: {infos:?}"
    );
    assert_eq!(bestmoves(&output).len(), 1);
}

#[test]
fn clear_hash_button_is_accepted_and_leaves_the_engine_responsive() {
    let output = run_uci(&[
        "position startpos",
        "go depth 4",
        "setoption name Clear Hash",
        "isready",
        "go depth 4",
        "quit",
    ]);
    assert!(output.status.success());
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"readyok"));
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("info string unable to clear Hash")),
        "a default pool must clear successfully: {lines:?}"
    );
    assert_eq!(bestmoves(&output).len(), 2);
}

#[test]
fn the_d_command_prints_a_diagram_whose_fen_names_the_current_position() {
    let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    let mut engine = InteractiveUci::spawn();
    engine.send(&format!("position fen {fen}"));
    engine.send("d");
    let fen_line = engine
        .receive_until(watchdog(Duration::from_secs(5)), |line| {
            line.starts_with("Fen: ")
        })
        .expect("d should print the FEN");
    assert_eq!(fen_line, format!("Fen: {fen}"));
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("Key: ")
            })
            .is_some(),
        "d should print the Zobrist key"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn the_eval_command_is_deterministic_for_a_fixed_position() {
    let mut engine = InteractiveUci::spawn();
    engine.send("position startpos moves e2e4 e7e5");
    engine.send("eval");
    let first = engine
        .receive_until(watchdog(Duration::from_secs(30)), |line| {
            line.starts_with("NNUE evaluation: ")
        })
        .expect("eval should print an evaluation");
    engine.send("eval");
    let second = engine
        .receive_until(watchdog(Duration::from_secs(5)), |line| {
            line.starts_with("NNUE evaluation: ")
        })
        .expect("a second eval should print an evaluation");
    assert_eq!(
        first, second,
        "eval must be a pure function of the position"
    );
    let centipawns = first
        .strip_prefix("NNUE evaluation: ")
        .and_then(|rest| rest.split_whitespace().next())
        .expect("eval line should carry a number");
    assert!(
        centipawns.parse::<i32>().is_ok(),
        "eval must print an integer centipawn value: {first}"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn a_negative_clock_is_clamped_and_still_moves_immediately() {
    // GUIs report a negative clock once the flag has fallen. Rejecting the `go` left
    // the engine mute in exactly the time pressure where a move matters most, so the
    // value is clamped to zero and must still produce a prompt bestmove -- crucially
    // without needing a `stop`, which would mean it had fallen back to infinite.
    let mut engine = InteractiveUci::spawn();
    engine.send("position startpos");
    engine.send("go wtime -134 btime 5000 winc 1000 binc 1000");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(3)), |line| {
                line.starts_with("bestmove ")
            })
            .is_some(),
        "a negative clock must still answer without a stop"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}

#[test]
fn go_perft_tolerates_trailing_arguments() {
    let output = run_uci(&["position startpos", "go perft 2 extra", "quit"]);
    assert!(output.status.success());
    assert!(stdout_lines(&output).contains(&"Nodes searched: 400"));
}

#[test]
fn finite_search_can_be_stopped_before_its_budget_expires() {
    let mut engine = InteractiveUci::spawn();
    engine.send("position startpos");
    engine.send("go movetime 3000");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(1)), |line| {
                is_completed_iteration(line)
            })
            .is_some(),
        "stop test requires an active finite search"
    );

    engine.send("stop");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(1)), |line| {
                line.starts_with("bestmove ")
            })
            .is_some(),
        "stop should interrupt a finite search"
    );

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(1))));
}

/// A completed iteration, as opposed to a transient `currmove` progress line.
///
/// Both spellings start with `info depth `, so a prefix test alone counts every root
/// move of every iteration as a finished depth. Filtering on `currmove` is what makes
/// this test measure iterations rather than root moves.
fn is_completed_iteration(line: &str) -> bool {
    line.starts_with("info depth ") && !line.contains(" currmove ")
}

#[test]
fn interrupted_iteration_does_not_duplicate_a_completed_depth() {
    let mut engine = InteractiveUci::spawn();
    engine.send("position startpos");
    engine.send("go nodes 1000000");
    let mut depths = Vec::new();
    let mut latest_nodes = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while depths.len() < 2 || !matches!(latest_nodes, Some(nodes) if nodes < 1_000_000) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = engine
            .lines
            .recv_timeout(remaining)
            .expect("search should complete two iterations below the node budget");
        if let Some(nodes) = optional_field(&line, "nodes") {
            latest_nodes = Some(nodes);
        }
        if is_completed_iteration(&line) {
            depths.push(field(&line, "depth"));
        }
        assert!(
            !line.starts_with("bestmove "),
            "search exhausted its node budget before it could be interrupted"
        );
    }

    engine.send("stop");
    loop {
        let line = engine
            .lines
            .recv_timeout(watchdog(Duration::from_secs(1)))
            .expect("stop should finish the search");
        if is_completed_iteration(&line) {
            depths.push(field(&line, "depth"));
        }
        if line.starts_with("bestmove ") {
            break;
        }
    }

    assert!(
        depths.windows(2).all(|pair| pair[0] < pair[1]),
        "completed depths must be strictly increasing: {depths:?}"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(1))));
}

#[test]
fn infinite_overrides_depth_and_node_limits_until_stop() {
    for go in ["go infinite depth 1", "go infinite nodes 1"] {
        let mut engine = InteractiveUci::spawn();
        engine.send("position startpos");
        engine.send(go);
        assert!(
            engine
                .receive_until(Duration::from_millis(150), |line| {
                    line.starts_with("bestmove ")
                })
                .is_none(),
            "{go} must not terminate before stop"
        );
        engine.send("stop");
        assert!(
            engine
                .receive_until(watchdog(Duration::from_secs(1)), |line| {
                    line.starts_with("bestmove ")
                })
                .is_some(),
            "{go} should return bestmove after stop"
        );
        engine.send("quit");
        assert!(engine.wait_for_exit(watchdog(Duration::from_secs(1))));
    }
}

#[test]
fn terminal_infinite_search_waits_for_stop() {
    let mut engine = InteractiveUci::spawn();
    engine.send("position fen 7k/5KQ1/8/8/8/8/8/8 b - - 0 1");
    engine.send("go infinite");
    assert!(
        engine
            .receive_until(Duration::from_millis(150), |line| {
                line.starts_with("bestmove ")
            })
            .is_none(),
        "terminal infinite search must wait for stop"
    );

    engine.send("stop");
    assert_eq!(
        engine.receive_until(watchdog(Duration::from_secs(1)), |line| line
            .starts_with("bestmove ")),
        Some("bestmove 0000".to_string())
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(1))));
}

#[test]
fn quit_during_infinite_search_exits_cleanly() {
    let mut engine = InteractiveUci::spawn();
    engine.send("setoption name Threads value 4");
    engine.send("position startpos");
    engine.send("go infinite");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(2)), |line| {
                line.starts_with("info depth 2 ")
            })
            .is_some(),
        "quit test requires an active search"
    );

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
    assert_eq!(
        engine
            .child
            .try_wait()
            .expect("exit status should be readable")
            .expect("process should have exited")
            .code(),
        Some(0)
    );
}

#[test]
fn quit_during_finite_search_exits_cleanly() {
    let mut engine = InteractiveUci::spawn();
    engine.send("position startpos");
    engine.send("go movetime 3000");
    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(1)), |line| {
                line.starts_with("info depth 2 ")
            })
            .is_some(),
        "quit test requires an active finite search"
    );

    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
    assert_eq!(
        engine
            .child
            .try_wait()
            .expect("exit status should be readable")
            .expect("process should have exited")
            .code(),
        Some(0)
    );
}

#[test]
fn checkmate_uses_the_uci_null_move_token() {
    let output = run_uci(&[
        "position fen 7k/5KQ1/8/8/8/8/8/8 b - - 0 1",
        "go depth 5",
        "isready",
        "quit",
    ]);

    assert!(output.status.success());
    let infos = search_info_lines(&output);
    let info = infos.last().expect("terminal search should emit info");
    assert!(info.contains(" score mate 0 "));
    assert_eq!(bestmoves(&output), ["0000"]);
    assert!(stdout_lines(&output).contains(&"readyok"));
}

#[test]
fn stalemate_uses_the_uci_null_move_token() {
    let output = run_uci(&[
        "position fen 7k/5Q2/6K1/8/8/8/8/8 b - - 0 1",
        "go movetime 200",
        "isready",
        "quit",
    ]);

    assert!(output.status.success());
    let infos = search_info_lines(&output);
    let info = infos.last().expect("terminal search should emit info");
    assert!(info.contains(" score cp 0 "));
    assert_eq!(bestmoves(&output), ["0000"]);
    assert!(stdout_lines(&output).contains(&"readyok"));
}

#[test]
fn mate_scores_use_uci_sign_and_move_count_conventions() {
    let cases = [
        ("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", 8, "mate 1"),
        ("k7/7R/1K6/8/8/8/8/8 b - - 0 1", 10, "mate -1"),
        // The FEN this case ran with (`.../6R1/6Rk w`) had the black king in check
        // with white to move. This is its legal mate-in-two sibling (1. Rh2+ Kg1
        // 2. Rh1#).
        ("8/8/8/8/8/6K1/6R1/7k w - - 0 1", 12, "mate 2"),
    ];

    for (fen, depth, expected) in cases {
        let position = format!("position fen {fen}");
        let go = format!("go depth {depth}");
        let output = run_uci(&[&position, &go, "quit"]);
        assert!(output.status.success());
        let line = search_info_lines(&output)
            .last()
            .copied()
            .expect("mate search should emit info");
        assert!(
            line.contains(&format!(" score {expected} ")),
            "expected score {expected} for {fen}, got {line}"
        );
        let pv = line
            .split(" pv ")
            .nth(1)
            .expect("mate line should contain a PV");
        assert_eq!(
            bestmoves(&output),
            [pv.split_whitespace()
                .next()
                .expect("PV should be non-empty")]
        );
    }
}

#[test]
fn fifty_move_and_pawn_reset_state_are_observable_through_uci() {
    let claimable = run_uci(&[
        "position fen 7k/8/8/8/8/8/P1q5/K7 w - - 100 1",
        "go depth 6",
        "quit",
    ]);
    let near_draw = run_uci(&[
        "position fen 8/8/8/4k3/8/8/8/K1Q5 w - - 98 1",
        "go depth 6",
        "quit",
    ]);
    let fresh = run_uci(&[
        "position fen 8/8/8/4k3/8/8/8/K1Q5 w - - 0 1",
        "go depth 6",
        "quit",
    ]);
    let reset = run_uci(&[
        "position fen 8/8/8/4k3/8/8/P7/K1Q5 w - - 99 1 moves a2a4",
        "go depth 6",
        "quit",
    ]);

    assert!(
        search_info_lines(&claimable)
            .last()
            .is_some_and(|line| line.contains(" score cp 0 "))
    );
    assert!(
        search_info_lines(&near_draw)
            .last()
            .is_some_and(|line| line.contains(" score cp 0 "))
    );
    assert!(
        search_info_lines(&fresh)
            .last()
            .is_some_and(|line| line.contains(" score cp ") && !line.contains(" score cp 0 "))
    );
    assert!(
        search_info_lines(&reset)
            .last()
            .is_some_and(|line| line.contains(" score cp -"))
    );
}

#[test]
fn position_moves_history_survives_into_repetition_aware_search() {
    let output = run_uci(&[
        "position fen 1q5k/8/8/8/8/8/8/R5K1 w - - 0 1 moves a1a2 b8b7 a2a1 b7b8 a1a2 b8b7 a2a1 b7b8 a1a2 b8b7",
        "go depth 6",
        "quit",
    ]);

    assert!(output.status.success());
    assert!(
        search_info_lines(&output)
            .last()
            .is_some_and(|line| line.contains(" score cp 0 "))
    );
    assert_eq!(bestmoves(&output), ["a2a1"]);
}

#[test]
fn insufficient_material_and_stalemate_resources_are_scored_as_draws() {
    for fen in [
        "8/8/8/4k3/8/8/8/4K3 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KB2 w - - 0 1",
        "8/8/8/4k3/8/8/8/4KN2 w - - 0 1",
    ] {
        let position = format!("position fen {fen}");
        let output = run_uci(&[&position, "go depth 4", "quit"]);
        assert!(
            search_info_lines(&output)
                .last()
                .is_some_and(|line| line.contains(" score cp 0 ")),
            "{fen} should be scored as a draw"
        );
    }

    let stalemate = run_uci(&[
        "position fen 1r5k/7p/8/8/8/8/1r6/K6Q w - - 0 1",
        "go depth 4",
        "quit",
    ]);
    assert!(
        search_info_lines(&stalemate)
            .last()
            .is_some_and(|line| line.contains(" score cp 0 "))
    );
    assert_eq!(bestmoves(&stalemate), ["h1h7"]);
}
