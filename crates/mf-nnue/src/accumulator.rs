// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

use core::fmt;

use mf_core::{CastlingSide, Color, Move, Piece, PieceKind, Position, Square, Undo};

use crate::halfka;
use crate::network::{L1, Network, PSQT_BUCKETS};
use crate::simd::{
    ForwardMode, SimdBackend, UnsupportedBackend, add_i8_row, add_i16_row, add_psqt_row,
    fused_accumulator_update, production_forward_mode,
};
#[cfg(feature = "instrumentation")]
use crate::threats::discover_changed_threats_profiled;
use crate::threats::{
    ChangedThreatBuffer, MAX_ACTIVE, MAX_CHANGED, append_active_threats,
    append_changed_threat_indices, discover_changed_threats,
};

/// Maximum number of child plies retained after the root position.
pub const ACCUMULATOR_STACK_CAPACITY: usize = 128;

const STACK_STATES: usize = ACCUMULATOR_STACK_CAPACITY + 1;
const MAX_PIECE_DELTAS: usize = 4;
const A1: Square = match Square::new(0) {
    Some(square) => square,
    None => unreachable!(),
};
const EMPTY_DELTA: PieceDelta = PieceDelta {
    square: A1,
    piece: Piece::new(Color::White, mf_core::PieceKind::Pawn),
};

/// One perspective's merged HalfKAv2_hm and FullThreats accumulator.
#[repr(C, align(64))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Accumulator {
    pub(crate) values: [i16; L1],
    pub(crate) psqt: [i32; PSQT_BUCKETS],
}

impl Accumulator {
    fn build(
        network: &Network,
        position: &Position,
        perspective: Color,
        backend: SimdBackend,
    ) -> Self {
        let mut accumulator = Self {
            values: *network.feature_transformer_biases(),
            psqt: [0; PSQT_BUCKETS],
        };
        let king_square = position.king_square(perspective);

        for square in position.occupancy() {
            let piece = position
                .piece_at(square)
                .expect("occupied NNUE feature square must contain a piece");
            let feature = halfka::make_index(perspective, piece, square, king_square);
            add_i16_row(
                backend,
                &mut accumulator.values,
                network
                    .half_ka_weights()
                    .row(feature)
                    .expect("HalfKA feature must be in range"),
            );
            add_psqt_row(
                backend,
                &mut accumulator.psqt,
                network
                    .psqt_row(feature)
                    .expect("HalfKA PSQT feature must be in range"),
            );
        }

        let mut active_threats = [0; MAX_ACTIVE];
        let threat_count = append_active_threats(perspective, position, &mut active_threats);
        for &feature in &active_threats[..threat_count] {
            add_i8_row(
                backend,
                &mut accumulator.values,
                network
                    .threat_weights()
                    .row(feature)
                    .expect("FullThreats feature must be in range"),
            );
            add_psqt_row(
                backend,
                &mut accumulator.psqt,
                network
                    .threat_psqt_row(feature)
                    .expect("FullThreats PSQT feature must be in range"),
            );
        }

        accumulator
    }
}

/// Complete merged NNUE accumulator state for both king perspectives.
#[repr(C, align(64))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccumulatorState {
    accumulators: [Accumulator; 2],
}

impl AccumulatorState {
    /// Builds both perspectives with the scalar oracle implementation.
    #[must_use]
    pub fn from_position(network: &Network, position: &Position) -> Self {
        Self::build_with_backend(network, position, SimdBackend::Scalar)
    }

    /// Builds both perspectives with the process-wide production backend.
    #[must_use]
    pub fn from_position_production(network: &Network, position: &Position) -> Self {
        let backend = production_forward_mode().backend();
        Self::build_with_backend(network, position, backend)
    }

    /// Builds both perspectives with an explicitly selected SIMD backend.
    pub fn from_position_with_backend(
        network: &Network,
        position: &Position,
        backend: SimdBackend,
    ) -> Result<Self, UnsupportedBackend> {
        let backend = ForwardMode::new(backend, false)?.backend();
        Ok(Self::build_with_backend(network, position, backend))
    }

    fn build_with_backend(network: &Network, position: &Position, backend: SimdBackend) -> Self {
        Self {
            accumulators: [
                Accumulator::build(network, position, Color::White, backend),
                Accumulator::build(network, position, Color::Black, backend),
            ],
        }
    }

