// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

use core::fmt;

use mf_core::{CastlingSide, Color, Move, Piece, PieceKind, Position, Square, Undo};

use crate::finny::{FinnyTable, HalfKaEntry};
use crate::halfka;
#[cfg(feature = "instrumentation")]
use crate::instrumentation;
use crate::network::{L1, Network, PSQT_BUCKETS};
use crate::simd::{
    ForwardMode, SimdBackend, UnsupportedBackend, add_i8_row, add_i16_row, add_psqt_row,
    fused_accumulator_update, production_forward_mode, rebase_accumulator, subtract_i8_row,
    subtract_psqt_row,
};
use crate::threats;
use crate::threats::{
    ChangedThreatBuffer, MAX_ACTIVE, MAX_CHANGED, append_active_threats,
    append_changed_threat_indices, discover_changed_threats,
};

/// Maximum number of child plies retained after the root position.
pub const ACCUMULATOR_STACK_CAPACITY: usize = 128;

const STACK_STATES: usize = ACCUMULATOR_STACK_CAPACITY + 1;
/// Maximum number of piece features one move can remove or add; bounds the delta
/// slices handed to the fused update kernel.
pub(crate) const MAX_PIECE_DELTAS: usize = 4;
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
            #[cfg(feature = "instrumentation")]
            let started = instrumentation::cycles();
            *self = Self::build_with_backend(context.network, context.child, context.backend);
            *child_metadata = FrameMetadata::from_position(context.child);
            #[cfg(feature = "instrumentation")]
            {
                let elapsed = instrumentation::cycles().wrapping_sub(started);
                instrumentation::record(|counters| {
                    counters.overflow_rebuilds += 1;
                    counters.rebuild_cycles += elapsed;
                });
            }
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
                // A king move invalidates every HalfKA index for this perspective, but the
                // FullThreats indices survive as long as the king stays on the same side of the
                // mirror line: `threats::make_index` consults the king square only through
                // `ORIENT_TABLE`, which is a pure function of its file. When the mirror holds,
                // the parent's threat contribution is still exactly right, so the Finny table
                // can swap the HalfKA half out from under it instead of rebuilding both halves.
                #[cfg(feature = "instrumentation")]
                let started = instrumentation::cycles();
                let mirror_held = threats::mirrors_alike(previous_king, child_king);
                let child_entry = context.finny.refresh(
                    context.network,
                    context.backend,
                    perspective,
                    context.child,
                );

                if mirror_held {
                    // The parent's threat rows are still valid, so keep them and net in only the
                    // move-local threat deltas while the piece half is swapped underneath.
                    let previous_entry = context.finny.refresh(
                        context.network,
                        context.backend,
                        perspective,
                        context.parent,
                    );
                    let (addition_count, removal_count) = append_changed_threat_indices(
                        perspective,
                        context.child,
                        changed_threats,
                        threat_additions,
                        threat_removals,
                    );
                    prefetch_threat_rows(context.network, &threat_removals[..removal_count]);
                    prefetch_threat_rows(context.network, &threat_additions[..addition_count]);
                    rebase_halfka(
                        context.backend,
                        context.network,
                        &parent.accumulators[index],
                        &mut self.accumulators[index],
                        context.finny.entry(previous_entry),
                        context.finny.entry(child_entry),
                        &threat_removals[..removal_count],
                        &threat_additions[..addition_count],
                    );
                } else {
                    // Every threat index changed with the orientation, so the threat half has to
                    // be recomputed from the position. The piece half still comes from the cache,
                    // which is the larger of the two: the M2-F1 profile measured a rebuild as
                    // 49.4% HalfKA rows against 46.9% threat scan plus threat rows.
                    rebuild_threats_onto(
                        context.backend,
                        context.network,
                        context.child,
                        perspective,
                        context.finny.entry(child_entry),
                        &mut self.accumulators[index],
                    );
                }

                #[cfg(feature = "instrumentation")]
                {
                    let elapsed = instrumentation::cycles().wrapping_sub(started);
                    instrumentation::record(|counters| {
                        counters.finny_king_updates += 1;
                        counters.finny_cycles += elapsed;
                        if !mirror_held {
                            counters.finny_threat_rebuilds += 1;
                        }
                    });
                }
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
            prefetch_threat_rows(context.network, &threat_removals[..removal_count]);
            prefetch_threat_rows(context.network, &threat_additions[..addition_count]);
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

