use std::fs;
use std::path::PathBuf;

use mf_search::{
    CorrectionFeatures, CorrectionSample, SearchLimits, SearchOptions, SharedHistory,
    TranspositionTable, search_with_correction_samples, search_with_shared_history,
};

use crate::corpus::{Split, parse_epd, select_roots};
use crate::regression::{Metrics, Observation, fit, metrics, select_lambda, shipped_prediction};
use crate::report::{ReportData, write_outputs};
use crate::reservoir::Reservoir;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub epd: PathBuf,
    pub nodes: u64,
    pub warm_roots: usize,
    pub roots: usize,
    pub max_samples: usize,
    pub seed: u64,
    pub ridge: Vec<f64>,
    pub output: PathBuf,
    pub eval_file: Option<PathBuf>,
}

impl Config {
    pub fn parse_args<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = args.into_iter().map(Into::into);
        match args.next().as_deref() {
            Some("corrhist-regression") => {}
            Some(command) => return Err(format!("unknown command `{command}`")),
            None => return Err(usage()),
        }

        let mut epd = None;
        let mut nodes = 10_000;
        let mut warm_roots = 2_000;
        let mut roots = 8_000;
        let mut max_samples = 1_000_000;
        let mut seed = 1;
        let mut ridge = vec![0.0, 0.01, 0.1, 1.0, 10.0];
        let mut output = None;
        let mut eval_file = None;
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--epd" => epd = Some(PathBuf::from(value)),
                "--nodes" => nodes = parse_positive(&flag, &value)?,
                "--warm-roots" => warm_roots = parse_number(&flag, &value)?,
                "--roots" => roots = parse_positive(&flag, &value)?,
                "--max-samples" => max_samples = parse_positive(&flag, &value)?,
                "--seed" => seed = parse_number(&flag, &value)?,
                "--ridge" => {
                    ridge = value
                        .split(',')
                        .map(|part| {
                            let lambda = part
                                .parse::<f64>()
                                .map_err(|_| format!("invalid --ridge value `{part}`"))?;
                            if !lambda.is_finite() || lambda < 0.0 {
                                return Err(format!(
                                    "--ridge values must be finite and non-negative: `{part}`"
                                ));
                            }
                            Ok(lambda)
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    if ridge.is_empty() {
                        return Err("--ridge must contain at least one value".to_owned());
                    }
                }
                "--output" => output = Some(PathBuf::from(value)),
                "--eval-file" => eval_file = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown option `{flag}`")),
            }
        }

        Ok(Self {
            epd: epd.ok_or_else(|| "missing required --epd".to_owned())?,
            nodes,
            warm_roots,
            roots,
            max_samples,
            seed,
            ridge,
            output: output.ok_or_else(|| "missing required --output".to_owned())?,
            eval_file,
        })
    }
}

