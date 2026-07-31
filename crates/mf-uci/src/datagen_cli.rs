//! The `manifold datagen` subcommand.
//!
//! Kept in its own module rather than added to `lib.rs`, which is already large: this
//! is a self-contained command whose only coupling to the rest of the crate is being
//! dispatched from `main.rs`.
//!
//! Two modes:
//!
//! * **generate** — `datagen --out <file> [--format bulletformat] --games N --nodes N
//!   --threads N --seed N [--score-bound N]`
//! * **validate** — `datagen --validate <file> [--format bulletformat] [--check-filters]
//!   [--score-bound N] [--report <file>]`
//!
//! Validation deliberately re-reads the file from disk and re-derives every count,
//! rather than reporting what the generator believed it wrote. That is what makes the
//! record count in the validation contract a real check on the writer.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

use mf_datagen::{
    Filter, GenerateConfig, GenerateStats, RECORD_BYTES, Rejection, ValidationReport, generate,
    validate_file,
};

/// The only output format currently supported.
///
/// `--format` is accepted and validated rather than assumed so that adding
/// `viriformat` or `binpack` later is not a breaking change to the command line, and
/// so that a typo fails loudly instead of silently writing the wrong format.
const FORMAT_BULLETFORMAT: &str = "bulletformat";

/// Runs the `datagen` subcommand.
pub fn run_datagen_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        writeln!(writer, "{}", datagen_help()).map_err(|error| error.to_string())?;
        return Ok(());
    }

    match parse_arguments(&arguments)? {
        Command::Generate(options) => run_generate(options, writer),
        Command::Validate(options) => run_validate(options, writer),
    }
}

enum Command {
    Generate(GenerateOptions),
    Validate(ValidateOptions),
}

struct GenerateOptions {
    out: PathBuf,
    config: GenerateConfig,
}

struct ValidateOptions {
    input: PathBuf,
    check_filters: bool,
    filter: Filter,
    report: Option<PathBuf>,
}

fn parse_arguments(arguments: &[String]) -> Result<Command, String> {
    let mut out = None;
    let mut validate = None;
    let mut report = None;
    let mut format = None;
    let mut check_filters = false;
    let mut games = None;
    let mut nodes = None;
    let mut threads = None;
    let mut seed = None;
    let mut score_bound = None;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        let mut value = || -> Result<String, String> {
            index += 1;
            arguments
                .get(index)
                .cloned()
                .ok_or_else(|| datagen_usage(&format!("{argument} requires a value")))
        };
        match argument {
            "--out" => set_once(&mut out, PathBuf::from(value()?), "--out")?,
            "--validate" => set_once(&mut validate, PathBuf::from(value()?), "--validate")?,
            "--report" => set_once(&mut report, PathBuf::from(value()?), "--report")?,
            "--format" => set_once(&mut format, value()?, "--format")?,
            "--check-filters" => check_filters = true,
            "--games" => set_once(&mut games, parse_u64(&value()?, "--games")?, "--games")?,
            "--nodes" => set_once(&mut nodes, parse_u64(&value()?, "--nodes")?, "--nodes")?,
            "--threads" => set_once(
                &mut threads,
                parse_usize(&value()?, "--threads")?,
                "--threads",
            )?,
            "--seed" => set_once(&mut seed, parse_u64(&value()?, "--seed")?, "--seed")?,
            "--score-bound" => set_once(
                &mut score_bound,
                parse_i32(&value()?, "--score-bound")?,
                "--score-bound",
            )?,
            unknown => {
                return Err(datagen_usage(&format!(
                    "unknown datagen argument '{unknown}'"
                )));
            }
        }
        index += 1;
    }

    if let Some(format) = format.as_deref()
        && format != FORMAT_BULLETFORMAT
    {
        return Err(datagen_usage(&format!(
            "unsupported --format '{format}': only '{FORMAT_BULLETFORMAT}' is supported"
        )));
    }

    let filter = Filter {
        score_bound: score_bound.unwrap_or(mf_datagen::DEFAULT_SCORE_BOUND),
    };
    if filter.score_bound < 0 {
        return Err(datagen_usage("--score-bound must not be negative"));
    }

    match (out, validate) {
        (Some(_), Some(_)) => Err(datagen_usage(
            "--out and --validate are mutually exclusive: use one per invocation",
        )),
        (None, None) => Err(datagen_usage("one of --out or --validate is required")),
        (None, Some(input)) => {
            if let Some(rejected) = first_generate_only_flag(games, nodes, threads, seed) {
                return Err(datagen_usage(&format!(
                    "{rejected} applies to generation, not to --validate"
                )));
            }
            Ok(Command::Validate(ValidateOptions {
                input,
                check_filters,
                filter,
                report,
            }))
        }
        (Some(out), None) => {
            if check_filters {
                return Err(datagen_usage(
                    "--check-filters applies to --validate, not to generation",
                ));
            }
            let threads = threads.unwrap_or(1);
            if threads == 0 {
                return Err(datagen_usage("--threads must be at least 1"));
            }
            let games = games.unwrap_or(GenerateConfig::default().games);
            let nodes = nodes.unwrap_or(GenerateConfig::default().nodes);
            if nodes == 0 {
                return Err(datagen_usage("--nodes must be at least 1"));
            }
            Ok(Command::Generate(GenerateOptions {
                out,
                config: GenerateConfig {
                    games,
                    nodes,
                    threads,
                    seed: seed.unwrap_or(0),
                    filter,
                },
            }))
        }
    }
}

