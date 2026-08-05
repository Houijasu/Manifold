//! Parity tests for lazy accumulator updates.
//!
//! Lazy updates defer the accumulator work a push implies until an evaluation actually reads it.
//! The invariant that makes that safe is exactly the one the eager path already had: the state
//! an evaluation reads must equal a scalar full rebuild of the position it is reading. These
//! tests drive the deferral patterns a real search produces — pushes that are popped unread,
//! chains of several deferred plies collapsed by one deep evaluation, deferred king moves in
//! both mirror tiers, null moves interleaved with deferred real moves, and re-entering a
//! sibling after an unread pop — and check that invariant at every evaluation.

use std::path::PathBuf;

use mf_core::{Move, Position, Undo, generate_legal_moves, parse_uci_move};
use mf_nnue::{AccumulatorStack, AccumulatorState, L1, Network};

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
        eprintln!("SKIPPED: lazy-update test is missing {}", path.display());
        return None;
    }
    Some(
        Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }),
    )
}

/// Asserts the lazily materialized state equals a full rebuild, through every read path.
fn assert_reads_match_rebuild(
    network: &Network,
    stack: &mut AccumulatorStack<'_>,
    position: &Position,
) {
    let rebuilt = AccumulatorState::from_position(network, position);
    assert_eq!(
        stack.current(),
        &rebuilt,
        "lazily materialized accumulator diverged in {position:?}"
    );

    let mut lazy_features = [0; L1];
    let mut rebuilt_features = [0; L1];
    assert_eq!(
        stack.dump_features(position, &mut lazy_features),
        network.dump_features(position, &mut rebuilt_features)
    );
    assert_eq!(lazy_features, rebuilt_features);
    assert_eq!(
        stack.evaluate_internal(position),
        network.evaluate_internal(position)
    );
    assert_eq!(stack.evaluate(position), network.evaluate(position));
}

fn parse(position: &Position, notation: &str, chess960: bool) -> Move {
    parse_uci_move(position, notation, chess960)
        .unwrap_or_else(|| panic!("{notation} should be legal in {position:?}"))
}

/// Pushes a line of moves without evaluating any of it, returning the unwind history.
fn push_line(
    stack: &mut AccumulatorStack<'_>,
    position: &mut Position,
    notations: &[&str],
    chess960: bool,
) -> Vec<(Move, Undo)> {
    let mut history = Vec::new();
    for notation in notations {
        let mv = parse(position, notation, chess960);
        let undo = position.make_move(mv);
        stack
            .push_real(position, mv, &undo)
            .expect("line push should fit");
        history.push((mv, undo));
    }
    history
}

fn unwind(stack: &mut AccumulatorStack<'_>, position: &mut Position, history: Vec<(Move, Undo)>) {
    for (mv, undo) in history.into_iter().rev() {
        position.unmake_move(mv, undo);
        stack.pop().expect("unwind pop should have a parent");
    }
}

#[test]
fn a_chain_of_unevaluated_pushes_materializes_correctly_at_the_first_evaluation() {
    let Some(network) = local_network() else {
        return;
    };
    // A pruned subtree in a real search looks exactly like this: many plies pushed with no
    // evaluation, then one evaluation deep inside that must collapse the whole pending chain.
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);

    let history = push_line(
        &mut stack,
        &mut position,
        &[
            "e2e4", "e7e5", "g1f3", "b8c6", "f1b5", "a7a6", "b5a4", "g8f6", "e1g1", "f8e7",
        ],
        false,
    );

    assert_reads_match_rebuild(&network, &mut stack, &position);
    unwind(&mut stack, &mut position, history);
    assert_reads_match_rebuild(&network, &mut stack, &Position::startpos());
}

