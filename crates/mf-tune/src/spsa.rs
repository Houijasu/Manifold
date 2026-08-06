//! The SPSA update, isolated from everything that makes it slow to test.
//!
//! Nothing here knows about fastchess, files, or chess. An iteration hands out a pair of
//! perturbed points and later receives one number saying how much better the first was;
//! that is the entire interface. Keeping it that narrow is what lets the update be tested
//! against a synthetic objective in milliseconds instead of against games in hours.
//!
//! The schedule is the fishtest one, which is the only SPSA schedule with a decade of
//! evidence behind it on this exact objective:
//!
//! ```text
//! c_k = c / k^gamma            c = c_end * N^gamma
//! a_k = a / (A + k)^alpha      a = a_end * (A + N)^alpha,  a_end = r_end * c_end^2
//! theta <- theta + (a_k / c_k) * result * flip
//! ```
//!
//! `c_end` and `r_end` are per-parameter and expressed in the parameter's own units, so
//! a parameter measured in 1024ths of a ply and one measured in centipawns can share a
//! run without either drowning the other. `A` damps the gain over the first iterations,
//! when the gradient estimate is worst.

use mf_datagen::Rng;

/// Standard SPSA decay exponents (Spall). Exposed through the config only so a run can
/// be reproduced from a file that records them, not because they are meant to be tuned.
pub const DEFAULT_ALPHA: f64 = 0.602;
pub const DEFAULT_GAMMA: f64 = 0.101;
/// `A` as a fraction of the iteration budget. Fishtest's convention.
pub const DEFAULT_A_RATIO: f64 = 0.1;

/// The run-wide part of the schedule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schedule {
    pub iterations: u64,
    pub alpha: f64,
    pub gamma: f64,
    /// The gain-damping constant, in iterations.
    pub big_a: f64,
}

impl Schedule {
    pub fn new(iterations: u64, alpha: f64, gamma: f64, a_ratio: f64) -> Self {
        Self {
            iterations,
            alpha,
            gamma,
            big_a: a_ratio * iterations as f64,
        }
    }
}

/// One tunable dimension: where it may go, and how far it should still be moving at the
/// end of the run.
#[derive(Clone, Debug, PartialEq)]
pub struct Dimension {
    pub name: String,
    pub min: i32,
    pub max: i32,
    /// Perturbation half-width at the final iteration, in the parameter's own units.
    pub c_end: f64,
    /// Learning rate at the final iteration. One unit of `result` moves theta by
    /// `r_end * c_end` there.
    pub r_end: f64,
}

/// The gains in force at one iteration for one dimension.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gains {
    pub c_k: f64,
    pub a_k: f64,
}

/// The two points an iteration compares, and the signs that produced them.
#[derive(Clone, Debug, PartialEq)]
pub struct Perturbation {
    /// `+1` or `-1` per dimension.
    pub flips: Vec<f64>,
    /// `theta + c_k * flip`, rounded to the spin the engine will actually be given.
    pub plus: Vec<i32>,
    /// `theta - c_k * flip`, likewise.
    pub minus: Vec<i32>,
}

/// A tuning run's live state: the schedule, the dimensions, and the current point.
#[derive(Clone, Debug)]
pub struct Spsa {
    schedule: Schedule,
    dimensions: Vec<Dimension>,
    theta: Vec<f64>,
}

impl Spsa {
    /// Fails rather than silently degrading: a zero `c_end` divides by zero in the
    /// update, and a theta outside its range would be tuned somewhere the engine clamps.
    pub fn new(
        schedule: Schedule,
        dimensions: Vec<Dimension>,
        theta: Vec<f64>,
    ) -> Result<Self, String> {
        if dimensions.is_empty() {
            return Err("a tuning run needs at least one parameter".to_string());
        }
        if dimensions.len() != theta.len() {
            return Err(format!(
                "{} parameters but {} starting values",
                dimensions.len(),
                theta.len()
            ));
        }
        if schedule.iterations == 0 {
            return Err("a tuning run needs at least one iteration".to_string());
        }
        for (dimension, value) in dimensions.iter().zip(&theta) {
            // `is_finite` first, and not folded into a single comparison: a NaN gain
            // compares false against everything, so `<= 0.0` alone would admit it and the
            // whole run would silently become NaN at the first update.
            if !dimension.c_end.is_finite() || dimension.c_end <= 0.0 {
                return Err(format!("{}: c_end must be positive", dimension.name));
            }
            if !dimension.r_end.is_finite() || dimension.r_end <= 0.0 {
                return Err(format!("{}: r_end must be positive", dimension.name));
            }
            if dimension.min >= dimension.max {
                return Err(format!(
                    "{}: min {} must be below max {}",
                    dimension.name, dimension.min, dimension.max
                ));
            }
            if *value < f64::from(dimension.min) || *value > f64::from(dimension.max) {
                return Err(format!(
                    "{}: starting value {value} is outside [{}, {}]",
                    dimension.name, dimension.min, dimension.max
                ));
            }
        }
        Ok(Self {
            schedule,
            dimensions,
            theta,
        })
    }

