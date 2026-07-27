use mf_core::{PieceKind, Position};
use mf_search::{DEFAULT_PARAMETERS, evaluate, evaluate_with_parameters};

#[test]
fn public_evaluator_uses_default_parameters() {
    let position = Position::from_fen("4k3/8/8/8/8/8/8/3QK3 w - - 0 1", false).unwrap();

    assert_eq!(
        evaluate_with_parameters(&position, &DEFAULT_PARAMETERS),
        evaluate(&position)
    );
}

#[test]
fn material_coefficients_are_directly_tunable() {
    let position = Position::from_fen("4k3/8/8/8/8/8/8/3QK3 w - - 0 1", false).unwrap();
    let baseline = evaluate(&position);
    let mut tuned = DEFAULT_PARAMETERS.clone();
    tuned.material[PieceKind::Queen.index()].middle_game += 96;
    tuned.material[PieceKind::Queen.index()].end_game += 96;

    assert_eq!(evaluate_with_parameters(&position, &tuned), baseline + 96);
}
