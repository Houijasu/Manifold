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
//!   [--sample-stride N] [--resume] [--score-bound N]`, reading the CC0 Lichess
//!   evaluation database
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

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use mf_datagen::{
    ConvertConfig, ConvertStats, Filter, GenerateConfig, GenerateStats, RECORD_BYTES, Rejection,
    SkipReason, ValidationReport, convert, generate_from, validate_file,
};

/// The only output format currently supported.
///
/// `--format` is accepted and validated rather than assumed so that adding
/// `viriformat` or `binpack` later is not a breaking change to the command line, and
/// so that a typo fails loudly instead of silently writing the wrong format.
const FORMAT_BULLETFORMAT: &str = "bulletformat";
const GENERATION_CHECKPOINT_GAMES: u64 = 100;

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
    /// `';'`-separated Syzygy tablebase directories for game adjudication.
    syzygy_path: Option<String>,
    resume: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GenerationCheckpoint {
    games_completed: u64,
    output_bytes: u64,
    games: u64,
    nodes: u64,
    seed: u64,
    score_bound: i32,
    syzygy_path: String,
    stats: GenerateStats,
}

#[derive(Clone, Copy)]
struct GenerationRunPolicy {
    checkpoint_every: u64,
    stop_after: Option<u64>,
}

impl GenerationCheckpoint {
    fn to_line(&self) -> String {
        format!(
            "games_completed={}\toutput_bytes={}\tgames={}\tnodes={}\tseed={}\t\
             score_bound={}\tsyzygy_path={}\tstats_games={}\tpositions={}\t\
             considered={}\trejected={},{},{},{},{}\tdeduplicated={}\t\
             results={},{},{}\ttb_adjudicated={}",
            self.games_completed,
            self.output_bytes,
            self.games,
            self.nodes,
            self.seed,
            self.score_bound,
            encode_checkpoint_string(&self.syzygy_path),
            self.stats.games,
            self.stats.positions,
            self.stats.considered,
            self.stats.rejected[0],
            self.stats.rejected[1],
            self.stats.rejected[2],
            self.stats.rejected[3],
            self.stats.rejected[4],
            self.stats.deduplicated,
            self.stats.results[0],
            self.stats.results[1],
            self.stats.results[2],
            self.stats.tb_adjudicated,
        )
    }
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
    let mut sample_stride = None;
    let mut syzygy_path = None;

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
            "--sample-stride" => set_once(
                &mut sample_stride,
                parse_u64(&value()?, "--sample-stride")?,
                "--sample-stride",
            )?,
            "--games" => set_once(&mut games, parse_u64(&value()?, "--games")?, "--games")?,
            "--nodes" => set_once(&mut nodes, parse_u64(&value()?, "--nodes")?, "--nodes")?,
            "--threads" => set_once(
                &mut threads,
                parse_usize(&value()?, "--threads")?,
                "--threads",
            )?,
            "--seed" => set_once(&mut seed, parse_u64(&value()?, "--seed")?, "--seed")?,
            "--syzygy-path" => set_once(&mut syzygy_path, value()?, "--syzygy-path")?,
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
            if let Some(rejected) =
                first_generate_only_flag(games, nodes, threads, seed, syzygy_path.as_deref())
            {
                return Err(datagen_usage(&format!(
                    "{rejected} applies to generation, not to --validate"
                )));
            }
            if from_jsonl.is_some() {
                return Err(datagen_usage(
                    "--from-jsonl is an input source for --out, not for --validate",
                ));
            }
            if sample_stride.is_some() {
                return Err(datagen_usage(
                    "--sample-stride applies to --from-jsonl conversion, not to --validate",
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
            if let Some(rejected) =
                first_generate_only_flag(games, nodes, threads, seed, syzygy_path.as_deref())
            {
                return Err(datagen_usage(&format!(
                    "{rejected} applies to self-play generation, not to --from-jsonl"
                )));
            }
            let sample_stride = sample_stride.unwrap_or(1);
            if sample_stride == 0 {
                return Err(datagen_usage("--sample-stride must be at least 1"));
            }
            let source = from_jsonl.expect("guarded by the match arm");
            Ok(Command::Convert(ConvertOptions {
                out,
                source: (source != "-").then(|| PathBuf::from(source)),
                resume,
                config: ConvertConfig {
                    filter,
                    max_positions,
                    sample_stride,
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
            if sample_stride.is_some() {
                return Err(datagen_usage(
                    "--sample-stride subsamples a --from-jsonl source; self-play \
                     generation is bounded by --games instead",
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
                syzygy_path,
                resume,
            }))
        }
    }
}

fn first_generate_only_flag(
    games: Option<u64>,
    nodes: Option<u64>,
    threads: Option<usize>,
    seed: Option<u64>,
    syzygy_path: Option<&str>,
) -> Option<&'static str> {
    games
        .map(|_| "--games")
        .or(nodes.map(|_| "--nodes"))
        .or(threads.map(|_| "--threads"))
        .or(seed.map(|_| "--seed"))
        .or(syzygy_path.map(|_| "--syzygy-path"))
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

fn encode_checkpoint_string(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_checkpoint_string(value: &str) -> Result<String, String> {
    if !value.len().is_multiple_of(2) {
        return Err("invalid checkpoint syzygy_path encoding".to_string());
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let pair = std::str::from_utf8(pair)
            .map_err(|_| "invalid checkpoint syzygy_path encoding".to_string())?;
        decoded.push(
            u8::from_str_radix(pair, 16)
                .map_err(|_| "invalid checkpoint syzygy_path encoding".to_string())?,
        );
    }
    String::from_utf8(decoded).map_err(|_| "checkpoint syzygy_path is not UTF-8".to_string())
}

fn checkpoint_value<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    key: &str,
) -> Result<&'a str, String> {
    let field = fields
        .next()
        .ok_or_else(|| format!("checkpoint is missing {key}"))?;
    field
        .strip_prefix(key)
        .and_then(|value| value.strip_prefix('='))
        .ok_or_else(|| format!("checkpoint expected {key}, found '{field}'"))
}

fn checkpoint_number<'a, T>(
    fields: &mut impl Iterator<Item = &'a str>,
    key: &str,
) -> Result<T, String>
where
    T: std::str::FromStr,
{
    let value = checkpoint_value(fields, key)?;
    value
        .parse()
        .map_err(|_| format!("checkpoint has invalid {key} value '{value}'"))
}

fn checkpoint_array<'a, const N: usize>(
    fields: &mut impl Iterator<Item = &'a str>,
    key: &str,
) -> Result<[u64; N], String> {
    let value = checkpoint_value(fields, key)?;
    let values = value
        .split(',')
        .map(|part| {
            part.parse()
                .map_err(|_| format!("checkpoint has invalid {key} value '{value}'"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    values
        .try_into()
        .map_err(|_| format!("checkpoint has invalid {key} value '{value}'"))
}

fn parse_generation_checkpoint(line: &str) -> Result<GenerationCheckpoint, String> {
    let mut fields = line.split('\t');
    let games_completed = checkpoint_number(&mut fields, "games_completed")?;
    let output_bytes = checkpoint_number(&mut fields, "output_bytes")?;
    let games = checkpoint_number(&mut fields, "games")?;
    let nodes = checkpoint_number(&mut fields, "nodes")?;
    let seed = checkpoint_number(&mut fields, "seed")?;
    let score_bound = checkpoint_number(&mut fields, "score_bound")?;
    let syzygy_path = decode_checkpoint_string(checkpoint_value(&mut fields, "syzygy_path")?)?;
    let stats = GenerateStats {
        games: checkpoint_number(&mut fields, "stats_games")?,
        positions: checkpoint_number(&mut fields, "positions")?,
        considered: checkpoint_number(&mut fields, "considered")?,
        rejected: checkpoint_array(&mut fields, "rejected")?,
        deduplicated: checkpoint_number(&mut fields, "deduplicated")?,
        results: checkpoint_array(&mut fields, "results")?,
        tb_adjudicated: checkpoint_number(&mut fields, "tb_adjudicated")?,
    };
    if let Some(field) = fields.next() {
        return Err(format!("checkpoint has unexpected field '{field}'"));
    }
    if stats.games != games_completed {
        return Err("checkpoint stats_games does not match games_completed".to_string());
    }
    if games_completed > games {
        return Err("checkpoint games_completed exceeds games".to_string());
    }
    if stats.positions.checked_mul(RECORD_BYTES as u64) != Some(output_bytes) {
        return Err("checkpoint output_bytes does not match positions".to_string());
    }
    Ok(GenerationCheckpoint {
        games_completed,
        output_bytes,
        games,
        nodes,
        seed,
        score_bound,
        syzygy_path,
        stats,
    })
}

fn append_generation_checkpoint(
    path: &Path,
    checkpoint: &GenerationCheckpoint,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            format!(
                "unable to open generation checkpoint '{}': {error}",
                path.display()
            )
        })?;
    writeln!(file, "{}", checkpoint.to_line()).map_err(|error| {
        format!(
            "unable to write generation checkpoint '{}': {error}",
            path.display()
        )
    })?;
    file.flush().map_err(|error| {
        format!(
            "unable to flush generation checkpoint '{}': {error}",
            path.display()
        )
    })
}

fn read_generation_checkpoint(path: &Path) -> Result<GenerationCheckpoint, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "unable to open generation checkpoint '{}': {error}",
            path.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut checkpoint = None;
    loop {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line).map_err(|error| {
            format!(
                "unable to read generation checkpoint '{}': {error}",
                path.display()
            )
        })?;
        if bytes == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            break;
        }
        line.pop();
        if line.ends_with(b"\r") {
            line.pop();
        }
        let Ok(line) = std::str::from_utf8(&line) else {
            continue;
        };
        if let Ok(parsed) = parse_generation_checkpoint(line) {
            checkpoint = Some(parsed);
        }
    }
    checkpoint.ok_or_else(|| {
        format!(
            "generation checkpoint '{}' has no complete valid line",
            path.display()
        )
    })
}

fn validate_generation_checkpoint(
    checkpoint: &GenerationCheckpoint,
    config: &GenerateConfig,
    syzygy_path: Option<&str>,
) -> Result<(), String> {
    let expected_path = syzygy_path.unwrap_or_default();
    for (key, matches) in [
        ("games", checkpoint.games == config.games),
        ("nodes", checkpoint.nodes == config.nodes),
        ("seed", checkpoint.seed == config.seed),
        (
            "score_bound",
            checkpoint.score_bound == config.filter.score_bound,
        ),
        ("syzygy_path", checkpoint.syzygy_path == expected_path),
    ] {
        if !matches {
            return Err(format!(
                "generation checkpoint {key} does not match the requested run"
            ));
        }
    }
    Ok(())
}

fn run_generate<W: Write>(options: GenerateOptions, mut writer: W) -> Result<(), String> {
    run_generate_with_policy(
        options,
        &mut writer,
        GenerationRunPolicy {
            checkpoint_every: GENERATION_CHECKPOINT_GAMES,
            stop_after: None,
        },
    )
}

fn run_generate_with_policy<W: Write>(
    options: GenerateOptions,
    mut writer: W,
    policy: GenerationRunPolicy,
) -> Result<(), String> {
    if policy.checkpoint_every == 0 {
        return Err("generation checkpoint interval must be at least 1".to_string());
    }
    let progress_path = progress_path(&options.out);
    let (first_game, prior_stats, checkpoint_bytes) = if options.resume {
        let checkpoint = read_generation_checkpoint(&progress_path).map_err(|error| {
            format!(
                "--resume needs a valid checkpoint '{}': {error}",
                progress_path.display()
            )
        })?;
        validate_generation_checkpoint(
            &checkpoint,
            &options.config,
            options.syzygy_path.as_deref(),
        )?;
        let output_bytes = std::fs::metadata(&options.out)
            .map_err(|error| {
                format!(
                    "unable to inspect checkpointed output '{}': {error}",
                    options.out.display()
                )
            })?
            .len();
        if output_bytes < checkpoint.output_bytes {
            return Err(format!(
                "checkpoint output_bytes {} exceeds output length {output_bytes}",
                checkpoint.output_bytes
            ));
        }
        (
            checkpoint.games_completed,
            checkpoint.stats,
            checkpoint.output_bytes,
        )
    } else {
        (0, GenerateStats::default(), 0)
    };

    // Self-play evaluates with NNUE, so datagen resolves a network the same way the
    // engine does: explicit EvalFile is not in play here, so this is the automatic
    // lookup ending in the embedded network.
    let (network, _) = mf_nnue::resolve_network(None)
        .map_err(|error| format!("unable to resolve the datagen NNUE network: {error}"))?
        .into_parts();

    // A failed tablebase load is a hard error: datagen is a batch tool, and silently
    // degrading to no adjudication would corrupt a run the caller expected to be
    // TB-adjudicated.
    let tablebases = options
        .syzygy_path
        .as_deref()
        .map(|paths| {
            mf_tb::Tablebases::new(paths)
                .map_err(|error| format!("unable to load Syzygy tablebases '{paths}': {error}"))
        })
        .transpose()?;

    let file = if options.resume {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&options.out)
            .map_err(|error| {
                format!(
                    "unable to open checkpointed output '{}': {error}",
                    options.out.display()
                )
            })?;
        file.set_len(checkpoint_bytes).map_err(|error| {
            format!(
                "unable to truncate checkpointed output '{}': {error}",
                options.out.display()
            )
        })?;
        file.seek(SeekFrom::Start(checkpoint_bytes))
            .map_err(|error| {
                format!(
                    "unable to seek checkpointed output '{}': {error}",
                    options.out.display()
                )
            })?;
        file
    } else {
        let file = File::create(&options.out)
            .map_err(|error| format!("unable to create '{}': {error}", options.out.display()))?;
        File::create(&progress_path).map_err(|error| {
            format!(
                "unable to create generation checkpoint '{}': {error}",
                progress_path.display()
            )
        })?;
        append_generation_checkpoint(
            &progress_path,
            &GenerationCheckpoint {
                games_completed: 0,
                output_bytes: 0,
                games: options.config.games,
                nodes: options.config.nodes,
                seed: options.config.seed,
                score_bound: options.config.filter.score_bound,
                syzygy_path: options.syzygy_path.clone().unwrap_or_default(),
                stats: GenerateStats::default(),
            },
        )?;
        file
    };
    let output = RefCell::new(BufWriter::with_capacity(1 << 20, file));

