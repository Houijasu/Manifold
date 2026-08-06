//! Crash-safe run state.
//!
//! A tuning session is hours long and will be interrupted — by Ctrl+C, by a reboot, by
//! the machine being wanted for a match. Losing it means losing the games, which are the
//! expensive part, so the checkpoint is written after every iteration and is the single
//! authority on where the run got to.
//!
//! It is written to a temporary file and renamed over the real one. A checkpoint
//! truncated by a kill during the write would be worse than no checkpoint at all: the
//! resume would start from a half-parsed theta and quietly tune the wrong point.

use std::path::{Path, PathBuf};

use crate::document::{Document, Table};
use crate::spsa::Dimension;

/// Everything needed to resume: which iteration is next, and where theta is.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    /// Iterations already completed. The next one to run is `completed + 1`.
    pub completed: u64,
    /// Games actually played so far, for the results doc and the budget.
    pub games_played: u64,
    pub theta: Vec<(String, f64)>,
}

impl Checkpoint {
    pub fn new(dimensions: &[Dimension], theta: &[f64]) -> Self {
        Self {
            completed: 0,
            games_played: 0,
            theta: dimensions
                .iter()
                .zip(theta)
                .map(|(dimension, value)| (dimension.name.clone(), *value))
                .collect(),
        }
    }

    pub fn render(&self) -> String {
        let mut document = Document::new("checkpoint");
        document
            .root
            .set_integer("completed", self.completed as i64);
        document
            .root
            .set_integer("games_played", self.games_played as i64);
        for (name, value) in &self.theta {
            let mut table = Table::new("theta");
            table.set_text("name", name.as_str());
            table.set_decimal("value", *value);
            document.push_section("theta", table);
        }
        document.render()
    }

    pub fn parse(text: &str, context: &str) -> Result<Self, String> {
        let document = Document::parse(text, context)?;
        let completed = u64::try_from(document.root.integer("completed")?)
            .map_err(|_| format!("{context}: 'completed' must not be negative"))?;
        let games_played = u64::try_from(document.root.integer("games_played")?)
            .map_err(|_| format!("{context}: 'games_played' must not be negative"))?;
        let mut theta = Vec::new();
        for table in document.section("theta") {
            theta.push((table.text("name")?.to_string(), table.decimal("value")?));
        }
        if theta.is_empty() {
            return Err(format!("{context}: checkpoint records no parameters"));
        }
        Ok(Self {
            completed,
            games_played,
            theta,
        })
    }

    /// Reads the checkpoint back as theta in the config's parameter order.
    ///
    /// The names must match the config exactly. A checkpoint from a run over a different
    /// parameter set resumed against this config would put each value on the wrong axis,
    /// which is silent and catastrophic, so it is refused.
    pub fn theta_for(&self, dimensions: &[Dimension]) -> Result<Vec<f64>, String> {
        if self.theta.len() != dimensions.len() {
            return Err(format!(
                "checkpoint covers {} parameters but the config lists {}; \
                 this checkpoint belongs to a different run",
                self.theta.len(),
                dimensions.len()
            ));
        }
        dimensions
            .iter()
            .zip(&self.theta)
            .map(|(dimension, (name, value))| {
                if dimension.name != *name {
                    return Err(format!(
                        "checkpoint parameter '{name}' does not match config parameter \
                         '{}' at the same position; this checkpoint belongs to a \
                         different run",
                        dimension.name
                    ));
                }
                Ok(*value)
            })
            .collect()
    }