#[test]
fn evaluating_at_every_ply_of_a_deferred_chain_matches_a_full_rebuild() {
    let Some(network) = local_network() else {
        return;
    };
    // Walks the same line but evaluates on the way *back up*, so each evaluation reads a frame
    // whose descendants were materialized first. Materializing a child must never corrupt the
    // parent frames it walked back through.
    let root = Position::startpos();
    let mut position = root.clone();
    let mut stack = AccumulatorStack::new(&network, &position);
    let notations = ["d2d4", "d7d5", "c2c4", "e7e6", "b1c3", "g8f6", "c1g5"];

    let mut history = push_line(&mut stack, &mut position, &notations, false);
    assert_reads_match_rebuild(&network, &mut stack, &position);

    while let Some((mv, undo)) = history.pop() {
        position.unmake_move(mv, undo);
        stack.pop().expect("deferred pop should have a parent");
        assert_reads_match_rebuild(&network, &mut stack, &position);
    }
    assert_eq!(position, root);
}

#[test]
fn a_deferred_king_move_materializes_through_both_mirror_tiers() {
    let Some(network) = local_network() else {
        return;
    };
    // King moves are the branch with the most state: the Finny table is consulted for the
    // parent's king square in the mirror-held tier, so a deferred king move must materialize its
    // parent before it reads that entry. This walks the king across the mirror line repeatedly,
    // evaluating only at the end of each deferred run.
    let mut position = Position::from_fen("4k3/8/8/3p4/3P4/8/8/4K3 w - - 0 1", false)
        .expect("king-walk FEN should parse");
    let root = position.clone();
    let mut stack = AccumulatorStack::new(&network, &position);

    // Each run starts from the root, so every list is legal from the same position.
    for run in [
        // Both kings cross the d/e mirror line and keep going.
        ["e1d1", "e8d8", "d1c1", "d8c8"].as_slice(),
        // Cross the mirror and immediately cross back, alternating the two Finny tiers.
        ["e1d1", "e8d8", "d1e1", "d8e8"].as_slice(),
        // Stay on the e-h half throughout, so the mirror holds for every ply.
        ["e1f1", "e8f8", "f1g1", "f8g8"].as_slice(),
    ] {
        let history = push_line(&mut stack, &mut position, run, false);
        assert_reads_match_rebuild(&network, &mut stack, &position);
        unwind(&mut stack, &mut position, history);
    }
    assert_eq!(position, root);
}

#[test]
fn a_sibling_searched_after_an_unread_pop_still_matches_a_full_rebuild() {
    let Some(network) = local_network() else {
        return;
    };
    // The pruning pattern that breaks a naive dirty-flag scheme: push a child, never evaluate
    // it, pop it, then push a *different* child from the same parent. Any pending state left
    // behind by the discarded branch must not leak into the sibling.
    let root = Position::from_fen(
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        false,
    )
    .expect("sibling FEN should parse");
    let mut position = root.clone();
    let mut stack = AccumulatorStack::new(&network, &position);

    for notation in ["e1g1", "e1c1", "e5d7", "e5g6", "f3f6", "d5d6"] {
        let mv = parse(&position, notation, false);
        let undo = position.make_move(mv);
        stack
            .push_real(&position, mv, &undo)
            .expect("sibling push should fit");
        // Deliberately no evaluation here for the first branches: they are popped unread.
        position.unmake_move(mv, undo);
        stack.pop().expect("sibling pop should return to root");
    }

    // Now search one sibling for real. If a discarded branch left pending state behind, this
    // evaluation reads a corrupted accumulator.
    let mv = parse(&position, "e1g1", false);
    let undo = position.make_move(mv);
    stack
        .push_real(&position, mv, &undo)
        .expect("evaluated sibling push should fit");
    assert_reads_match_rebuild(&network, &mut stack, &position);
    position.unmake_move(mv, undo);
    stack.pop().expect("evaluated sibling should pop");
    assert_reads_match_rebuild(&network, &mut stack, &root);
}