fn first_generate_only_flag(
    games: Option<u64>,
    nodes: Option<u64>,
    threads: Option<usize>,
    seed: Option<u64>,
) -> Option<&'static str> {
    games
        .map(|_| "--games")
        .or(nodes.map(|_| "--nodes"))
        .or(threads.map(|_| "--threads"))
        .or(seed.map(|_| "--seed"))
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(datagen_usage(&format!(
            "duplicate datagen argument '{name}'"
        )));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| datagen_usage(&format!("invalid {name} value '{value}'")))
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| datagen_usage(&format!("invalid {name} value '{value}'")))
}

fn parse_i32(value: &str, name: &str) -> Result<i32, String> {
    value
        .parse()
        .map_err(|_| datagen_usage(&format!("invalid {name} value '{value}'")))
}

fn run_generate<W: Write>(options: GenerateOptions, mut writer: W) -> Result<(), String> {
    let file = File::create(&options.out)
        .map_err(|error| format!("unable to create '{}': {error}", options.out.display()))?;
    let mut output = BufWriter::with_capacity(1 << 20, file);

    let started = Instant::now();
    let stats = generate(options.config, |batch| {
        for record in batch {
            output
                .write_all(&record.to_bytes())
                .map_err(|error| format!("unable to write training record: {error}"))?;
        }
        Ok(())
    })?;
    output
        .flush()
        .map_err(|error| format!("unable to flush training data: {error}"))?;
    drop(output);

    let elapsed = started.elapsed();
    let per_second = (stats.positions as f64) / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    write_generate_summary(
        &mut writer,
        &options,
        &stats,
        elapsed.as_secs_f64(),
        per_second,
    )
}

