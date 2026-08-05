//! Test-only helpers shared between the crate's unit-test modules.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::network::Network;

pub(crate) fn resolve_network_path(explicit_path: Option<OsString>) -> (PathBuf, bool) {
    let is_explicit = explicit_path.is_some();
    let path = explicit_path.map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    (path, is_explicit)
}

/// Loads the shared test network, or reports a skip when it is absent.
pub(crate) fn local_network(test_name: &str) -> Option<Network> {
    let (path, is_explicit) = resolve_network_path(std::env::var_os("MF_NNUE_TEST_NET"));
    if !path.is_file() {
        assert!(
            !is_explicit,
            "MF_NNUE_TEST_NET requires an existing network file: {}",
            path.display()
        );
        eprintln!("SKIPPED: {test_name} is missing {}", path.display());
        return None;
    }
    Some(
        Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}
