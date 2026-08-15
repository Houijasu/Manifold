//! Deterministic Markdown and CSV output for corrhist regression.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::corrhist::{CollectedSample, Config};
use crate::regression::{CrossValidation, Metrics, Model, PREDICTORS};

const NAMES: [&str; PREDICTORS] = [
    "pawn",
    "minor",
    "major",
    "material",
    "continuation_2",
    "continuation_4",
];
const SHIPPED: [f64; PREDICTORS] = [
    15_341.0 / 131_072.0,
    10_569.0 / 131_072.0,
    0.0,
    0.0,
    8_761.0 / 131_072.0,
    8_761.0 / 131_072.0,
];

pub struct ReportData<'a> {
    pub config: &'a Config,
    pub network_source: &'a str,
    pub corpus_hash: u64,
    pub corpus_roots: usize,
    pub warm_roots: usize,
    pub train_roots: usize,
    pub test_roots: usize,
    pub train_seen: u64,
    pub test_seen: u64,
    pub train_samples: &'a [CollectedSample],
    pub test_samples: &'a [CollectedSample],
    pub model: &'a Model,
    pub fitted_metrics: Metrics,
    pub shipped_metrics: Metrics,
    pub cross_validation: &'a CrossValidation,
}

pub fn write_outputs(output: &Path, data: &ReportData<'_>) -> Result<(), String> {
    fs::create_dir_all(output)
        .map_err(|error| format!("create output directory {}: {error}", output.display()))?;
    fs::write(
        output.join("samples-summary.csv"),
        samples_summary_csv(data),
    )
    .map_err(|error| format!("write samples-summary.csv: {error}"))?;
    fs::write(output.join("coefficients.csv"), coefficients_csv(data))
        .map_err(|error| format!("write coefficients.csv: {error}"))?;
    fs::write(output.join("report.md"), markdown_report(data))
        .map_err(|error| format!("write report.md: {error}"))?;
    Ok(())
}

fn samples_summary_csv(data: &ReportData<'_>) -> String {
    let mut output = String::from("split,eligible_seen,stored,variable,mean,stddev,min,max\n");
    for (split, seen, samples) in [
        ("train", data.train_seen, data.train_samples),
        ("test", data.test_seen, data.test_samples),
    ] {
        for (name, values) in variables(samples) {
            let (mean, standard_deviation, minimum, maximum) = summary(&values);
            writeln!(
                output,
                "{split},{seen},{},{name},{mean:.9},{standard_deviation:.9},{minimum:.9},{maximum:.9}",
                samples.len()
            )
            .expect("writing to String cannot fail");
        }
    }
    output
}

