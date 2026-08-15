pub const PREDICTORS: usize = 6;
const COLUMNS: usize = PREDICTORS + 1;
const SINGULAR_EPSILON: f64 = 1e-12;

#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub features: [f64; PREDICTORS],
    pub target: f64,
    pub root: u64,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub intercept: f64,
    pub coefficients: [f64; PREDICTORS],
    pub means: [f64; PREDICTORS],
    pub standard_deviations: [f64; PREDICTORS],
    pub lambda: f64,
}

impl Model {
    pub fn predict(&self, features: [f64; PREDICTORS]) -> f64 {
        self.intercept
            + self
                .coefficients
                .iter()
                .zip(features)
                .map(|(coefficient, value)| coefficient * value)
                .sum::<f64>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub r_squared: f64,
    pub mae: f64,
    pub rmse: f64,
}

pub fn fit(samples: &[Observation], lambda: f64) -> Result<Model, String> {
    if samples.is_empty() {
        return Err("cannot fit an empty sample set".to_owned());
    }
    if !lambda.is_finite() || lambda < 0.0 {
        return Err("ridge lambda must be finite and non-negative".to_owned());
    }

    let count = samples.len() as f64;
    let means = std::array::from_fn(|column| {
        samples
            .iter()
            .map(|sample| sample.features[column])
            .sum::<f64>()
            / count
    });
    let standard_deviations = std::array::from_fn(|column| {
        (samples
            .iter()
            .map(|sample| {
                let delta = sample.features[column] - means[column];
                delta * delta
            })
            .sum::<f64>()
            / count)
            .sqrt()
    });

    let mut normal = [[0.0; COLUMNS]; COLUMNS];
    let mut rhs = [0.0; COLUMNS];
    for sample in samples {
        let mut row = [0.0; COLUMNS];
        row[0] = 1.0;
        for column in 0..PREDICTORS {
            row[column + 1] = if standard_deviations[column] > SINGULAR_EPSILON {
                (sample.features[column] - means[column]) / standard_deviations[column]
            } else {
                0.0
            };
        }
        for left in 0..COLUMNS {
            rhs[left] += row[left] * sample.target;
            for right in 0..COLUMNS {
                normal[left][right] += row[left] * row[right];
            }
        }
    }
    for (diagonal, row) in normal.iter_mut().enumerate().skip(1) {
        row[diagonal] += lambda;
    }

    let standardized = solve(normal, rhs)?;
    let coefficients = std::array::from_fn(|column| {
        if standard_deviations[column] > SINGULAR_EPSILON {
            standardized[column + 1] / standard_deviations[column]
        } else {
            0.0
        }
    });
    let intercept = standardized[0]
        - coefficients
            .iter()
            .zip(means)
            .map(|(coefficient, mean)| coefficient * mean)
            .sum::<f64>();
    Ok(Model {
        intercept,
        coefficients,
        means,
        standard_deviations,
        lambda,
    })
}

fn solve(
    mut matrix: [[f64; COLUMNS]; COLUMNS],
    mut rhs: [f64; COLUMNS],
) -> Result<[f64; COLUMNS], String> {
    for pivot_column in 0..COLUMNS {
        let pivot_row = (pivot_column..COLUMNS)
            .max_by(|&left, &right| {
                matrix[left][pivot_column]
                    .abs()
                    .total_cmp(&matrix[right][pivot_column].abs())
            })
            .expect("nonempty pivot range");
        if matrix[pivot_row][pivot_column].abs() <= SINGULAR_EPSILON {
            return Err("singular normal equations".to_owned());
        }
        matrix.swap(pivot_column, pivot_row);
        rhs.swap(pivot_column, pivot_row);

        let pivot_values = matrix[pivot_column];
        for row in (pivot_column + 1)..COLUMNS {
            let factor = matrix[row][pivot_column] / matrix[pivot_column][pivot_column];
            matrix[row][pivot_column] = 0.0;
            for (column, value) in matrix[row].iter_mut().enumerate().skip(pivot_column + 1) {
                *value -= factor * pivot_values[column];
            }
            rhs[row] -= factor * rhs[pivot_column];
        }
    }

    let mut solution = [0.0; COLUMNS];
    for row in (0..COLUMNS).rev() {
        let tail = ((row + 1)..COLUMNS)
            .map(|column| matrix[row][column] * solution[column])
            .sum::<f64>();
        solution[row] = (rhs[row] - tail) / matrix[row][row];
    }
    Ok(solution)
}

pub fn metrics(actual: &[f64], predicted: &[f64]) -> Metrics {
    assert_eq!(actual.len(), predicted.len());
    assert!(!actual.is_empty());
    let mean = actual.iter().sum::<f64>() / actual.len() as f64;
    let mut absolute_error = 0.0;
    let mut squared_error = 0.0;
    let mut total_variation = 0.0;
    for (&actual, &predicted) in actual.iter().zip(predicted) {
        let error = actual - predicted;
        absolute_error += error.abs();
        squared_error += error * error;
        let centered = actual - mean;
        total_variation += centered * centered;
    }
    Metrics {
        r_squared: if total_variation <= SINGULAR_EPSILON {
            f64::from(squared_error <= SINGULAR_EPSILON)
        } else {
            1.0 - squared_error / total_variation
        },
        mae: absolute_error / actual.len() as f64,
        rmse: (squared_error / actual.len() as f64).sqrt(),
    }
}

pub fn model_metrics(model: &Model, samples: &[Observation]) -> Metrics {
    let actual = samples
        .iter()
        .map(|sample| sample.target)
        .collect::<Vec<_>>();
    let predicted = samples
        .iter()
        .map(|sample| model.predict(sample.features))
        .collect::<Vec<_>>();
    metrics(&actual, &predicted)
}

pub fn shipped_prediction(features: [i32; PREDICTORS]) -> i32 {
    (15_341 * features[0] + 10_569 * features[1] + 8_761 * features[4] + 8_761 * features[5])
        / 131_072
}

#[derive(Clone, Debug, PartialEq)]
pub struct CrossValidation {
    pub lambda: f64,
    pub fold_metrics: Vec<Metrics>,
    pub fold_coefficients: Vec<[f64; PREDICTORS]>,
}

pub fn fold_for_root(root: u64, folds: usize, seed: u64) -> usize {
    assert!(folds > 0);
    let mut value = root ^ seed.rotate_left(23) ^ 0xA076_1D64_78BD_642F;
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    ((value ^ (value >> 31)) % folds as u64) as usize
}

pub fn select_lambda(
    samples: &[Observation],
    candidates: &[f64],
    folds: usize,
    seed: u64,
) -> Result<CrossValidation, String> {
    if candidates.is_empty() {
        return Err("ridge candidate list is empty".to_owned());
    }
    if folds < 2 {
        return Err("cross-validation requires at least two folds".to_owned());
    }

    let mut best: Option<(f64, CrossValidation)> = None;
    for &lambda in candidates {
        let mut fold_metrics = Vec::new();
        let mut fold_coefficients = Vec::new();
        let mut valid = true;
        for fold in 0..folds {
            let training = samples
                .iter()
                .copied()
                .filter(|sample| fold_for_root(sample.root, folds, seed) != fold)
                .collect::<Vec<_>>();
            let validation = samples
                .iter()
                .copied()
                .filter(|sample| fold_for_root(sample.root, folds, seed) == fold)
                .collect::<Vec<_>>();
            if training.is_empty() || validation.is_empty() {
                continue;
            }
            let Ok(model) = fit(&training, lambda) else {
                valid = false;
                break;
            };
            fold_metrics.push(model_metrics(&model, &validation));
            fold_coefficients.push(model.coefficients);
        }
        if !valid || fold_metrics.len() < 2 {
            continue;
        }
        let mean_rmse = fold_metrics.iter().map(|metrics| metrics.rmse).sum::<f64>()
            / fold_metrics.len() as f64;
        let candidate = CrossValidation {
            lambda,
            fold_metrics,
            fold_coefficients,
        };
        let replaces = best.as_ref().is_none_or(|(best_rmse, best_result)| {
            mean_rmse < *best_rmse || (mean_rmse == *best_rmse && lambda < best_result.lambda)
        });
        if replaces {
            best = Some((mean_rmse, candidate));
        }
    }
    best.map(|(_, result)| result)
        .ok_or_else(|| "no ridge candidate produced at least two valid folds".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(features: [f64; 6], target: f64, root: u64) -> Observation {
        Observation {
            features,
            target,
            root,
        }
    }

    #[test]
    fn standardized_ols_recovers_known_raw_coefficients_and_intercept() {
        let coefficients = [2.0, -3.0, 0.5, 4.0, -1.5, 0.25];
        let intercept = 7.0;
        let samples = (0..40)
            .map(|index| {
                let x = [
                    index as f64,
                    (index * index % 17) as f64,
                    (index * 7 % 13) as f64,
                    (index * 11 % 19) as f64,
                    (index * 5 % 23) as f64,
                    (index * 3 % 29) as f64,
                ];
                let target = intercept
                    + x.iter()
                        .zip(coefficients)
                        .map(|(value, coefficient)| value * coefficient)
                        .sum::<f64>();
                sample(x, target, index % 5)
            })
            .collect::<Vec<_>>();

        let model = fit(&samples, 0.0).expect("full-rank OLS should solve");
        assert!((model.intercept - intercept).abs() < 1e-8);
        for (actual, expected) in model.coefficients.iter().zip(coefficients) {
            assert!((actual - expected).abs() < 1e-8);
        }
    }

    #[test]
    fn singular_ols_fails_but_collinear_ridge_is_finite() {
        let samples = (0..10)
            .map(|index| {
                let value = index as f64;
                sample([value; 6], 3.0 + 2.0 * value, index % 2)
            })
            .collect::<Vec<_>>();

        assert!(fit(&samples, 0.0).is_err());
        let ridge = fit(&samples, 1.0).expect("ridge should regularize collinearity");
        assert!(ridge.intercept.is_finite());
        assert!(ridge.coefficients.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn metrics_compute_r_squared_mae_and_rmse() {
        let actual = [1.0, 2.0, 3.0, 4.0];
        let predicted = [1.0, 3.0, 2.0, 4.0];
        let metrics = metrics(&actual, &predicted);

        assert!((metrics.r_squared - 0.6).abs() < 1e-12);
        assert!((metrics.mae - 0.5).abs() < 1e-12);
        assert!((metrics.rmse - (0.5_f64).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn shipped_integer_blend_uses_exact_truncating_arithmetic() {
        let features = [101, -202, 303, -404, 505, -606];
        let expected = (15_341 * features[0]
            + 10_569 * features[1]
            + 8_761 * features[4]
            + 8_761 * features[5])
            / 131_072;

        assert_eq!(shipped_prediction(features), expected);
    }

    #[test]
    fn lambda_selection_is_deterministic_and_keeps_roots_inside_folds() {
        let samples = (0..80)
            .map(|index| {
                let root = (index / 8) as u64;
                let x = [
                    index as f64,
                    (index * 3 % 17) as f64,
                    (index * 5 % 19) as f64,
                    (index * 7 % 23) as f64,
                    (index * 11 % 29) as f64,
                    (index * 13 % 31) as f64,
                ];
                sample(x, 4.0 + 0.75 * x[0] - 0.25 * x[3], root)
            })
            .collect::<Vec<_>>();

        for root in 0..10 {
            let folds = samples
                .iter()
                .filter(|sample| sample.root == root)
                .map(|sample| fold_for_root(sample.root, 5, 9))
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(folds.len(), 1);
        }
        let first =
            select_lambda(&samples, &[0.0, 0.1, 1.0], 5, 9).expect("cross-validation should fit");
        let second =
            select_lambda(&samples, &[0.0, 0.1, 1.0], 5, 9).expect("cross-validation should fit");
        assert_eq!(first.lambda, second.lambda);
        assert_eq!(first.fold_metrics, second.fold_metrics);
        assert_eq!(first.fold_coefficients, second.fold_coefficients);
    }
}
