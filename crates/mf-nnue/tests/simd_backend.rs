use std::path::PathBuf;

use mf_core::{Position, parse_uci_move};
use mf_nnue::{
    AccumulatorStack, AccumulatorState, ForwardMode, L1, Network, SimdBackend,
    UnsupportedBackendReason, production_forward_mode,
};

fn local_network() -> Option<Network> {
    let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    if !path.is_file() {
        eprintln!("skipping SIMD backend test: {} is absent", path.display());
        return None;
    }
    Some(Network::load(path).expect("local FullThreats net should load"))
}

fn supported_backends() -> Vec<SimdBackend> {
    let mut backends = vec![SimdBackend::Scalar];
    for backend in [SimdBackend::Avx2, SimdBackend::Avx2Vnni] {
        if backend.is_supported() {
            backends.push(backend);
        }
    }
    backends
}

fn supported_forward_modes() -> Vec<ForwardMode> {
    let mut modes = vec![ForwardMode::scalar()];
    for backend in [SimdBackend::Avx2, SimdBackend::Avx2Vnni] {
        if backend.is_supported() {
            modes.push(ForwardMode::new(backend, false).expect("backend is supported"));
            modes.push(ForwardMode::new(backend, true).expect("backend is supported"));
        }
    }
    modes
}

#[test]
fn explicit_backend_selection_reports_unsupported_backends_safely() {
    assert_eq!(ForwardMode::scalar().backend(), SimdBackend::Scalar);
    assert!(!ForwardMode::scalar().sparse_fc0());

    for backend in [SimdBackend::Avx2, SimdBackend::Avx2Vnni] {
        assert_eq!(
            ForwardMode::new(backend, false).is_ok(),
            backend.is_supported()
        );
        assert_eq!(
            ForwardMode::new(backend, true).is_ok(),
            backend.is_supported()
        );
    }
    let sparse_scalar =
        ForwardMode::new(SimdBackend::Scalar, true).expect_err("scalar sparse mode must fail");
    assert_eq!(sparse_scalar.backend(), SimdBackend::Scalar);
    assert_eq!(
        sparse_scalar.reason(),
        UnsupportedBackendReason::SparseFc0RequiresSimd
    );
    assert_eq!(
        sparse_scalar.to_string(),
        "sparse FC0 is not supported by the scalar NNUE backend"
    );

    let production = production_forward_mode();
    assert!(production.backend().is_supported());
    assert!(!production.sparse_fc0() || production.backend() != SimdBackend::Scalar);
}

#[test]
fn scalar_defaults_and_production_constructors_match_explicit_backends() {
    let Some(network) = local_network() else {
        return;
    };
    let position = Position::startpos();

    let scalar =
        AccumulatorState::from_position_with_backend(&network, &position, SimdBackend::Scalar)
            .expect("scalar backend is always supported");
    assert_eq!(AccumulatorState::from_position(&network, &position), scalar);
    assert_eq!(
        AccumulatorStack::new(&network, &position).current(),
        &scalar
    );

    let production_backend = production_forward_mode().backend();
    let production =
        AccumulatorState::from_position_with_backend(&network, &position, production_backend)
            .expect("production backend must be supported");
    assert_eq!(
        AccumulatorState::from_position_production(&network, &position),
        production
    );
    assert_eq!(
        AccumulatorStack::new_production(&network, &position).current(),
        &production
    );
}

#[test]
fn supported_backends_match_scalar_for_normal_and_incremental_special_moves() {
    let Some(network) = local_network() else {
        return;
    };
    let cases = [
        (
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            None,
            false,
        ),
        ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", Some("e5d6"), false),
        ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", Some("a7a8q"), false),
        ("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", Some("e1g1"), false),
        ("4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1", Some("c1f1"), true),
    ];

    for (fen, notation, chess960) in cases {
        let root = Position::from_fen(fen, chess960).expect("test FEN should parse");
        let scalar_state =
            AccumulatorState::from_position_with_backend(&network, &root, SimdBackend::Scalar)
                .expect("scalar backend is always supported");
        for backend in supported_backends() {
            let state = AccumulatorState::from_position_with_backend(&network, &root, backend)
                .expect("mode was filtered for support");
            assert_eq!(
                state, scalar_state,
                "root accumulator mismatch: {fen}, {backend:?}"
            );

            let Some(notation) = notation else {
                continue;
            };
            let mut child = root.clone();
            let mv = parse_uci_move(&root, notation, chess960)
                .unwrap_or_else(|| panic!("{notation} should be legal"));
            let undo = child.make_move(mv);
            let mut stack = AccumulatorStack::new_with_backend(&network, &root, backend)
                .expect("mode was filtered for support");
            stack
                .push_real(&child, mv, &undo)
                .expect("one move must fit");

            let rebuilt = AccumulatorState::from_position_with_backend(&network, &child, backend)
                .expect("mode was filtered for support");
            assert_eq!(
                stack.current(),
                &rebuilt,
                "incremental accumulator mismatch: {fen}, {notation}, {backend:?}"
            );

            let scalar_child =
                AccumulatorState::from_position_with_backend(&network, &child, SimdBackend::Scalar)
                    .expect("scalar backend is always supported");
            assert_eq!(
                stack.current(),
                &scalar_child,
                "backend accumulator mismatch: {fen}, {notation}, {backend:?}"
            );
        }
    }
}

