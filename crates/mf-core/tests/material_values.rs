use mf_core::{PieceKind, material_value};

#[test]
fn canonical_material_values_match_engine_centipawn_scale() {
    assert_eq!(material_value(PieceKind::Pawn), 100);
    assert_eq!(material_value(PieceKind::Knight), 320);
    assert_eq!(material_value(PieceKind::Bishop), 330);
    assert_eq!(material_value(PieceKind::Rook), 500);
    assert_eq!(material_value(PieceKind::Queen), 900);
    assert_eq!(material_value(PieceKind::King), 0);
}
