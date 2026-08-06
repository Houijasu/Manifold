//! The tuning config: what to tune, against which engine, with which match settings.
//!
//! Parameter names, defaults and ranges are validated against `mf_search::SEARCH_PARAMETERS`
//! at load time rather than trusted. A config naming a parameter the engine does not
//! advertise, or bounding it outside what the engine accepts, would silently tune a value
//! the engine clamps — hours of games spent measuring nothing. That is caught in the first
//! second instead.

use std::path::{Path, PathBuf};

use mf_search::{SEARCH_PARAMETERS, search_parameter};

use crate::document::{Document, Table};
use crate::spsa::{DEFAULT_A_RATIO, DEFAULT_ALPHA, DEFAULT_GAMMA, Dimension, Schedule};

/// Fraction of a parameter's range used as `c_end` when a config does not say.
///
/// Fishtest's rule of thumb is that the perturbation should be a few percent of the
/// plausible range: wide enough that a batch can see the difference, narrow enough that
/// neither arm is a crippled engine.
const DEFAULT_C_END_RANGE_FRACTION: f64 = 0.05;
/// Fishtest's default `r_end`. One unit of result moves theta by `r_end * c_end`.
const DEFAULT_R_END: f64 = 0.002;

/// How a batch of games is played.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchSettings {
    pub engine: PathBuf,
    pub fastchess: PathBuf,
    pub book: PathBuf,
    /// Games per iteration. Rounded up to an even number: openings are played twice, once
    /// with colours reversed, and an odd count would hand one arm an extra white.
    pub games_per_iteration: u32,
    pub time_control: String,
    pub hash_mebibytes: u32,
    /// Threads per engine. The affinity policy is derived from this, never configured:
    /// see `crate::batch`.
    pub threads: u32,
    pub extra_options: Vec<String>,
}

/// A whole tuning run, as read off disk.
#[derive(Clone, Debug)]
pub struct TuningConfig {
    /// The gain schedule, whose horizon `iterations` is what `c_end` and `r_end` are the
    /// gains *at*. It is a property of the tuning problem, not of how far this invocation
    /// intends to get.
    pub schedule: Schedule,
    /// Where this invocation stops, at most `schedule.iterations`.
    ///
    /// Separate from the horizon because shortening a run must not re-derive the gains:
    /// `c = c_end * N^gamma` and `a = a_end * (A + N)^alpha` both read `N`, so a 20-
    /// iteration smoke run that also moved the horizon would step with gains a hundred
    /// times larger than the session it is supposed to be smoke-testing, and would
    /// therefore be testing different arithmetic than the thing it stands in for.
    pub budget: u64,
    pub dimensions: Vec<Dimension>,
    pub start: Vec<f64>,
    pub seed: u64,
    pub match_settings: MatchSettings,
}

impl TuningConfig {
    /// Stops this invocation after `budget` iterations, leaving the schedule alone.
    pub fn set_budget(&mut self, budget: u64) -> Result<(), String> {
        if budget == 0 || budget > self.schedule.iterations {
            return Err(format!(
                "budget {budget} must be between 1 and the configured horizon of {}",
                self.schedule.iterations
            ));
        }
        self.budget = budget;
        Ok(())
    }

