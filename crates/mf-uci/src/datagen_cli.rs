//! The `manifold datagen` subcommand.
//!
//! Kept in its own module rather than added to `lib.rs`, which is already large: this
//! is a self-contained command whose only coupling to the rest of the crate is being
//! dispatched from `main.rs`.
//!
//! Three modes:
//!
//! * **generate** — `datagen --out <file> [--format bulletformat] --games N --nodes N
//!   --threads N --seed N [--score-bound N]`
//! * **convert** — `datagen --out <file> --from-jsonl <file> [--max-positions N]
//!   [--resume] [--score-bound N]`, reading the CC0 Lichess evaluation database
//! * **validate** — `datagen --validate <file> [--format bulletformat] [--check-filters]
//!   [--score-bound N] [--report <file>]`
//!
//! Conversion is a second *input source* onto the same encoder, not a second encoder:
//! both modes write through `mf_datagen::record`, which is the path proven
//! byte-identical to the `bulletformat` crate. A second writer would be a second place
//! the format could silently be wrong.
//!
//! Validation deliberately re-reads the file from disk and re-derives every count,
//! rather than reporting what the generator believed it wrote. That is what makes the
//! record count in the validation contract a real check on the writer.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use mf_datagen::{
    ConvertConfig, ConvertStats, Filter, GenerateConfig, GenerateStats, RECORD_BYTES, Rejection,
    SkipReason, ValidationReport, convert, generate, validate_file,
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
        Command::Convert(options) => run_convert(options, writer),
        Command::Validate(options) => run_validate(options, writer),
    }
}

enum Command {
    Generate(GenerateOptions),
    Convert(ConvertOptions),
    Validate(ValidateOptions),
}

struct GenerateOptions {
    out: PathBuf,
    config: GenerateConfig,
}