fn variables(samples: &[CollectedSample]) -> Vec<(&'static str, Vec<f64>)> {
    let mut variables = NAMES
        .iter()
        .enumerate()
        .map(|(index, &name)| {
            (
                name,
                samples
                    .iter()
                    .map(|sample| f64::from(sample.features[index]))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    variables.extend([
        (
            "target",
            samples
                .iter()
                .map(|sample| f64::from(sample.target))
                .collect(),
        ),
        (
            "raw_static_eval",
            samples
                .iter()
                .map(|sample| f64::from(sample.raw_static_eval))
                .collect(),
        ),
        (
            "search_value",
            samples
                .iter()
                .map(|sample| f64::from(sample.search_value))
                .collect(),
        ),
        (
            "depth",
            samples
                .iter()
                .map(|sample| f64::from(sample.depth))
                .collect(),
        ),
        (
            "ply",
            samples.iter().map(|sample| sample.ply as f64).collect(),
        ),
    ]);
    variables
}

fn summary(values: &[f64]) -> (f64, f64, f64, f64) {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let standard_deviation = (values
        .iter()
        .map(|value| {
            let delta = value - mean;
            delta * delta
        })
        .sum::<f64>()
        / values.len() as f64)
        .sqrt();
    let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, standard_deviation, minimum, maximum)
}

fn coefficients_csv(data: &ReportData<'_>) -> String {
    let fold_count = data.cross_validation.fold_coefficients.len();
    let mut output = String::from("predictor,shipped,fitted,train_mean,train_stddev");
    for fold in 0..fold_count {
        write!(output, ",fold_{}", fold + 1).expect("writing to String cannot fail");
    }
    output.push_str(",sign_agreement\n");
    for index in 0..PREDICTORS {
        write!(
            output,
            "{},{:.12},{:.12},{:.12},{:.12}",
            NAMES[index],
            SHIPPED[index],
            data.model.coefficients[index],
            data.model.means[index],
            data.model.standard_deviations[index]
        )
        .expect("writing to String cannot fail");
        for coefficients in &data.cross_validation.fold_coefficients {
            write!(output, ",{:.12}", coefficients[index]).expect("writing to String cannot fail");
        }
        writeln!(
            output,
            ",{:.6}",
            sign_agreement(
                data.model.coefficients[index],
                &data.cross_validation.fold_coefficients,
                index
            )
        )
        .expect("writing to String cannot fail");
    }
    writeln!(
        output,
        "intercept,0.000000000000,{:.12},,,{}",
        data.model.intercept,
        ",".repeat(fold_count)
    )
    .expect("writing to String cannot fail");
    output
}

fn sign(value: f64) -> i8 {
    if value > 1e-9 {
        1
    } else if value < -1e-9 {
        -1
    } else {
        0
    }
}

fn sign_agreement(final_coefficient: f64, folds: &[[f64; PREDICTORS]], index: usize) -> f64 {
    let expected = sign(final_coefficient);
    folds
        .iter()
        .filter(|coefficients| sign(coefficients[index]) == expected)
        .count() as f64
        / folds.len() as f64
}

fn markdown_report(data: &ReportData<'_>) -> String {
    let rmse_improvement =
        relative_improvement(data.shipped_metrics.rmse, data.fitted_metrics.rmse);
    let mae_improvement = relative_improvement(data.shipped_metrics.mae, data.fitted_metrics.mae);
    let minimum_sign_agreement = (0..PREDICTORS)
        .map(|index| {
            sign_agreement(
                data.model.coefficients[index],
                &data.cross_validation.fold_coefficients,
                index,
            )
        })
        .fold(1.0, f64::min);
    let proceed = data.fitted_metrics.r_squared > 0.0
        && rmse_improvement >= 0.02
        && mae_improvement >= 0.02
        && minimum_sign_agreement >= 0.6;
    let recommendation = if proceed {
        "PROCEED TO EXP-C"
    } else {
        "STOP; DO NOT PROCEED TO EXP-C"
    };
    let mut output = String::new();
    writeln!(output, "# EXP-D Corrhist Regression\n").unwrap();
    writeln!(output, "## Configuration\n").unwrap();
    writeln!(output, "- Command: `{}`", command(data.config)).unwrap();
    writeln!(output, "- Corpus: `{}`", data.config.epd.display()).unwrap();
    writeln!(output, "- Corpus FNV-1a-64: `{:#018x}`", data.corpus_hash).unwrap();
    writeln!(output, "- Corpus roots parsed: {}", data.corpus_roots).unwrap();
    writeln!(output, "- Network source: {}", data.network_source).unwrap();
    writeln!(output, "- Warm roots: {}", data.warm_roots).unwrap();
    writeln!(
        output,
        "- Measured roots: {} train / {} test",
        data.train_roots, data.test_roots
    )
    .unwrap();
    writeln!(
        output,
        "- Eligible samples: {} train / {} test",
        data.train_seen, data.test_seen
    )
    .unwrap();
    writeln!(
        output,
        "- Stored reservoir samples: {} train / {} test",
        data.train_samples.len(),
        data.test_samples.len()
    )
    .unwrap();
    writeln!(
        output,
        "- Selected ridge lambda: {:.12}\n",
        data.model.lambda
    )
    .unwrap();

    writeln!(output, "## Test metrics\n").unwrap();
    output.push_str("| Model | R² | MAE | RMSE |\n|---|---:|---:|---:|\n");
    writeln!(
        output,
        "| Fitted | {:.6} | {:.6} | {:.6} |",
        data.fitted_metrics.r_squared, data.fitted_metrics.mae, data.fitted_metrics.rmse
    )
    .unwrap();
    writeln!(
        output,
        "| Shipped integer blend | {:.6} | {:.6} | {:.6} |\n",
        data.shipped_metrics.r_squared, data.shipped_metrics.mae, data.shipped_metrics.rmse
    )
    .unwrap();
    writeln!(
        output,
        "- RMSE improvement over shipped: {:.2}%",
        100.0 * rmse_improvement
    )
    .unwrap();
    writeln!(
        output,
        "- MAE improvement over shipped: {:.2}%\n",
        100.0 * mae_improvement
    )
    .unwrap();

    writeln!(output, "## Coefficients in raw history-entry units\n").unwrap();
    output
        .push_str("| Predictor | Shipped | Fitted | Fold sign agreement |\n|---|---:|---:|---:|\n");
    for index in 0..PREDICTORS {
        writeln!(
            output,
            "| {} | {:.9} | {:.9} | {:.1}% |",
            NAMES[index],
            SHIPPED[index],
            data.model.coefficients[index],
            100.0
                * sign_agreement(
                    data.model.coefficients[index],
                    &data.cross_validation.fold_coefficients,
                    index
                )
        )
        .unwrap();
    }
    writeln!(output, "\n- Fitted intercept: {:.9}", data.model.intercept).unwrap();
    writeln!(
        output,
        "- Minimum coefficient sign agreement: {:.1}%\n",
        100.0 * minimum_sign_agreement
    )
    .unwrap();

    writeln!(output, "## EXP-C decision gate\n").unwrap();
    writeln!(
        output,
        "- Positive out-of-sample R²: {}",
        yes_no(data.fitted_metrics.r_squared > 0.0)
    )
    .unwrap();
    writeln!(
        output,
        "- At least 2% RMSE and MAE improvement: {}",
        yes_no(rmse_improvement >= 0.02 && mae_improvement >= 0.02)
    )
    .unwrap();
    writeln!(
        output,
        "- At least 60% sign agreement for every coefficient: {}",
        yes_no(minimum_sign_agreement >= 0.6)
    )
    .unwrap();
    writeln!(output, "\n**Recommendation: {recommendation}.**").unwrap();
    output
}

fn relative_improvement(baseline: f64, fitted: f64) -> f64 {
    if baseline == 0.0 {
        f64::from(fitted == 0.0)
    } else {
        (baseline - fitted) / baseline
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn command(config: &Config) -> String {
    let mut command = format!(
        "mf-lab corrhist-regression --epd {} --nodes {} --warm-roots {} --roots {} \
         --max-samples {} --seed {} --ridge {} --output {}",
        config.epd.display(),
        config.nodes,
        config.warm_roots,
        config.roots,
        config.max_samples,
        config.seed,
        config
            .ridge
            .iter()
            .map(|lambda| lambda.to_string())
            .collect::<Vec<_>>()
            .join(","),
        config.output.display()
    );
    if let Some(eval_file) = &config.eval_file {
        write!(command, " --eval-file {}", eval_file.display()).unwrap();
    }
    command
}