    #[inline]
    pub(crate) fn accumulator(&self, perspective: Color) -> &Accumulator {
        &self.accumulators[perspective.index()]
    }

    #[allow(clippy::too_many_arguments)]
    fn update_from<const CAPACITY: usize>(
        &mut self,
        parent: &Self,
        context: UpdateContext<'_>,
        previous_metadata: &FrameMetadata,
        child_metadata: &mut FrameMetadata,
        changed_threats: &ChangedThreatBuffer<CAPACITY>,
        removed: &[PieceDelta],
        added: &[PieceDelta],
        threat_additions: &mut [u32; MAX_CHANGED],
        threat_removals: &mut [u32; MAX_CHANGED],
    ) {
        if changed_threats.overflowed() {
            *self = Self::build_with_backend(context.network, context.child, context.backend);
            *child_metadata = FrameMetadata::from_position(context.child);
            return;
        }

        let mut halfka_removals = [0_usize; MAX_PIECE_DELTAS];
        let mut halfka_additions = [0_usize; MAX_PIECE_DELTAS];

        for perspective in Color::ALL {
            let index = perspective.index();
            let previous_king = previous_metadata.king_squares[index];
            let child_king = context.child.king_square(perspective);
            child_metadata.king_squares[index] = child_king;

            if previous_king != child_king {
                self.accumulators[index] = Accumulator::build(
                    context.network,
                    context.child,
                    perspective,
                    context.backend,
                );
                continue;
            }

            for (feature, delta) in halfka_removals.iter_mut().zip(removed) {
                *feature =
                    halfka::make_index(perspective, delta.piece, delta.square, previous_king);
            }
            for (feature, delta) in halfka_additions.iter_mut().zip(added) {
                *feature =
                    halfka::make_index(perspective, delta.piece, delta.square, previous_king);
            }
            let (addition_count, removal_count) = append_changed_threat_indices(
                perspective,
                context.child,
                changed_threats,
                threat_additions,
                threat_removals,
            );
            let parent_accumulator = &parent.accumulators[index];
            let child_accumulator = &mut self.accumulators[index];
            fused_accumulator_update(
                context.backend,
                context.network,
                &parent_accumulator.values,
                &mut child_accumulator.values,
                &parent_accumulator.psqt,
                &mut child_accumulator.psqt,
                &halfka_removals[..removed.len()],
                &halfka_additions[..added.len()],
                &threat_removals[..removal_count],
                &threat_additions[..addition_count],
            );
        }
    }
}

#[derive(Clone, Copy)]
struct FrameMetadata {
    king_squares: [Square; 2],
}

impl FrameMetadata {
    fn from_position(position: &Position) -> Self {
        Self {
            king_squares: [
                position.king_square(Color::White),
                position.king_square(Color::Black),
            ],
        }
    }
}

#[repr(C, align(64))]
#[derive(Clone)]
struct AccumulatorFrame {
    state: AccumulatorState,
    metadata: FrameMetadata,
    // FullThreats depends only on physical piece placement, so null frames can reuse this snapshot.
    position: Position,
}

struct UpdateScratch {
    changed_threats: ChangedThreatBuffer<MAX_CHANGED>,
    threat_additions: [u32; MAX_CHANGED],
    threat_removals: [u32; MAX_CHANGED],
}

impl UpdateScratch {
    const fn new() -> Self {
        Self {
            changed_threats: ChangedThreatBuffer::new(),
            threat_additions: [0; MAX_CHANGED],
            threat_removals: [0; MAX_CHANGED],
        }
    }
}

/// Recoverable accumulator-stack depth errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccumulatorStackError {
    /// A push would exceed the fixed ply capacity.
    CapacityExceeded { capacity: usize },
    /// The caller attempted to pop the root state.
    AtRoot,
}

/// Per-move work discovered before an incremental accumulator update.
#[cfg(feature = "instrumentation")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateProfile {
    /// Number of removed HalfKA piece features.
    pub halfka_removals: usize,
    /// Number of added HalfKA piece features.
    pub halfka_additions: usize,
    /// Number of net physical FullThreats edge changes.
    pub changed_threat_edges: usize,
    /// Number of slider candidates inspected around affected squares.
    pub sliders_scanned: usize,
}

