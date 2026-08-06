//! The tuning loop: perturb, play, update, checkpoint, repeat.
//!
//! The loop is generic over what plays a batch. The real implementation shells out to
//! fastchess; a test substitutes a synthetic objective and drives thousands of iterations
//! in milliseconds. That is the only way the resume path gets tested at all — a test that
//! had to play real games to reach iteration 2 could never assert what iteration 500 of a
//! resumed run does.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::batch::{BatchResult, pgn_path, play};
use crate::checkpoint::Checkpoint;
use crate::config::TuningConfig;
use crate::spsa::Spsa;

/// Plays one iteration's games. `plus` and `minus` are the two arms' spins, in the
/// config's parameter order.
pub trait Arena {
    fn play(
        &mut self,
        iteration: u64,
        names: &[String],
        plus: &[i32],
        minus: &[i32],
    ) -> Result<BatchResult, String>;
}

/// The real arena: fastchess, one process per iteration.
pub struct FastchessArena {
    config: TuningConfig,
    run_directory: PathBuf,
}

impl FastchessArena {
    pub fn new(config: TuningConfig, run_directory: PathBuf) -> Self {
        Self {
            config,
            run_directory,
        }
    }
}

impl Arena for FastchessArena {
    fn play(
        &mut self,
        iteration: u64,
        names: &[String],
        plus: &[i32],
        minus: &[i32],
    ) -> Result<BatchResult, String> {
        play(
            &self.config.match_settings,
            names,
            plus,
            minus,
            // A per-iteration seed, so consecutive iterations do not replay the same
            // openings and measure the same few positions over and over.
            self.config.seed.wrapping_add(iteration),
            &pgn_path(&self.run_directory, iteration),
        )
    }
}

/// Why the loop stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The configured iteration budget was reached.
    Finished,
    /// Ctrl+C. The checkpoint is current; rerunning resumes.
    Interrupted,
}

/// A completed (or interrupted) run.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub stop: Stop,
    pub completed: u64,
    pub games_played: u64,
    pub theta: Vec<f64>,
    pub spins: Vec<i32>,
}

/// The paths a run reads and writes.
pub struct RunPaths {
    pub checkpoint: PathBuf,
    pub history: PathBuf,
}

impl RunPaths {
    pub fn in_directory(directory: &Path) -> Self {
        Self {
            checkpoint: directory.join("checkpoint.toml"),
            history: directory.join("history.csv"),
        }
    }
}

