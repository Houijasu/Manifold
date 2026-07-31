// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

use core::fmt;

use mf_core::{CastlingSide, Color, Move, Piece, PieceKind, Position, Square, Undo};

use crate::halfka;
use crate::network::{L1, Network, PSQT_BUCKETS};
use crate::simd::{
    ForwardMode, SimdBackend, UnsupportedBackend, add_i8_row, add_i16_row, add_psqt_row,
    production_forward_mode, subtract_i8_row, subtract_i16_row, subtract_psqt_row,
};
use crate::threats::{MAX_ACTIVE, append_active_threats};

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

    fn remove_threats(
        &mut self,
        network: &Network,
        active_threats: &ActiveThreats,
        backend: SimdBackend,
    ) {
        for feature in active_threats.iter() {
            subtract_i8_row(
                backend,
                &mut self.values,
                network
                    .threat_weights()
                    .row(feature)
                    .expect("stored FullThreats feature must be in range"),
            );
            subtract_psqt_row(
                backend,
                &mut self.psqt,
                network
                    .threat_psqt_row(feature)
                    .expect("stored FullThreats PSQT feature must be in range"),
            );
        }
    }

    fn add_threats(
        &mut self,
        network: &Network,
        active_threats: &ActiveThreats,
        backend: SimdBackend,
    ) {
        for feature in active_threats.iter() {
            add_i8_row(
                backend,
                &mut self.values,
                network
                    .threat_weights()
                    .row(feature)
                    .expect("child FullThreats feature must be in range"),
            );
            add_psqt_row(
                backend,
                &mut self.psqt,
                network
                    .threat_psqt_row(feature)
                    .expect("child FullThreats PSQT feature must be in range"),
            );
        }
    }

    fn update_halfka(
        &mut self,
        context: UpdateContext<'_>,
        perspective: Color,
        previous_king: Square,
        child_king: Square,
        removed: &[PieceDelta],
        added: &[PieceDelta],
    ) {
        if previous_king == child_king {
            for &delta in removed {
                self.remove_halfka(
                    context.network,
                    perspective,
                    delta,
                    previous_king,
                    context.backend,
                );
            }
            for &delta in added {
                self.add_halfka(
                    context.network,
                    perspective,
                    delta,
                    previous_king,
                    context.backend,
                );
            }
            return;
        }

        self.values = *context.network.feature_transformer_biases();
        self.psqt = [0; PSQT_BUCKETS];
        for square in context.child.occupancy() {
            let piece = context
                .child
                .piece_at(square)
                .expect("occupied child square must contain a piece");
            self.add_halfka(
                context.network,
                perspective,
                PieceDelta { square, piece },
                child_king,
                context.backend,
            );
        }
    }

    fn add_halfka(
        &mut self,
        network: &Network,
        perspective: Color,
        delta: PieceDelta,
        king_square: Square,
        backend: SimdBackend,
    ) {
        let feature = halfka::make_index(perspective, delta.piece, delta.square, king_square);
        add_i16_row(
            backend,
            &mut self.values,
            network
                .half_ka_weights()
                .row(feature)
                .expect("added HalfKA feature must be in range"),
        );
        add_psqt_row(
            backend,
            &mut self.psqt,
            network
                .psqt_row(feature)
                .expect("added HalfKA PSQT feature must be in range"),
        );
    }

    fn remove_halfka(
        &mut self,
        network: &Network,
        perspective: Color,
        delta: PieceDelta,
        king_square: Square,
        backend: SimdBackend,
    ) {
        let feature = halfka::make_index(perspective, delta.piece, delta.square, king_square);
        subtract_i16_row(
            backend,
            &mut self.values,
            network
                .half_ka_weights()
                .row(feature)
                .expect("removed HalfKA feature must be in range"),
        );
        subtract_psqt_row(
            backend,
            &mut self.psqt,
            network
                .psqt_row(feature)
                .expect("removed HalfKA PSQT feature must be in range"),
        );
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

    fn update(
        &mut self,
        context: UpdateContext<'_>,
        previous_metadata: &FrameMetadata,
        child_metadata: &mut FrameMetadata,
        removed: &[PieceDelta],
        added: &[PieceDelta],
    ) {
        for perspective in Color::ALL {
            let index = perspective.index();
            let accumulator = &mut self.accumulators[perspective.index()];
            accumulator.remove_threats(
                context.network,
                &previous_metadata.active_threats[index],
                context.backend,
            );

            let child_king = context.child.king_square(perspective);
            accumulator.update_halfka(
                context,
                perspective,
                previous_metadata.king_squares[index],
                child_king,
                removed,
                added,
            );

            child_metadata.king_squares[index] = child_king;
            child_metadata.active_threats[index].fill(perspective, context.child);
            accumulator.add_threats(
                context.network,
                &child_metadata.active_threats[index],
                context.backend,
            );
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveThreats {
    indices: [u16; MAX_ACTIVE],
    count: u16,
}

impl ActiveThreats {
    const EMPTY: Self = Self {
        indices: [0; MAX_ACTIVE],
        count: 0,
    };

    fn from_position(perspective: Color, position: &Position) -> Self {
        let mut active = Self::EMPTY;
        active.fill(perspective, position);
        active
    }

    fn fill(&mut self, perspective: Color, position: &Position) {
        let mut indices = [0; MAX_ACTIVE];
        let count = append_active_threats(perspective, position, &mut indices);
        for (destination, feature) in self.indices.iter_mut().zip(indices).take(count) {
            *destination =
                u16::try_from(feature).expect("FullThreats feature index must fit in u16");
        }
        self.count = u16::try_from(count).expect("active FullThreats count must fit in u16");
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.indices[..usize::from(self.count)]
            .iter()
            .map(|&feature| usize::from(feature))
    }
}

#[derive(Clone, Copy)]
struct FrameMetadata {
    king_squares: [Square; 2],
    active_threats: [ActiveThreats; 2],
}

impl FrameMetadata {
    fn from_position(position: &Position) -> Self {
        Self {
            king_squares: [
                position.king_square(Color::White),
                position.king_square(Color::Black),
            ],
            active_threats: [
                ActiveThreats::from_position(Color::White, position),
                ActiveThreats::from_position(Color::Black, position),
            ],
        }
    }
}

#[repr(C, align(64))]
#[derive(Clone)]
struct AccumulatorFrame {
    state: AccumulatorState,
    metadata: FrameMetadata,
}

/// Recoverable accumulator-stack depth errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccumulatorStackError {
    /// A push would exceed the fixed ply capacity.
    CapacityExceeded { capacity: usize },
    /// The caller attempted to pop the root state.
    AtRoot,
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
    depth: usize,
    mode: ForwardMode,
}

impl<'network> AccumulatorStack<'network> {
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
        };
        Self {
            network,
            frames: vec![root; STACK_STATES].into_boxed_slice(),
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

        let next_depth = self.depth + 1;
        let (parents, children) = self.frames.split_at_mut(next_depth);
        children[0].clone_from(&parents[self.depth]);
        children[0].state.update(
            UpdateContext {
                network: self.network,
                child,
                backend: self.mode.backend(),
            },
            &parents[self.depth].metadata,
            &mut children[0].metadata,
            &removed[..removed_count],
            &added[..added_count],
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
    use std::path::PathBuf;

    use mf_core::{Color, Position};

    use super::{
        Accumulator, AccumulatorFrame, AccumulatorStack, AccumulatorState, ActiveThreats,
        FrameMetadata, build_i32_oracle,
    };
    use crate::simd::{reset_sparse_fc0_calls, sparse_fc0_calls};
    use crate::{ForwardMode, L1, Network, SimdBackend};

    #[test]
    fn stack_frames_keep_compact_metadata_and_cache_line_alignment() {
        assert_eq!(
            core::mem::size_of::<ActiveThreats>(),
            crate::threats::MAX_ACTIVE * core::mem::size_of::<u16>() + core::mem::size_of::<u16>()
        );
        assert_eq!(
            core::mem::size_of::<FrameMetadata>(),
            2 * core::mem::size_of::<mf_core::Square>() + 2 * core::mem::size_of::<ActiveThreats>()
        );
        assert_eq!(core::mem::size_of::<ActiveThreats>(), 514);
        assert_eq!(core::mem::size_of::<FrameMetadata>(), 1_030);
        assert_eq!(core::mem::size_of::<AccumulatorState>(), 4_224);
        assert_eq!(core::mem::align_of::<AccumulatorFrame>(), 64);
        assert_eq!(core::mem::size_of::<AccumulatorFrame>(), 5_312);
    }

    #[test]
    fn real_position_accumulators_are_narrowed_i32_oracles() {
        let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
            PathBuf::from,
        );
        if !path.is_file() {
            return;
        }
        let network = Network::load(path).expect("local FullThreats net should load");
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
    fn stack_evaluation_uses_its_stored_forward_mode() {
        let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
            PathBuf::from,
        );
        if !path.is_file() {
            return;
        }
        let Some(backend) = [SimdBackend::Avx2, SimdBackend::Avx2Vnni]
            .into_iter()
            .find(|backend| backend.is_supported())
        else {
            return;
        };
        let network = Network::load(path).expect("local FullThreats net should load");
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
}