#[test]
fn null_moves_interleaved_with_deferred_real_moves_keep_parity() {
    let Some(network) = local_network() else {
        return;
    };
    // Null-move pruning pushes a null and searches below it, so a null frame frequently sits in
    // the middle of a pending chain. A null copies its parent, which means materializing a frame
    // below it has to reach through the null to the last real materialized frame.
    let mut position = Position::startpos();
    let mut stack = AccumulatorStack::new(&network, &position);

    let first = push_line(&mut stack, &mut position, &["e2e4", "c7c5"], false);
    let null_undo = position.make_null_move();
    stack.push_null().expect("null push should fit");
    // The null passed the move back to Black, so the deferred line below it starts with Black.
    let second = push_line(&mut stack, &mut position, &["d7d6", "g1f3"], false);

    assert_reads_match_rebuild(&network, &mut stack, &position);

    unwind(&mut stack, &mut position, second);
    position.unmake_null_move(null_undo);
    stack.pop().expect("null pop should have a parent");
    assert_reads_match_rebuild(&network, &mut stack, &position);
    unwind(&mut stack, &mut position, first);
    assert_reads_match_rebuild(&network, &mut stack, &Position::startpos());
}

#[test]
fn a_randomized_deferred_walk_matches_full_rebuilds_in_standard_and_chess960() {
    let Some(network) = local_network() else {
        return;
    };
    // The broad net. Evaluations happen at pseudorandom plies rather than at every ply, so
    // pending chains of every length from 0 to many are exercised, including across king moves,
    // captures, promotions, castling and Chess960 king-takes-rook castling.
    let roots = [
        (
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            false,
        ),
        (
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        ),
        ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", false),
        ("r1k2r2/pppppppp/8/8/8/8/PPPPPPPP/R1K2R2 w FAfa - 0 1", true),
        ("3r2kr/pppppppp/8/8/8/8/PPPPPPPP/3R2KR w HDhd - 0 1", true),
    ];
    let mut evaluations = 0_usize;

    for (root_index, (fen, chess960)) in roots.into_iter().enumerate() {
        let root = Position::from_fen(fen, chess960).expect("walk root should parse");
        let mut position = root.clone();
        let mut stack = AccumulatorStack::new(&network, &root);
        let mut history: Vec<(Move, Undo)> = Vec::new();
        let mut random =
            0xA24B_AED4_963E_E407_u64 ^ (root_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);

        for _ in 0..900 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;

            let moves = generate_legal_moves(&position);
            // Unwind when the branch is exhausted or deep enough, so pops of unread frames and
            // pops of materialized frames both occur.
            if moves.is_empty() || history.len() >= 20 || random.is_multiple_of(5) {
                if let Some((mv, undo)) = history.pop() {
                    position.unmake_move(mv, undo);
                    stack.pop().expect("walk pop should have a parent");
                    // Evaluate on only some pops, so parents are read both before and after
                    // their children were materialized.
                    if random.is_multiple_of(3) {
                        assert_reads_match_rebuild(&network, &mut stack, &position);
                        evaluations += 1;
                    }
                }
                continue;
            }

            let mv = moves[(random >> 32) as usize % moves.len()];
            let undo = position.make_move(mv);
            stack
                .push_real(&position, mv, &undo)
                .expect("walk push should fit");
            history.push((mv, undo));

            // Evaluate at only a fraction of pushes: the rest are the deferred ones.
            if random.is_multiple_of(4) {
                assert_reads_match_rebuild(&network, &mut stack, &position);
                evaluations += 1;
            }
        }

        while let Some((mv, undo)) = history.pop() {
            position.unmake_move(mv, undo);
            stack.pop().expect("final unwind should have a parent");
        }
        assert_eq!(position, root);
        assert_reads_match_rebuild(&network, &mut stack, &root);
    }

    // Guards against the walk silently degenerating into a no-op that asserts nothing.
    assert!(
        evaluations > 400,
        "randomized walk verified only {evaluations} evaluations"
    );
}
