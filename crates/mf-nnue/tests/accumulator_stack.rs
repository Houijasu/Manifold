use std::path::PathBuf;

use mf_core::{Move, Position, Undo, generate_legal_moves, parse_uci_move};
use mf_nnue::{
    ACCUMULATOR_STACK_CAPACITY, AccumulatorStack, AccumulatorStackError, AccumulatorState, L1,
    Network,
};

fn local_network() -> Option<Network> {
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
        eprintln!(
            "SKIPPED: local NNUE accumulator-stack test is missing {}",
            path.display()
        );
        return None;
    }
    Some(
        Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}

fn assert_matches_from_scratch(
    network: &Network,
    stack: &AccumulatorStack<'_>,
    position: &Position,
) {
    let rebuilt = AccumulatorState::from_position(network, position);
    assert_eq!(stack.current(), &rebuilt);

    let mut incremental_features = [0; L1];
    let mut rebuilt_features = [0; L1];
    let incremental_dump = stack.dump_features(position, &mut incremental_features);
    let rebuilt_dump = network.dump_features(position, &mut rebuilt_features);
    assert_eq!(incremental_dump, rebuilt_dump);
    assert_eq!(incremental_features, rebuilt_features);
    assert_eq!(
        stack.evaluate_internal(position),
        network.evaluate_internal(position)
    );
    assert_eq!(stack.evaluate(position), network.evaluate(position));

    let mut supplied_features = [0; L1];
    assert_eq!(
        network.dump_features_from_state(position, stack.current(), &mut supplied_features),
        rebuilt_dump
    );
    assert_eq!(supplied_features, rebuilt_features);
    assert_eq!(
        network.evaluate_internal_from_state(position, stack.current()),
        network.evaluate_internal(position)
    );
    assert_eq!(
        network.evaluate_from_state(position, stack.current()),
        network.evaluate(position)
    );
}

fn parse(position: &Position, notation: &str, chess960: bool) -> Move {
    parse_uci_move(position, notation, chess960)
        .unwrap_or_else(|| panic!("{notation} should be legal in {position:?}"))
}

fn push_one(
    network: &Network,
    fen: &str,
    notation: &str,
    chess960: bool,
) -> (Position, Move, Undo) {
    let parent = Position::from_fen(fen, chess960).expect("targeted FEN should parse");
    let mut child = parent.clone();
    let mv = parse(&parent, notation, chess960);
    let undo = child.make_move(mv);
    let mut stack = AccumulatorStack::new(network, &parent);

    stack
        .push_real(&child, mv, &undo)
        .expect("targeted real push should fit");
    assert_matches_from_scratch(network, &stack, &child);

    child.unmake_move(mv, undo.clone());
    stack.pop().expect("targeted pop should return to root");
    assert_eq!(child, parent);
    assert_matches_from_scratch(network, &stack, &parent);
    (parent, mv, undo)
}

#[test]
fn real_push_uses_only_the_already_updated_child_position() {
    let Some(network) = local_network() else {
        return;
    };
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);
    let mv = parse(&position, "e2e4", false);
    let undo = position.make_move(mv);

    stack
        .push_real(&position, mv, &undo)
        .expect("parent-free real push should fit");
    assert_matches_from_scratch(&network, &stack, &position);
}

#[test]
fn root_state_dump_and_evaluation_match_from_scratch() {
    let Some(network) = local_network() else {
        return;
    };
    let position = Position::startpos();
    let stack = AccumulatorStack::new(&network, &position);

    assert_eq!(stack.depth(), 0);
    assert_eq!(stack.capacity(), ACCUMULATOR_STACK_CAPACITY);
    assert_eq!(std::mem::align_of::<AccumulatorState>(), 64);
    assert_eq!(
        (stack.current() as *const AccumulatorState as usize) % 64,
        0
    );
    assert_matches_from_scratch(&network, &stack, &position);
}

#[test]
fn deterministic_legal_walk_matches_from_scratch_and_unwinds_exactly() {
    let Some(network) = local_network() else {
        return;
    };
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);
    let mut history = Vec::new();
    let mut state = 0xD1B5_4A32_D192_ED03_u64;

    for ply in 0..96 {
        let moves = generate_legal_moves(&position);
        assert!(
            !moves.is_empty(),
            "deterministic walk ended before requested ply {ply}"
        );
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mv = moves[state as usize % moves.len()];
        let undo = position.make_move(mv);
        stack
            .push_real(&position, mv, &undo)
            .expect("walk should fit accumulator capacity");
        assert_eq!(stack.depth(), ply + 1);
        assert_matches_from_scratch(&network, &stack, &position);
        history.push((mv, undo));
    }

    while let Some((mv, undo)) = history.pop() {
        position.unmake_move(mv, undo);
        stack.pop().expect("walk pop should have a parent");
        assert_matches_from_scratch(&network, &stack, &position);
    }
    assert_eq!(position, Position::startpos());
    assert_eq!(stack.depth(), 0);
}

