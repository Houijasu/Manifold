//! The `mf-tune` command line.
//!
//! Two subcommands: `init` writes a starter config from the engine's own parameter table,
//! `run` executes (or resumes) a tuning session. Resume is not a flag — rerunning `run`
//! against a directory that already holds a checkpoint continues it. A run that had to be
//! told to resume would eventually be restarted by someone who forgot the flag, silently
//! discarding hours of games.

use std::io::Write;
use std::path::PathBuf;

use crate::config::{TuningConfig, starter_config};
use crate::interrupt;
use crate::run::{FastchessArena, RunPaths, Stop, run};

pub fn run_cli<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    if arguments.is_empty()
        || arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        writeln!(writer, "{}", help()).map_err(|error| error.to_string())?;
        return Ok(());
    }

    match arguments[0].as_str() {
        "init" => run_init(&arguments[1..], writer),
        "run" => run_session(&arguments[1..], writer),
        unknown => Err(usage(&format!("unknown subcommand '{unknown}'"))),
    }
}

fn run_init<W: Write>(arguments: &[String], mut writer: W) -> Result<(), String> {
    let mut out: Option<PathBuf> = None;
    let mut names = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--out" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| usage("--out requires a path"))?;
                if out.replace(PathBuf::from(value)).is_some() {
                    return Err(usage("--out given twice"));
                }
            }
            "--params" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| usage("--params requires a comma-separated list"))?;
                names.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string),
                );
            }
            unknown => return Err(usage(&format!("unknown init argument '{unknown}'"))),
        }
        index += 1;
    }

    if names.is_empty() {
        return Err(usage("init requires --params <Name,Name,...>"));
    }
    let text = starter_config(&names)?;
    match out {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
            }
            std::fs::write(&path, &text)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            writeln!(
                writer,
                "wrote {} ({} parameters)",
                path.display(),
                names.len()
            )
            .map_err(|error| error.to_string())
        }
        None => write!(writer, "{text}").map_err(|error| error.to_string()),
    }
}

