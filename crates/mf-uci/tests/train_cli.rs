//! Integration coverage for the `manifold train` subcommand.
//!
//! The assertion this command exists to satisfy (`A-NNUE-016`) is that the wrapper
//! **detects VRAM pressure before launching bullet** rather than OOM-crashing mid-run.
//! A preflight that has only ever printed `OK` is untested, so the tests below force
//! the insufficient branch by overriding the requirement upward with `--require-mib`.
//!
//! The `train` subcommand and `parse_train_config` are not implemented yet, so this
//! file is gated behind the `train` feature: it describes the intended behaviour and
//! compiles once that work lands, without breaking `cargo test --workspace` until then.

#![cfg(feature = "train")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!("manifold{}", std::env::consts::EXE_SUFFIX))
}

fn run(arguments: &[&str]) -> Output {
    Command::new(binary())
        .args(arguments)
        .output()
        .expect("manifold runs")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Writes the shipped rung-1 config into a temporary directory, so the tests exercise
/// the real file rather than a hand-rolled stand-in that could drift away from it.
fn shipped_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../training/m5-ladder-rung1.toml")
        .canonicalize()
        .expect("the shipped rung-1 training config exists")
}

fn assert_no_panic(output: &Output) {
    let combined = format!("{}{}", stdout_of(output), stderr_of(output));
    for marker in ["panicked at", "RUST_BACKTRACE", "0xc0000005"] {
        assert!(
            !combined.contains(marker),
            "output must not contain '{marker}':\n{combined}"
        );
    }
}

#[test]
fn train_help_documents_the_preflight_flags() {
    let output = run(&["train", "--help"]);
    assert!(output.status.success(), "train --help must succeed");
    let text = stdout_of(&output);
    for flag in ["--preflight", "--wait", "--config", "--require-mib"] {
        assert!(text.contains(flag), "help must document {flag}:\n{text}");
    }
}

#[test]
fn preflight_reports_free_vram_the_requirement_and_a_verdict() {
    let config = shipped_config();
    let output = run(&[
        "train",
        "--preflight",
        "--config",
        &config.display().to_string(),
    ]);
    assert_no_panic(&output);
    let text = stdout_of(&output);

    assert!(
        text.contains("free_vram_mib="),
        "preflight must report observed free VRAM:\n{text}"
    );
    assert!(
        text.contains("required_vram_mib="),
        "preflight must report the estimated requirement:\n{text}"
    );
    assert!(
        text.contains("verdict=OK")
            || text.contains("verdict=INSUFFICIENT-waiting")
            || text.contains("verdict=INSUFFICIENT-aborting"),
        "preflight must print an explicit verdict:\n{text}"
    );
}

#[test]
fn preflight_aborts_non_zero_when_the_requirement_exceeds_free_vram() {
    let config = shipped_config();
    let output = run(&[
        "train",
        "--preflight",
        "--config",
        &config.display().to_string(),
        // Far above any consumer GPU: forces the insufficient branch deterministically,
        // which is the only way this code path is ever exercised on an idle GPU.
        "--require-mib",
        "999999",
    ]);
    assert_no_panic(&output);
    let text = stdout_of(&output);

    assert!(
        text.contains("verdict=INSUFFICIENT-aborting"),
        "an impossible requirement must abort:\n{text}"
    );
    assert!(
        text.contains("required_vram_mib=999999"),
        "the overridden requirement must be reported:\n{text}"
    );
    assert!(
        !output.status.success(),
        "aborting preflight must exit non-zero"
    );
}

#[test]
fn waiting_preflight_blocks_then_gives_up_at_the_deadline() {
    let config = shipped_config();
    let output = run(&[
        "train",
        "--preflight",
        "--config",
        &config.display().to_string(),
        "--require-mib",
        "999999",
        "--wait",
        "--wait-timeout",
        "3",
        "--poll-interval",
        "1",
    ]);
    assert_no_panic(&output);
    let text = stdout_of(&output);

    assert!(
        text.contains("verdict=INSUFFICIENT-waiting"),
        "--wait must report that it is waiting rather than aborting immediately:\n{text}"
    );
    assert!(
        text.contains("verdict=INSUFFICIENT-aborting"),
        "--wait must still abort once the deadline passes:\n{text}"
    );
    assert!(
        !output.status.success(),
        "a wait that times out must exit non-zero"
    );
}

#[test]
fn preflight_never_names_a_process_to_kill() {
    let config = shipped_config();
    let output = run(&[
        "train",
        "--preflight",
        "--config",
        &config.display().to_string(),
        "--require-mib",
        "999999",
    ]);
    let text = stdout_of(&output).to_ascii_lowercase();
    for forbidden in ["kill", "taskkill", "stop-process", "terminate"] {
        assert!(
            !text.contains(forbidden),
            "preflight must never suggest killing a foreign GPU process, found '{forbidden}':\n{text}"
        );
    }
}

#[test]
fn a_missing_config_is_a_clear_error_not_a_panic() {
    let output = run(&[
        "train",
        "--preflight",
        "--config",
        "does-not-exist-anywhere.toml",
    ]);
    assert_no_panic(&output);
    assert!(!output.status.success(), "a missing config must fail");
    assert!(
        stderr_of(&output).contains("does-not-exist-anywhere.toml"),
        "the error must name the file:\n{}",
        stderr_of(&output)
    );
}

#[test]
fn the_shipped_rung_one_config_matches_the_ladder_specification() {
    let text = std::fs::read_to_string(shipped_config()).expect("config reads");
    let config = mf_uci::parse_train_config(&text).expect("the shipped config parses");

    assert_eq!(config.hidden_size, 1024, "rung 1 is (768 -> 1024)x2 -> 1");
    assert_eq!(config.quantisation_a, 255);
    assert_eq!(config.quantisation_b, 64);
    assert_eq!(config.eval_scale, 400.0);
    assert_eq!(
        config.wdl_lambda, 0.0,
        "the bootstrap corpus has no game results, so rung 1 is pure eval"
    );
}
