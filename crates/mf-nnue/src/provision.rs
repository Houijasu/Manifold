use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::{LoadError, Network};

/// Identifies where a resolved NNUE network came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkSource {
    /// A path supplied explicitly through configuration such as UCI `EvalFile`.
    Explicit(PathBuf),
    /// A `nets/main.nnue` file discovered through automatic lookup.
    Discovered(PathBuf),
    /// The network compiled into an `embedded-net` build.
    Embedded,
}

impl fmt::Display for NetworkSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit(path) => write!(formatter, "explicit path {}", path.display()),
            Self::Discovered(path) => write!(formatter, "discovered path {}", path.display()),
            Self::Embedded => formatter.write_str("embedded network"),
        }
    }
}

/// A parsed network paired with the source selected by provisioning.
pub struct ResolvedNetwork {
    network: Network,
    source: NetworkSource,
}

impl ResolvedNetwork {
    /// Returns the parsed network.
    #[must_use]
    pub const fn network(&self) -> &Network {
        &self.network
    }

    /// Returns the source selected by provisioning.
    #[must_use]
    pub const fn source(&self) -> &NetworkSource {
        &self.source
    }

    /// Splits the resolution result into its network and source.
    #[must_use]
    pub fn into_parts(self) -> (Network, NetworkSource) {
        (self.network, self.source)
    }
}

impl fmt::Debug for ResolvedNetwork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedNetwork")
            .field("source", &self.source)
            .field("description", &self.network.description())
            .finish()
    }
}

/// Failure to locate or parse the selected NNUE source.
#[derive(Debug)]
pub enum ResolveError {
    /// The process executable path could not be queried.
    CurrentExecutable(io::Error),
    /// The process executable path had no containing directory.
    ExecutableHasNoParent(PathBuf),
    /// The process working directory could not be queried.
    CurrentDirectory(io::Error),
    /// A selected explicit or discovered source could not be loaded strictly.
    Load {
        source: NetworkSource,
        error: LoadError,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExecutable(error) => {
                write!(formatter, "could not determine executable path: {error}")
            }
            Self::ExecutableHasNoParent(path) => write!(
                formatter,
                "executable path has no containing directory: {}",
                path.display()
            ),
            Self::CurrentDirectory(error) => {
                write!(formatter, "could not determine working directory: {error}")
            }
            Self::Load { source, error } => {
                write!(formatter, "unable to load NNUE from {source}: {error}")
            }
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExecutable(error) | Self::CurrentDirectory(error) => Some(error),
            Self::Load { error, .. } => Some(error),
            Self::ExecutableHasNoParent(_) => None,
        }
    }
}

/// Resolves an NNUE network using the engine's strict source precedence.
///
/// An explicit path is always authoritative. Automatic lookup checks
/// `nets/main.nnue` beside the executable, then beneath the current working
/// directory, and finally the optional embedded network.
pub fn resolve_network(
    explicit_path: Option<&Path>,
) -> Result<Option<ResolvedNetwork>, ResolveError> {
    let executable = std::env::current_exe().map_err(ResolveError::CurrentExecutable)?;
    let executable_directory = executable
        .parent()
        .ok_or_else(|| ResolveError::ExecutableHasNoParent(executable.clone()))?;
    let working_directory = std::env::current_dir().map_err(ResolveError::CurrentDirectory)?;
    resolve_network_from(explicit_path, executable_directory, &working_directory)
}

pub(crate) fn resolve_network_from(
    explicit_path: Option<&Path>,
    executable_directory: &Path,
    working_directory: &Path,
) -> Result<Option<ResolvedNetwork>, ResolveError> {
    if let Some(path) = explicit_path {
        return load(path, NetworkSource::Explicit(path.to_path_buf())).map(Some);
    }

    for path in default_candidate_paths(executable_directory, working_directory) {
        match std::fs::metadata(&path) {
            Ok(_) => {
                let source = NetworkSource::Discovered(path.clone());
                return load(&path, source).map(Some);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ResolveError::Load {
                    source: NetworkSource::Discovered(path),
                    error: LoadError::Io(error),
                });
            }
        }
    }

    #[cfg(feature = "embedded-net")]
    {
        return Network::from_embedded()
            .map(|network| {
                Some(ResolvedNetwork {
                    network,
                    source: NetworkSource::Embedded,
                })
            })
            .map_err(|error| ResolveError::Load {
                source: NetworkSource::Embedded,
                error,
            });
    }

    #[cfg(not(feature = "embedded-net"))]
    Ok(None)
}

fn load(path: &Path, source: NetworkSource) -> Result<ResolvedNetwork, ResolveError> {
    Network::load(path)
        .map(|network| ResolvedNetwork {
            network,
            source: source.clone(),
        })
        .map_err(|error| ResolveError::Load { source, error })
}

pub(crate) fn default_candidate_paths(
    executable_directory: &Path,
    working_directory: &Path,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    [executable_directory, working_directory]
        .into_iter()
        .map(|directory| directory.join("nets/main.nnue"))
        .filter(|candidate| seen.insert(candidate_identity(candidate)))
        .collect()
}

