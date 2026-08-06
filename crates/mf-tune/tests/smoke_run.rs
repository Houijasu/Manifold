//! End-to-end smoke: the real `mf-tune` binary, the real release engine, real fastchess,
//! real games.
//!
//! Everything below this is unit-tested against a synthetic arena, which proves the
//! update and the resume but says nothing about whether the spins the tuner emits are
//! spins the engine accepts, or whether the PGN fastchess actually writes is the PGN the
//! scorer parses. That is what this test is for, and it is why it insists on the *release*
//! engine: a debug engine at a 5+0.05 tuning time control loses on time, and the run would
//! then fail for a reason that has nothing to do with the tuner.
//!
//! `#[ignore]` because it plays real games (a few minutes), following the same convention
//! as `mf-nnue`'s 10k parity gate. Run it with:
//!
//! ```text
//! cargo test -p mf-tune --test smoke_run -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/mf-tune is two levels below the workspace root")
        .to_path_buf()
}

#[test]
#[ignore = "plays real games against the release engine; takes a few minutes"]
fn a_two_parameter_two_iteration_run_completes_and_checkpoints() {
    let root = workspace_root();
    let engine = root.join("target/release/manifold.exe");
    let fastchess = root.join("tools/fastchess/fastchess.exe");
    let book = root.join("tools/books/UHO_4060_v4.epd");
    for path in [&engine, &fastchess, &book] {
        assert!(
            path.exists(),
            "{} is missing; build the release engine first",
            path.display()
        );
    }

    let out = root.join("target/mf-tune-smoke");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("scratch directory");
    let config_path = out.join("tune.toml");

    // Two parameters from the top of the M5-F3 sensitivity ranking, so the arms really do
    // play differently. A short horizon keeps the gains large enough that two iterations
    // visibly move theta.
    std::fs::write(
        &config_path,
        format!(
            concat!(
                "engine = \"{}\"\n",
                "fastchess = \"{}\"\n",
                "book = \"{}\"\n",
                "iterations = 20\n",
                "games_per_iteration = 4\n",
                "time_control = \"5+0.05\"\n",
                "hash = 16\n",
                "threads = 1\n",
                "seed = 20260807\n",
                "\n[[param]]\nname = \"LmrCoefficient\"\nc_end = 200.0\n",
                "\n[[param]]\nname = \"RfpMarginPerDepth\"\nc_end = 20.0\n",
            ),
            engine.display().to_string().replace('\\', "/"),
            fastchess.display().to_string().replace('\\', "/"),
            book.display().to_string().replace('\\', "/"),
        ),
    )
    .expect("config is written");

    let output = Command::new(env!("CARGO_BIN_EXE_mf-tune"))
        .args([
            "run",
            "--config",
            &config_path.display().to_string(),
            "--out",
            &out.display().to_string(),
            "--iterations",
            "2",
        ])
        .current_dir(&root)
        .output()
        .expect("mf-tune starts");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "mf-tune failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("iteration 1/2"), "{stdout}");
    assert!(stdout.contains("iteration 2/2"), "{stdout}");
    assert!(
        stdout.contains("finished after 2 iteration(s), 8 games"),
        "{stdout}"
    );

    // The checkpoint is the thing a multi-hour session depends on.
    let checkpoint = std::fs::read_to_string(out.join("checkpoint.toml")).expect("a checkpoint");
    assert!(checkpoint.contains("completed = 2"), "{checkpoint}");
    assert!(checkpoint.contains("games_played = 8"), "{checkpoint}");
    assert!(checkpoint.contains("LmrCoefficient"), "{checkpoint}");
    assert!(checkpoint.contains("RfpMarginPerDepth"), "{checkpoint}");

    let history = std::fs::read_to_string(out.join("history.csv")).expect("a history log");
    let lines: Vec<&str> = history.lines().collect();
    assert_eq!(lines.len(), 3, "header plus two iterations: {history}");
    assert!(lines[0].starts_with("iteration,wins,losses,draws,score,"));

    // Real games really happened, with both arms present and the affinity policy applied.
    for iteration in 1..=2 {
        let pgn = std::fs::read_to_string(out.join(format!("iteration-{iteration:06}.pgn")))
            .expect("the iteration's games");
        assert_eq!(
            pgn.matches("[Result ").count(),
            4,
            "iteration {iteration} should have played 4 games"
        );
        assert!(pgn.contains("[White \"plus\"]"), "iteration {iteration}");
        assert!(pgn.contains("[White \"minus\"]"), "iteration {iteration}");
        assert!(
            !pgn.contains("time forfeit"),
            "iteration {iteration} forfeited on time, which invalidates the batch"
        );
    }

    // Resuming runs only the iterations that are missing.
    let output = Command::new(env!("CARGO_BIN_EXE_mf-tune"))
        .args([
            "run",
            "--config",
            &config_path.display().to_string(),
            "--out",
            &out.display().to_string(),
            "--iterations",
            "3",
        ])
        .current_dir(&root)
        .output()
        .expect("mf-tune starts");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "resume failed:\n{stdout}");
    assert!(stdout.contains("resuming at iteration 3"), "{stdout}");
    assert!(
        !stdout.contains("iteration 1/3"),
        "a resume must not replay: {stdout}"
    );
    assert!(stdout.contains("iteration 3/3"), "{stdout}");

    let history = std::fs::read_to_string(out.join("history.csv")).expect("a history log");
    assert_eq!(
        history.lines().count(),
        4,
        "the resume must append one row, not restart the log: {history}"
    );
}