/// Runs the loop, resuming from `paths.checkpoint` when one exists.
///
/// `should_stop` is checked between iterations; in production it is
/// [`interrupt::interrupted`]. It is a parameter rather than a direct call so that the
/// interrupt path is testable — the flag behind it is process-wide, and a test that set
/// it would stop every other test in the binary.
pub fn run<A: Arena>(
    config: &TuningConfig,
    paths: &RunPaths,
    arena: &mut A,
    should_stop: &dyn Fn() -> bool,
    mut progress: impl Write,
) -> Result<RunOutcome, String> {
    let mut spsa = Spsa::new(
        config.schedule,
        config.dimensions.clone(),
        config.start.clone(),
    )?;
    let names: Vec<String> = config
        .dimensions
        .iter()
        .map(|dimension| dimension.name.clone())
        .collect();

    let mut state = match Checkpoint::read(&paths.checkpoint)? {
        Some(checkpoint) => {
            spsa.set_theta(checkpoint.theta_for(&config.dimensions)?)?;
            let _ = writeln!(
                progress,
                "resuming at iteration {} of {} ({} games played)",
                checkpoint.completed + 1,
                config.budget,
                checkpoint.games_played
            );
            checkpoint
        }
        None => {
            // The header is written only for a fresh run: a resumed run appends to the
            // history it already has.
            write_history_header(&paths.history, &names)?;
            Checkpoint::new(&config.dimensions, spsa.theta())
        }
    };

    while state.completed < config.budget {
        if should_stop() {
            let _ = writeln!(
                progress,
                "interrupted after iteration {}; rerun to resume",
                state.completed
            );
            return Ok(outcome(Stop::Interrupted, &state, &spsa));
        }

        let iteration = state.completed + 1;
        let perturbation = spsa.perturbation(iteration, config.seed);
        let result = arena.play(iteration, &names, &perturbation.plus, &perturbation.minus)?;
        if result.forfeits > 0 {
            return Err(format!(
                "iteration {iteration}: {} game(s) lost on time. A batch with a forfeit is \
                 not measuring strength, and learning from it would move theta on a \
                 harness fault. The checkpoint is current; fix the cause and rerun to \
                 resume.",
                result.forfeits
            ));
        }

        spsa.apply(iteration, &perturbation, result.score());
        state.completed = iteration;
        state.games_played += u64::from(result.games());
        state.theta = config
            .dimensions
            .iter()
            .zip(spsa.theta())
            .map(|(dimension, value)| (dimension.name.clone(), *value))
            .collect();
        // Checkpoint BEFORE the history line: the checkpoint is what a resume trusts, and
        // a history row for an iteration the checkpoint does not know about is merely
        // duplicated, whereas the reverse silently replays a batch.
        state.write(&paths.checkpoint)?;
        append_history(
            &paths.history,
            iteration,
            &result,
            spsa.theta(),
            &spsa.spins(),
        )?;

        let _ = writeln!(
            progress,
            "iteration {iteration}/{} +{} ={} -{} score {:+.0} theta [{}]",
            config.budget,
            result.wins,
            result.draws,
            result.losses,
            result.score(),
            spsa.spins()
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(outcome(Stop::Finished, &state, &spsa))
}

fn outcome(stop: Stop, state: &Checkpoint, spsa: &Spsa) -> RunOutcome {
    RunOutcome {
        stop,
        completed: state.completed,
        games_played: state.games_played,
        theta: spsa.theta().to_vec(),
        spins: spsa.spins(),
    }
}

fn write_history_header(path: &Path, names: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut header = String::from("iteration,wins,losses,draws,score");
    for name in names {
        header.push_str(&format!(",{name},{name}_spin"));
    }
    header.push('\n');
    std::fs::write(path, header)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn append_history(
    path: &Path,
    iteration: u64,
    result: &BatchResult,
    theta: &[f64],
    spins: &[i32],
) -> Result<(), String> {
    let mut row = format!(
        "{iteration},{},{},{},{}",
        result.wins,
        result.losses,
        result.draws,
        result.score()
    );
    for (value, spin) in theta.iter().zip(spins) {
        row.push_str(&format!(",{value:.4},{spin}"));
    }
    row.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    file.write_all(row.as_bytes())
        .map_err(|error| format!("cannot append to {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Arena, RunPaths, Stop, run};
    use crate::batch::BatchResult;
    use crate::checkpoint::Checkpoint;
    use crate::config::TuningConfig;

    const CONFIG: &str = concat!(
        "engine = \"target/release/manifold.exe\"\n",
        "fastchess = \"tools/fastchess/fastchess.exe\"\n",
        "book = \"tools/books/UHO_4060_v4.epd\"\n",
        "iterations = 400\n",
        "games_per_iteration = 8\n",
        "seed = 20260807\n",
        "[[param]]\n",
        "name = \"LmrCoefficient\"\n",
        "value = 5500\n",
        "c_end = 100.0\n",
        "[[param]]\n",
        "name = \"RfpMarginPerDepth\"\n",
        "value = 105\n",
        "c_end = 5.0\n",
    );

    /// An arena whose "games" are a deterministic function of the spins: the plus arm
    /// wins in proportion to how much closer it is to a hidden optimum.
    struct SyntheticArena {
        targets: [f64; 2],
        iterations_seen: Vec<u64>,
        forfeit_at: Option<u64>,
    }

    impl SyntheticArena {
        fn new() -> Self {
            Self {
                targets: [2_500.0, 250.0],
                iterations_seen: Vec::new(),
                forfeit_at: None,
            }
        }

        fn quality(&self, spins: &[i32]) -> f64 {
            -((f64::from(spins[0]) - self.targets[0]).powi(2) / 400_000.0)
                - ((f64::from(spins[1]) - self.targets[1]).powi(2) / 4_000.0)
        }
    }

    impl Arena for SyntheticArena {
        fn play(
            &mut self,
            iteration: u64,
            names: &[String],
            plus: &[i32],
            minus: &[i32],
        ) -> Result<BatchResult, String> {
            assert_eq!(names, ["LmrCoefficient", "RfpMarginPerDepth"]);
            self.iterations_seen.push(iteration);
            if self.forfeit_at == Some(iteration) {
                return Ok(BatchResult {
                    wins: 3,
                    losses: 4,
                    draws: 1,
                    forfeits: 1,
                });
            }
            let margin = 8.0 * (self.quality(plus) - self.quality(minus));
            let wins = margin.max(0.0).round().min(8.0) as u32;
            let losses = (-margin).max(0.0).round().min(8.0) as u32;
            Ok(BatchResult {
                wins,
                losses,
                draws: 8 - wins - losses,
                forfeits: 0,
            })
        }
    }

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mf-tune-run-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    #[test]
    fn a_complete_run_converges_checkpoints_and_logs_every_iteration() {
        let directory = scratch("complete");
        let paths = RunPaths::in_directory(&directory);
        let config = TuningConfig::parse(CONFIG, "test.toml").unwrap();
        let mut arena = SyntheticArena::new();

        let outcome =
            run(&config, &paths, &mut arena, &|| false, Vec::new()).expect("run succeeds");
        assert_eq!(outcome.stop, Stop::Finished);
        assert_eq!(outcome.completed, 400);
        assert_eq!(outcome.games_played, 400 * 8);
        // The RATE of convergence is `spsa`'s claim, tested there over 4000 iterations.
        // What the loop must show is that the sign is wired up end to end: 400 iterations
        // of real batches move theta a long way TOWARDS the arena's hidden optimum and
        // never past it.
        assert!(
            (2_500.0..5_500.0).contains(&outcome.theta[0]),
            "theta should be between the start and the optimum, got {}",
            outcome.theta[0]
        );
        assert!(
            5_500.0 - outcome.theta[0] > 500.0,
            "400 iterations should close a substantial part of the gap, got {}",
            outcome.theta[0]
        );

        let checkpoint = Checkpoint::read(&paths.checkpoint).unwrap().unwrap();
        assert_eq!(checkpoint.completed, 400);
        assert_eq!(
            checkpoint.theta_for(&config.dimensions).unwrap(),
            outcome.theta
        );

        let history = std::fs::read_to_string(&paths.history).unwrap();
        let lines: Vec<&str> = history.lines().collect();
        assert_eq!(lines.len(), 401, "one header plus one row per iteration");
        assert_eq!(
            lines[0],
            "iteration,wins,losses,draws,score,LmrCoefficient,LmrCoefficient_spin,\
             RfpMarginPerDepth,RfpMarginPerDepth_spin"
        );
        assert!(lines[1].starts_with("1,"));
        assert!(lines[400].starts_with("400,"));
        assert_eq!(lines[400].split(',').count(), 9);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_resumed_run_continues_from_the_checkpoint_and_reproduces_the_uninterrupted_result() {
        let uninterrupted = scratch("uninterrupted");
        let config = TuningConfig::parse(CONFIG, "test.toml").unwrap();
        let whole = run(
            &config,
            &RunPaths::in_directory(&uninterrupted),
            &mut SyntheticArena::new(),
            &|| false,
            Vec::new(),
        )
        .expect("run succeeds");

        // The same run cut into three pieces, each one a fresh process would-be.
        let resumed = scratch("resumed");
        let paths = RunPaths::in_directory(&resumed);
        let mut arena = SyntheticArena::new();
        let mut seen = Vec::new();
        for budget in [50_u64, 130, 400] {
            let mut piece = config.clone();
            // The BUDGET moves; the schedule must not, or the gains would be re-derived
            // from a shorter horizon and each piece would step differently.
            piece.set_budget(budget).expect("budget inside the horizon");
            assert_eq!(piece.schedule, config.schedule);
            let outcome =
                run(&piece, &paths, &mut arena, &|| false, Vec::new()).expect("piece succeeds");
            assert_eq!(outcome.completed, budget);
            seen.append(&mut arena.iterations_seen);
        }

        // Every iteration ran exactly once: no batch replayed, none skipped.
        assert_eq!(seen, (1..=400).collect::<Vec<u64>>());

        let after = Checkpoint::read(&paths.checkpoint).unwrap().unwrap();
        assert_eq!(after.completed, 400);
        assert_eq!(after.games_played, 400 * 8);
        assert_eq!(
            after.theta_for(&config.dimensions).unwrap(),
            whole.theta,
            "a run resumed twice must land on exactly the same theta as one that was \
             never interrupted"
        );

        let history = std::fs::read_to_string(&paths.history).unwrap();
        assert_eq!(
            history.lines().count(),
            401,
            "resume must append, not restart"
        );

        let _ = std::fs::remove_dir_all(&uninterrupted);
        let _ = std::fs::remove_dir_all(&resumed);
    }

    #[test]
    fn a_forfeited_batch_stops_the_run_and_leaves_the_last_good_checkpoint() {
        let directory = scratch("forfeit");
        let paths = RunPaths::in_directory(&directory);
        let config = TuningConfig::parse(CONFIG, "test.toml").unwrap();
        let mut arena = SyntheticArena::new();
        arena.forfeit_at = Some(4);

        let error =
            run(&config, &paths, &mut arena, &|| false, Vec::new()).expect_err("should stop");
        assert!(error.contains("lost on time"), "{error}");
        assert!(error.contains("resume"), "{error}");

        let checkpoint = Checkpoint::read(&paths.checkpoint).unwrap().unwrap();
        assert_eq!(
            checkpoint.completed, 3,
            "the forfeited iteration must not be recorded as completed"
        );
        assert_eq!(checkpoint.games_played, 24);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_interrupt_stops_between_iterations_and_the_checkpoint_resumes_it_exactly() {
        let directory = scratch("interrupt");
        let paths = RunPaths::in_directory(&directory);
        let mut config = TuningConfig::parse(CONFIG, "test.toml").unwrap();
        config.set_budget(20).expect("budget inside the horizon");

        // Ctrl+C arrives during iteration 5; the loop notices before starting 6.
        let stop_after = std::cell::Cell::new(false);
        let mut arena = SyntheticArena::new();
        let outcome = run(
            &config,
            &paths,
            &mut arena,
            &|| stop_after.replace(true),
            Vec::new(),
        )
        .expect("an interrupt is a clean stop, not an error");
        assert_eq!(outcome.stop, Stop::Interrupted);
        assert_eq!(
            outcome.completed, 1,
            "the flag was raised after iteration 1"
        );

        let checkpoint = Checkpoint::read(&paths.checkpoint).unwrap().unwrap();
        assert_eq!(checkpoint.completed, 1);
        assert_eq!(checkpoint.games_played, 8);

        // Rerunning finishes the budget, and every iteration ran exactly once overall.
        let mut seen = std::mem::take(&mut arena.iterations_seen);
        let outcome = run(&config, &paths, &mut arena, &|| false, Vec::new()).expect("resume");
        assert_eq!(outcome.stop, Stop::Finished);
        assert_eq!(outcome.completed, 20);
        seen.append(&mut arena.iterations_seen);
        assert_eq!(seen, (1..=20).collect::<Vec<u64>>());

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_run_whose_budget_is_already_met_does_no_games_and_reports_finished() {
        let directory = scratch("already-done");
        let paths = RunPaths::in_directory(&directory);
        let mut config = TuningConfig::parse(CONFIG, "test.toml").unwrap();
        config.set_budget(3).expect("budget inside the horizon");

        run(
            &config,
            &paths,
            &mut SyntheticArena::new(),
            &|| false,
            Vec::new(),
        )
        .expect("first run");
        let mut arena = SyntheticArena::new();
        let outcome = run(&config, &paths, &mut arena, &|| false, Vec::new()).expect("second run");
        assert_eq!(outcome.stop, Stop::Finished);
        assert_eq!(outcome.completed, 3);
        assert!(
            arena.iterations_seen.is_empty(),
            "a finished run must not replay its last iteration"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_checkpoint_from_another_parameter_set_stops_the_run_rather_than_mismapping_it() {
        let directory = scratch("wrong-checkpoint");
        let paths = RunPaths::in_directory(&directory);
        let config = TuningConfig::parse(CONFIG, "test.toml").unwrap();

        let mut foreign = config.clone();
        foreign.dimensions[1].name = "FutilityBaseMargin".to_string();
        Checkpoint::new(&foreign.dimensions, &[2_872.0, 124.0])
            .write(&paths.checkpoint)
            .unwrap();

        let error = run(
            &config,
            &paths,
            &mut SyntheticArena::new(),
            &|| false,
            Vec::new(),
        )
        .expect_err("should refuse");
        assert!(error.contains("different run"), "{error}");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn progress_output_names_the_iteration_the_score_and_the_current_spins() {
        let directory = scratch("progress");
        let paths = RunPaths::in_directory(&directory);
        let mut config = TuningConfig::parse(CONFIG, "test.toml").unwrap();
        config.set_budget(2).expect("budget inside the horizon");
        let mut sink = Vec::new();

        run(
            &config,
            &paths,
            &mut SyntheticArena::new(),
            &|| false,
            &mut sink,
        )
        .expect("run succeeds");
        let text = String::from_utf8(sink).unwrap();
        assert!(text.contains("iteration 1/2"), "{text}");
        assert!(text.contains("iteration 2/2"), "{text}");
        assert!(text.contains("theta ["), "{text}");

        let mut sink = Vec::new();
        config.set_budget(3).expect("budget inside the horizon");
        run(
            &config,
            &paths,
            &mut SyntheticArena::new(),
            &|| false,
            &mut sink,
        )
        .expect("resume succeeds");
        let text = String::from_utf8(sink).unwrap();
        assert!(text.contains("resuming at iteration 3"), "{text}");

        let _ = std::fs::remove_dir_all(&directory);
    }
}