/// The work a push implied, not yet applied to the frame's accumulator.
///
/// A pending real frame stores only the move and its `Undo`. That is enough to reconstruct
/// everything the update needs — the HalfKA piece deltas *and* the changed threat edges — from
/// the parent and child positions, which the stack keeps eagerly. Deferring the move rather than
/// its computed deltas is what lets a skipped push avoid changed-threat discovery too, which the
/// M2-F1 profile measured at a fifth of all NNUE time.
#[derive(Clone)]
enum PendingUpdate {
    Real {
        mv: Move,
        undo: Undo,
    },
    /// A null move changes no piece, so the frame's accumulator is a copy of its parent's — but
    /// the parent may itself still be pending, so the copy cannot be made at push time.
    Null,
}

#[repr(C, align(64))]
#[derive(Clone)]
struct AccumulatorFrame {
    state: AccumulatorState,
    metadata: FrameMetadata,
    // FullThreats depends only on physical piece placement, so null frames can reuse this
    // snapshot. Maintained eagerly on every push, because the *next* ply's changed-threat
    // discovery reads it as the parent position whether or not this frame is ever evaluated.
    position: Position,
    /// `Some` while `state` and `metadata` are stale and this frame's update is still owed.
    pending: Option<PendingUpdate>,
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
    finny: FinnyTable,
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
            discover_changed_threats(&parent, child, mv, undo, &mut changed_threats);

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
            pending: None,
        };
        Self {
            network,
            frames: vec![root; STACK_STATES].into_boxed_slice(),
            scratch: UpdateScratch::new(),
            finny: FinnyTable::new(network),
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
    ///
    /// Takes `&mut self` because the state at the current depth may still be owed a deferred
    /// update; handing out a shared reference to a stale frame is exactly the bug lazy updates
    /// invite, so the type system rules it out rather than a convention.
    #[must_use]
    pub fn current(&mut self) -> &AccumulatorState {
        self.materialize();
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

        let next_depth = self.depth + 1;
        // The position is kept eagerly even though the accumulator is not: the *next* ply's
        // changed-threat discovery reads it as the parent position whether or not this frame is
        // ever evaluated, and reconstructing it later would cost more than storing it now.
        let child_frame = &mut self.frames[next_depth];
        child_frame.position.clone_from(child);
        child_frame.pending = Some(PendingUpdate::Real {
            mv,
            undo: undo.clone(),
        });
        #[cfg(feature = "instrumentation")]
        instrumentation::record(|counters| counters.real_pushes += 1);
        self.depth = next_depth;
        Ok(())
    }

    /// Applies every deferred update from the last materialized frame down to the current ply.
    ///
    /// Frames are materialized in order from the shallowest pending one, because each update
    /// reads its parent's finished accumulator. The scan stops at the first frame that is
    /// already materialized, so a chain is only ever walked once: materializing a frame leaves
    /// every ancestor materialized too.
    fn materialize(&mut self) {
        let mut oldest_pending = self.depth;
        while oldest_pending > 0 && self.frames[oldest_pending].pending.is_some() {
            oldest_pending -= 1;
        }

        for depth in (oldest_pending + 1)..=self.depth {
            let Some(pending) = self.frames[depth].pending.take() else {
                continue;
            };
            match pending {
                PendingUpdate::Null => {
                    let (parents, children) = self.frames.split_at_mut(depth);
                    // Only the accumulator and its metadata are copied: the null frame's own
                    // position was already stored at push time and must not be overwritten.
                    children[0].state.clone_from(&parents[depth - 1].state);
                    children[0].metadata = parents[depth - 1].metadata;
                }
                PendingUpdate::Real { mv, undo } => {
                    self.apply_real(depth, mv, &undo);
                }
            }
        }
    }

    /// Applies one deferred real move onto its already-materialized parent.
    fn apply_real(&mut self, depth: usize, mv: Move, undo: &Undo) {
        let mut removed = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let mut added = [EMPTY_DELTA; MAX_PIECE_DELTAS];
        let (removed_count, added_count) = move_deltas(mv, undo, &mut removed, &mut added);
        self.scratch.changed_threats.reset();
        let (parents, children) = self.frames.split_at_mut(depth);
        let parent = &parents[depth - 1];
        let child = &children[0].position;

        #[cfg(feature = "instrumentation")]
        let discovery_started = instrumentation::cycles();
        #[cfg_attr(not(feature = "instrumentation"), allow(unused_variables))]
        let sliders_scanned = discover_changed_threats(
            &parent.position,
            child,
            mv,
            undo,
            &mut self.scratch.changed_threats,
        );
        #[cfg(feature = "instrumentation")]
        let update_started = instrumentation::cycles();

        // `update_from` needs `&mut` on the child state while reading the child position, which
        // lives in the same frame. Splitting the frame's fields borrows them independently.
        let AccumulatorFrame {
            state: child_state,
            metadata: child_metadata,
            position: child_position,
            ..
        } = &mut children[0];
        child_state.update_from(
            &parent.state,
            UpdateContext {
                network: self.network,
                parent: &parent.position,
                child: child_position,
                backend: self.mode.backend(),
                finny: &mut self.finny,
            },
            &parent.metadata,
            child_metadata,
            &self.scratch.changed_threats,
            &removed[..removed_count],
            &added[..added_count],
            &mut self.scratch.threat_additions,
            &mut self.scratch.threat_removals,
        );

        #[cfg(feature = "instrumentation")]
        {
            let finished = instrumentation::cycles();
            let edges = self.scratch.changed_threats.len() as u64;
            instrumentation::record(|counters| {
                counters.changed_threat_edges += edges;
                counters.sliders_scanned += sliders_scanned as u64;
                counters.threat_discovery_cycles += update_started.wrapping_sub(discovery_started);
                counters.accumulator_update_cycles += finished.wrapping_sub(update_started);
            });
        }
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
                parent: &parents[self.depth].position,
                child,
                backend: self.mode.backend(),
                finny: &mut self.finny,
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
        // FullThreats depends only on physical placement, which a null move does not change, so
        // the parent's position snapshot is exactly this frame's. The accumulator copy is
        // deferred with everything else, because the parent may itself still be pending.
        children[0]
            .position
            .clone_from(&parents[self.depth].position);
        children[0].pending = Some(PendingUpdate::Null);
        #[cfg(feature = "instrumentation")]
        instrumentation::record(|counters| counters.null_pushes += 1);
        self.depth = next_depth;
        Ok(())
    }

    /// Discards the current ply and exposes its parent.
    pub fn pop(&mut self) -> Result<(), AccumulatorStackError> {
        if self.depth == 0 {
            return Err(AccumulatorStackError::AtRoot);
        }
        #[cfg(feature = "instrumentation")]
        if matches!(
            self.frames[self.depth].pending,
            Some(PendingUpdate::Real { .. })
        ) {
            instrumentation::record(|counters| counters.deferred_pushes_skipped += 1);
        }
        // Dropping the pending update is the whole point: a frame popped before any evaluation
        // reached it never pays for its accumulator. Clearing it also keeps a discarded branch
        // from leaking into the sibling that reuses this slot.
        self.frames[self.depth].pending = None;
        self.depth -= 1;
        Ok(())
    }

    /// Evaluates the current state in side-to-move-relative centipawns.
    ///
    /// This is the point deferred work is paid for: the pending chain below the last
    /// materialized frame is applied before the forward pass reads the accumulator.
    #[must_use]
    pub fn evaluate(&mut self, position: &Position) -> i32 {
        self.materialize();
        #[cfg(feature = "instrumentation")]
        let started = instrumentation::cycles();
        let evaluation = self.network.evaluate_from_state_with_mode(
            position,
            &self.frames[self.depth].state,
            self.mode,
        );
        #[cfg(feature = "instrumentation")]
        {
            let elapsed = instrumentation::cycles().wrapping_sub(started);
            instrumentation::record(|counters| {
                counters.forward_evaluations += 1;
                counters.forward_cycles += elapsed;
            });
        }
        evaluation
    }

    /// Returns the raw blended NNUE value for the current state.
    #[must_use]
    pub fn evaluate_internal(&mut self, position: &Position) -> i32 {
        self.materialize();
        self.network.evaluate_internal_from_state_with_mode(
            position,
            &self.frames[self.depth].state,
            self.mode,
        )
    }

    /// Dumps the current feature-transform output and evaluation metadata.
    #[must_use]
    pub fn dump_features(
        &mut self,
        position: &Position,
        transformed: &mut [u8; L1],
    ) -> crate::EvaluationDump {
        self.materialize();
        self.network.dump_features_from_state_with_mode(
            position,
            &self.frames[self.depth].state,
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

struct UpdateContext<'position> {
    network: &'position Network,
    parent: &'position Position,
    child: &'position Position,
    backend: SimdBackend,
    finny: &'position mut FinnyTable,
}

/// Prefetches the FullThreats weight rows the subsequent accumulate loop will read.
///
/// The threat table is ~62 MB of 1 KiB rows indexed by feature, so the rows a node
/// touches are scattered far beyond any cache. Index computation is the natural
/// prefetch point (the reference engine's `append_changed_indices` takes a
/// `prefetchBase`/`prefetchStride` pair for exactly this): by the time the SIMD loop
/// asks for a row, the line holding its head is already in flight and the hardware
/// prefetcher streams the rest. Pure hint -- no behavior change.
#[inline]
fn prefetch_threat_rows(network: &Network, features: &[u32]) {
    #[cfg(target_arch = "x86_64")]
    for &feature in features {
        if let Some(row) = network.threat_weights().row(feature as usize) {
            use core::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
            // SAFETY: `_mm_prefetch` only issues a hardware hint. The pointer is
            // derived from a live weight row and is not dereferenced by Rust.
            unsafe { _mm_prefetch::<_MM_HINT_T0>(row.as_ptr().cast()) };
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (network, features);
}

/// Issues one T0 prefetch hint for the cache line starting at `address`.
#[inline]
fn prefetch_t0(address: *const u8) {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
        // SAFETY: `_mm_prefetch` only issues a hardware hint; the address is
        // never dereferenced by Rust.
        unsafe { _mm_prefetch::<_MM_HINT_T0>(address.cast()) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = address;
}

/// `prefetch_threat_rows` for `usize` feature indices, also covering the PSQT rows.
///
/// The mirror-flip rebuild streams every active threat row straight from the ~62 MB
/// table, so issuing the prefetches between index computation and the add loop hides
/// part of each cold miss. Pure hint -- no behavior change.
#[inline]
fn prefetch_threat_rows_indexed(network: &Network, features: &[usize]) {
    #[cfg(target_arch = "x86_64")]
    for &feature in features {
        if let Some(row) = network.threat_weights().row(feature) {
            prefetch_t0(row.as_ptr().cast());
        }
        if let Some(row) = network.threat_psqt_row(feature) {
            prefetch_t0(row.as_ptr().cast());
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (network, features);
}

/// Prefetches the HalfKAv2_hm weight and PSQT rows for the given feature indices.
///
/// A Finny refresh computes all diff indices before applying any 2 KiB rows; issued
/// there, each prefetch hides part of the cold-miss latency of the apply loops.
/// Pure hint -- no behavior change.
#[inline]
pub(crate) fn prefetch_half_ka_rows(network: &Network, features: &[usize]) {
    #[cfg(target_arch = "x86_64")]
    for &feature in features {
        if let Some(row) = network.half_ka_weights().row(feature) {
            prefetch_t0(row.as_ptr().cast());
        }
        if let Some(row) = network.psqt_row(feature) {
            prefetch_t0(row.as_ptr().cast());
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (network, features);
}

/// Rebuilds one perspective from a cached HalfKA accumulator plus a fresh threat enumeration.
///
/// Used when a king move flips the mirror, which changes every FullThreats index and so leaves
/// no threat contribution worth salvaging. The piece half is still exact in the cache, so this
/// pays for the threat scan and rows only.
fn rebuild_threats_onto(
    backend: SimdBackend,
    network: &Network,
    position: &Position,
    perspective: Color,
    entry: &HalfKaEntry,
    accumulator: &mut Accumulator,
) {
    accumulator.values = entry.values;
    accumulator.psqt = entry.psqt;

    let mut active_threats = [0; MAX_ACTIVE];
    let threat_count = append_active_threats(perspective, position, &mut active_threats);
    prefetch_threat_rows_indexed(network, &active_threats[..threat_count]);
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
}

/// Swaps one perspective's HalfKA contribution for another, keeping its threat contribution.
///
/// `parent` already holds the correct threat rows for the parent's king orientation; the caller
/// guarantees the child's orientation matches, so the threat contribution needs no rebuild.
/// Subtracting the cached HalfKA accumulator for the parent's king square and adding the one for
/// the child's replaces the piece half exactly, and the caller's move-local threat deltas are
/// folded in during the same pass.
#[allow(clippy::too_many_arguments)]
fn rebase_halfka(
    backend: SimdBackend,
    network: &Network,
    parent: &Accumulator,
    child: &mut Accumulator,
    previous_entry: &HalfKaEntry,
    child_entry: &HalfKaEntry,
    threat_removals: &[u32],
    threat_additions: &[u32],
) {
    rebase_accumulator(
        backend,
        &parent.values,
        &mut child.values,
        &parent.psqt,
        &mut child.psqt,
        &previous_entry.values,
        &child_entry.values,
        &previous_entry.psqt,
        &child_entry.psqt,
    );

    for &feature in threat_removals {
        let feature = feature as usize;
        subtract_i8_row(
            backend,
            &mut child.values,
            network
                .threat_weights()
                .row(feature)
                .expect("rebased FullThreats removal must be in range"),
        );
        subtract_psqt_row(
            backend,
            &mut child.psqt,
            network
                .threat_psqt_row(feature)
                .expect("rebased FullThreats PSQT removal must be in range"),
        );
    }
    for &feature in threat_additions {
        let feature = feature as usize;
        add_i8_row(
            backend,
            &mut child.values,
            network
                .threat_weights()
                .row(feature)
                .expect("rebased FullThreats addition must be in range"),
        );
        add_psqt_row(
            backend,
            &mut child.psqt,
            network
                .threat_psqt_row(feature)
                .expect("rebased FullThreats PSQT addition must be in range"),
        );
    }
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
        Accumulator, AccumulatorFrame, AccumulatorStack, AccumulatorState, EMPTY_DELTA,
        FrameMetadata, MAX_PIECE_DELTAS, STACK_STATES, UpdateContext, build_i32_oracle,
        move_deltas,
    };
    use crate::simd::{reset_sparse_fc0_calls, sparse_fc0_calls};
    use crate::test_support::{local_network, resolve_network_path};
    use crate::threats::{ChangedThreatBuffer, MAX_CHANGED, discover_changed_threats};
    use crate::{ForwardMode, L1, Network, SimdBackend};

    #[test]
    fn stack_frames_keep_compact_metadata_and_cache_line_alignment() {
        assert_eq!(
            core::mem::size_of::<FrameMetadata>(),
            2 * core::mem::size_of::<mf_core::Square>()
        );
        assert_eq!(core::mem::size_of::<FrameMetadata>(), 2);
        assert_eq!(core::mem::size_of::<AccumulatorState>(), 4_224);
        assert_eq!(core::mem::align_of::<AccumulatorFrame>(), 64);
        // Lazy updates added `pending`, which costs one cache line per frame (4,544 -> 4,608):
        // a `Move`, an `Undo`, and the enum tag, rounded up by the 64-byte alignment. That is
        // 8 KiB more per search thread, paid to skip the accumulator work of unread pushes.
        assert_eq!(core::mem::size_of::<AccumulatorFrame>(), 4_608);
        assert_eq!(
            core::mem::size_of::<AccumulatorFrame>() * STACK_STATES,
            594_432
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
    fn stack_counters_separate_incremental_updates_from_rebuilds_and_forwards() {
        let Some(network) = local_network("stack update counter test") else {
            return;
        };
        let root = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .expect("counter-test FEN should parse");
        let mut stack = AccumulatorStack::new(&network, &root);

        crate::reset_update_counters();
        let mut current = root;
        for notation in ["e1c1", "e7d6"] {
            let mv = mf_core::parse_uci_move(&current, notation, false)
                .expect("counter-test move should be legal");
            let mut child = current.clone();
            let undo = child.make_move(mv);
            stack.push_real(&child, mv, &undo).expect("push should fit");
            let _ = stack.evaluate(&child);
            current = child;
        }
        stack.push_null().expect("null push should fit");

        let counters = crate::update_counters();
        assert_eq!(counters.real_pushes, 2);
        assert_eq!(counters.null_pushes, 1);
        assert_eq!(counters.forward_evaluations, 2);
        // The castle moves the white king, so exactly one perspective leaves the plain
        // incremental path. Since Finny tables landed, that perspective is served from the cache
        // rather than rebuilt, so `king_rebuilds` stays zero and `finny_king_updates` carries it.
        assert_eq!(counters.king_rebuilds, 0);
        assert_eq!(counters.finny_king_updates, 1);
        assert_eq!(counters.overflow_rebuilds, 0);
        assert!(counters.changed_threat_edges > 0);
        assert!(counters.sliders_scanned > 0);
        assert!(counters.threat_discovery_cycles > 0);
        assert!(counters.accumulator_update_cycles > 0);
        assert!(counters.forward_cycles > 0);
        // The Finny timer is a strict subset of the update timer it sits inside.
        assert!(counters.finny_cycles > 0);
        assert!(counters.finny_cycles < counters.accumulator_update_cycles);
    }

    /// Replays a UCI move list, asserting incremental state equals a full rebuild at every ply.
    fn assert_parity_along(network: &Network, fen: &str, chess960: bool, notations: &[&str]) {
        let root = Position::from_fen(fen, chess960).expect("parity FEN should parse");
        let mut stack = AccumulatorStack::new_production(network, &root);
        let mut position = root;

        for notation in notations {
            let mv = mf_core::parse_uci_move(&position, notation, chess960)
                .unwrap_or_else(|| panic!("{fen}: {notation} should be a legal move"));
            let mut child = position.clone();
            let undo = child.make_move(mv);
            stack.push_real(&child, mv, &undo).expect("push should fit");

            let expected = AccumulatorState::from_position_production(network, &child);
            assert_eq!(stack.current(), &expected, "{fen} after {notation}");
            position = child;
        }
    }

    #[test]
    fn king_walks_keep_incremental_state_equal_to_a_full_rebuild() {
        let Some(network) = local_network("king-walk parity test") else {
            return;
        };
        // Kings walking every direction, crossing the d/e mirror line repeatedly in both
        // directions and in both colours, so cached and rebuilt paths alternate many times.
        assert_parity_along(
            &network,
            "4k3/8/8/3p4/3P4/8/8/4K3 w - - 0 1",
            false,
            &[
                "e1d1", "e8d8", "d1c1", "d8c8", "c1b1", "c8b8", "b1c1", "b8c8", "c1d1", "c8d8",
                "d1e1", "d8e8", "e1f1", "e8f8", "f1g1", "f8g8", "g1h1", "g8h8", "h1g1", "h8g8",
                "g1f1", "g8f8", "f1e2", "f8e8", "e2e3", "e8d8", "e3d3", "d8e8", "d3e3", "e8d8",
            ],
        );
    }

    #[test]
    fn castling_keeps_incremental_state_equal_to_a_full_rebuild() {
        let Some(network) = local_network("castling parity test") else {
            return;
        };
        // Kingside and queenside for both colours, each moving the king across two files.
        assert_parity_along(
            &network,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
            &["e1g1", "e8c8", "g1h1", "c8b8"],
        );
        assert_parity_along(
            &network,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
            &["e1c1", "e8g8"],
        );
    }

    #[test]
    fn chess960_castling_keeps_incremental_state_equal_to_a_full_rebuild() {
        let Some(network) = local_network("Chess960 castling parity test") else {
            return;
        };
        // King-takes-rook notation, including a castle whose king does not change square.
        assert_parity_along(
            &network,
            "4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1",
            true,
            &["c1f1"],
        );
        assert_parity_along(
            &network,
            "4k3/8/8/8/8/8/8/R1K2R2 w FA - 0 1",
            true,
            &["c1a1"],
        );
        // Queenside castle to b1/b8: the king crosses the mirror line via a castling move.
        assert_parity_along(
            &network,
            "1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1",
            true,
            &["c1b1", "c8b8"],
        );
    }

    #[cfg(feature = "instrumentation")]
    #[test]
    fn a_king_move_that_keeps_the_mirror_uses_the_cache_instead_of_rebuilding() {
        let Some(network) = local_network("Finny fast-path counter test") else {
            return;
        };
        let root = Position::from_fen("4k3/8/8/3p4/3P4/8/8/4K3 w - - 0 1", false)
            .expect("fast-path FEN should parse");
        let mut stack = AccumulatorStack::new(&network, &root);
        // e1->f1 keeps the king on the e-h half, so FullThreats indices are unchanged.
        let mv = mf_core::parse_uci_move(&root, "e1f1", false).expect("move should be legal");
        let mut child = root.clone();
        let undo = child.make_move(mv);

        crate::reset_update_counters();
        stack.push_real(&child, mv, &undo).expect("push should fit");

        let counters = crate::update_counters();
        assert_eq!(
            counters.king_rebuilds, 0,
            "the rebuild path must be avoided"
        );
        assert_eq!(counters.finny_king_updates, 1);
        // Two entries are refreshed: the vacated king square and the occupied one.
        assert_eq!(counters.finny_refreshes, 2);
    }

    #[cfg(feature = "instrumentation")]
    #[test]
    fn a_king_move_that_flips_the_mirror_reuses_the_cached_piece_rows() {
        let Some(network) = local_network("Finny mirror-flip counter test") else {
            return;
        };
        let root = Position::from_fen("4k3/8/8/3p4/3P4/8/8/4K3 w - - 0 1", false)
            .expect("mirror-flip FEN should parse");
        let mut stack = AccumulatorStack::new(&network, &root);
        // e1->d1 crosses the d/e mirror line, so every FullThreats index changes and the threat
        // half genuinely has to be recomputed -- but the piece half still comes from the cache.
        let mv = mf_core::parse_uci_move(&root, "e1d1", false).expect("move should be legal");
        let mut child = root.clone();
        let undo = child.make_move(mv);

        crate::reset_update_counters();
        stack.push_real(&child, mv, &undo).expect("push should fit");

        let counters = crate::update_counters();
        assert_eq!(
            counters.king_rebuilds, 0,
            "no king move should rebuild the piece rows from scratch"
        );
        assert_eq!(counters.finny_threat_rebuilds, 1);
        // Only the child's entry is consulted: the parent's HalfKA rows are replaced, not netted.
        assert_eq!(counters.finny_refreshes, 1);
    }

    #[cfg(feature = "instrumentation")]
    #[test]
    fn overflowing_pushes_are_counted_as_full_rebuilds() {
        let Some(network) = local_network("overflow counter test") else {
            return;
        };
        let parent = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .expect("overflow counter FEN should parse");
        let mv = mf_core::parse_uci_move(&parent, "e5d7", false)
            .expect("overflow counter move should be legal");
        let mut child = parent.clone();
        let undo = child.make_move(mv);
        let mut stack = AccumulatorStack::new(&network, &parent);

        crate::reset_update_counters();
        stack
            .push_real_with_threat_capacity::<0>(&child, mv, &undo)
            .expect("overflow push should fit");

        let counters = crate::update_counters();
        assert_eq!(counters.overflow_rebuilds, 1);
        assert_eq!(counters.king_rebuilds, 0);
        assert!(counters.rebuild_cycles > 0);
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
            let mut finny = crate::finny::FinnyTable::new(&network);
            fused.update_from(
                &parent_state,
                UpdateContext {
                    network: &network,
                    parent: &parent,
                    child: &child,
                    backend,
                    finny: &mut finny,
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
        let mut sparse_stack = AccumulatorStack::new_with_mode(&network, &position, sparse_mode)
            .expect("mode is supported");
        let mut dense_stack = AccumulatorStack::new_with_backend(&network, &position, backend)
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