    let started = Instant::now();
    let current_stats = generate_from(
        options.config,
        first_game,
        &network,
        tablebases.as_ref(),
        |batch| {
            let mut output = output.borrow_mut();
            for record in batch {
                output
                    .write_all(&record.to_bytes())
                    .map_err(|error| format!("unable to write training record: {error}"))?;
            }
            Ok(())
        },
        |games_completed, current_stats| {
            let should_checkpoint = games_completed % policy.checkpoint_every == 0
                || games_completed == options.config.games;
            if should_checkpoint {
                let mut output = output.borrow_mut();
                output
                    .flush()
                    .map_err(|error| format!("unable to flush training data: {error}"))?;
                let output_bytes = output
                    .get_ref()
                    .metadata()
                    .map_err(|error| {
                        format!(
                            "unable to inspect generated output '{}': {error}",
                            options.out.display()
                        )
                    })?
                    .len();
                let mut stats = prior_stats;
                stats.merge(current_stats);
                append_generation_checkpoint(
                    &progress_path,
                    &GenerationCheckpoint {
                        games_completed,
                        output_bytes,
                        games: options.config.games,
                        nodes: options.config.nodes,
                        seed: options.config.seed,
                        score_bound: options.config.filter.score_bound,
                        syzygy_path: options.syzygy_path.clone().unwrap_or_default(),
                        stats,
                    },
                )?;
            }
            if policy
                .stop_after
                .is_some_and(|stop_after| games_completed >= stop_after)
            {
                return Err(format!(
                    "generation interrupted after {games_completed} games"
                ));
            }
            Ok(())
        },
    )?;
    output
        .borrow_mut()
        .flush()
        .map_err(|error| format!("unable to flush training data: {error}"))?;
    let mut stats = prior_stats;
    stats.merge(&current_stats);
    let elapsed = started.elapsed();
    let per_second = (stats.positions as f64) / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    write_generate_summary(
        &mut writer,
        &options,
        &stats,
        elapsed.as_secs_f64(),
        per_second,
    )?;
    std::fs::remove_file(&progress_path).map_err(|error| {
        format!(
            "unable to remove completed generation checkpoint '{}': {error}",
            progress_path.display()
        )
    })
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
    emit(format!(
        "syzygy_path={}",
        options.syzygy_path.as_deref().unwrap_or("<none>")
    ))?;
    emit(format!("games={}", stats.games))?;
    emit(format!("tb_adjudicated={}", stats.tb_adjudicated))?;
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
    emit(format!("sample_stride={}", options.config.sample_stride))?;
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
     \x20                  [--syzygy-path <DIRS>] [--resume]\n\
     \x20 manifold datagen --out <FILE> --from-jsonl <FILE|-> [--max-positions N]\n\
     \x20                  [--sample-stride N] [--resume] [--score-bound N]\n\
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
     \x20 --sample-stride N   keep one source line in every N, spreading a bounded\n\
     \x20                     sample across the whole file (--from-jsonl only)\n\
     \x20 --resume            continue a self-play or --from-jsonl run from its\n\
     \x20                     .progress checkpoint\n\
     \x20 --format <NAME>     output format; only 'bulletformat' is supported\n\
     \x20 --games N           self-play games to generate (default 100)\n\
     \x20 --nodes N           per-move search node budget (default 5000)\n\
     \x20 --threads N         worker threads; output is identical at any count (default 1)\n\
     \x20 --seed N            master seed; fixing it fixes the run (default 0)\n\
     \x20 --score-bound N     drop positions with |score| > N centipawns (default 10000)\n\
     \x20 --syzygy-path DIRS  ';'-separated Syzygy directories; adjudicate a game the\n\
     \x20                     moment it enters tablebase range (generation only)\n\
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
     The source file is ORDERED - later positions have markedly fewer pieces and\n\
     larger evals - so its first N lines are not a sample of it. Use --sample-stride\n\
     to spread a bounded corpus across the whole file rather than truncating it.\n\
     \n\
     Examples:\n\
     \x20 manifold datagen --out data.bullet --games 2000 --nodes 5000 --threads 8 --seed 7\n\
     \x20 zstd -dc lichess_db_eval.jsonl.zst | manifold datagen --out data.bullet --from-jsonl -\n\
     \x20 manifold datagen --validate data.bullet --check-filters --score-bound 10000"
}