fn write_generate_summary<W: Write>(
    writer: &mut W,
    options: &GenerateOptions,
    stats: &GenerateStats,
    seconds: f64,
    per_second: f64,
) -> Result<(), String> {
    let mut emit = |line: String| -> Result<(), String> {
        writeln!(writer, "{line}").map_err(|error| error.to_string())
    };

    emit(format!("format={FORMAT_BULLETFORMAT}"))?;
    emit(format!("out={}", options.out.display()))?;
    emit(format!("seed={}", options.config.seed))?;
    emit(format!("threads={}", options.config.threads))?;
    emit(format!("nodes={}", options.config.nodes))?;
    emit(format!("score_bound={}", options.config.filter.score_bound))?;
    emit(format!("games={}", stats.games))?;
    emit(format!("considered={}", stats.considered))?;
    emit(format!("positions={}", stats.positions))?;
    emit(format!("bytes={}", stats.positions * RECORD_BYTES as u64))?;
    for (rejection, count) in Rejection::ALL.iter().zip(stats.rejected) {
        emit(format!("rejected_{}={count}", rejection.label()))?;
    }
    emit(format!("deduplicated={}", stats.deduplicated))?;
    emit(format!(
        "results loss={} draw={} win={}",
        stats.results[0], stats.results[1], stats.results[2]
    ))?;
    emit(format!("seconds={seconds:.3}"))?;
    emit(format!("positions_per_second={per_second:.1}"))?;
    Ok(())
}

fn run_validate<W: Write>(options: ValidateOptions, mut writer: W) -> Result<(), String> {
    let filter = options.check_filters.then_some(options.filter);
    let report = validate_file(&options.input, filter)
        .map_err(|error| format!("unable to validate '{}': {error}", options.input.display()))?;

    let mut rendered = String::new();
    render_validation(&mut rendered, &options, &report);

    write!(writer, "{rendered}").map_err(|error| error.to_string())?;
    if let Some(path) = &options.report {
        std::fs::write(path, &rendered)
            .map_err(|error| format!("unable to write report '{}': {error}", path.display()))?;
    }

    // A file with structural defects or filter violations is a failed validation, and
    // must exit non-zero so a harness cannot mistake a broken corpus for a good one.
    if report.invalid > 0 {
        return Err(format!(
            "{} of {} records are structurally invalid",
            report.invalid, report.records
        ));
    }
    if report.total_filter_violations() > 0 {
        return Err(format!(
            "{} records violate the datagen filters",
            report.total_filter_violations()
        ));
    }
    Ok(())
}

fn render_validation(out: &mut String, options: &ValidateOptions, report: &ValidationReport) {
    use core::fmt::Write as _;

    // Fields are emitted as `key=value`, which is the form the validation contract
    // greps for (`in_check=0`, `records=N`). Keep the `=` — a space-separated variant
    // silently fails every one of those checks.
    let _ = writeln!(out, "format={FORMAT_BULLETFORMAT}");
    let _ = writeln!(out, "file={}", options.input.display());
    let _ = writeln!(out, "record_bytes={RECORD_BYTES}");
    let _ = writeln!(out, "records={}", report.records);
    let _ = writeln!(out, "invalid={}", report.invalid);

    for (error, count) in &report.structural {
        let _ = writeln!(out, "structural[{error}]={count}");
    }

    let _ = writeln!(out, "filters_checked={}", report.filters_checked);
    if report.filters_checked {
        for (rejection, count) in Rejection::ALL.iter().zip(report.filter_violations) {
            let _ = writeln!(out, "{}={count}", rejection.label());
        }
        // `tactical_move` cannot be re-derived from the file: bulletformat stores no
        // move, so the played move is gone by the time a record is on disk. It is
        // reported as 0 above because the filter IS enforced, at generation time — but
        // saying so explicitly keeps that 0 from being read as an independent
        // measurement it is not. The generator's own `rejected_tactical_move` counter
        // is the evidence, and `datagen --out` prints it.
        let _ = writeln!(
            out,
            "tactical_move_note=enforced-at-generation; bulletformat stores no move, so \
             this cannot be re-derived from the file"
        );
        let _ = writeln!(out, "score_bound={}", options.filter.score_bound);
        let _ = writeln!(out, "castling_kept={}", report.castling_kept);
    }

    let _ = writeln!(out, "duplicates={}", report.duplicates);
    let _ = writeln!(out, "duplicate_fens={:.3}%", report.duplicate_percent());
    let _ = writeln!(
        out,
        "results loss={} draw={} win={}",
        report.results[0], report.results[1], report.results[2]
    );
    let _ = writeln!(
        out,
        "max_result_share={:.3}%",
        report.max_result_share_percent()
    );
}

