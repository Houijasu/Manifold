use std::path::PathBuf;

use mf_nnue::{ARCHITECTURE_HASH, FEATURE_TRANSFORMER_HASH, LoadError, Network, VERSION};

/// Reads the shared test network bytes, or reports a skip when absent.
fn network_bytes(test_name: &str) -> Option<Vec<u8>> {
    let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
    let path = explicit_path.clone().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    if !path.is_file() {
        assert!(
            explicit_path.is_none(),
            "MF_NNUE_TEST_NET requires an existing network file: {}",
            path.display()
        );
        eprintln!("SKIPPED: {test_name} is missing {}", path.display());
        return None;
    }
    Some(std::fs::read(&path).expect("test network should remain readable"))
}

/// Overwrites the little-endian `u32` at byte `offset` without changing the file length.
fn patch_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// The field layout in front of the description: version, then architecture hash.
const ARCHITECTURE_HASH_OFFSET: usize = 4;

/// `version | architecture hash | description length (i32) | description`.
fn feature_transformer_hash_offset(bytes: &[u8]) -> usize {
    let description_length = i32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .expect("header holds the description length"),
    );
    let description_length = usize::try_from(description_length)
        .expect("the shipped description length is non-negative");
    12 + description_length
}

#[test]
fn unmodified_net_bytes_still_load() {
    let Some(bytes) = network_bytes("unmodified net load test") else {
        return;
    };
    let network = Network::from_bytes(&bytes).expect("unmodified net should load");
    assert_eq!(network.version(), VERSION);
    assert_eq!(network.architecture_hash(), ARCHITECTURE_HASH);
    assert_eq!(network.feature_transformer_hash(), FEATURE_TRANSFORMER_HASH);
}

#[test]
fn corrupted_architecture_hash_is_rejected_with_a_named_error() {
    let Some(mut bytes) = network_bytes("architecture-hash corruption test") else {
        return;
    };
    patch_u32(
        &mut bytes,
        ARCHITECTURE_HASH_OFFSET,
        ARCHITECTURE_HASH ^ 0xFFFF_0000,
    );
    assert!(matches!(
        Network::from_bytes(&bytes),
        Err(LoadError::UnexpectedArchitectureHash {
            found,
            expected: ARCHITECTURE_HASH,
        }) if found == ARCHITECTURE_HASH ^ 0xFFFF_0000
    ));
}

#[test]
fn corrupted_feature_transformer_hash_is_rejected_with_a_named_error() {
    let Some(mut bytes) = network_bytes("feature-transformer-hash corruption test") else {
        return;
    };
    let offset = feature_transformer_hash_offset(&bytes);
    let corrupted = FEATURE_TRANSFORMER_HASH.rotate_left(8);
    patch_u32(&mut bytes, offset, corrupted);
    assert!(matches!(
        Network::from_bytes(&bytes),
        Err(LoadError::UnexpectedFeatureTransformerHash {
            found,
            expected: FEATURE_TRANSFORMER_HASH,
        }) if found == corrupted
    ));
}
