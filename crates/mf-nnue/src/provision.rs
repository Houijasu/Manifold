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
    /// No source yielded a network.
    ///
    /// Only reachable in a build without the `embedded-net` feature. The engine is pure
    /// NNUE, so this is fatal rather than a cue to fall back to something weaker.
    NoNetwork { searched: Vec<PathBuf> },
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
            Self::NoNetwork { searched } => {
                write!(
                    formatter,
                    "no NNUE network found (searched: {}); build with the `embedded-net` \
                     feature or set the EvalFile option",
                    searched
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExecutable(error) | Self::CurrentDirectory(error) => Some(error),
            Self::Load { error, .. } => Some(error),
            Self::ExecutableHasNoParent(_) | Self::NoNetwork { .. } => None,
        }
    }
}

/// Resolves an NNUE network using the engine's strict source precedence.
///
/// An explicit path is always authoritative. Automatic lookup checks `nets/main.nnue`
/// beside the executable and beneath the current working directory -- each along with
/// its ancestors -- and finally the embedded network.
///
/// Resolution either yields a network or fails. The engine has no hand-crafted
/// evaluation to fall back on, so "no network" is not a degraded mode it can run in,
/// and a default build embeds one precisely so this cannot fail in practice.
pub fn resolve_network(explicit_path: Option<&Path>) -> Result<ResolvedNetwork, ResolveError> {
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
) -> Result<ResolvedNetwork, ResolveError> {
    if let Some(path) = explicit_path {
        return load(path, NetworkSource::Explicit(path.to_path_buf()));
    }

    let candidates = default_candidate_paths(executable_directory, working_directory);
    for path in &candidates {
        match std::fs::metadata(path) {
            Ok(_) => {
                let source = NetworkSource::Discovered(path.clone());
                return load(path, source);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ResolveError::Load {
                    source: NetworkSource::Discovered(path.clone()),
                    error: LoadError::Io(error),
                });
            }
        }
    }

    #[cfg(feature = "embedded-net")]
    let outcome = Network::from_embedded()
        .map(|network| ResolvedNetwork {
            network,
            source: NetworkSource::Embedded,
        })
        .map_err(|error| ResolveError::Load {
            source: NetworkSource::Embedded,
            error,
        });

    #[cfg(not(feature = "embedded-net"))]
    let outcome = Err(ResolveError::NoNetwork {
        searched: candidates,
    });

    outcome
}

fn load(path: &Path, source: NetworkSource) -> Result<ResolvedNetwork, ResolveError> {
    Network::load(path)
        .map(|network| ResolvedNetwork {
            network,
            source: source.clone(),
        })
        .map_err(|error| ResolveError::Load { source, error })
}

/// Every `nets/main.nnue` location automatic lookup will try, in precedence order.
///
/// Both the executable directory and the working directory are searched *along with
/// their ancestors*. Ancestors matter because the engine is normally run from a build
/// tree: the binary sits in `target/release/`, two levels below the `nets/` it ships
/// beside. A GUI launches the executable with its own working directory -- ChessBase
/// products use the GUI's install directory, not the engine's -- so a lookup anchored
/// only at those two exact directories finds nothing, and the engine silently falls
/// back to HCE. That fallback costs far more strength than any search bug, and it is
/// invisible unless you read the `info string evaluation` line.
///
/// Walking upward is safe because the filename is specific: a directory only matches if
/// it actually contains `nets/main.nnue`.
pub(crate) fn default_candidate_paths(
    executable_directory: &Path,
    working_directory: &Path,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    [executable_directory, working_directory]
        .into_iter()
        .flat_map(|directory| directory.ancestors())
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
            let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
            let source = explicit_path.clone().map_or_else(
                || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
                PathBuf::from,
            );
            assert!(
                source.is_file(),
                "{} requires an existing network file: {}",
                if explicit_path.is_some() {
                    "MF_NNUE_TEST_NET"
                } else {
                    "NNUE provisioning tests"
                },
                source.display()
            );
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
            .expect("the first candidate should load");

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
            .expect("working-directory candidate should load");

        assert_eq!(
            resolved.source(),
            &NetworkSource::Discovered(working_candidate)
        );
    }

    #[test]
    fn duplicate_default_candidate_paths_are_represented_once() {
        let tree = TempTree::new("duplicate");
        let directory = tree.directory("same");

        let candidates = default_candidate_paths(&directory, &directory);
        let mut unique = candidates.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(candidates.len(), unique.len(), "candidates must not repeat");
        assert!(candidates.contains(&directory.join("nets/main.nnue")));
    }

    #[test]
    fn a_network_above_the_executable_is_found_from_a_foreign_working_directory() {
        // The shipping layout: binary in `<root>/target/release`, network in
        // `<root>/nets`, and a GUI supplying its own unrelated working directory.
        let tree = TempTree::new("ancestor");
        let root = tree.directory("root");
        let executable_directory = tree.directory("root/target/release");
        let working_directory = tree.directory("elsewhere");
        let network = tree.candidate(&root);
        tree.link_main_network(&network);

        let resolved = resolve_network_from(None, &executable_directory, &working_directory)
            .expect("ancestor lookup should succeed");

        assert_eq!(resolved.source(), &NetworkSource::Discovered(network));
    }

    #[test]
    fn the_executable_directory_still_wins_over_an_ancestor() {
        let tree = TempTree::new("precedence");
        let root = tree.directory("root");
        let executable_directory = tree.directory("root/target/release");
        let working_directory = tree.directory("elsewhere");
        let ancestor = tree.candidate(&root);
        tree.link_main_network(&ancestor);
        let beside = tree.candidate(&executable_directory);
        tree.link_main_network(&beside);

        let resolved = resolve_network_from(None, &executable_directory, &working_directory)
            .expect("lookup should succeed");

        assert_eq!(resolved.source(), &NetworkSource::Discovered(beside));
    }

    #[test]
    fn absent_candidates_fall_back_to_the_embedded_network_or_fail_loudly() {
        let tree = TempTree::new("none");
        let executable_directory = tree.directory("exe");
        let working_directory = tree.directory("cwd");

        let resolved = resolve_network_from(None, &executable_directory, &working_directory);

        // Without an embedded network there is nothing left to evaluate with, and the
        // engine has no hand-crafted fallback, so this must be an error rather than a
        // `None` some caller could quietly ignore.
        #[cfg(not(feature = "embedded-net"))]
        assert!(matches!(
            resolved.expect_err("a build with no network must fail"),
            ResolveError::NoNetwork { .. }
        ));
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
            .expect("embedded network should parse");

        assert_eq!(resolved.source(), &NetworkSource::Embedded);
        assert!(!resolved.network().description().is_empty());
    }
}