fn datagen_usage(message: &str) -> String {
    format!("{message}\n\n{}", datagen_help())
}

fn datagen_help() -> &'static str {
    "Usage:\n\
     \x20 manifold datagen --out <FILE> [--format bulletformat] [--games N] [--nodes N]\n\
     \x20                  [--threads N] [--seed N] [--score-bound N]\n\
     \x20 manifold datagen --validate <FILE> [--format bulletformat] [--check-filters]\n\
     \x20                  [--score-bound N] [--report <FILE>]\n\
     \n\
     Generates NNUE training data as 32-byte bulletformat ChessBoard records, or\n\
     validates a previously generated file.\n\
     \n\
     Options:\n\
     \x20 --out <FILE>        write generated records here\n\
     \x20 --validate <FILE>   validate an existing file instead of generating\n\
     \x20 --format <NAME>     output format; only 'bulletformat' is supported\n\
     \x20 --games N           self-play games to generate (default 100)\n\
     \x20 --nodes N           per-move search node budget (default 5000)\n\
     \x20 --threads N         worker threads; output is identical at any count (default 1)\n\
     \x20 --seed N            master seed; fixing it fixes the run (default 0)\n\
     \x20 --score-bound N     drop positions with |score| > N centipawns (default 10000)\n\
     \x20 --check-filters     re-check filter rules against the validated file\n\
     \x20 --report <FILE>     also write the validation report here\n\
     \n\
     Filters applied to every emitted position: the side to move is not in check, the\n\
     best move is not a capture/en-passant/promotion, and |score| is within the bound.\n\
     Castling moves are KEPT, per the canonical viriformat filter.\n\
     \n\
     Examples:\n\
     \x20 manifold datagen --out data.bullet --games 2000 --nodes 5000 --threads 8 --seed 7\n\
     \x20 manifold datagen --validate data.bullet --format bulletformat\n\
     \x20 manifold datagen --validate data.bullet --check-filters --score-bound 10000"
}

#[cfg(test)]
mod tests {
    use super::run_datagen_subcommand;

