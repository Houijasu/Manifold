use crate::{Move, Position, generate_legal_moves};

/// Counts legal leaf nodes at exactly `depth`, using make/unmake at every ply.
pub fn perft(position: &mut Position, depth: u32) -> u64 {
    if depth == 0 {
        return 1;
    }

    let moves = generate_legal_moves(position);
    if depth == 1 {
        return moves.len() as u64;
    }

    let mut nodes = 0;
    for &mv in &moves {
        let undo = position.make_move(mv);
        nodes += perft(position, depth - 1);
        position.unmake_move(mv, undo);
    }
    nodes
}

/// Counts each root move independently for machine-parseable divide output.
pub fn perft_divide(position: &mut Position, depth: u32) -> Vec<(Move, u64)> {
    if depth == 0 {
        return Vec::new();
    }

    let moves = generate_legal_moves(position);
    let mut divide = Vec::with_capacity(moves.len());
    for &mv in &moves {
        let undo = position.make_move(mv);
        let nodes = perft(position, depth - 1);
        position.unmake_move(mv, undo);
        divide.push((mv, nodes));
    }
    divide
}