impl fmt::Display for AccumulatorStackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { capacity } => {
                write!(formatter, "accumulator stack capacity {capacity} exceeded")
            }
            Self::AtRoot => formatter.write_str("cannot pop the accumulator root"),
        }
    }
}

impl std::error::Error for AccumulatorStackError {}

/// Fixed-capacity, heap-backed incremental NNUE accumulator stack.
pub struct AccumulatorStack<'network> {
    network: &'network Network,
    frames: Box<[AccumulatorFrame]>,
    scratch: UpdateScratch,
    depth: usize,
    mode: ForwardMode,
}

impl<'network> AccumulatorStack<'network> {
    /// Profiles the move-local work used by a real accumulator push.
    #[cfg(feature = "instrumentation")]
    #[must_use]
    pub fn profile_real_update(child: &Position, mv: Move, undo: &Undo) -> UpdateProfile {
        let mut removed = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let mut added = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let (halfka_removals, halfka_additions) = move_deltas(mv, undo, &mut removed, &mut added);
        let mut parent = child.clone();
        parent.unmake_move(mv, undo.clone());
        let mut changed_threats = ChangedThreatBuffer::<MAX_CHANGED>::new();
        let sliders_scanned =
            discover_changed_threats_profiled(&parent, child, mv, undo, &mut changed_threats);

        UpdateProfile {
            halfka_removals,
            halfka_additions,
            changed_threat_edges: changed_threats.len(),
            sliders_scanned,
        }
    }

    /// Allocates a stack using the scalar oracle implementation.
    #[must_use]
    pub fn new(network: &'network Network, root: &Position) -> Self {
        Self::build(network, root, ForwardMode::scalar())
    }

    /// Allocates a stack using the process-wide production mode.
    #[must_use]
    pub fn new_production(network: &'network Network, root: &Position) -> Self {
        Self::build(network, root, production_forward_mode())
    }

    /// Allocates a stack using an explicitly selected SIMD backend.
    pub fn new_with_backend(
        network: &'network Network,
        root: &Position,
        backend: SimdBackend,
    ) -> Result<Self, UnsupportedBackend> {
        let mode = ForwardMode::new(backend, false)?;
        Ok(Self::build(network, root, mode))
    }

    /// Allocates a stack using an explicitly selected validated forward mode.
    pub fn new_with_mode(
        network: &'network Network,
        root: &Position,
        mode: ForwardMode,
    ) -> Result<Self, UnsupportedBackend> {
        let mode = ForwardMode::new(mode.backend(), mode.sparse_fc0())?;
        Ok(Self::build(network, root, mode))
    }

    fn build(network: &'network Network, root: &Position, mode: ForwardMode) -> Self {
        let root = AccumulatorFrame {
            state: AccumulatorState::build_with_backend(network, root, mode.backend()),
            metadata: FrameMetadata::from_position(root),
            position: root.clone(),
        };
        Self {
            network,
            frames: vec![root; STACK_STATES].into_boxed_slice(),
            scratch: UpdateScratch::new(),
            depth: 0,
            mode,
        }
    }