#[cfg(test)]
mod tests {
    use mf_datagen::{Filter, GenerateConfig, GenerateStats};

    use super::{
        Command, GenerationCheckpoint, GenerationRunPolicy, parse_arguments,
        parse_generation_checkpoint, read_generation_checkpoint, run_datagen_subcommand,
        run_generate_with_policy, validate_generation_checkpoint,
    };

    fn run(arguments: &[&str]) -> Result<String, String> {
        let mut output = Vec::new();
        run_datagen_subcommand(arguments.iter().map(|s| s.to_string()), &mut output)?;
        Ok(String::from_utf8(output).expect("output is UTF-8"))
    }

    fn run_generation_with_policy(
        arguments: &[&str],
        policy: GenerationRunPolicy,
    ) -> Result<String, String> {
        let arguments = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect::<Vec<_>>();
        let Command::Generate(options) = parse_arguments(&arguments)? else {
            panic!("test arguments must select self-play generation");
        };
        let mut output = Vec::new();
        run_generate_with_policy(options, &mut output, policy)?;
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

    fn generation_checkpoint() -> GenerationCheckpoint {
        GenerationCheckpoint {
            games_completed: 17,
            output_bytes: 234 * 32,
            games: 100,
            nodes: 5_000,
            seed: 42,
            score_bound: 9_000,
            syzygy_path: r"C:\tb;D:\more=tb".to_string(),
            stats: GenerateStats {
                games: 17,
                positions: 234,
                considered: 345,
                rejected: [1, 2, 3, 4, 5],
                deduplicated: 6,
                results: [7, 8, 9],
                tb_adjudicated: 10,
            },
        }
    }

    #[test]
    fn generation_checkpoint_round_trip_preserves_every_field() {
        let checkpoint = generation_checkpoint();
        let encoded = checkpoint.to_line();
        let decoded = parse_generation_checkpoint(&encoded).expect("checkpoint parses");
        assert_eq!(decoded, checkpoint);
    }

    #[test]
    fn generation_checkpoint_reader_returns_the_last_complete_valid_line() {
        let path = temp("checkpoint-last");
        let first = generation_checkpoint();
        let mut second = first.clone();
        second.games_completed = 18;
        second.stats.games = 18;
        std::fs::write(
            &path,
            format!(
                "{}\nnot-a-checkpoint\n{}\n",
                first.to_line(),
                second.to_line()
            ),
        )
        .expect("checkpoint log writes");

        assert_eq!(
            read_generation_checkpoint(&path).expect("last checkpoint reads"),
            second
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn generation_checkpoint_reader_ignores_a_truncated_final_line() {
        let path = temp("checkpoint-truncated");
        let checkpoint = generation_checkpoint();
        std::fs::write(
            &path,
            format!(
                "{}\ngames_completed=18\toutput_bytes=",
                checkpoint.to_line()
            ),
        )
        .expect("checkpoint log writes");

        assert_eq!(
            read_generation_checkpoint(&path).expect("complete checkpoint reads"),
            checkpoint
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn generation_checkpoint_config_mismatch_names_the_mismatched_key() {
        let checkpoint = generation_checkpoint();
        let base = GenerateConfig {
            games: checkpoint.games,
            nodes: checkpoint.nodes,
            threads: 99,
            seed: checkpoint.seed,
            filter: Filter {
                score_bound: checkpoint.score_bound,
            },
        };

        let mut config = base;
        config.games += 1;
        let error =
            validate_generation_checkpoint(&checkpoint, &config, Some(&checkpoint.syzygy_path))
                .expect_err("games mismatch must fail");
        assert!(error.contains("games"), "{error}");

        let mut config = base;
        config.nodes += 1;
        let error =
            validate_generation_checkpoint(&checkpoint, &config, Some(&checkpoint.syzygy_path))
                .expect_err("nodes mismatch must fail");
        assert!(error.contains("nodes"), "{error}");

        let mut config = base;
        config.seed += 1;
        let error =
            validate_generation_checkpoint(&checkpoint, &config, Some(&checkpoint.syzygy_path))
                .expect_err("seed mismatch must fail");
        assert!(error.contains("seed"), "{error}");

        let mut config = base;
        config.filter.score_bound += 1;
        let error =
            validate_generation_checkpoint(&checkpoint, &config, Some(&checkpoint.syzygy_path))
                .expect_err("score-bound mismatch must fail");
        assert!(error.contains("score_bound"), "{error}");

        let error = validate_generation_checkpoint(&checkpoint, &base, Some("different"))
            .expect_err("Syzygy path mismatch must fail");
        assert!(error.contains("syzygy_path"), "{error}");

        validate_generation_checkpoint(&checkpoint, &base, Some(&checkpoint.syzygy_path))
            .expect("thread count is not checkpoint identity");
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
            "--syzygy-path",
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
    fn self_play_interrupted_before_the_first_periodic_checkpoint_resumes_identically() {
        let whole = temp("self-play-resume-whole");
        let resumed = temp("self-play-resume-partial");
        let whole_summary = run(&[
            "--out",
            whole.to_str().expect("path is UTF-8"),
            "--games",
            "6",
            "--nodes",
            "1000",
            "--threads",
            "2",
            "--seed",
            "2026",
        ])
        .expect("uninterrupted generation succeeds");

        let error = run_generation_with_policy(
            &[
                "--out",
                resumed.to_str().expect("path is UTF-8"),
                "--games",
                "6",
                "--nodes",
                "1000",
                "--threads",
                "2",
                "--seed",
                "2026",
            ],
            GenerationRunPolicy {
                checkpoint_every: super::GENERATION_CHECKPOINT_GAMES,
                stop_after: Some(2),
            },
        )
        .expect_err("the test policy interrupts generation");
        assert!(error.contains("interrupted"), "{error}");
        assert!(
            super::progress_path(&resumed).exists(),
            "interruption must retain the sidecar"
        );
        assert_eq!(
            read_generation_checkpoint(&super::progress_path(&resumed))
                .expect("the initial checkpoint is complete and valid"),
            GenerationCheckpoint {
                games_completed: 0,
                output_bytes: 0,
                games: 6,
                nodes: 1_000,
                seed: 2026,
                score_bound: mf_datagen::DEFAULT_SCORE_BOUND,
                syzygy_path: String::new(),
                stats: GenerateStats::default(),
            }
        );

        let resumed_summary = run(&[
            "--out",
            resumed.to_str().expect("path is UTF-8"),
            "--games",
            "6",
            "--nodes",
            "1000",
            "--threads",
            "1",
            "--seed",
            "2026",
            "--resume",
        ])
        .expect("resumed generation succeeds");

        assert_eq!(
            std::fs::read(&resumed).expect("resumed corpus"),
            std::fs::read(&whole).expect("whole corpus"),
            "resume must reproduce uninterrupted output byte for byte"
        );
        for key in [
            "games=",
            "tb_adjudicated=",
            "considered=",
            "positions=",
            "bytes=",
            "rejected_in_check=",
            "rejected_tactical_move=",
            "rejected_score_out_of_bounds=",
            "rejected_mate_scores=",
            "rejected_no_best_move=",
            "deduplicated=",
            "results ",
        ] {
            assert_eq!(
                field(&resumed_summary, key),
                field(&whole_summary, key),
                "resumed summary differs for {key}"
            );
        }
        assert!(
            !super::progress_path(&resumed).exists(),
            "normal completion removes the sidecar"
        );

        let _ = std::fs::remove_file(whole);
        let _ = std::fs::remove_file(resumed);
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
        assert!(run(&["--out", "x.bin", "--sample-stride", "4"]).is_err());
        assert!(run(&["--validate", "x.bin", "--sample-stride", "4"]).is_err());
    }

    #[test]
    fn a_sample_stride_subsamples_the_source_and_is_recorded_in_the_summary() {
        let source = write_source("stride");
        let out = temp("stride-out");
        let summary = run(&[
            "--out",
            out.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
            "--sample-stride",
            "2",
        ])
        .expect("strided conversion succeeds");

        assert_eq!(field(&summary, "sample_stride="), "2");
        let consumed: u64 = field(&summary, "lines_consumed=").parse().expect("number");
        let considered: u64 = field(&summary, "lines=").parse().expect("number");
        assert!(consumed > considered, "a stride must skip lines it reads");

        // The stride must be a real selection knob, not a no-op that happens to parse.
        let whole = temp("stride-whole");
        run(&[
            "--out",
            whole.to_str().expect("path is UTF-8"),
            "--from-jsonl",
            source.to_str().expect("path is UTF-8"),
        ])
        .expect("unstrided conversion succeeds");
        assert!(
            std::fs::metadata(&out).expect("strided").len()
                < std::fs::metadata(&whole).expect("whole").len()
        );

        assert!(
            run(&[
                "--out",
                out.to_str().expect("path is UTF-8"),
                "--from-jsonl",
                source.to_str().expect("path is UTF-8"),
                "--sample-stride",
                "0",
            ])
            .is_err()
        );

        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&whole);
    }

    #[test]
    fn a_syzygy_path_is_parsed_once_and_rejected_where_it_cannot_apply() {
        // Duplicate flags fail like every other datagen argument.
        let error = run(&["--out", "x.bin", "--syzygy-path", "a", "--syzygy-path", "b"])
            .expect_err("duplicate --syzygy-path must fail");
        assert!(error.contains("duplicate"), "{error}");

        // Adjudication is a self-play concept: neither validation nor conversion
        // plays games, so accepting the flag there would imply adjudication that
        // never happens.
        let error = run(&["--validate", "x.bin", "--syzygy-path", "a"])
            .expect_err("--syzygy-path must be rejected for --validate");
        assert!(error.contains("--syzygy-path"), "{error}");
        let error = run(&[
            "--out",
            "x.bin",
            "--from-jsonl",
            "s.jsonl",
            "--syzygy-path",
            "a",
        ])
        .expect_err("--syzygy-path must be rejected for --from-jsonl");
        assert!(error.contains("--syzygy-path"), "{error}");
    }

    #[test]
    fn a_nonexistent_syzygy_path_is_a_hard_error_rather_than_silent_degradation() {
        let out = temp("bad-syzygy");
        let error = run(&[
            "--out",
            out.to_str().expect("path is UTF-8"),
            "--games",
            "1",
            "--syzygy-path",
            "Z:\\definitely\\not\\a\\tablebase\\directory",
        ])
        .expect_err("a bad --syzygy-path must fail the run");
        assert!(error.contains("Syzygy"), "{error}");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn help_documents_the_conversion_source() {
        let help = run(&["--help"]).expect("help succeeds");
        for flag in [
            "--from-jsonl",
            "--max-positions",
            "--sample-stride",
            "--resume",
        ] {
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