    fn run(arguments: &[&str]) -> Result<String, String> {
        let mut output = Vec::new();
        run_datagen_subcommand(arguments.iter().map(|s| s.to_string()), &mut output)?;
        Ok(String::from_utf8(output).expect("output is UTF-8"))
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mf-datagen-cli-{name}-{}.bullet",
            std::process::id()
        ));
        path
    }

    #[test]
    fn help_lists_every_documented_flag() {
        let help = run(&["--help"]).expect("help succeeds");
        for flag in [
            "--out",
            "--validate",
            "--format",
            "--games",
            "--nodes",
            "--threads",
            "--seed",
            "--score-bound",
            "--check-filters",
            "--report",
        ] {
            assert!(help.contains(flag), "help must document {flag}");
        }
    }

    #[test]
    fn an_unsupported_format_is_rejected_by_name() {
        let error = run(&["--out", "x.bin", "--format", "binpack"])
            .expect_err("unsupported format must fail");
        assert!(error.contains("binpack"), "error must name the format");
    }

    #[test]
    fn missing_mode_and_conflicting_modes_are_both_rejected() {
        assert!(run(&["--games", "1"]).is_err());
        assert!(run(&["--out", "a.bin", "--validate", "b.bin"]).is_err());
    }

    #[test]
    fn generation_writes_a_record_aligned_file_and_reports_a_matching_count() {
        let path = temp("generate");
        let output = run(&[
            "--out",
            path.to_str().expect("path is UTF-8"),
            "--format",
            "bulletformat",
            "--games",
            "4",
            "--nodes",
            "1000",
            "--threads",
            "2",
            "--seed",
            "11",
        ])
        .expect("generation succeeds");

        let positions: u64 = output
            .lines()
            .find_map(|line| line.strip_prefix("positions="))
            .expect("summary reports positions")
            .parse()
            .expect("positions is a number");
        assert!(positions > 0);
        assert!(
            output.contains("positions_per_second"),
            "throughput must be reported"
        );

        let length = std::fs::metadata(&path).expect("file exists").len();
        assert_eq!(length % 32, 0, "file must be record-aligned");
        assert_eq!(length / 32, positions, "reported count must match the file");

        let report = run(&[
            "--validate",
            path.to_str().expect("path is UTF-8"),
            "--format",
            "bulletformat",
        ])
        .expect("validation succeeds");
        assert!(report.contains(&format!("records={positions}")));
        assert!(report.contains("invalid=0"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_fixed_seed_reproduces_a_byte_identical_file() {
        let first = temp("determinism-a");
        let second = temp("determinism-b");
        let arguments = |path: &std::path::Path| {
            vec![
                "--out".to_string(),
                path.to_str().expect("path is UTF-8").to_string(),
                "--games".to_string(),
                "4".to_string(),
                "--nodes".to_string(),
                "1000".to_string(),
                "--threads".to_string(),
                "2".to_string(),
                "--seed".to_string(),
                "4242".to_string(),
            ]
        };

        let mut sink = Vec::new();
        run_datagen_subcommand(arguments(&first), &mut sink).expect("first run");
        run_datagen_subcommand(arguments(&second), &mut sink).expect("second run");

        assert_eq!(
            std::fs::read(&first).expect("first file"),
            std::fs::read(&second).expect("second file"),
            "a fixed seed must reproduce byte-identical output"
        );

        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
    }

    #[test]
    fn check_filters_reports_zero_violations_and_positive_castling_retention() {
        let path = temp("filters");
        run(&[
            "--out",
            path.to_str().expect("path is UTF-8"),
            "--games",
            "6",
            "--nodes",
            "1200",
            "--threads",
            "2",
            "--seed",
            "31",
        ])
        .expect("generation succeeds");

        let report = run(&[
            "--validate",
            path.to_str().expect("path is UTF-8"),
            "--check-filters",
            "--score-bound",
            "10000",
        ])
        .expect("validation succeeds");

        assert!(report.contains("in_check=0"), "{report}");
        assert!(report.contains("score_out_of_bounds=0"), "{report}");
        assert!(report.contains("mate_scores=0"), "{report}");
        assert!(report.contains("tactical_move=0"), "{report}");
        assert!(
            report.contains("filters_checked=true"),
            "the report must state that filters were checked"
        );

        let duplicates: f64 = report
            .lines()
            .find_map(|line| line.strip_prefix("duplicate_fens="))
            .and_then(|value| value.trim_end_matches('%').parse().ok())
            .expect("duplicate share is reported");
        assert!(
            duplicates <= 1.0,
            "duplicate share {duplicates}% must stay within the 1% contract bound"
        );

        let castling: u64 = report
            .lines()
            .find_map(|line| line.strip_prefix("castling_kept="))
            .expect("castling retention is reported")
            .parse()
            .expect("castling_kept is a number");
        assert!(
            castling > 0,
            "castling positions must be kept, not filtered out"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_misaligned_file_fails_validation_with_a_clear_message() {
        let path = temp("misaligned");
        std::fs::write(&path, [0u8; 40]).expect("write");
        let error = run(&["--validate", path.to_str().expect("path is UTF-8")])
            .expect_err("misaligned file must fail validation");
        assert!(error.contains("not a multiple of 32"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_file_fails_validation_without_panicking() {
        let path = temp("corrupt");
        std::fs::write(&path, [0xffu8; 64]).expect("write");
        let error = run(&["--validate", path.to_str().expect("path is UTF-8")])
            .expect_err("corrupt records must fail validation");
        assert!(error.contains("structurally invalid"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn generation_only_flags_are_rejected_on_validate() {
        assert!(run(&["--validate", "x.bin", "--games", "5"]).is_err());
        assert!(run(&["--out", "x.bin", "--check-filters"]).is_err());
    }
}