    pub fn schedule(&self) -> Schedule {
        self.schedule
    }

    pub fn dimensions(&self) -> &[Dimension] {
        &self.dimensions
    }

    pub fn theta(&self) -> &[f64] {
        &self.theta
    }

    /// The current point as the integers the engine will be sent.
    pub fn spins(&self) -> Vec<i32> {
        self.theta
            .iter()
            .zip(&self.dimensions)
            .map(|(value, dimension)| round_to_spin(*value, dimension))
            .collect()
    }

    /// Gains for `iteration`, which is 1-based: iteration 0 would divide by zero.
    pub fn gains(&self, iteration: u64) -> Vec<Gains> {
        let k = iteration.max(1) as f64;
        let n = self.schedule.iterations as f64;
        self.dimensions
            .iter()
            .map(|dimension| {
                let c = dimension.c_end * n.powf(self.schedule.gamma);
                let a_end = dimension.r_end * dimension.c_end * dimension.c_end;
                let a = a_end * (self.schedule.big_a + n).powf(self.schedule.alpha);
                Gains {
                    c_k: c / k.powf(self.schedule.gamma),
                    a_k: a / (self.schedule.big_a + k).powf(self.schedule.alpha),
                }
            })
            .collect()
    }

    /// Draws the iteration's `±1` signs and returns both perturbed points.
    ///
    /// The signs come from a stream derived from `(seed, iteration)` rather than from a
    /// generator carried across iterations, so a resumed run replays exactly the signs
    /// the uninterrupted run would have drawn without the checkpoint having to store
    /// generator state.
    pub fn perturbation(&self, iteration: u64, seed: u64) -> Perturbation {
        let mut rng = Rng::for_index(seed, iteration);
        let gains = self.gains(iteration);
        let mut flips = Vec::with_capacity(self.dimensions.len());
        let mut plus = Vec::with_capacity(self.dimensions.len());
        let mut minus = Vec::with_capacity(self.dimensions.len());
        for (index, dimension) in self.dimensions.iter().enumerate() {
            let flip = if rng.next_u64() & 1 == 0 { 1.0 } else { -1.0 };
            let step = gains[index].c_k * flip;
            flips.push(flip);
            plus.push(round_to_spin(self.theta[index] + step, dimension));
            minus.push(round_to_spin(self.theta[index] - step, dimension));
        }
        Perturbation { flips, plus, minus }
    }

    /// Applies one measurement. `result` is how much better the `plus` arm was, in game
    /// points (wins minus losses over the batch).
    pub fn apply(&mut self, iteration: u64, perturbation: &Perturbation, result: f64) {
        let gains = self.gains(iteration);
        for (index, dimension) in self.dimensions.iter().enumerate() {
            let Gains { c_k, a_k } = gains[index];
            let moved = self.theta[index] + (a_k / c_k) * result * perturbation.flips[index];
            self.theta[index] = moved.clamp(f64::from(dimension.min), f64::from(dimension.max));
        }
    }

    /// Replaces the current point, e.g. when resuming from a checkpoint.
    pub fn set_theta(&mut self, theta: Vec<f64>) -> Result<(), String> {
        if theta.len() != self.dimensions.len() {
            return Err(format!(
                "checkpoint has {} values but the config has {} parameters",
                theta.len(),
                self.dimensions.len()
            ));
        }
        self.theta = theta;
        Ok(())
    }
}

