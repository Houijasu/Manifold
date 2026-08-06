//! Playing one iteration's games and turning them into a number.
//!
//! ## The affinity policy is derived, never configured
//!
//! `AGENTS.md` records a ~600 Elo measurement artifact produced by a single misplaced
//! `-use-affinity`, and `harness/run_match.ps1` refuses to run rather than emit a number
//! under the wrong flag. Tuning batches bypass that script — thousands of them at a few
//! seconds each would spend most of their wall clock in PowerShell startup and provenance
//! hashing — so the same rule is reimplemented here, in the same refusing form:
//!
//! * both engines at `Threads=1`  → `-use-affinity`, `-concurrency 8`
//! * either engine above 1 thread → no affinity, `-concurrency 1`
//!
//! Both arms of an SPSA iteration are the same binary with different spins, so the thread
//! count is always the same on both sides and one setting decides it. The policy is a
//! function of the thread count only, and it is not reachable from the config.
//!
//! ## The result is win-minus-loss, not Elo
//!
//! SPSA needs a signed comparison between two arms, and at eight games per iteration
//! every Elo estimate is noise anyway. Wins minus losses over the batch is the raw signal
//! the update is scaled for, and it is read out of the PGN rather than off the fastchess
//! summary: the summary reports Elo with error bars whose parse would break with the next
//! fastchess release, whereas the `[Result]` tag has been stable for decades.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::MatchSettings;

/// What one iteration's games measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchResult {
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    /// Games where an engine lost on time. Non-zero means the batch is not measuring
    /// strength, and the run stops.
    pub forfeits: u32,
}

impl BatchResult {
    pub fn games(&self) -> u32 {
        self.wins + self.losses + self.draws
    }

    /// The SPSA measurement: how much better the `plus` arm was, in game points.
    pub fn score(&self) -> f64 {
        f64::from(self.wins) - f64::from(self.losses)
    }
}

/// The engine names given to fastchess, and therefore what appears in the PGN.
pub const PLUS_ARM: &str = "plus";
pub const MINUS_ARM: &str = "minus";

/// Assembles the fastchess argument vector for one iteration.
///
/// Separated from running it so the guardrail can be tested without playing games, which
/// is the only way to test a rule whose whole purpose is to be present in every run.
pub fn batch_arguments(
    settings: &MatchSettings,
    names: &[String],
    plus: &[i32],
    minus: &[i32],
    seed: u64,
    pgn: &Path,
) -> Vec<String> {
    let (use_affinity, concurrency) = affinity_policy(settings.threads);
    let engine_path = settings.engine.display().to_string();

    let mut arguments = Vec::new();
    for (name, spins) in [(PLUS_ARM, plus), (MINUS_ARM, minus)] {
        arguments.push("-engine".to_string());
        arguments.push(format!("cmd={engine_path}"));
        arguments.push(format!("name={name}"));
        arguments.push(format!("option.Hash={}", settings.hash_mebibytes));
        arguments.push(format!("option.Threads={}", settings.threads));
        for (parameter, value) in names.iter().zip(spins) {
            arguments.push(format!("option.{parameter}={value}"));
        }
        for option in &settings.extra_options {
            arguments.push(option.clone());
        }
    }

    arguments.extend(
        [
            "-each",
            "proto=uci",
            &format!("tc={}", settings.time_control),
            "-openings",
            &format!("file={}", settings.book.display()),
            "format=epd",
            "order=random",
            "-repeat",
            "-games",
            "2",
            "-rounds",
            &(settings.games_per_iteration / 2).to_string(),
            "-concurrency",
            &concurrency.to_string(),
            "-srand",
            &seed.to_string(),
            "-pgnout",
            &format!("file={}", pgn.display()),
            "append=false",
        ]
        .into_iter()
        .map(str::to_string),
    );
    if use_affinity {
        arguments.push("-use-affinity".to_string());
    }
    arguments
}