fn parse_positive<T>(flag: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr + PartialEq + Default,
{
    let parsed = parse_number(flag, value)?;
    if parsed == T::default() {
        return Err(format!("{flag} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_number<T>(flag: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| format!("invalid {flag} value `{value}`"))
}

pub fn usage() -> String {
    "usage: mf-lab corrhist-regression --epd PATH --output DIR [--nodes N] \
     [--warm-roots N] [--roots N] [--max-samples N] [--seed N] \
     [--ridge CSV] [--eval-file PATH]"
        .to_owned()
}

#[derive(Clone, Copy, Debug)]
pub struct CollectedSample {
    pub features: [i32; 6],
    pub target: i32,
    pub raw_static_eval: i32,
    pub search_value: i32,
    pub depth: u32,
    pub ply: usize,
    pub position_key: u64,
    pub root_key: u64,
}

pub fn run(config: &Config) -> Result<(), String> {
    let corpus_bytes =
        fs::read(&config.epd).map_err(|error| format!("read {}: {error}", config.epd.display()))?;
    let corpus_text = std::str::from_utf8(&corpus_bytes)
        .map_err(|error| format!("{} is not UTF-8: {error}", config.epd.display()))?;
    let roots = parse_epd(corpus_text)?;
    let selection = select_roots(&roots, config.warm_roots, config.roots, config.seed)?;
    let resolved = mf_nnue::resolve_network(config.eval_file.as_deref())
        .map_err(|error| format!("resolve NNUE network: {error}"))?;
    let network_source = resolved.source().to_string();

    let train_history = SharedHistory::new();
    let test_history = SharedHistory::new();
    let train_table =
        TranspositionTable::new(1).map_err(|error| format!("allocate train TT: {error}"))?;
    let test_table =
        TranspositionTable::new(1).map_err(|error| format!("allocate test TT: {error}"))?;
    let limits = SearchLimits {
        nodes: Some(config.nodes),
        ..SearchLimits::default()
    };
    let options = SearchOptions::default();

    for &index in &selection.warmup {
        for (history, table) in [(&train_history, &train_table), (&test_history, &test_table)] {
            table.clear();
            search_with_shared_history(
                &roots[index].position,
                table,
                limits,
                options,
                history,
                resolved.network(),
                None,
                None,
            );
        }
    }

    let train_capacity = config.max_samples / 2 + config.max_samples % 2;
    let test_capacity = config.max_samples / 2;
    let mut train_samples = Reservoir::new(train_capacity, config.seed ^ 0x0054_5241_494E);
    let mut test_samples = Reservoir::new(test_capacity, config.seed ^ 0x5445_5354);
    let mut train_roots = 0usize;
    let mut test_roots = 0usize;
    for selected in &selection.measured {
        let root = &roots[selected.index];
        let (history, table, reservoir) = match selected.split {
            Split::Train => {
                train_roots += 1;
                (&train_history, &train_table, &mut train_samples)
            }
            Split::Test => {
                test_roots += 1;
                (&test_history, &test_table, &mut test_samples)
            }
        };
        table.clear();
        search_with_correction_samples(
            &root.position,
            table,
            limits,
            options,
            history,
            resolved.network(),
            |sample| reservoir.push(collected_sample(sample, root.key)),
        );
    }

    let train_seen = train_samples.seen();
    let test_seen = test_samples.seen();
    let train_samples = train_samples.into_items();
    let test_samples = test_samples.into_items();
    if train_samples.is_empty() || test_samples.is_empty() {
        return Err(format!(
            "sampling produced insufficient split data: train={} test={}",
            train_samples.len(),
            test_samples.len()
        ));
    }
    let train_observations = observations(&train_samples);
    let test_observations = observations(&test_samples);
    let cross_validation = select_lambda(
        &train_observations,
        &config.ridge,
        5,
        config.seed ^ 0x464F_4C44,
    )?;
    let model = fit(&train_observations, cross_validation.lambda)?;
    let fitted_metrics = crate::regression::model_metrics(&model, &test_observations);
    let shipped_metrics = shipped_metrics(&test_samples);
    let report = ReportData {
        config,
        network_source: &network_source,
        corpus_hash: fnv1a64(&corpus_bytes),
        corpus_roots: roots.len(),
        warm_roots: selection.warmup.len(),
        train_roots,
        test_roots,
        train_seen,
        test_seen,
        train_samples: &train_samples,
        test_samples: &test_samples,
        model: &model,
        fitted_metrics,
        shipped_metrics,
        cross_validation: &cross_validation,
    };
    write_outputs(&config.output, &report)
}

fn collected_sample(sample: CorrectionSample, root_key: u64) -> CollectedSample {
    let CorrectionFeatures {
        pawn,
        minor,
        major,
        material,
        continuation_2,
        continuation_4,
    } = sample.features;
    CollectedSample {
        features: [
            i32::from(pawn),
            i32::from(minor),
            i32::from(major),
            i32::from(material),
            i32::from(continuation_2),
            i32::from(continuation_4),
        ],
        target: sample.search_value - sample.raw_static_eval,
        raw_static_eval: sample.raw_static_eval,
        search_value: sample.search_value,
        depth: sample.depth,
        ply: sample.ply,
        position_key: sample.position_key,
        root_key,
    }
}

fn observations(samples: &[CollectedSample]) -> Vec<Observation> {
    samples
        .iter()
        .map(|sample| Observation {
            features: sample.features.map(f64::from),
            target: f64::from(sample.target),
            root: sample.root_key,
        })
        .collect()
}

fn shipped_metrics(samples: &[CollectedSample]) -> Metrics {
    let actual = samples
        .iter()
        .map(|sample| f64::from(sample.target))
        .collect::<Vec<_>>();
    let predicted = samples
        .iter()
        .map(|sample| f64::from(shipped_prediction(sample.features)))
        .collect::<Vec<_>>();
    metrics(&actual, &predicted)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01B3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_every_design_argument() {
        let config = Config::parse_args([
            "corrhist-regression",
            "--epd",
            "book.epd",
            "--nodes",
            "123",
            "--warm-roots",
            "4",
            "--roots",
            "8",
            "--max-samples",
            "99",
            "--seed",
            "7",
            "--ridge",
            "0,0.1,1",
            "--output",
            "out",
            "--eval-file",
            "net.nnue",
        ])
        .expect("valid arguments");

        assert_eq!(config.epd, std::path::PathBuf::from("book.epd"));
        assert_eq!(config.nodes, 123);
        assert_eq!(config.warm_roots, 4);
        assert_eq!(config.roots, 8);
        assert_eq!(config.max_samples, 99);
        assert_eq!(config.seed, 7);
        assert_eq!(config.ridge, vec![0.0, 0.1, 1.0]);
        assert_eq!(config.output, std::path::PathBuf::from("out"));
        assert_eq!(config.eval_file, Some(std::path::PathBuf::from("net.nnue")));
    }

    #[test]
    fn cli_rejects_invalid_values_with_clear_errors() {
        let error = Config::parse_args([
            "corrhist-regression",
            "--epd",
            "book.epd",
            "--nodes",
            "0",
            "--output",
            "out",
        ])
        .expect_err("zero nodes must fail");
        assert!(error.contains("--nodes must be greater than zero"));

        let error = Config::parse_args([
            "corrhist-regression",
            "--epd",
            "book.epd",
            "--ridge",
            "0,-1",
            "--output",
            "out",
        ])
        .expect_err("negative ridge must fail");
        assert!(error.contains("--ridge"));
    }

    #[test]
    fn small_fixed_seed_run_writes_byte_identical_outputs() {
        if mf_nnue::resolve_network(None).is_err() {
            eprintln!("SKIPPED: corrhist integration test has no NNUE network");
            return;
        }
        let book = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/books/UHO_4060_v4.epd");
        if !book.is_file() {
            eprintln!("SKIPPED: corrhist integration test has no UHO book");
            return;
        }
        let corpus = std::fs::read_to_string(&book).expect("book should be readable");
        let corpus = corpus.lines().take(80).collect::<Vec<_>>().join("\n");
        let base = std::env::temp_dir().join(format!("mf-lab-corrhist-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("temporary directory should be created");
        let epd = base.join("sample.epd");
        std::fs::write(&epd, corpus).expect("temporary EPD should be written");

        let output_one = base.join("one");
        let config = Config {
            epd,
            nodes: 200,
            warm_roots: 4,
            roots: 60,
            max_samples: 10_000,
            seed: 7,
            ridge: vec![0.01, 0.1, 1.0],
            output: output_one.clone(),
            eval_file: None,
        };
        run(&config).expect("first experiment should succeed");
        let first = ["samples-summary.csv", "coefficients.csv", "report.md"]
            .map(|name| std::fs::read(output_one.join(name)).expect("first output"));
        run(&config).expect("second experiment should succeed");

        for (index, name) in ["samples-summary.csv", "coefficients.csv", "report.md"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                first[index],
                std::fs::read(output_one.join(name)).expect("second output"),
                "{name} must be deterministic"
            );
        }
    }
}