/// Rounds half away from zero, then clamps. `f64::round` already rounds half away from
/// zero, which is what keeps a negative parameter symmetric with a positive one.
fn round_to_spin(value: f64, dimension: &Dimension) -> i32 {
    let rounded = value.round();
    if rounded <= f64::from(dimension.min) {
        return dimension.min;
    }
    if rounded >= f64::from(dimension.max) {
        return dimension.max;
    }
    rounded as i32
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_A_RATIO, DEFAULT_ALPHA, DEFAULT_GAMMA, Dimension, Schedule, Spsa};

    fn dimension(name: &str, min: i32, max: i32, c_end: f64, r_end: f64) -> Dimension {
        Dimension {
            name: name.to_string(),
            min,
            max,
            c_end,
            r_end,
        }
    }

    fn quadratic_run(iterations: u64, seed: u64) -> (Vec<f64>, Vec<f64>) {
        // A synthetic objective standing in for "Elo of these spins": a paraboloid with
        // very different curvature per axis, which is exactly the case that breaks a
        // tuner with one shared step size.
        let targets = [2_500.0_f64, 300.0_f64];
        let curvature = [1.0 / 400_000.0, 1.0 / 4_000.0];
        let objective = |point: &[f64]| -> f64 {
            -curvature[0] * (point[0] - targets[0]).powi(2)
                - curvature[1] * (point[1] - targets[1]).powi(2)
        };

        let schedule = Schedule::new(iterations, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let dimensions = vec![
            dimension("LmrCoefficient", 1_000, 6_000, 100.0, 0.002),
            dimension("RfpMarginPerDepth", 20, 300, 5.0, 0.002),
        ];
        let start = vec![5_500.0, 105.0];
        let mut spsa = Spsa::new(schedule, dimensions, start.clone()).expect("valid run");

        for iteration in 1..=iterations {
            let perturbation = spsa.perturbation(iteration, seed);
            let plus: Vec<f64> = perturbation.plus.iter().map(|v| f64::from(*v)).collect();
            let minus: Vec<f64> = perturbation.minus.iter().map(|v| f64::from(*v)).collect();
            // Scaled to the magnitude of a small batch's win-minus-loss count.
            let result = 8.0 * (objective(&plus) - objective(&minus));
            spsa.apply(iteration, &perturbation, result);
        }
        (start, spsa.theta().to_vec())
    }

    #[test]
    fn the_update_converges_on_a_synthetic_quadratic_objective() {
        let targets = [2_500.0_f64, 300.0_f64];
        let (start, finish) = quadratic_run(4_000, 20_260_807);
        for (index, name) in ["LmrCoefficient", "RfpMarginPerDepth"].iter().enumerate() {
            let before = (start[index] - targets[index]).abs();
            let after = (finish[index] - targets[index]).abs();
            assert!(
                after < before / 4.0,
                "{name}: started {before:.1} from the optimum and finished {after:.1} away \
                 (start {:.1}, finish {:.1}, target {:.1})",
                start[index],
                finish[index],
                targets[index]
            );
        }
    }

    #[test]
    fn convergence_does_not_depend_on_one_lucky_seed() {
        let targets = [2_500.0_f64, 300.0_f64];
        for seed in [1_u64, 7, 4_242, 20_260_807] {
            let (start, finish) = quadratic_run(2_000, seed);
            for index in 0..2 {
                let before = (start[index] - targets[index]).abs();
                let after = (finish[index] - targets[index]).abs();
                assert!(
                    after < before / 2.0,
                    "seed {seed}, dimension {index}: {before:.1} -> {after:.1}"
                );
            }
        }
    }

    #[test]
    fn a_measurement_of_zero_leaves_theta_exactly_where_it_was() {
        let schedule = Schedule::new(100, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let mut spsa = Spsa::new(
            schedule,
            vec![dimension("LmrBase", -1_024, 3_072, 20.0, 0.002)],
            vec![982.0],
        )
        .expect("valid run");
        let perturbation = spsa.perturbation(1, 5);
        spsa.apply(1, &perturbation, 0.0);
        assert_eq!(spsa.theta(), &[982.0]);
    }

    #[test]
    fn a_positive_result_moves_theta_along_the_flip_and_a_negative_one_against_it() {
        let schedule = Schedule::new(100, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let build = || {
            Spsa::new(
                schedule,
                vec![dimension("LmrBase", -1_024, 3_072, 20.0, 0.002)],
                vec![982.0],
            )
            .expect("valid run")
        };
        let mut up = build();
        let mut down = build();
        let perturbation = up.perturbation(1, 5);
        up.apply(1, &perturbation, 4.0);
        down.apply(1, &perturbation, -4.0);

        let flip = perturbation.flips[0];
        assert!(
            (up.theta()[0] - 982.0) * flip > 0.0,
            "a winning plus arm must move theta towards it (flip {flip}, theta {})",
            up.theta()[0]
        );
        assert!(
            (down.theta()[0] - 982.0) * flip < 0.0,
            "a losing plus arm must move theta away from it (flip {flip}, theta {})",
            down.theta()[0]
        );
        assert!(
            ((up.theta()[0] - 982.0) + (down.theta()[0] - 982.0)).abs() < 1e-9,
            "opposite results must move theta by equal and opposite amounts"
        );
    }

    #[test]
    fn perturbations_and_theta_never_leave_the_advertised_range() {
        let schedule = Schedule::new(50, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        // c_end far wider than the range, and a result large enough to bolt theta at a
        // bound: a tuner must never emit a spin the engine would clamp behind its back.
        let mut spsa = Spsa::new(
            schedule,
            vec![dimension("NmpReductionBase", 1, 10, 40.0, 5.0)],
            vec![5.0],
        )
        .expect("valid run");
        for iteration in 1..=50 {
            let perturbation = spsa.perturbation(iteration, 3);
            assert!((1..=10).contains(&perturbation.plus[0]), "{perturbation:?}");
            assert!(
                (1..=10).contains(&perturbation.minus[0]),
                "{perturbation:?}"
            );
            spsa.apply(iteration, &perturbation, 400.0 * perturbation.flips[0]);
            assert!(
                (1.0..=10.0).contains(&spsa.theta()[0]),
                "theta escaped its range: {}",
                spsa.theta()[0]
            );
            assert!((1..=10).contains(&spsa.spins()[0]));
        }
    }

    #[test]
    fn the_gains_decay_and_the_final_learning_rate_is_r_end_times_c_end() {
        let schedule = Schedule::new(1_000, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let spsa = Spsa::new(
            schedule,
            vec![dimension("LmrCoefficient", 1_000, 6_000, 100.0, 0.002)],
            vec![2_872.0],
        )
        .expect("valid run");

        let first = spsa.gains(1)[0];
        let last = spsa.gains(1_000)[0];
        assert!(first.c_k > last.c_k, "c must decay: {first:?} -> {last:?}");
        assert!(first.a_k > last.a_k, "a must decay: {first:?} -> {last:?}");
        assert!(
            (last.c_k - 100.0).abs() < 1e-9,
            "c_k at the final iteration is c_end by construction, got {}",
            last.c_k
        );
        assert!(
            (last.a_k / last.c_k - 0.002 * 100.0).abs() < 1e-9,
            "the final step per unit of result is r_end * c_end, got {}",
            last.a_k / last.c_k
        );
    }

    #[test]
    fn the_same_seed_and_iteration_always_draw_the_same_signs() {
        let schedule = Schedule::new(100, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let spsa = Spsa::new(
            schedule,
            vec![
                dimension("LmrCoefficient", 1_000, 6_000, 100.0, 0.002),
                dimension("LmrBase", -1_024, 3_072, 20.0, 0.002),
            ],
            vec![2_872.0, 982.0],
        )
        .expect("valid run");
        for iteration in 1..=20 {
            assert_eq!(
                spsa.perturbation(iteration, 99),
                spsa.perturbation(iteration, 99),
                "iteration {iteration} must be reproducible from (seed, iteration) alone"
            );
        }
        assert_ne!(spsa.perturbation(1, 99), spsa.perturbation(2, 99));
    }

    #[test]
    fn both_signs_are_drawn_and_neither_dominates() {
        let schedule = Schedule::new(2_000, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let spsa = Spsa::new(
            schedule,
            vec![dimension("LmrCoefficient", 1_000, 6_000, 100.0, 0.002)],
            vec![2_872.0],
        )
        .expect("valid run");
        let positive = (1..=2_000)
            .filter(|iteration| spsa.perturbation(*iteration, 20_260_807).flips[0] > 0.0)
            .count();
        assert!(
            (800..=1_200).contains(&positive),
            "signs should be roughly balanced over 2000 draws, got {positive} positive"
        );
    }

    #[test]
    fn a_malformed_run_is_rejected_rather_than_started() {
        let schedule = Schedule::new(10, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO);
        let good = dimension("LmrCoefficient", 1_000, 6_000, 100.0, 0.002);
        assert!(Spsa::new(schedule, Vec::new(), Vec::new()).is_err());
        assert!(Spsa::new(schedule, vec![good.clone()], Vec::new()).is_err());
        assert!(Spsa::new(schedule, vec![good.clone()], vec![100.0]).is_err());
        assert!(
            Spsa::new(
                schedule,
                vec![dimension("Zero", 0, 10, 0.0, 0.002)],
                vec![5.0]
            )
            .is_err()
        );
        assert!(
            Spsa::new(
                schedule,
                vec![dimension("Inverted", 10, 0, 1.0, 0.002)],
                vec![5.0]
            )
            .is_err()
        );
        assert!(
            Spsa::new(
                Schedule::new(0, DEFAULT_ALPHA, DEFAULT_GAMMA, DEFAULT_A_RATIO),
                vec![good],
                vec![2_872.0]
            )
            .is_err()
        );
        for bad in [f64::NAN, f64::INFINITY] {
            assert!(
                Spsa::new(
                    schedule,
                    vec![dimension("NotFinite", 0, 10, bad, 0.002)],
                    vec![5.0]
                )
                .is_err(),
                "a non-finite c_end ({bad}) would turn theta into NaN on the first update"
            );
        }
    }
}