fn candidate_identity(candidate: &Path) -> PathBuf {
    candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{NetworkSource, ResolveError, default_candidate_paths, resolve_network_from};
    use crate::LoadError;

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(name: &str) -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "manifold-nnue-provision-{name}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("unique temporary tree should be created");
            Self { root }
        }

        fn directory(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(&path).expect("temporary directory should be created");
            path
        }

        fn candidate(&self, directory: &Path) -> PathBuf {
            let path = directory.join("nets/main.nnue");
            fs::create_dir_all(path.parent().expect("candidate should have a parent"))
                .expect("candidate parent should be created");
            path
        }

        fn link_main_network(&self, destination: &Path) {
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
            fs::hard_link(source, destination).expect("test network hard link should be created");
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn explicit_missing_path_fails_without_checking_defaults() {
        let tree = TempTree::new("explicit-missing");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");
        let default = tree.candidate(&executable_directory);
        tree.link_main_network(&default);
        let missing = tree.root.join("missing.nnue");

        let error = resolve_network_from(Some(&missing), &executable_directory, &working_directory)
            .expect_err("missing explicit path must be a hard error");

        assert!(matches!(
            error,
            ResolveError::Load {
                source: NetworkSource::Explicit(path),
                error: LoadError::Io(_),
            } if path == missing
        ));
    }

    #[test]
    fn explicit_invalid_file_fails_without_checking_defaults() {
        let tree = TempTree::new("explicit-invalid");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");
        let default = tree.candidate(&executable_directory);
        tree.link_main_network(&default);
        let invalid = tree.root.join("invalid.nnue");
        fs::write(&invalid, b"not an NNUE network").expect("invalid fixture should be written");

        let error = resolve_network_from(Some(&invalid), &executable_directory, &working_directory)
            .expect_err("invalid explicit path must be a hard error");

        assert!(matches!(
            error,
            ResolveError::Load {
                source: NetworkSource::Explicit(path),
                error: LoadError::UnexpectedVersion { .. },
            } if path == invalid
        ));
    }

    #[test]
    fn executable_relative_candidate_wins_over_working_directory_candidate() {
        let tree = TempTree::new("executable-wins");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");
        let executable_candidate = tree.candidate(&executable_directory);
        let working_candidate = tree.candidate(&working_directory);
        tree.link_main_network(&executable_candidate);
        fs::write(&working_candidate, b"invalid fallback")
            .expect("invalid fallback should be written");

        let resolved = resolve_network_from(None, &executable_directory, &working_directory)
            .expect("the first candidate should load")
            .expect("the first candidate should resolve");

        assert_eq!(
            resolved.source(),
            &NetworkSource::Discovered(executable_candidate)
        );
    }

    #[test]
    fn invalid_existing_executable_candidate_does_not_fall_through() {
        let tree = TempTree::new("invalid-executable");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");
        let executable_candidate = tree.candidate(&executable_directory);
        let working_candidate = tree.candidate(&working_directory);
        fs::write(&executable_candidate, b"invalid first candidate")
            .expect("invalid first candidate should be written");
        tree.link_main_network(&working_candidate);

        let error = resolve_network_from(None, &executable_directory, &working_directory)
            .expect_err("an invalid existing first candidate must be a hard error");

        assert!(matches!(
            error,
            ResolveError::Load {
                source: NetworkSource::Discovered(path),
                error: LoadError::UnexpectedVersion { .. },
            } if path == executable_candidate
        ));
    }

    #[test]
    fn working_directory_candidate_is_used_when_executable_candidate_is_absent() {
        let tree = TempTree::new("working-fallback");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");
        let working_candidate = tree.candidate(&working_directory);
        tree.link_main_network(&working_candidate);

        let resolved = resolve_network_from(None, &executable_directory, &working_directory)
            .expect("working-directory candidate should load")
            .expect("working-directory candidate should resolve");

        assert_eq!(
            resolved.source(),
            &NetworkSource::Discovered(working_candidate)
        );
    }

    #[test]
    fn duplicate_default_candidate_paths_are_represented_once() {
        let tree = TempTree::new("duplicate");
        let directory = tree.directory("same");

        assert_eq!(default_candidate_paths(&directory, &directory).len(), 1);
    }

    #[test]
    fn no_candidates_and_no_embed_returns_none() {
        let tree = TempTree::new("none");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");

        let resolved = resolve_network_from(None, &executable_directory, &working_directory)
            .expect("absent default candidates should not be errors");

        #[cfg(not(feature = "embedded-net"))]
        assert!(resolved.is_none());
        #[cfg(feature = "embedded-net")]
        assert_eq!(
            resolved
                .expect("embedded feature should supply a network")
                .source(),
            &NetworkSource::Embedded
        );
    }

    #[cfg(feature = "embedded-net")]
    #[test]
    fn embedded_source_loads_and_reports_itself() {
        let tree = TempTree::new("embedded");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");

        let resolved = resolve_network_from(None, &executable_directory, &working_directory)
            .expect("embedded network should parse")
            .expect("embedded network should resolve");

        assert_eq!(resolved.source(), &NetworkSource::Embedded);
        assert!(!resolved.network().description().is_empty());
    }
}