    /// Returns the current child-ply depth, where the root is depth zero.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the maximum number of child plies retained after the root.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        ACCUMULATOR_STACK_CAPACITY
    }

    /// Returns the complete accumulator state at the current depth.
    #[must_use]
    pub fn current(&self) -> &AccumulatorState {
        &self.frames[self.depth].state
    }

    /// Pushes a real move after it has already been applied to `child`.
    ///
    /// `undo` must be the value returned when applying `mv` to the position represented by the
    /// current frame.
    pub fn push_real(
        &mut self,
        child: &Position,
        mv: Move,
        undo: &Undo,
    ) -> Result<(), AccumulatorStackError> {
        if self.depth == ACCUMULATOR_STACK_CAPACITY {
            return Err(AccumulatorStackError::CapacityExceeded {
                capacity: ACCUMULATOR_STACK_CAPACITY,
            });
        }

        let mut removed = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let mut added = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let (removed_count, added_count) = move_deltas(mv, undo, &mut removed, &mut added);
        self.scratch.changed_threats.reset();
        let next_depth = self.depth + 1;
        let (parents, children) = self.frames.split_at_mut(next_depth);
        discover_changed_threats(
            &parents[self.depth].position,
            child,
            mv,
            undo,
            &mut self.scratch.changed_threats,
        );
        children[0].position.clone_from(child);
        children[0].state.update_from(
            &parents[self.depth].state,
            UpdateContext {
                network: self.network,
                child,
                backend: self.mode.backend(),
            },
            &parents[self.depth].metadata,
            &mut children[0].metadata,
            &self.scratch.changed_threats,
            &removed[..removed_count],
            &added[..added_count],
            &mut self.scratch.threat_additions,
            &mut self.scratch.threat_removals,
        );
        self.depth = next_depth;
        Ok(())
    }

    #[cfg(test)]
    fn push_real_with_threat_capacity<const CAPACITY: usize>(
        &mut self,
        child: &Position,
        mv: Move,
        undo: &Undo,
    ) -> Result<(), AccumulatorStackError> {
        if self.depth == ACCUMULATOR_STACK_CAPACITY {
            return Err(AccumulatorStackError::CapacityExceeded {
                capacity: ACCUMULATOR_STACK_CAPACITY,
            });
        }

        let mut removed = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let mut added = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let (removed_count, added_count) = move_deltas(mv, undo, &mut removed, &mut added);
        let mut changed_threats = ChangedThreatBuffer::<CAPACITY>::new();
        let mut threat_additions = [0; MAX_CHANGED];
        let mut threat_removals = [0; MAX_CHANGED];
        let next_depth = self.depth + 1;
        let (parents, children) = self.frames.split_at_mut(next_depth);
        discover_changed_threats(
            &parents[self.depth].position,
            child,
            mv,
            undo,
            &mut changed_threats,
        );
        children[0].position.clone_from(child);
        children[0].state.update_from(
            &parents[self.depth].state,
            UpdateContext {
                network: self.network,
                child,
                backend: self.mode.backend(),
            },
            &parents[self.depth].metadata,
            &mut children[0].metadata,
            &changed_threats,
            &removed[..removed_count],
            &added[..added_count],
            &mut threat_additions,
            &mut threat_removals,
        );
        self.depth = next_depth;
        Ok(())
    }

    /// Pushes a null move by copying the current complete state unchanged.
    pub fn push_null(&mut self) -> Result<(), AccumulatorStackError> {
        if self.depth == ACCUMULATOR_STACK_CAPACITY {
            return Err(AccumulatorStackError::CapacityExceeded {
                capacity: ACCUMULATOR_STACK_CAPACITY,
            });
        }

        let next_depth = self.depth + 1;
        let (parents, children) = self.frames.split_at_mut(next_depth);
        children[0].clone_from(&parents[self.depth]);
        self.depth = next_depth;
        Ok(())
    }

    /// Discards the current ply and exposes its parent.
    pub fn pop(&mut self) -> Result<(), AccumulatorStackError> {
        if self.depth == 0 {
            return Err(AccumulatorStackError::AtRoot);
        }
        self.depth -= 1;
        Ok(())
    }

    /// Evaluates the current state in side-to-move-relative centipawns.
    #[must_use]
    pub fn evaluate(&self, position: &Position) -> i32 {
        self.network
            .evaluate_from_state_with_mode(position, self.current(), self.mode)
    }

    /// Returns the raw blended NNUE value for the current state.
    #[must_use]
    pub fn evaluate_internal(&self, position: &Position) -> i32 {
        self.network
            .evaluate_internal_from_state_with_mode(position, self.current(), self.mode)
    }

    /// Dumps the current feature-transform output and evaluation metadata.
    #[must_use]
    pub fn dump_features(
        &self,
        position: &Position,
        transformed: &mut [u8; L1],
    ) -> crate::EvaluationDump {
        self.network.dump_features_from_state_with_mode(
            position,
            self.current(),
            transformed,
            self.mode,
        )
    }
}

#[derive(Clone, Copy)]
struct PieceDelta {
    square: Square,
    piece: Piece,
}

#[derive(Clone, Copy)]
struct UpdateContext<'position> {
    network: &'position Network,
    child: &'position Position,
    backend: SimdBackend,
}

