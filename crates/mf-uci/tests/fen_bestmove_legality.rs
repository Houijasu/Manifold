//! Drives the engine binary the way a GUI does and asserts every `bestmove` is legal
//! in the position that was actually sent.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use mf_core::{Position, format_uci_move, generate_legal_moves};

const FENS: &[&str] = &[
    "r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5Q2/PPPP1PPP/RNB1K1NR w KQkq - 4 4",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
    "8/8/8/8/8/8/6k1/4K2R w K - 0 1",
    "4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 2",
    "8/5k2/8/8/8/8/5K2/8 w - - 0 1",
    "2rq1rk1/pp1bppbp/2np1np1/8/3NP3/1BN1BP2/PPPQ2PP/2KR3R w - - 0 11",
    "r1b1k2r/ppppnppp/2n2q2/2b5/3NP3/2P1B3/PP3PPP/RN1QKB1R w KQkq - 0 1",
];

fn engine_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_manifold"));
    path.set_extension(std::env::consts::EXE_EXTENSION);
    path
}

/// Sends a script to the engine and returns every line it wrote.
fn run_engine(script: &str) -> Vec<String> {
    let mut child = Command::new(engine_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("engine binary should start");

    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(script.as_bytes())
        .expect("engine should accept input");

    let stdout = child.stdout.take().expect("stdout was piped");
    let lines: Vec<String> = BufReader::new(stdout)
        .lines()
        .map(|line| line.expect("engine output should be valid UTF-8"))
        .collect();
    let _ = child.wait();
    lines
}

fn legal_notations(fen: &str) -> Vec<String> {
    let position = Position::from_fen(fen, false).expect("test FEN should parse");
    generate_legal_moves(&position)
        .into_iter()
        .map(|mv| format_uci_move(&position, *mv, false))
        .collect()
}

#[test]
fn bestmove_is_legal_in_every_pasted_fen() {
    for fen in FENS {
        let script = format!("uci\nisready\nposition fen {fen}\ngo depth 6\nquit\n");
        let output = run_engine(&script);
        let bestmove = output
            .iter()
            .find_map(|line| line.strip_prefix("bestmove "))
            .unwrap_or_else(|| panic!("no bestmove for '{fen}'\n{}", output.join("\n")))
            .split_whitespace()
            .next()
            .expect("bestmove carries a move")
            .to_string();

        let legal = legal_notations(fen);
        assert!(
            legal.contains(&bestmove),
            "engine played illegal move '{bestmove}' in '{fen}'\nlegal: {legal:?}"
        );
    }
}

#[test]
fn a_chess960_castling_dialect_does_not_fall_back_to_the_previous_board() {
    // The original report: pasting this FEN while the engine was still on the start
    // position made it answer `d2d4` -- a square that is empty here -- because the
    // castling field failed to parse and `position` silently kept the old board.
    let fen = "bqnb1rkr/pp3ppp/3ppn2/2p5/5P2/P2P4/NPP1P1PP/BQ1BNRKR w HFhf - 2 9";
    let output = run_engine(&format!(
        "uci\nisready\nposition fen {fen}\ngo depth 6\nquit\n"
    ));

    assert!(
        !output
            .iter()
            .any(|line| line.starts_with("info string invalid position command:")),
        "the FEN should parse\n{}",
        output.join("\n")
    );

    let bestmove = output
        .iter()
        .find_map(|line| line.strip_prefix("bestmove "))
        .expect("engine should answer")
        .split_whitespace()
        .next()
        .expect("bestmove carries a move")
        .to_string();

    let position = Position::from_fen(fen, true).expect("chess960 FEN should parse");
    let legal: Vec<String> = generate_legal_moves(&position)
        .into_iter()
        .map(|mv| format_uci_move(&position, *mv, true))
        .collect();
    assert!(
        legal.contains(&bestmove),
        "engine played '{bestmove}', illegal in the pasted position\nlegal: {legal:?}"
    );
}

/// A GUI composes the FEN itself, so the counters and the en-passant dash are not
/// always present: position-setup dialogs leave the fields blank, and FEN snippets
/// copied from books or study databases routinely carry `0 0` counters. Rejecting
/// these is invisible in the GUI -- the engine answers `bestmove 0000` and the
/// analysis pane stays empty -- so each spelling must still be analysed.
#[test]
fn abbreviated_or_zeroed_fen_counters_still_analyse_the_pasted_position() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "no counters and no en-passant dash (three fields)",
            "r3k2r/8/8/8/8/8/8/R3K2R w KQkq",
            "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1",
        ),
        (
            "fullmove number zeroed, as study and exercise FENs often carry",
            "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 0",
            "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1",
        ),
        (
            "both counters zeroed",
            "r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5Q2/PPPP1PPP/RNB1K1NR w KQkq - 0 0",
            "r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5Q2/PPPP1PPP/RNB1K1NR w KQkq - 0 1",
        ),
    ];

    for (label, fen, canonical) in cases {
        let script = format!("uci\nisready\nposition fen {fen}\ngo depth 6\nquit\n");
        let output = run_engine(&script);
        assert!(
            !output
                .iter()
                .any(|line| line.starts_with("info string invalid position command:")),
            "{label} failed to parse '{fen}'\n{}",
            output.join("\n")
        );
        let bestmove = output
            .iter()
            .find_map(|line| line.strip_prefix("bestmove "))
            .unwrap_or_else(|| panic!("no bestmove for {label} '{fen}'\n{}", output.join("\n")))
            .split_whitespace()
            .next()
            .expect("bestmove carries a move")
            .to_string();
        assert_ne!(
            bestmove,
            "0000",
            "{label}: engine declined to analyse '{fen}'\n{}",
            output.join("\n")
        );

        let legal = legal_notations(canonical);
        assert!(
            legal.contains(&bestmove),
            "{label}: engine played '{bestmove}', illegal in the pasted position\nlegal: {legal:?}"
        );
    }
}