#[test]
fn all_forward_modes_match_scalar_for_supplied_state_eval_and_dump() {
    let Some(network) = local_network() else {
        return;
    };
    let position = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R b KQkq - 0 1",
        false,
    )
    .expect("test FEN should parse");
    let state = AccumulatorState::from_position(&network, &position);
    let mut expected_ft = [0_u8; L1];
    let expected = network.dump_features_from_state(&position, &state, &mut expected_ft);

    for mode in supported_forward_modes() {
        let mut actual_ft = [0_u8; L1];
        let actual =
            network.dump_features_from_state_with_mode(&position, &state, &mut actual_ft, mode);
        assert_eq!(actual, expected, "{mode:?} supplied-state dump");
        assert_eq!(actual_ft, expected_ft, "{mode:?} supplied-state FT");
        assert_eq!(
            network.evaluate_internal_from_state_with_mode(&position, &state, mode),
            expected.eval_internal,
            "{mode:?} supplied-state internal eval"
        );
        assert_eq!(
            network.evaluate_from_state_with_mode(&position, &state, mode),
            network.evaluate_from_state(&position, &state),
            "{mode:?} supplied-state centipawn eval"
        );
    }

    let production_mode = production_forward_mode();
    let mut production_ft = [0_u8; L1];
    let production =
        network.dump_features_from_state_production(&position, &state, &mut production_ft);
    let mut explicit_ft = [0_u8; L1];
    let explicit = network.dump_features_from_state_with_mode(
        &position,
        &state,
        &mut explicit_ft,
        production_mode,
    );
    assert_eq!(production, explicit);
    assert_eq!(production_ft, explicit_ft);
    assert_eq!(
        network.evaluate_from_state_production(&position, &state),
        network.evaluate_from_state_with_mode(&position, &state, production_mode)
    );
}

#[test]
fn stack_forward_modes_match_scalar_after_incremental_special_moves() {
    let Some(network) = local_network() else {
        return;
    };
    let cases = [
        ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6", false),
        ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8q", false),
        ("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1g1", false),
        ("4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1", "c1f1", true),
    ];

    for (fen, notation, chess960) in cases {
        let root = Position::from_fen(fen, chess960).expect("test FEN should parse");
        let mv = parse_uci_move(&root, notation, chess960)
            .unwrap_or_else(|| panic!("{notation} should be legal"));
        let mut child = root.clone();
        let undo = child.make_move(mv);

        for mode in supported_forward_modes() {
            let mut stack = AccumulatorStack::new_with_mode(&network, &root, mode)
                .expect("mode was filtered for support");
            stack
                .push_real(&child, mv, &undo)
                .expect("one move must fit");
            let mut scalar_ft = [0_u8; L1];
            let scalar = network.dump_features_from_state(&child, stack.current(), &mut scalar_ft);
            let mut actual_ft = [0_u8; L1];
            let actual = stack.dump_features(&child, &mut actual_ft);
            assert_eq!(actual, scalar, "{mode:?}: {fen}, {notation}");
            assert_eq!(actual_ft, scalar_ft, "{mode:?}: {fen}, {notation}");
            assert_eq!(
                stack.evaluate_internal(&child),
                scalar.eval_internal,
                "{mode:?}: {fen}, {notation}"
            );
            assert_eq!(
                stack.evaluate(&child),
                network.evaluate_from_state(&child, stack.current()),
                "{mode:?}: {fen}, {notation}"
            );
        }
    }
}