/// `(use_affinity, concurrency)` for a batch whose engines each run `threads` threads.
pub fn affinity_policy(threads: u32) -> (bool, u32) {
    if threads > 1 { (false, 1) } else { (true, 8) }
}

/// Plays one iteration and returns the plus arm's margin.
pub fn play(
    settings: &MatchSettings,
    names: &[String],
    plus: &[i32],
    minus: &[i32],
    seed: u64,
    pgn: &Path,
) -> Result<BatchResult, String> {
    let arguments = batch_arguments(settings, names, plus, minus, seed, pgn);
    if let Some(parent) = pgn.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let _ = std::fs::remove_file(pgn);

    let status = Command::new(&settings.fastchess)
        .args(&arguments)
        // fastchess prints a game-by-game log that would bury the tuner's own progress
        // output, and there is nothing in it the PGN does not also carry.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            format!(
                "cannot start fastchess at {}: {error}",
                settings.fastchess.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "fastchess exited with {status}; command: {} {}",
            settings.fastchess.display(),
            arguments.join(" ")
        ));
    }

    let pgn_text = std::fs::read_to_string(pgn)
        .map_err(|error| format!("cannot read {}: {error}", pgn.display()))?;
    let result = score_pgn(&pgn_text);
    if result.games() < settings.games_per_iteration {
        return Err(format!(
            "expected {} games but {} finished; see {}",
            settings.games_per_iteration,
            result.games(),
            pgn.display()
        ));
    }
    Ok(result)
}

/// Counts the plus arm's wins, losses and draws out of a PGN.
pub fn score_pgn(text: &str) -> BatchResult {
    let mut result = BatchResult {
        wins: 0,
        losses: 0,
        draws: 0,
        forfeits: 0,
    };
    let mut plus_is_white = None;
    let mut outcome: Option<&str> = None;

    for line in text.lines() {
        if let Some(name) = tag_value(line, "White") {
            // A new game's header: the previous game is complete.
            flush(&mut result, &mut plus_is_white, &mut outcome);
            plus_is_white = Some(name == PLUS_ARM);
        } else if let Some(value) = tag_value(line, "Result") {
            outcome = match value {
                "1-0" => Some("1-0"),
                "0-1" => Some("0-1"),
                "1/2-1/2" => Some("1/2-1/2"),
                _ => None,
            };
        } else if let Some(value) = tag_value(line, "Termination")
            && value == "time forfeit"
        {
            result.forfeits += 1;
        }
    }
    flush(&mut result, &mut plus_is_white, &mut outcome);
    result
}

fn flush(result: &mut BatchResult, plus_is_white: &mut Option<bool>, outcome: &mut Option<&str>) {
    if let (Some(plus_is_white), Some(outcome)) = (plus_is_white.take(), outcome.take()) {
        match (outcome, plus_is_white) {
            ("1-0", true) | ("0-1", false) => result.wins += 1,
            ("0-1", true) | ("1-0", false) => result.losses += 1,
            _ => result.draws += 1,
        }
    }
    *plus_is_white = None;
    *outcome = None;
}

/// Reads `[Name "value"]`, which is the whole of the PGN header syntax used here.
fn tag_value<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (name, value) = rest.split_once(' ')?;
    if name != tag {
        return None;
    }
    value.strip_prefix('"')?.strip_suffix('"')
}