    pub fn parse(text: &str, context: &str) -> Result<TuningConfig, String> {
        let document = Document::parse(text, context)?;
        let root = &document.root;

        let iterations = positive_u64(root.integer("iterations")?, "iterations")?;
        let games_per_iteration = positive_u64(
            root.optional_integer("games_per_iteration")?.unwrap_or(8),
            "games_per_iteration",
        )?;
        let games_per_iteration = u32::try_from(games_per_iteration.div_ceil(2) * 2)
            .map_err(|_| "games_per_iteration is implausibly large".to_string())?;
        let threads = u32::try_from(positive_u64(
            root.optional_integer("threads")?.unwrap_or(1),
            "threads",
        )?)
        .map_err(|_| "threads is implausibly large".to_string())?;
        let hash_mebibytes = u32::try_from(positive_u64(
            root.optional_integer("hash")?.unwrap_or(16),
            "hash",
        )?)
        .map_err(|_| "hash is implausibly large".to_string())?;

        let match_settings = MatchSettings {
            engine: PathBuf::from(root.text("engine")?),
            fastchess: PathBuf::from(root.text("fastchess")?),
            book: PathBuf::from(root.text("book")?),
            games_per_iteration,
            time_control: root
                .optional_text("time_control")?
                .unwrap_or("5+0.05")
                .to_string(),
            hash_mebibytes,
            threads,
            extra_options: Vec::new(),
        };

        let schedule = Schedule::new(
            iterations,
            root.optional_decimal("alpha")?.unwrap_or(DEFAULT_ALPHA),
            root.optional_decimal("gamma")?.unwrap_or(DEFAULT_GAMMA),
            root.optional_decimal("a_ratio")?.unwrap_or(DEFAULT_A_RATIO),
        );

        let seed = root
            .optional_integer("seed")?
            .map(|value| value as u64)
            .unwrap_or(0);

        let mut dimensions = Vec::new();
        let mut start = Vec::new();
        for table in document.section("param") {
            let (dimension, value) = parse_parameter(table)?;
            if dimensions
                .iter()
                .any(|existing: &Dimension| existing.name == dimension.name)
            {
                return Err(format!(
                    "{context}: parameter '{}' is listed twice",
                    dimension.name
                ));
            }
            dimensions.push(dimension);
            start.push(value);
        }
        if dimensions.is_empty() {
            return Err(format!(
                "{context}: no [[param]] tables; there is nothing to tune"
            ));
        }

        Ok(Self {
            schedule,
            budget: iterations,
            dimensions,
            start,
            seed,
            match_settings,
        })
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read config {}: {error}", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }
}

fn parse_parameter(table: &Table) -> Result<(Dimension, f64), String> {
    let name = table.text("name")?;
    let spec = search_parameter(name).ok_or_else(|| {
        format!(
            "{}: '{name}' is not a search parameter this engine advertises",
            table.context()
        )
    })?;

    let min = i32::try_from(
        table
            .optional_integer("min")?
            .unwrap_or(i64::from(spec.min)),
    )
    .map_err(|_| format!("{}: min is out of range", table.context()))?;
    let max = i32::try_from(
        table
            .optional_integer("max")?
            .unwrap_or(i64::from(spec.max)),
    )
    .map_err(|_| format!("{}: max is out of range", table.context()))?;
    if min < spec.min || max > spec.max {
        return Err(format!(
            "{}: [{min}, {max}] is wider than the range the engine advertises for \
             {} ([{}, {}]); the engine would clamp and the tuner would be optimising a \
             value it never played",
            table.context(),
            spec.name,
            spec.min,
            spec.max
        ));
    }
    if min >= max {
        return Err(format!(
            "{}: min {min} must be below max {max}",
            table.context()
        ));
    }

    let value = table
        .optional_integer("value")?
        .unwrap_or(i64::from(spec.default)) as f64;
    if value < f64::from(min) || value > f64::from(max) {
        return Err(format!(
            "{}: starting value {value} is outside [{min}, {max}]",
            table.context()
        ));
    }

    let span = f64::from(max) - f64::from(min);
    let c_end = table
        .optional_decimal("c_end")?
        .unwrap_or((span * DEFAULT_C_END_RANGE_FRACTION).max(1.0));
    let r_end = table.optional_decimal("r_end")?.unwrap_or(DEFAULT_R_END);
    if c_end <= 0.0 {
        return Err(format!("{}: c_end must be positive", table.context()));
    }
    if r_end <= 0.0 {
        return Err(format!("{}: r_end must be positive", table.context()));
    }

    for key in table.keys() {
        if !matches!(key, "name" | "value" | "min" | "max" | "c_end" | "r_end") {
            return Err(format!("{}: unknown key '{key}'", table.context()));
        }
    }

    Ok((
        Dimension {
            // The engine's own spelling, not the config's, so `setoption` always matches
            // the handshake regardless of how the config was capitalised.
            name: spec.name.to_string(),
            min,
            max,
            c_end,
            r_end,
        },
        value,
    ))
}

fn positive_u64(value: i64, key: &str) -> Result<u64, String> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{key} must be positive, got {value}"))
}