#[test]
fn a_rejected_position_makes_go_decline_instead_of_answering_from_a_stale_board() {
    let output = run_engine("uci\nisready\nposition fen totally-not-a-fen\ngo depth 4\nquit\n");
    let bestmove = output
        .iter()
        .find_map(|line| line.strip_prefix("bestmove "))
        .expect("UCI requires a bestmove for every go");

    assert_eq!(
        bestmove.trim(),
        "0000",
        "a search on an unknown board must not return a confident move\n{}",
        output.join("\n")
    );
}

#[test]
fn a_successful_position_clears_the_stale_flag() {
    let output = run_engine(
        "uci\nisready\nposition fen totally-not-a-fen\ngo depth 4\n\
         position fen 8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1\ngo depth 6\nquit\n",
    );
    let bestmoves: Vec<&str> = output
        .iter()
        .filter_map(|line| line.strip_prefix("bestmove "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();

    assert_eq!(bestmoves.len(), 2, "{}", output.join("\n"));
    assert_eq!(bestmoves[0], "0000");
    let legal = legal_notations("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1");
    assert!(
        legal.contains(&bestmoves[1].to_string()),
        "recovery search played illegal '{}'",
        bestmoves[1]
    );
}

#[test]
fn bestmove_is_legal_after_a_fen_switch() {
    // A GUI pasting a second position into a session that already searched a first one.
    // If any per-search state survives the switch, the second answer is the one that
    // goes wrong.
    for pair in FENS.windows(2) {
        let (first, second) = (pair[0], pair[1]);
        let script = format!(
            "uci\nisready\nposition fen {first}\ngo depth 6\nposition fen {second}\ngo depth 6\nquit\n"
        );
        let output = run_engine(&script);
        let bestmoves: Vec<&str> = output
            .iter()
            .filter_map(|line| line.strip_prefix("bestmove "))
            .filter_map(|rest| rest.split_whitespace().next())
            .collect();

        assert_eq!(
            bestmoves.len(),
            2,
            "expected two bestmoves for '{first}' then '{second}'\n{}",
            output.join("\n")
        );

        let legal = legal_notations(second);
        assert!(
            legal.contains(&bestmoves[1].to_string()),
            "after switching to '{second}' the engine played illegal '{}'\nlegal: {legal:?}",
            bestmoves[1]
        );
    }
}