#[test]
fn captures_en_passant_promotions_and_castling_match_from_scratch() {
    let Some(network) = local_network() else {
        return;
    };

    push_one(&network, "4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "e4d5", false);
    push_one(&network, "4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6", false);

    for suffix in ["n", "b", "r", "q"] {
        push_one(
            &network,
            "4k3/P7/8/8/8/8/8/4K3 w - - 0 1",
            &format!("a7a8{suffix}"),
            false,
        );
    }

    push_one(&network, "4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1g1", false);
    push_one(&network, "4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1", "c1f1", true);
    push_one(&network, "4k3/8/8/8/8/8/8/3R2KR w HD - 0 1", "g1d1", true);
    push_one(&network, "4k3/8/8/8/8/8/8/6KR w H - 0 1", "g1h1", true);
    push_one(&network, "4k3/8/8/8/8/8/8/4KR2 w F - 0 1", "e1f1", true);
}

#[test]
fn stored_threat_lists_survive_pop_and_multiple_sibling_branches() {
    let Some(network) = local_network() else {
        return;
    };
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);

    let first = parse(&position, "e2e4", false);
    let first_undo = position.make_move(first);
    stack
        .push_real(&position, first, &first_undo)
        .expect("first branch should fit");
    assert_matches_from_scratch(&network, &stack, &position);

    for notation in ["d7d5", "c7c5", "g8f6"] {
        let sibling = parse(&position, notation, false);
        let sibling_undo = position.make_move(sibling);
        stack
            .push_real(&position, sibling, &sibling_undo)
            .expect("child sibling should fit");
        assert_matches_from_scratch(&network, &stack, &position);
        position.unmake_move(sibling, sibling_undo);
        stack.pop().expect("child sibling should return to parent");
        assert_matches_from_scratch(&network, &stack, &position);
    }

    position.unmake_move(first, first_undo);
    stack.pop().expect("first branch should return to root");
    assert_matches_from_scratch(&network, &stack, &position);

    for notation in ["d2d4", "g1f3", "c2c4"] {
        let sibling = parse(&position, notation, false);
        let sibling_undo = position.make_move(sibling);
        stack
            .push_real(&position, sibling, &sibling_undo)
            .expect("root sibling should fit");
        assert_matches_from_scratch(&network, &stack, &position);
        position.unmake_move(sibling, sibling_undo);
        stack.pop().expect("root sibling should return to root");
        assert_matches_from_scratch(&network, &stack, &position);
    }
}

#[test]
fn null_push_copies_state_and_pop_restores_depth() {
    let Some(network) = local_network() else {
        return;
    };
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);
    let root = stack.current().clone();
    let undo = position.make_null_move();

    stack.push_null().expect("null push should fit");
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current(), &root);
    assert_matches_from_scratch(&network, &stack, &position);

    position.unmake_null_move(undo);
    stack.pop().expect("null pop should return to root");
    assert_eq!(stack.depth(), 0);
    assert_eq!(stack.current(), &root);
    assert_matches_from_scratch(&network, &stack, &position);
}

#[test]
fn real_push_after_null_uses_copied_frame_metadata() {
    let Some(network) = local_network() else {
        return;
    };
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);
    let null_undo = position.make_null_move();
    stack.push_null().expect("null push should fit");

    let mv = parse(&position, "e7e5", false);
    let undo = position.make_move(mv);
    stack
        .push_real(&position, mv, &undo)
        .expect("real push after null should fit");
    assert_matches_from_scratch(&network, &stack, &position);

    position.unmake_move(mv, undo);
    stack.pop().expect("real pop should return to null frame");
    assert_matches_from_scratch(&network, &stack, &position);
    position.unmake_null_move(null_undo);
    stack.pop().expect("null pop should return to root");
    assert_matches_from_scratch(&network, &stack, &position);
}

#[test]
fn depth_errors_are_reported_without_indexing_out_of_bounds() {
    let Some(network) = local_network() else {
        return;
    };
    let position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);

    assert_eq!(stack.pop(), Err(AccumulatorStackError::AtRoot));
    for _ in 0..ACCUMULATOR_STACK_CAPACITY {
        stack.push_null().expect("all advertised plies should fit");
    }
    assert_eq!(stack.depth(), ACCUMULATOR_STACK_CAPACITY);
    assert_eq!(
        stack.push_null(),
        Err(AccumulatorStackError::CapacityExceeded {
            capacity: ACCUMULATOR_STACK_CAPACITY,
        })
    );
}