fn move_deltas(
    mv: Move,
    undo: &Undo,
    removed: &mut [PieceDelta; MAX_PIECE_DELTAS],
    added: &mut [PieceDelta; MAX_PIECE_DELTAS],
) -> (usize, usize) {
    let mut removed_count = 0;
    let mut added_count = 0;
    let moved = undo.moved();

    if mv.flag().is_castling() {
        let color = moved.color();
        let side = CastlingSide::from_rook_origin(mv.from(), mv.to());
        let king_destination = side.king_destination(color);
        let rook_destination = side.rook_destination(color);
        let rook = Piece::new(color, PieceKind::Rook);

        append_relocation(
            mv.from(),
            king_destination,
            moved,
            removed,
            &mut removed_count,
            added,
            &mut added_count,
        );
        append_relocation(
            mv.to(),
            rook_destination,
            rook,
            removed,
            &mut removed_count,
            added,
            &mut added_count,
        );
        return (removed_count, added_count);
    }

    append_delta(
        removed,
        &mut removed_count,
        PieceDelta {
            square: mv.from(),
            piece: moved,
        },
    );
    if let Some((square, piece)) = undo.captured() {
        append_delta(removed, &mut removed_count, PieceDelta { square, piece });
    }

    let placed = mv
        .flag()
        .promotion()
        .map_or(moved, |kind| Piece::new(moved.color(), kind));
    append_delta(
        added,
        &mut added_count,
        PieceDelta {
            square: mv.to(),
            piece: placed,
        },
    );

    (removed_count, added_count)
}

#[allow(clippy::too_many_arguments)]
fn append_relocation(
    from: Square,
    to: Square,
    piece: Piece,
    removed: &mut [PieceDelta; MAX_PIECE_DELTAS],
    removed_count: &mut usize,
    added: &mut [PieceDelta; MAX_PIECE_DELTAS],
    added_count: &mut usize,
) {
    if from == to {
        return;
    }
    append_delta(
        removed,
        removed_count,
        PieceDelta {
            square: from,
            piece,
        },
    );
    append_delta(added, added_count, PieceDelta { square: to, piece });
}

fn append_delta(deltas: &mut [PieceDelta; MAX_PIECE_DELTAS], count: &mut usize, delta: PieceDelta) {
    assert!(
        *count < MAX_PIECE_DELTAS,
        "legal move produced too many NNUE piece deltas"
    );
    deltas[*count] = delta;
    *count += 1;
}

#[cfg(test)]
fn add_i16_row_oracle(accumulator: &mut [i32], row: &[i16]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value += i32::from(weight);
    }
}