fn run_session<W: Write>(arguments: &[String], mut writer: W) -> Result<(), String> {
    let mut config_path: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut iterations: Option<u64> = None;
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        index += 1;
        let value = || -> Result<&String, String> {
            arguments
                .get(index)
                .ok_or_else(|| usage(&format!("{flag} requires a value")))
        };
        match flag {
            "--config" => {
                if config_path.replace(PathBuf::from(value()?)).is_some() {
                    return Err(usage("--config given twice"));
                }
                index += 1;
            }
            "--out" => {
                if out.replace(PathBuf::from(value()?)).is_some() {
                    return Err(usage("--out given twice"));
                }
                index += 1;
            }
            "--iterations" => {
                let parsed = value()?
                    .parse::<u64>()
                    .map_err(|_| usage("--iterations must be a positive integer"))?;
                if parsed == 0 {
                    return Err(usage("--iterations must be a positive integer"));
                }
                iterations = Some(parsed);
                index += 1;
            }
            unknown => return Err(usage(&format!("unknown run argument '{unknown}'"))),
        }
    }

    let config_path = config_path.ok_or_else(|| usage("run requires --config <file>"))?;
    let out = out.ok_or_else(|| usage("run requires --out <directory>"))?;

    let mut config = TuningConfig::read(&config_path)?;
    if let Some(iterations) = iterations {
        config.set_budget(iterations)?;
    }
    if !config.match_settings.engine.exists() {
        return Err(format!(
            "engine not found at {}",
            config.match_settings.engine.display()
        ));
    }
    if !config.match_settings.fastchess.exists() {
        return Err(format!(
            "fastchess not found at {}",
            config.match_settings.fastchess.display()
        ));
    }
    if !config.match_settings.book.exists() {
        return Err(format!(
            "opening book not found at {}",
            config.match_settings.book.display()
        ));
    }

    std::fs::create_dir_all(&out)
        .map_err(|error| format!("cannot create {}: {error}", out.display()))?;
    interrupt::install();

    writeln!(
        writer,
        "tuning {} parameter(s) over {} of {} iteration(s) of {} game(s) at tc={} into {}",
        config.dimensions.len(),
        config.budget,
        config.schedule.iterations,
        config.match_settings.games_per_iteration,
        config.match_settings.time_control,
        out.display()
    )
    .map_err(|error| error.to_string())?;

    let paths = RunPaths::in_directory(&out);
    let mut arena = FastchessArena::new(config.clone(), out.clone());
    let outcome = run(
        &config,
        &paths,
        &mut arena,
        &interrupt::interrupted,
        &mut writer,
    )?;

    writeln!(
        writer,
        "{} after {} iteration(s), {} games",
        match outcome.stop {
            Stop::Finished => "finished",
            Stop::Interrupted => "interrupted",
        },
        outcome.completed,
        outcome.games_played
    )
    .map_err(|error| error.to_string())?;
    for (dimension, spin) in config.dimensions.iter().zip(&outcome.spins) {
        writeln!(writer, "  {} = {spin}", dimension.name).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn usage(problem: &str) -> String {
    format!("{problem}\n\n{}", help())
}

fn help() -> String {
    let mut text = String::from(concat!(
        "mf-tune - SPSA tuning for Manifold's search parameters\n\n",
        "USAGE\n",
        "  mf-tune init --params <Name,Name,...> [--out <file>]\n",
        "  mf-tune run --config <file> --out <directory> [--iterations N]\n\n",
        "  init  writes a starter config with each parameter's default and range taken\n",
        "        from the engine's own table. Without --out it is printed.\n",
        "  run   runs, or RESUMES, a tuning session. If <directory> already holds a\n",
        "        checkpoint.toml the run continues from it; there is no resume flag.\n",
        "        --iterations stops early WITHOUT changing the gain schedule, so a short\n",
        "        run is a prefix of the configured one rather than a different one.\n",
        "        Ctrl+C stops after the current iteration with the checkpoint intact.\n\n",
        "OUTPUT (in <directory>)\n",
        "  checkpoint.toml        theta and iteration count; the authority on a resume\n",
        "  history.csv           one row per iteration: result and theta\n",
        "  iteration-NNNNNN.pgn  that iteration's games\n\n",
        "PARAMETERS\n",
    ));
    for spec in mf_search::SEARCH_PARAMETERS {
        text.push_str(&format!(
            "  {:<32} default {:>6}  range [{}, {}]\n",
            spec.name, spec.default, spec.min, spec.max
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::run_cli;
    use crate::config::TuningConfig;

    fn capture(arguments: &[&str]) -> Result<String, String> {
        let mut sink = Vec::new();
        run_cli(arguments.iter().map(|a| a.to_string()), &mut sink)?;
        Ok(String::from_utf8(sink).expect("utf-8 output"))
    }

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mf-tune-cli-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn help_lists_every_parameter_the_engine_advertises() {
        for arguments in [vec![], vec!["--help"], vec!["run", "-h"]] {
            let text = capture(&arguments).expect("help succeeds");
            assert!(text.contains("mf-tune init"), "{text}");
            for spec in mf_search::SEARCH_PARAMETERS {
                assert!(text.contains(spec.name), "help omits {}", spec.name);
            }
        }
    }

    #[test]
    fn init_prints_a_config_that_parses_and_writes_it_when_asked() {
        let text = capture(&["init", "--params", "LmrCoefficient,LmrBase"]).expect("init succeeds");
        let config = TuningConfig::parse(&text, "stdout").expect("printed config parses");
        assert_eq!(config.dimensions.len(), 2);

        let directory = scratch("init");
        let path = directory.join("nested").join("tune.toml");
        let message = capture(&[
            "init",
            "--params",
            "LmrCoefficient, LmrBase ,",
            "--out",
            &path.display().to_string(),
        ])
        .expect("init succeeds");
        assert!(message.contains("2 parameters"), "{message}");
        let written = std::fs::read_to_string(&path).expect("config was written");
        assert_eq!(
            TuningConfig::parse(&written, "file")
                .unwrap()
                .dimensions
                .len(),
            2
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn bad_command_lines_are_rejected_with_the_usage_text() {
        for (arguments, expected) in [
            (vec!["tune"], "unknown subcommand 'tune'"),
            (vec!["init"], "requires --params"),
            (vec!["init", "--params"], "--params requires"),
            (vec!["init", "--params", "Nonsense"], "Nonsense"),
            (vec!["init", "--wat", "x"], "unknown init argument"),
            (vec!["run"], "requires --config"),
            (vec!["run", "--config", "a.toml"], "requires --out"),
            (
                vec![
                    "run",
                    "--config",
                    "a.toml",
                    "--out",
                    "d",
                    "--iterations",
                    "0",
                ],
                "--iterations must be a positive integer",
            ),
            (vec!["run", "--nope"], "unknown run argument"),
        ] {
            let error = capture(&arguments).expect_err("should be rejected");
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }

    #[test]
    fn a_run_whose_engine_or_book_is_missing_fails_before_playing_anything() {
        let directory = scratch("missing");
        std::fs::create_dir_all(&directory).unwrap();
        let config = directory.join("tune.toml");
        std::fs::write(
            &config,
            concat!(
                "engine = \"no-such-engine.exe\"\n",
                "fastchess = \"no-such-fastchess.exe\"\n",
                "book = \"no-such-book.epd\"\n",
                "iterations = 4\n",
                "[[param]]\nname = \"LmrCoefficient\"\n",
            ),
        )
        .unwrap();

        let error = capture(&[
            "run",
            "--config",
            &config.display().to_string(),
            "--out",
            &directory.join("out").display().to_string(),
        ])
        .expect_err("should refuse");
        assert!(error.contains("engine not found"), "{error}");
        assert!(
            !directory.join("out").join("checkpoint.toml").exists(),
            "a refused run must not leave a checkpoint"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_unreadable_config_names_the_file() {
        let error = capture(&["run", "--config", "definitely-not-here.toml", "--out", "x"])
            .expect_err("should fail");
        assert!(error.contains("definitely-not-here.toml"), "{error}");
    }
}
