use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run_uci(commands: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("manifold binary should start");

    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        for command in commands {
            writeln!(stdin, "{command}").expect("command should be written");
        }
    }

    child
        .wait_with_output()
        .expect("manifold process should exit")
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

fn bestmoves(output: &Output) -> Vec<&str> {
    stdout_lines(output)
        .into_iter()
        .filter_map(|line| line.strip_prefix("bestmove "))
        .collect()
}

fn search_info_lines(output: &Output) -> Vec<&str> {
    stdout_lines(output)
        .into_iter()
        .filter(|line| line.starts_with("info depth "))
        .collect()
}

fn field(line: &str, name: &str) -> u64 {
    let tokens: Vec<_> = line.split_whitespace().collect();
    let index = tokens
        .iter()
        .position(|token| *token == name)
        .unwrap_or_else(|| panic!("missing field '{name}' in '{line}'"));
    tokens[index + 1]
        .parse()
        .unwrap_or_else(|_| panic!("non-numeric field '{name}' in '{line}'"))
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
    assert!(lines[..uciok].contains(&"option name UCI_Chess960 type check default false"));
    assert!(lines[..uciok].contains(&"option name EvalFile type string default <empty>"));
    assert_eq!(lines.iter().filter(|line| **line == "uciok").count(), 1);
    assert!(
        !lines[uciok + 1..]
            .iter()
            .any(|line| line.starts_with("id ") || line.starts_with("option "))
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
            "{mv} should produce the Stockfish perft anchor"
        );
    }
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

#[test]
fn hash_option_resizes_case_insensitively_and_rejects_extremes_without_crashing() {
    let output = run_uci(&[
        "SeToPtIoN NaMe hAsH VaLuE 3",
        "setoption name Hash value 0",
        "setoption name Hash value -5",
        "setoption name Hash value banana",
        "setoption name Hash value 99999999999",
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
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("info string unable to allocate Hash"))
    );
    assert!(lines.contains(&"readyok"));
    assert_eq!(bestmoves(&output).len(), 1);
    assert!(output.stderr.is_empty());
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
    assert_eq!(field(infos.last().unwrap(), "nodes"), 1000);
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
        for required in ["score", "nodes", "nps", "time", "pv"] {
            assert!(
                line.split_whitespace().any(|token| token == required),
                "missing '{required}' in '{line}'"
            );
        }
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
fn node_limited_search_is_repeatable_at_exact_budget() {
    let commands = [
        "setoption name Threads value 1",
        "position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "go nodes 20000 wtime 1 btime 1",
        "go nodes 20000 wtime 1 btime 1",
        "quit",
    ];
    let output = run_uci(&commands);

    assert!(output.status.success());
    let moves = bestmoves(&output);
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0], moves[1]);
    let exact_budget_lines: Vec<_> = search_info_lines(&output)
        .into_iter()
        .filter(|line| field(line, "nodes") == 20_000)
        .collect();
    assert_eq!(exact_budget_lines.len(), 2);
}

#[test]
fn movetime_and_clock_go_forms_honor_bounded_budgets() {
    let started = Instant::now();
    let movetime = run_uci(&["position startpos", "go movetime 80", "quit"]);
    let movetime_elapsed = started.elapsed();
    assert!(movetime.status.success());
    assert_eq!(bestmoves(&movetime).len(), 1);
    assert!(movetime_elapsed >= Duration::from_millis(40));
    assert!(movetime_elapsed <= Duration::from_millis(500));

    let fast_started = Instant::now();
    let fast = run_uci(&[
        "position startpos",
        "go wtime 1000 btime 1000 movestogo 40",
        "quit",
    ]);
    let fast_elapsed = fast_started.elapsed();

    let slow_started = Instant::now();
    let slow = run_uci(&[
        "position startpos",
        "go wtime 1000 btime 1000 movestogo 2",
        "quit",
    ]);
    let slow_elapsed = slow_started.elapsed();

    assert!(fast.status.success());
    assert!(slow.status.success());
    assert_eq!(bestmoves(&fast).len(), 1);
    assert_eq!(bestmoves(&slow).len(), 1);
    assert!(
        slow_elapsed > fast_elapsed + Duration::from_millis(100),
        "movestogo=2 ({slow_elapsed:?}) must budget more than movestogo=40 ({fast_elapsed:?})"
    );
}

#[test]
fn mate_scores_use_uci_sign_and_move_count_conventions() {
    let cases = [
        ("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", 8, "mate 1"),
        ("k7/7R/1K6/8/8/8/8/8 b - - 0 1", 10, "mate -1"),
        ("8/8/8/8/8/6K1/6R1/6Rk w - - 0 1", 12, "mate 2"),
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