#[cfg(test)]
fn build_i32_oracle(
    network: &Network,
    position: &Position,
    perspective: Color,
) -> ([i32; L1], [i32; PSQT_BUCKETS]) {
    let mut values = network.feature_transformer_biases().map(i32::from);
    let mut psqt = [0; PSQT_BUCKETS];
    let king_square = position.king_square(perspective);

    for square in position.occupancy() {
        let piece = position
            .piece_at(square)
            .expect("occupied NNUE feature square must contain a piece");
        let feature = halfka::make_index(perspective, piece, square, king_square);
        add_i16_row_oracle(
            &mut values,
            network
                .half_ka_weights()
                .row(feature)
                .expect("HalfKA feature must be in range"),
        );
        for (value, &weight) in psqt.iter_mut().zip(
            network
                .psqt_row(feature)
                .expect("HalfKA PSQT feature must be in range"),
        ) {
            *value += weight;
        }
    }

    let mut active_threats = [0; MAX_ACTIVE];
    let threat_count = append_active_threats(perspective, position, &mut active_threats);
    for &feature in &active_threats[..threat_count] {
        for (value, &weight) in values.iter_mut().zip(
            network
                .threat_weights()
                .row(feature)
                .expect("FullThreats feature must be in range"),
        ) {
            *value += i32::from(weight);
        }
        for (value, &weight) in psqt.iter_mut().zip(
            network
                .threat_psqt_row(feature)
                .expect("FullThreats PSQT feature must be in range"),
        ) {
            *value += weight;
        }
    }

    (values, psqt)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use mf_core::{Color, Position};

    use super::{
        Accumulator, AccumulatorFrame, AccumulatorStack, AccumulatorState, EMPTY_DELTA,
        FrameMetadata, MAX_PIECE_DELTAS, STACK_STATES, UpdateContext, build_i32_oracle,
        move_deltas,
    };
    use crate::simd::{reset_sparse_fc0_calls, sparse_fc0_calls};
    use crate::threats::{ChangedThreatBuffer, MAX_CHANGED, discover_changed_threats};
    use crate::{ForwardMode, L1, Network, SimdBackend};

    fn resolve_network_path(explicit_path: Option<OsString>) -> (PathBuf, bool) {
        let is_explicit = explicit_path.is_some();
        let path = explicit_path.map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
            PathBuf::from,
        );
        (path, is_explicit)
    }

    fn local_network(test_name: &str) -> Option<Network> {
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
        Some(Network::load(&path).unwrap_or_else(|error| {
            panic!("failed to load NNUE network {}: {error}", path.display())
        }))
    }

    #[test]
    fn stack_frames_keep_compact_metadata_and_cache_line_alignment() {
        assert_eq!(
            core::mem::size_of::<FrameMetadata>(),
            2 * core::mem::size_of::<mf_core::Square>()
        );
        assert_eq!(core::mem::size_of::<FrameMetadata>(), 2);
        assert_eq!(core::mem::size_of::<AccumulatorState>(), 4_224);
        assert_eq!(core::mem::align_of::<AccumulatorFrame>(), 64);
        assert_eq!(core::mem::size_of::<AccumulatorFrame>(), 4_544);
        assert_eq!(
            core::mem::size_of::<AccumulatorFrame>() * STACK_STATES,
            586_176
        );
    }

    #[test]
    fn reusable_update_scratch_keeps_threat_indices_narrow() {
        assert!(core::mem::size_of::<super::UpdateScratch>() <= 1_600);
    }

    #[test]
    fn real_push_retains_the_child_for_the_next_incremental_update() {
        let Some(network) = local_network("retained child position test") else {
            return;
        };
        let root = Position::startpos();
        let mut child = root.clone();
        let mv = mf_core::parse_uci_move(&root, "e2e4", false)
            .expect("retained-position move should be legal");
        let undo = child.make_move(mv);
        let mut stack = AccumulatorStack::new(&network, &root);

        stack
            .push_real(&child, mv, &undo)
            .expect("retained-position push should fit");

        assert_eq!(stack.frames[0].position, root);
        assert_eq!(stack.frames[1].position, child);
    }

    #[test]
    fn network_path_resolution_marks_only_environment_paths_as_explicit() {
        let explicit = PathBuf::from("explicit-test.nnue");
        assert_eq!(
            resolve_network_path(Some(explicit.clone().into_os_string())),
            (explicit, true)
        );

        let (default, is_explicit) = resolve_network_path(None);
        assert_eq!(
            default,
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue")
        );
        assert!(!is_explicit);
    }

    #[cfg(feature = "instrumentation")]
    #[test]
    fn real_update_profile_reports_piece_and_threat_work() {
        let parent = Position::startpos();
        let mv =
            mf_core::parse_uci_move(&parent, "e2e4", false).expect("profile move should be legal");
        let mut child = parent.clone();
        let undo = child.make_move(mv);

        let profile = AccumulatorStack::profile_real_update(&child, mv, &undo);

        assert_eq!(profile.halfka_removals, 1);
        assert_eq!(profile.halfka_additions, 1);
        assert!(profile.changed_threat_edges > 0);
    }

    #[test]
    fn real_position_accumulators_are_narrowed_i32_oracles() {
        let Some(network) = local_network("real-position accumulator oracle test") else {
            return;
        };
        let positions = [
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        ];

        for fen in positions {
            let position = Position::from_fen(fen, false).expect("test FEN should parse");
            for perspective in Color::ALL {
                let production =
                    Accumulator::build(&network, &position, perspective, SimdBackend::Scalar);
                let oracle = build_i32_oracle(&network, &position, perspective);
                assert!(oracle.0.iter().all(|&value| i16::try_from(value).is_ok()));
                assert_eq!(
                    production.values,
                    oracle.0.map(|value| value as i16),
                    "{fen}, {perspective:?}"
                );
                assert_eq!(production.psqt, oracle.1, "{fen}, {perspective:?}");
            }
        }
    }

    #[test]
    fn fused_accumulator_update_matches_full_rebuilds() {
        let Some(network) = local_network("fused accumulator update test") else {
            return;
        };
        let backend = [SimdBackend::Avx2Vnni, SimdBackend::Avx2]
            .into_iter()
            .find(|backend| backend.is_supported())
            .unwrap_or(SimdBackend::Scalar);

        for (fen, notation, chess960) in [
            (
                "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
                "e2e4",
                false,
            ),
            ("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "e4d5", false),
            ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6", false),
            ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8q", false),
            ("4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1", "c1f1", true),
        ] {
            let parent =
                Position::from_fen(fen, chess960).expect("update comparison FEN should parse");
            let mv = mf_core::parse_uci_move(&parent, notation, chess960)
                .expect("update comparison move should be legal");
            let mut child = parent.clone();
            let undo = child.make_move(mv);
            let mut removed = [EMPTY_DELTA; MAX_PIECE_DELTAS];
            let mut added = [EMPTY_DELTA; MAX_PIECE_DELTAS];
            let (removed_count, added_count) = move_deltas(mv, &undo, &mut removed, &mut added);
            let mut changed_threats = ChangedThreatBuffer::<MAX_CHANGED>::new();
            discover_changed_threats(&parent, &child, mv, &undo, &mut changed_threats);
            let parent_metadata = FrameMetadata::from_position(&parent);
            let parent_state = AccumulatorState::build_with_backend(&network, &parent, backend);
            let expected = AccumulatorState::build_with_backend(&network, &child, backend);
            let expected_metadata = FrameMetadata::from_position(&child);

            let mut fused = parent_state.clone();
            let mut fused_metadata = parent_metadata;
            let mut threat_additions = [0; MAX_CHANGED];
            let mut threat_removals = [0; MAX_CHANGED];
            fused.update_from(
                &parent_state,
                UpdateContext {
                    network: &network,
                    child: &child,
                    backend,
                },
                &parent_metadata,
                &mut fused_metadata,
                &changed_threats,
                &removed[..removed_count],
                &added[..added_count],
                &mut threat_additions,
                &mut threat_removals,
            );

            assert_eq!(fused, expected, "{fen} {notation}");
            assert_eq!(
                fused_metadata.king_squares, expected_metadata.king_squares,
                "{fen} {notation}"
            );
        }
    }

    #[test]
    fn stack_evaluation_uses_its_stored_forward_mode() {
        let Some(network) = local_network("stored forward-mode test") else {
            return;
        };
        let Some(backend) = [SimdBackend::Avx2, SimdBackend::Avx2Vnni]
            .into_iter()
            .find(|backend| backend.is_supported())
        else {
            return;
        };
        let position = Position::startpos();
        let sparse_mode = ForwardMode::new(backend, true).expect("backend is supported");
        let sparse_stack = AccumulatorStack::new_with_mode(&network, &position, sparse_mode)
            .expect("mode is supported");
        let dense_stack = AccumulatorStack::new_with_backend(&network, &position, backend)
            .expect("backend is supported");
        let mut transformed = [0_u8; L1];

        reset_sparse_fc0_calls();
        let sparse = sparse_stack.dump_features(&position, &mut transformed);
        assert_eq!(sparse_fc0_calls(), 1);

        reset_sparse_fc0_calls();
        let dense = dense_stack.dump_features(&position, &mut transformed);
        assert_eq!(sparse_fc0_calls(), 0);
        assert_eq!(sparse, dense);
        assert_eq!(
            sparse_stack.evaluate_internal(&position),
            dense.eval_internal
        );
        assert_eq!(
            sparse_stack.evaluate(&position),
            dense_stack.evaluate(&position)
        );
    }

    #[test]
    fn dirty_threat_overflow_falls_back_to_an_exact_full_rebuild() {
        let Some(network) = local_network("dirty-threat overflow fallback test") else {
            return;
        };
        let parent = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .expect("test FEN should parse");
        let mv = mf_core::parse_uci_move(&parent, "e5d7", false)
            .expect("overflow-test move should be legal");
        let mut child = parent.clone();
        let undo = child.make_move(mv);
        let mut stack = AccumulatorStack::new(&network, &parent);

        stack
            .push_real_with_threat_capacity::<0>(&child, mv, &undo)
            .expect("overflow fallback push should fit");

        assert_eq!(
            stack.current(),
            &AccumulatorState::from_position(&network, &child)
        );
    }
}