/// Where an iteration's games are written. Iteration-numbered so a crash leaves the
/// offending games on disk rather than overwriting them on resume.
pub fn pgn_path(run_directory: &Path, iteration: u64) -> PathBuf {
    run_directory.join(format!("iteration-{iteration:06}.pgn"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{MINUS_ARM, PLUS_ARM, affinity_policy, batch_arguments, score_pgn};
    use crate::config::MatchSettings;

    fn settings(threads: u32) -> MatchSettings {
        MatchSettings {
            engine: PathBuf::from("target/release/manifold.exe"),
            fastchess: PathBuf::from("tools/fastchess/fastchess.exe"),
            book: PathBuf::from("tools/books/UHO_4060_v4.epd"),
            games_per_iteration: 8,
            time_control: "5+0.05".to_string(),
            hash_mebibytes: 16,
            threads,
            extra_options: Vec::new(),
        }
    }

    fn game(white: &str, black: &str, result: &str) -> String {
        format!(
            "[White \"{white}\"]\n[Black \"{black}\"]\n[Result \"{result}\"]\n\n1. e4 e5 {result}\n\n"
        )
    }

    #[test]
    fn a_single_threaded_batch_pins_and_a_multi_threaded_one_must_not() {
        // The whole point of AGENTS.md 4.451: this pairing is not a preference.
        assert_eq!(affinity_policy(1), (true, 8));
        assert_eq!(affinity_policy(2), (false, 1));
        assert_eq!(affinity_policy(8), (false, 1));

        let names = vec!["LmrCoefficient".to_string()];
        let one = batch_arguments(
            &settings(1),
            &names,
            &[2900],
            &[2800],
            5,
            Path::new("games.pgn"),
        );
        assert!(one.contains(&"-use-affinity".to_string()));
        assert_eq!(concurrency_of(&one), "8");

        let eight = batch_arguments(
            &settings(8),
            &names,
            &[2900],
            &[2800],
            5,
            Path::new("games.pgn"),
        );
        assert!(
            !eight.contains(&"-use-affinity".to_string()),
            "pinning a Threads>1 engine oversubscribes it into time forfeits"
        );
        assert_eq!(concurrency_of(&eight), "1");
    }

    fn concurrency_of(arguments: &[String]) -> &str {
        let index = arguments
            .iter()
            .position(|argument| argument == "-concurrency")
            .expect("every batch sets concurrency");
        &arguments[index + 1]
    }

    #[test]
    fn both_arms_get_the_same_binary_and_settings_and_differ_only_in_the_tuned_spins() {
        let names = vec!["LmrCoefficient".to_string(), "LmrBase".to_string()];
        let arguments = batch_arguments(
            &settings(1),
            &names,
            &[2900, 1000],
            &[2800, 964],
            5,
            Path::new("games.pgn"),
        );
        let engines: Vec<&[String]> = arguments
            .split(|argument| argument == "-engine")
            .skip(1)
            .collect();
        assert_eq!(engines.len(), 2);

        let plus: Vec<&String> = engines[0]
            .iter()
            .take_while(|a| !a.starts_with('-'))
            .collect();
        let minus: Vec<&String> = engines[1]
            .iter()
            .take_while(|a| !a.starts_with('-'))
            .collect();
        assert!(plus.contains(&&format!("name={PLUS_ARM}")));
        assert!(minus.contains(&&format!("name={MINUS_ARM}")));
        assert!(plus.contains(&&"cmd=target/release/manifold.exe".to_string()));
        assert!(minus.contains(&&"cmd=target/release/manifold.exe".to_string()));
        assert!(plus.contains(&&"option.LmrCoefficient=2900".to_string()));
        assert!(plus.contains(&&"option.LmrBase=1000".to_string()));
        assert!(minus.contains(&&"option.LmrCoefficient=2800".to_string()));
        assert!(minus.contains(&&"option.LmrBase=964".to_string()));
        assert!(plus.contains(&&"option.Hash=16".to_string()));
        assert!(minus.contains(&&"option.Hash=16".to_string()));
    }

    #[test]
    fn the_round_count_is_half_the_game_count_because_openings_are_played_twice() {
        let mut settings = settings(1);
        settings.games_per_iteration = 12;
        let arguments = batch_arguments(
            &settings,
            &["LmrBase".to_string()],
            &[1000],
            &[964],
            5,
            Path::new("games.pgn"),
        );
        let index = arguments.iter().position(|a| a == "-rounds").unwrap();
        assert_eq!(arguments[index + 1], "6");
        assert!(arguments.contains(&"-repeat".to_string()));
        let index = arguments.iter().position(|a| a == "-srand").unwrap();
        assert_eq!(arguments[index + 1], "5");
    }

    #[test]
    fn scoring_counts_the_plus_arm_from_both_colours() {
        let pgn = game(PLUS_ARM, MINUS_ARM, "1-0")      // plus wins as white
            + &game(MINUS_ARM, PLUS_ARM, "0-1")          // plus wins as black
            + &game(PLUS_ARM, MINUS_ARM, "0-1")          // plus loses as white
            + &game(MINUS_ARM, PLUS_ARM, "1-0")          // plus loses as black
            + &game(PLUS_ARM, MINUS_ARM, "1/2-1/2");
        let result = score_pgn(&pgn);
        assert_eq!(result.wins, 2);
        assert_eq!(result.losses, 2);
        assert_eq!(result.draws, 1);
        assert_eq!(result.games(), 5);
        assert_eq!(result.score(), 0.0);
    }

    #[test]
    fn a_batch_the_plus_arm_swept_scores_the_whole_batch() {
        let pgn = (0..4)
            .map(|index| {
                if index % 2 == 0 {
                    game(PLUS_ARM, MINUS_ARM, "1-0")
                } else {
                    game(MINUS_ARM, PLUS_ARM, "0-1")
                }
            })
            .collect::<String>();
        let result = score_pgn(&pgn);
        assert_eq!((result.wins, result.losses, result.draws), (4, 0, 0));
        assert_eq!(result.score(), 4.0);
        assert_eq!(
            score_pgn(
                &pgn.replace("1-0", "TMP")
                    .replace("0-1", "1-0")
                    .replace("TMP", "0-1")
            )
            .score(),
            -4.0
        );
    }

    #[test]
    fn a_time_forfeit_is_counted_so_the_run_can_refuse_to_learn_from_it() {
        let pgn = format!(
            "[White \"{PLUS_ARM}\"]\n[Black \"{MINUS_ARM}\"]\n[Result \"0-1\"]\n\
             [Termination \"time forfeit\"]\n\n1. e4 0-1\n\n"
        ) + &game(PLUS_ARM, MINUS_ARM, "1/2-1/2");
        let result = score_pgn(&pgn);
        assert_eq!(result.forfeits, 1);
        assert_eq!(result.games(), 2);
    }

    #[test]
    fn an_empty_or_headerless_pgn_scores_zero_games_rather_than_panicking() {
        assert_eq!(score_pgn("").games(), 0);
        assert_eq!(score_pgn("1. e4 e5 1/2-1/2\n").games(), 0);
        // A game whose header was written but whose result was not (a killed batch)
        // must not be counted as a draw.
        assert_eq!(
            score_pgn(&format!(
                "[White \"{PLUS_ARM}\"]\n[Black \"{MINUS_ARM}\"]\n"
            ))
            .games(),
            0
        );
    }

    #[test]
    fn real_fastchess_headers_from_this_repo_are_parsed() {
        // Copied from experiments/MSN-M5-sweep/UseLMR/games.pgn, renamed to the tuner's
        // arms: the parser must cope with the full tag set fastchess actually writes.
        let pgn = format!(
            "[Event \"Fastchess Tournament\"]\n[Site \"?\"]\n[Date \"2026.08.06\"]\n\
             [Round \"1\"]\n[White \"{PLUS_ARM}\"]\n[Black \"{MINUS_ARM}\"]\n\
             [Result \"1/2-1/2\"]\n[SetUp \"1\"]\n[FEN \"8/8/8/8/8/8/8/K6k w - - 0 1\"]\n\
             [GameDuration \"00:00:16\"]\n[PlyCount \"57\"]\n[Termination \"normal\"]\n\
             [TimeControl \"8+0.08\"]\n\n9. Bxc6 {{+0.66/10 0.294s}} 1/2-1/2\n\n"
        );
        let result = score_pgn(&pgn);
        assert_eq!(
            (result.wins, result.losses, result.draws, result.forfeits),
            (0, 0, 1, 0)
        );
    }
}