struct ConvertOptions {
    out: PathBuf,
    /// The JSONL source, or `None` to read the stream on standard input.
    ///
    /// Reading stdin is what makes `zstd -dc <archive> | manifold datagen --from-jsonl -`
    /// possible, so the 21.4 GB archive is stream-decompressed rather than materialised
    /// as ~100 GB of plain JSONL on disk.
    source: Option<PathBuf>,
    resume: bool,
    config: ConvertConfig,
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
    let mut from_jsonl = None;
    let mut report = None;
    let mut format = None;
    let mut check_filters = false;
    let mut resume = false;
    let mut games = None;
    let mut nodes = None;
    let mut threads = None;
    let mut seed = None;
    let mut score_bound = None;
    let mut max_positions = None;

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
            "--from-jsonl" => set_once(&mut from_jsonl, value()?, "--from-jsonl")?,
            "--report" => set_once(&mut report, PathBuf::from(value()?), "--report")?,
            "--format" => set_once(&mut format, value()?, "--format")?,
            "--check-filters" => check_filters = true,
            "--resume" => resume = true,
            "--max-positions" => set_once(
                &mut max_positions,
                parse_u64(&value()?, "--max-positions")?,
                "--max-positions",
            )?,
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
            if from_jsonl.is_some() {
                return Err(datagen_usage(
                    "--from-jsonl is an input source for --out, not for --validate",
                ));
            }
            Ok(Command::Validate(ValidateOptions {
                input,
                check_filters,
                filter,
                report,
            }))
        }
        (Some(out), None) if from_jsonl.is_some() => {
            if check_filters {
                return Err(datagen_usage(
                    "--check-filters applies to --validate, not to conversion",
                ));
            }
            // Self-play knobs have no meaning when positions come from a file, and
            // silently ignoring them would let a caller believe a search budget was
            // honoured when nothing searched anything.
            if let Some(rejected) = first_generate_only_flag(games, nodes, threads, seed) {
                return Err(datagen_usage(&format!(
                    "{rejected} applies to self-play generation, not to --from-jsonl"
                )));
            }
            let source = from_jsonl.expect("guarded by the match arm");
            Ok(Command::Convert(ConvertOptions {
                out,
                source: (source != "-").then(|| PathBuf::from(source)),
                resume,
                config: ConvertConfig {
                    filter,
                    max_positions,
                    ..ConvertConfig::default()
                },
            }))
        }
        (Some(out), None) => {
            if check_filters {
                return Err(datagen_usage(
                    "--check-filters applies to --validate, not to generation",
                ));
            }
            if max_positions.is_some() {
                return Err(datagen_usage(
                    "--max-positions applies to --from-jsonl conversion; \
                     self-play generation is bounded by --games",
                ));
            }
            if resume {
                return Err(datagen_usage(
                    "--resume applies to --from-jsonl conversion; self-play generation \
                     is reproduced from --seed instead",
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

/// The sidecar recording how far a conversion got, next to the output file.
fn progress_path(out: &Path) -> PathBuf {
    let mut path = out.as_os_str().to_owned();
    path.push(".progress");
    PathBuf::from(path)
}

fn run_convert<W: Write>(mut options: ConvertOptions, mut writer: W) -> Result<(), String> {
    let progress_path = progress_path(&options.out);

    // Resuming truncates the output to the last checkpointed record boundary and skips
    // exactly the source lines that produced it. Truncating is what makes the restart
    // sound: records written after the last checkpoint would otherwise be duplicated by
    // the lines this run re-reads. A 21.4 GB fetch and hours of conversion must not be
    // repeated after a crash, and a corpus silently containing a duplicated span is
    // worse than one that had to be rebuilt.
    let mut already_written = 0u64;
    if options.resume {
        let text = std::fs::read_to_string(&progress_path).map_err(|error| {
            format!(
                "--resume needs the checkpoint '{}': {error}",
                progress_path.display()
            )
        })?;
        let (lines_consumed, records) =
            mf_datagen::jsonl::read_progress(&text).ok_or_else(|| {
                format!(
                    "checkpoint '{}' is malformed; expected lines_consumed= and records=",
                    progress_path.display()
                )
            })?;
        options.config.skip_lines = lines_consumed;
        already_written = records;
        OpenOptions::new()
            .write(true)
            .open(&options.out)
            .and_then(|file| file.set_len(records * RECORD_BYTES as u64))
            .map_err(|error| {
                format!(
                    "unable to truncate '{}' to the checkpoint: {error}",
                    options.out.display()
                )
            })?;
    }

    let output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!options.resume)
        .open(&options.out)
        .map_err(|error| format!("unable to open '{}': {error}", options.out.display()))?;
    let mut output = BufWriter::with_capacity(1 << 22, output);
    if options.resume {
        output
            .seek(SeekFrom::End(0))
            .map_err(|error| format!("unable to append to the checkpointed output: {error}"))?;
    }
    // Both the record sink and the checkpoint callback need the writer — the second so
    // it can flush before recording a byte offset a restart will seek to. `RefCell`
    // rather than restructuring `convert`'s interface: the two closures are strictly
    // sequential, never reentrant, so the borrow can never actually conflict.
    let output = core::cell::RefCell::new(output);

    let source: Box<dyn Read> = match &options.source {
        Some(path) => Box::new(
            File::open(path)
                .map_err(|error| format!("unable to open '{}': {error}", path.display()))?,
        ),
        None => Box::new(std::io::stdin()),
    };
    let reader = BufReader::with_capacity(1 << 22, source);

    let started = Instant::now();
    let stats = convert(
        reader,
        options.config,
        |bytes| {
            output
                .borrow_mut()
                .write_all(bytes)
                .map_err(|error| format!("unable to write training record: {error}"))
        },
        |stats| {
            // Flushing before checkpointing is what makes the checkpoint truthful: a
            // marker naming records still sitting in the writer's buffer would send a
            // restart to a byte offset the file does not have.
            output
                .borrow_mut()
                .flush()
                .map_err(|error| format!("unable to flush training data: {error}"))?;
            let checkpoint = ConvertStats {
                positions: stats.positions + already_written,
                ..*stats
            };
            let file = File::create(&progress_path).map_err(|error| {
                format!(
                    "unable to write checkpoint '{}': {error}",
                    progress_path.display()
                )
            })?;
            mf_datagen::jsonl::write_progress(file, &checkpoint)
        },
    )?;
    output
        .borrow_mut()
        .flush()
        .map_err(|error| format!("unable to flush training data: {error}"))?;
    drop(output);

    let elapsed = started.elapsed();
    write_convert_summary(&mut writer, &options, &stats, already_written, elapsed)
}

fn write_convert_summary<W: Write>(
    writer: &mut W,
    options: &ConvertOptions,
    stats: &ConvertStats,
    already_written: u64,
    elapsed: std::time::Duration,
) -> Result<(), String> {
    let mut emit = |line: String| -> Result<(), String> {
        writeln!(writer, "{line}").map_err(|error| error.to_string())
    };
    let total = stats.positions + already_written;

    emit(format!("format={FORMAT_BULLETFORMAT}"))?;
    emit("source=lichess_db_eval.jsonl".to_string())?;
    emit(format!(
        "input={}",
        options
            .source
            .as_ref()
            .map_or_else(|| "<stdin>".to_string(), |path| path.display().to_string())
    ))?;
    emit(format!("out={}", options.out.display()))?;
    emit(format!("resumed={}", options.resume))?;
    emit(format!("resumed_from_records={already_written}"))?;
    emit(format!("score_bound={}", options.config.filter.score_bound))?;
    emit(format!(
        "mate_saturation_cp={}",
        options.config.effective_mate_cp()
    ))?;
    emit(format!("tie_break={}", mf_datagen::TIE_BREAK_RULE))?;
    emit(format!("wdl_lambda={:.1}", mf_datagen::RUNG1_WDL_LAMBDA))?;
    emit(
        "result_placeholder=draw; the source has no game result, so the result byte is \
         excluded from the loss at wdl_lambda=0.0"
            .to_string(),
    )?;
    emit(format!("lines={}", stats.lines))?;
    emit(format!("lines_consumed={}", stats.lines_consumed))?;
    emit(format!("considered={}", stats.considered))?;
    emit(format!("positions={total}"))?;
    emit(format!("bytes={}", total * RECORD_BYTES as u64))?;
    emit(format!("mate_converted={}", stats.mate_converted))?;
    for (rejection, count) in Rejection::ALL.iter().zip(stats.rejected) {
        emit(format!("rejected_{}={count}", rejection.label()))?;
    }
    for (reason, count) in SkipReason::ALL.iter().zip(stats.skipped) {
        emit(format!("skipped_{}={count}", reason.label()))?;
    }
    let seconds = elapsed.as_secs_f64();
    emit(format!("seconds={seconds:.3}"))?;
    emit(format!(
        "positions_per_second={:.1}",
        (stats.positions as f64) / seconds.max(f64::MIN_POSITIVE)
    ))?;
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
     \x20 manifold datagen --out <FILE> --from-jsonl <FILE|-> [--max-positions N]\n\
     \x20                  [--resume] [--score-bound N]\n\
     \x20 manifold datagen --validate <FILE> [--format bulletformat] [--check-filters]\n\
     \x20                  [--score-bound N] [--report <FILE>]\n\
     \n\
     Produces NNUE training data as 32-byte bulletformat ChessBoard records from one of\n\
     two input sources - self-play, or the CC0 Lichess evaluation database - or\n\
     validates a previously produced file.\n\
     \n\
     Options:\n\
     \x20 --out <FILE>        write records here\n\
     \x20 --validate <FILE>   validate an existing file instead of producing one\n\
     \x20 --from-jsonl <FILE> convert lichess_db_eval JSONL instead of self-playing;\n\
     \x20                     '-' reads standard input, so the .zst archive can be\n\
     \x20                     stream-decompressed rather than unpacked to disk\n\
     \x20 --max-positions N   stop after emitting N records (--from-jsonl only)\n\
     \x20 --resume            continue a --from-jsonl run from its .progress checkpoint\n\
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
     lichess_db_eval scores are WHITE-relative and are negated for black-to-move\n\
     records. Where a position carries several evals the deepest is taken, ties broken\n\
     by knodes and then by file order. Mate announcements saturate to the score bound.\n\
     The source has NO game result, so every converted record carries a neutral draw\n\
     placeholder and must be trained with bullet's WDL lambda at 0.0 (pure eval).\n\
     \n\
     Examples:\n\
     \x20 manifold datagen --out data.bullet --games 2000 --nodes 5000 --threads 8 --seed 7\n\
     \x20 zstd -dc lichess_db_eval.jsonl.zst | manifold datagen --out data.bullet --from-jsonl -\n\
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

    /// Two real `lichess_db_eval.jsonl` lines: one centipawn eval with black to move,
    /// one mate announcement with white to move carrying several evals at different
    /// depths. Trimmed to two principal variations each; nothing else is altered.
    const SOURCE_LINES: &str = concat!(
        r#"{"fen":"7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -","evals":[{"pvs":[{"cp":69,"line":"f7g7 e6e2 h8d8 e2d2"},{"cp":163,"line":"h8d8 d1e1 a6a5 a2a3"}],"knodes":4189972,"depth":46}]}"#,
        "\n",
        r#"{"fen":"6k1/6p1/8/4K3/4NN2/8/8/8 w - -","evals":[{"pvs":[{"mate":15,"line":"e5e6 g8f8 e4d6"}],"knodes":589893,"depth":95},{"pvs":[{"mate":20,"line":"e4g5 g8f8 f4g6"}],"knodes":74318,"depth":34}]}"#,
        "\n",
        r#"{"fen":"r1b2rk1/1p2bppp/p1nppn2/q7/2P1P3/N1N5/PP2BPPP/R1BQ1RK1 w - -","evals":[{"pvs":[{"cp":21,"line":"c1e3 f8d8 d1c2"}],"knodes":1000,"depth":40}]}"#,
        "\n",
    );

    fn write_source(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mf-datagen-cli-{name}-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, SOURCE_LINES).expect("source writes");
        path
    }

    fn field<'a>(report: &'a str, key: &str) -> &'a str {
        report
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| panic!("report must carry {key}\n{report}"))
    }

    #[test]
    fn converting_jsonl_writes_a_record_aligned_file_that_validates_clean() {
        let source = write_source("convert");
        let out = temp("convert-out");
        let summary = run(&[
            "--out",
            out.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
        ])
        .expect("conversion succeeds");

        let positions: u64 = field(&summary, "positions=")
            .parse()
            .expect("positions is a number");
        assert_eq!(positions, 3, "all three source lines convert\n{summary}");
        assert_eq!(field(&summary, "mate_converted="), "1");
        assert_eq!(field(&summary, "wdl_lambda="), "0.0");
        assert_eq!(field(&summary, "mate_saturation_cp="), "10000");
        assert_eq!(
            field(&summary, "tie_break="),
            "max-depth, then max-knodes, then first-in-file"
        );
        assert!(
            summary.contains("result_placeholder=draw"),
            "the placeholder must be stated, not implied\n{summary}"
        );

        // The record count must be re-derivable from the byte length rather than
        // trusted from the summary's own counter.
        let length = std::fs::metadata(&out).expect("file exists").len();
        assert_eq!(length % 32, 0, "file must be record-aligned");
        assert_eq!(length / 32, positions);

        let report = run(&[
            "--validate",
            out.to_str().expect("path is UTF-8"),
            "--check-filters",
        ])
        .expect("the existing --validate path accepts converted data unchanged");
        assert!(report.contains(&format!("records={positions}")), "{report}");
        assert!(report.contains("invalid=0"), "{report}");
        assert!(report.contains("in_check=0"), "{report}");
        assert!(report.contains("score_out_of_bounds=0"), "{report}");
        assert!(report.contains("mate_scores=0"), "{report}");

        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(super::progress_path(&out));
    }

    #[test]
    fn a_resumed_conversion_reproduces_the_uninterrupted_corpus_byte_for_byte() {
        let source = write_source("resume");
        let whole = temp("resume-whole");
        let partial = temp("resume-partial");

        run(&[
            "--out",
            whole.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
        ])
        .expect("uninterrupted conversion succeeds");

        // Stop after one record, then resume from the checkpoint the first run left.
        run(&[
            "--out",
            partial.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
            "--max-positions",
            "1",
        ])
        .expect("bounded conversion succeeds");
        assert_eq!(
            std::fs::metadata(&partial).expect("partial exists").len(),
            32
        );

        let summary = run(&[
            "--out",
            partial.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
            "--resume",
        ])
        .expect("resumed conversion succeeds");
        assert_eq!(field(&summary, "resumed="), "true");
        assert_eq!(field(&summary, "resumed_from_records="), "1");
        assert_eq!(field(&summary, "positions="), "3");

        assert_eq!(
            std::fs::read(&partial).expect("resumed file"),
            std::fs::read(&whole).expect("whole file"),
            "a restart must not duplicate or drop a single record"
        );

        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&whole);
        let _ = std::fs::remove_file(&partial);
        let _ = std::fs::remove_file(super::progress_path(&whole));
        let _ = std::fs::remove_file(super::progress_path(&partial));
    }

    #[test]
    fn resuming_without_a_checkpoint_fails_rather_than_silently_restarting() {
        let source = write_source("no-checkpoint");
        let out = temp("no-checkpoint-out");
        let error = run(&[
            "--out",
            out.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
            "--resume",
        ])
        .expect_err("--resume without a checkpoint must fail");
        assert!(error.contains(".progress"), "{error}");
        let _ = std::fs::remove_file(&source);
    }

    #[test]
    fn self_play_flags_and_conversion_flags_do_not_silently_cross_over() {
        // Accepting --nodes on a conversion would imply a search budget that nothing
        // honours, because no position in a downloaded corpus is searched.
        assert!(
            run(&[
                "--out",
                "x.bin",
                "--from-jsonl",
                "s.jsonl",
                "--nodes",
                "5000"
            ])
            .is_err()
        );
        assert!(run(&["--out", "x.bin", "--from-jsonl", "s.jsonl", "--games", "10"]).is_err());
        assert!(run(&["--out", "x.bin", "--max-positions", "10"]).is_err());
        assert!(run(&["--out", "x.bin", "--resume"]).is_err());
        assert!(run(&["--validate", "x.bin", "--from-jsonl", "s.jsonl"]).is_err());
    }

    #[test]
    fn help_documents_the_conversion_source() {
        let help = run(&["--help"]).expect("help succeeds");
        for flag in ["--from-jsonl", "--max-positions", "--resume"] {
            assert!(help.contains(flag), "help must document {flag}");
        }
        assert!(
            help.contains("WHITE-relative"),
            "help must state the source's score convention"
        );
        assert!(
            help.contains("WDL lambda at 0.0"),
            "help must state the lambda the placeholder result requires"
        );
    }
}