/// Writes a starter config covering `names`, with every default filled in from the
/// engine's own parameter table.
///
/// This is what makes "generate the config from the M5-F3 parameter table" a command
/// rather than a copying exercise, and therefore what stops the config from going stale
/// when a range changes.
pub fn starter_config(names: &[String]) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(concat!(
        "# Manifold SPSA tuning config, generated by `mf-tune init`.\n",
        "# Ranges and defaults come from the engine's own SEARCH_PARAMETERS table.\n",
        "# c_end is the perturbation half-width at the final iteration, in the\n",
        "# parameter's own units; r_end is the learning rate there.\n\n",
    ));
    out.push_str(concat!(
        "engine = \"target/release/manifold.exe\"\n",
        "fastchess = \"tools/fastchess/fastchess.exe\"\n",
        "book = \"tools/books/UHO_4060_v4.epd\"\n",
        "iterations = 1000\n",
        "games_per_iteration = 8\n",
        "time_control = \"5+0.05\"\n",
        "hash = 16\n",
        "threads = 1\n",
        "seed = 20260807\n",
    ));

    for name in names {
        let spec = search_parameter(name).ok_or_else(|| {
            format!(
                "'{name}' is not a search parameter this engine advertises; known names: {}",
                SEARCH_PARAMETERS
                    .iter()
                    .map(|spec| spec.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let span = f64::from(spec.max) - f64::from(spec.min);
        let c_end = (span * DEFAULT_C_END_RANGE_FRACTION).max(1.0);
        out.push_str(&format!(
            "\n[[param]]\nname = \"{}\"\nvalue = {}\nmin = {}\nmax = {}\nc_end = {c_end:?}\nr_end = {DEFAULT_R_END:?}\n",
            spec.name, spec.default, spec.min, spec.max
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{TuningConfig, starter_config};

    const MINIMAL: &str = concat!(
        "engine = \"target/release/manifold.exe\"\n",
        "fastchess = \"tools/fastchess/fastchess.exe\"\n",
        "book = \"tools/books/UHO_4060_v4.epd\"\n",
        "iterations = 500\n",
        "[[param]]\n",
        "name = \"LmrCoefficient\"\n",
    );

    #[test]
    fn a_minimal_config_inherits_every_default_from_the_engine() {
        let config = TuningConfig::parse(MINIMAL, "test.toml").expect("valid config");
        assert_eq!(config.schedule.iterations, 500);
        assert_eq!(config.dimensions.len(), 1);
        let spec = mf_search::search_parameter("LmrCoefficient").unwrap();
        assert_eq!(config.dimensions[0].min, spec.min);
        assert_eq!(config.dimensions[0].max, spec.max);
        assert_eq!(config.start[0], f64::from(spec.default));
        assert_eq!(config.match_settings.time_control, "5+0.05");
        assert_eq!(config.match_settings.games_per_iteration, 8);
        assert_eq!(config.match_settings.threads, 1);
    }

    #[test]
    fn every_setting_can_be_overridden_and_the_override_is_what_is_used() {
        let text = MINIMAL.to_string()
            + "value = 3000\nmin = 2000\nmax = 4000\nc_end = 60.0\nr_end = 0.004\n";
        let text = text.replace("iterations = 500", "iterations = 42")
            + "\n[[param]]\nname = \"RfpMarginPerDepth\"\n";
        let text = text.replace(
            "book = \"tools/books/UHO_4060_v4.epd\"",
            concat!(
                "book = \"b.epd\"\ntime_control = \"10+0.1\"\nhash = 32\n",
                "games_per_iteration = 20\nseed = 7\nalpha = 0.5\ngamma = 0.2\na_ratio = 0.25\n"
            ),
        );
        let config = TuningConfig::parse(&text, "test.toml").expect("valid config");
        assert_eq!(config.schedule.iterations, 42);
        assert_eq!(config.schedule.alpha, 0.5);
        assert_eq!(config.schedule.gamma, 0.2);
        assert_eq!(config.schedule.big_a, 0.25 * 42.0);
        assert_eq!(config.seed, 7);
        assert_eq!(config.match_settings.time_control, "10+0.1");
        assert_eq!(config.match_settings.hash_mebibytes, 32);
        assert_eq!(config.match_settings.games_per_iteration, 20);
        assert_eq!(config.dimensions[0].min, 2000);
        assert_eq!(config.dimensions[0].max, 4000);
        assert_eq!(config.dimensions[0].c_end, 60.0);
        assert_eq!(config.dimensions[0].r_end, 0.004);
        assert_eq!(config.start[0], 3000.0);
        assert_eq!(config.dimensions[1].name, "RfpMarginPerDepth");
    }

    #[test]
    fn shortening_a_run_moves_the_budget_and_leaves_the_gain_schedule_alone() {
        let mut config = TuningConfig::parse(MINIMAL, "test.toml").expect("valid config");
        assert_eq!(config.budget, 500);
        let schedule = config.schedule;

        config.set_budget(20).expect("20 is inside the horizon");
        assert_eq!(config.budget, 20);
        assert_eq!(
            config.schedule, schedule,
            "the gains are derived from the horizon; a short run must step exactly as the \
             first 20 iterations of the long one would"
        );

        assert!(config.set_budget(0).is_err());
        let error = config.set_budget(501).expect_err("beyond the horizon");
        assert!(error.contains("500"), "{error}");
    }

    #[test]
    fn an_odd_game_count_is_rounded_up_so_both_arms_get_equal_colours() {
        let text = MINIMAL.replace(
            "iterations = 500",
            "iterations = 500\ngames_per_iteration = 5",
        );
        let config = TuningConfig::parse(&text, "test.toml").expect("valid config");
        assert_eq!(config.match_settings.games_per_iteration, 6);
    }

    #[test]
    fn a_parameter_the_engine_does_not_advertise_is_rejected() {
        let text = MINIMAL.replace("LmrCoefficient", "LmrCoefficientt");
        let error = TuningConfig::parse(&text, "test.toml").expect_err("should be rejected");
        assert!(error.contains("LmrCoefficientt"), "{error}");
        assert!(error.contains("advertises"), "{error}");
    }

    #[test]
    fn a_range_wider_than_the_engine_accepts_is_rejected_rather_than_clamped() {
        let spec = mf_search::search_parameter("LmrCoefficient").unwrap();
        let text = MINIMAL.to_string() + &format!("max = {}\n", spec.max + 1);
        let error = TuningConfig::parse(&text, "test.toml").expect_err("should be rejected");
        assert!(error.contains("wider than the range"), "{error}");
    }

    #[test]
    fn structural_mistakes_in_a_config_are_all_rejected() {
        let cases: Vec<(String, &str)> = vec![
            (
                MINIMAL.replace("[[param]]\nname = \"LmrCoefficient\"\n", ""),
                "nothing to tune",
            ),
            (
                MINIMAL.to_string() + "\n[[param]]\nname = \"LmrCoefficient\"\n",
                "listed twice",
            ),
            (
                MINIMAL.replace("iterations = 500", "iterations = 0"),
                "iterations must be positive",
            ),
            (
                MINIMAL.to_string() + "c_end = 0.0\n",
                "c_end must be positive",
            ),
            (
                MINIMAL.to_string() + "r_end = -1.0\n",
                "r_end must be positive",
            ),
            (MINIMAL.to_string() + "value = 999999\n", "outside"),
            (
                MINIMAL.to_string() + "min = 3000\nmax = 2000\n",
                "must be below",
            ),
            (MINIMAL.to_string() + "cend = 4.0\n", "unknown key 'cend'"),
            (
                MINIMAL.replace("engine = \"target/release/manifold.exe\"\n", ""),
                "missing required key 'engine'",
            ),
            (
                MINIMAL.replace("fastchess = \"tools/fastchess/fastchess.exe\"\n", ""),
                "missing required key 'fastchess'",
            ),
        ];
        for (text, expected) in cases {
            let error = TuningConfig::parse(&text, "test.toml")
                .expect_err(&format!("should be rejected: {text}"));
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }

    #[test]
    fn a_config_name_is_matched_case_insensitively_but_stored_as_the_engine_spells_it() {
        let text = MINIMAL.replace("LmrCoefficient", "lmrcoefficient");
        let config = TuningConfig::parse(&text, "test.toml").expect("valid config");
        assert_eq!(config.dimensions[0].name, "LmrCoefficient");
    }

    #[test]
    fn a_generated_starter_config_parses_and_matches_the_engines_table() {
        let names: Vec<String> = ["LmrCoefficient", "LmrBase", "RfpMarginPerDepth"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let text = starter_config(&names).expect("known names");
        let config = TuningConfig::parse(&text, "generated.toml").expect("generated config parses");
        assert_eq!(config.dimensions.len(), 3);
        for (dimension, start) in config.dimensions.iter().zip(&config.start) {
            let spec = mf_search::search_parameter(&dimension.name).expect("known parameter");
            assert_eq!(dimension.min, spec.min);
            assert_eq!(dimension.max, spec.max);
            assert_eq!(*start, f64::from(spec.default));
            assert!(dimension.c_end > 0.0);
        }
    }

    #[test]
    fn generating_a_config_for_every_advertised_parameter_produces_a_valid_config() {
        let names: Vec<String> = mf_search::SEARCH_PARAMETERS
            .iter()
            .map(|spec| spec.name.to_string())
            .collect();
        let text = starter_config(&names).expect("all engine names are known");
        let config = TuningConfig::parse(&text, "generated.toml").expect("generated config parses");
        assert_eq!(config.dimensions.len(), mf_search::SEARCH_PARAMETERS.len());
    }

    #[test]
    fn generating_a_config_for_an_unknown_parameter_lists_what_is_available() {
        let error = starter_config(&["Nonsense".to_string()]).expect_err("should be rejected");
        assert!(error.contains("Nonsense"), "{error}");
        assert!(error.contains("LmrCoefficient"), "{error}");
    }
}
