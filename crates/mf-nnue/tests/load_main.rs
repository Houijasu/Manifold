use std::path::PathBuf;

use mf_nnue::{
    FC0_OUT, FC1_IN, FC1_OUT, FC2_IN, HALF_KA_DIMS, L1, LAYER_STACKS, LoadError, Network,
    PSQT_BUCKETS, THREAT_DIMS, VERSION,
};

const EXPECTED_FILE_SIZE: u64 = 111_261_604;
const EXPECTED_DESCRIPTION_LENGTH: usize = 84;

#[test]
fn local_eonego_full_threats_net_loads_strictly_and_is_aligned() {
    let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
    let path = explicit_path.clone().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    let Ok(metadata) = std::fs::metadata(&path) else {
        assert!(
            explicit_path.is_none(),
            "MF_NNUE_TEST_NET requires an existing network file: {}",
            path.display()
        );
        eprintln!(
            "SKIPPED: local NNUE load test is missing {}",
            path.display()
        );
        return;
    };

    assert_eq!(metadata.len(), EXPECTED_FILE_SIZE);

    let network = Network::load(&path).expect("local FullThreats net should load");
    assert_eq!(network.version(), VERSION);
    assert_eq!(network.description().len(), EXPECTED_DESCRIPTION_LENGTH);
    assert_eq!(network.feature_transformer_biases().len(), L1);
    assert_eq!(network.half_ka_weights().len(), HALF_KA_DIMS);
    assert_eq!(network.threat_weights().len(), THREAT_DIMS);
    assert_eq!(network.psqt_weights().len(), HALF_KA_DIMS * PSQT_BUCKETS);
    assert_eq!(
        network.threat_psqt_weights().len(),
        THREAT_DIMS * PSQT_BUCKETS
    );
    assert_eq!(network.layer_stacks().len(), LAYER_STACKS);

    for feature in 0..HALF_KA_DIMS {
        let row = network
            .half_ka_weights()
            .row(feature)
            .expect("HalfKA row should exist");
        assert_eq!((row.as_ptr() as usize) % 64, 0);
    }
    for feature in 0..THREAT_DIMS {
        let row = network
            .threat_weights()
            .row(feature)
            .expect("threat row should exist");
        assert_eq!((row.as_ptr() as usize) % 64, 0);
    }

    for stack in network.layer_stacks() {
        assert_eq!(stack.fc0_biases().len(), FC0_OUT);
        assert_eq!(stack.fc0_weights().len(), FC0_OUT * L1);
        assert_eq!(stack.fc1_biases().len(), FC1_OUT);
        assert_eq!(stack.fc1_weights().len(), FC1_OUT * FC1_IN);
        assert_eq!(stack.fc2_weights().len(), FC2_IN);
    }
    drop(network);

    let mut bytes = std::fs::read(path).expect("local FullThreats net should remain readable");
    bytes.push(0);
    assert!(matches!(
        Network::from_bytes(&bytes),
        Err(LoadError::TrailingBytes(1))
    ));
}