    /// Writes atomically: full write to a sibling temporary, then rename over the target.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let temporary = temporary_path(path);
        std::fs::write(&temporary, self.render())
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        // Windows `rename` fails if the destination exists, unlike POSIX.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path).map_err(|error| {
            format!(
                "cannot move {} onto {}: {error}",
                temporary.display(),
                path.display()
            )
        })
    }

    /// Reads a checkpoint, or `None` when there is none to resume from.
    pub fn read(path: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, &path.display().to_string()).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::Checkpoint;
    use crate::spsa::Dimension;

    fn dimensions() -> Vec<Dimension> {
        ["LmrCoefficient", "LmrBase"]
            .iter()
            .map(|name| Dimension {
                name: name.to_string(),
                min: -1_024,
                max: 6_000,
                c_end: 10.0,
                r_end: 0.002,
            })
            .collect()
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mf-tune-checkpoint-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_checkpoint_round_trips_through_a_file() {
        let directory = scratch("round-trip");
        let path = directory.join("checkpoint.toml");
        let mut checkpoint = Checkpoint::new(&dimensions(), &[2_872.5, -17.25]);
        checkpoint.completed = 137;
        checkpoint.games_played = 1_096;

        checkpoint.write(&path).expect("write succeeds");
        let read = Checkpoint::read(&path)
            .expect("read succeeds")
            .expect("a checkpoint exists");
        assert_eq!(read, checkpoint);
        assert_eq!(
            read.theta_for(&dimensions()).unwrap(),
            vec![2_872.5, -17.25]
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_fractional_theta_survives_exactly_so_a_resume_is_not_a_rounding_event() {
        let directory = scratch("precision");
        let path = directory.join("checkpoint.toml");
        // SPSA lives between the integers; a checkpoint that rounded would discard the
        // sub-spin progress of every iteration since the last whole step.
        let theta = [2_872.123_456_789_1_f64, -0.000_000_5];
        Checkpoint::new(&dimensions(), &theta)
            .write(&path)
            .expect("write succeeds");
        let read = Checkpoint::read(&path).unwrap().unwrap();
        assert_eq!(read.theta_for(&dimensions()).unwrap(), theta.to_vec());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_missing_checkpoint_reads_as_none_rather_than_an_error() {
        let directory = scratch("absent");
        let path = directory.join("nothing-here.toml");
        assert_eq!(Checkpoint::read(&path).expect("no error"), None);
    }

    #[test]
    fn writing_over_an_existing_checkpoint_replaces_it_and_leaves_no_partial_file() {
        let directory = scratch("overwrite");
        let path = directory.join("checkpoint.toml");
        Checkpoint::new(&dimensions(), &[1.0, 2.0])
            .write(&path)
            .expect("first write");
        let mut second = Checkpoint::new(&dimensions(), &[3.0, 4.0]);
        second.completed = 2;
        second.write(&path).expect("second write");

        assert_eq!(Checkpoint::read(&path).unwrap().unwrap(), second);
        assert!(
            !path.with_file_name("checkpoint.toml.partial").exists(),
            "the temporary file must be renamed away, not left behind"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_checkpoint_from_a_different_parameter_set_is_refused_not_reordered() {
        let checkpoint = Checkpoint::new(&dimensions(), &[1.0, 2.0]);

        let mut renamed = dimensions();
        renamed[1].name = "RfpMarginPerDepth".to_string();
        let error = checkpoint
            .theta_for(&renamed)
            .expect_err("should be refused");
        assert!(error.contains("different run"), "{error}");

        let error = checkpoint
            .theta_for(&dimensions()[..1])
            .expect_err("should be refused");
        assert!(error.contains("different run"), "{error}");
    }

    #[test]
    fn a_truncated_or_malformed_checkpoint_is_an_error_rather_than_a_silent_restart() {
        for (text, expected) in [
            ("games_played = 0\n", "missing required key 'completed'"),
            ("completed = 1\n", "missing required key 'games_played'"),
            ("completed = 1\ngames_played = 8\n", "records no parameters"),
            (
                "completed = -1\ngames_played = 8\n[[theta]]\nname = \"A\"\nvalue = 1.0\n",
                "must not be negative",
            ),
            (
                "completed = 1\ngames_played = 8\n[[theta]]\nname = \"A\"\n",
                "missing required key 'value'",
            ),
        ] {
            let error = Checkpoint::parse(text, "checkpoint.toml").expect_err("should be rejected");
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }
}
