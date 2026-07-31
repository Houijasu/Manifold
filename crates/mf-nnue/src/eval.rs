// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

use mf_core::{Color, Position};

use crate::accumulator::AccumulatorState;
use crate::simd::{
    ForwardMode, SimdBackend, affine_dense, affine_sparse_fc0,
    feature_transform as feature_transform_simd, production_forward_mode,
};
use crate::{FC0_OUT, FC1_IN, FC1_OUT, FC2_IN, HALF, L1, LayerStack, Network};

const FT_MAX: i32 = 255;
const EVAL_MAX: i32 = 10_000;
const NORMALIZE_TO_PAWN_VALUE: i64 = 356;

/// Scalar-evaluation metadata and raw output for parity tooling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvaluationDump {
    pub bucket: usize,
    pub stm: Color,
    pub psqt_internal: i32,
    pub eval_internal: i32,
}

impl Network {
    /// Evaluates a position in side-to-move-relative centipawns.
    #[must_use]
    pub fn evaluate(&self, position: &Position) -> i32 {
        let state = AccumulatorState::from_position(self, position);
        self.evaluate_from_state(position, &state)
    }

    /// Evaluates a position with an explicitly selected forward mode.
    #[must_use]
    pub fn evaluate_with_mode(&self, position: &Position, mode: ForwardMode) -> i32 {
        let state = AccumulatorState::from_position_with_backend(self, position, mode.backend())
            .expect("ForwardMode guarantees its accumulator backend is supported");
        self.evaluate_from_state_with_mode(position, &state, mode)
    }

    /// Evaluates a position with the process-wide production forward mode.
    #[must_use]
    pub fn evaluate_production(&self, position: &Position) -> i32 {
        self.evaluate_with_mode(position, production_forward_mode())
    }

    /// Evaluates a position using a caller-supplied complete accumulator state.
    #[must_use]
    pub fn evaluate_from_state(&self, position: &Position, state: &AccumulatorState) -> i32 {
        let internal = self.evaluate_internal_from_state(position, state);
        normalize_internal(internal)
    }

    /// Evaluates a supplied accumulator state with an explicit forward mode.
    #[must_use]
    pub fn evaluate_from_state_with_mode(
        &self,
        position: &Position,
        state: &AccumulatorState,
        mode: ForwardMode,
    ) -> i32 {
        let internal = self.evaluate_internal_from_state_with_mode(position, state, mode);
        normalize_internal(internal)
    }

    /// Evaluates a supplied state with the process-wide production forward mode.
    #[must_use]
    pub fn evaluate_from_state_production(
        &self,
        position: &Position,
        state: &AccumulatorState,
    ) -> i32 {
        self.evaluate_from_state_with_mode(position, state, production_forward_mode())
    }

    /// Returns the raw blended NNUE value before centipawn conversion.
    #[must_use]
    pub fn evaluate_internal(&self, position: &Position) -> i32 {
        let state = AccumulatorState::from_position(self, position);
        self.evaluate_internal_from_state(position, &state)
    }

    /// Returns the raw value with an explicitly selected forward mode.
    #[must_use]
    pub fn evaluate_internal_with_mode(&self, position: &Position, mode: ForwardMode) -> i32 {
        let state = AccumulatorState::from_position_with_backend(self, position, mode.backend())
            .expect("ForwardMode guarantees its accumulator backend is supported");
        self.evaluate_internal_from_state_with_mode(position, &state, mode)
    }

    /// Returns the raw value with the process-wide production forward mode.
    #[must_use]
    pub fn evaluate_internal_production(&self, position: &Position) -> i32 {
        self.evaluate_internal_with_mode(position, production_forward_mode())
    }

    /// Returns the raw blended NNUE value using a supplied accumulator state.
    #[must_use]
    pub fn evaluate_internal_from_state(
        &self,
        position: &Position,
        state: &AccumulatorState,
    ) -> i32 {
        let mut transformed = [0; L1];
        self.dump_features_from_state(position, state, &mut transformed)
            .eval_internal
    }

    /// Returns the raw value for a supplied state with an explicit forward mode.
    #[must_use]
    pub fn evaluate_internal_from_state_with_mode(
        &self,
        position: &Position,
        state: &AccumulatorState,
        mode: ForwardMode,
    ) -> i32 {
        if mode.backend() == SimdBackend::Scalar {
            return self.evaluate_internal_from_state(position, state);
        }
        let mut transformed = [0; L1];
        self.dump_features_from_state_with_mode(position, state, &mut transformed, mode)
            .eval_internal
    }

    /// Returns the raw value for a supplied state with the production mode.
    #[must_use]
    pub fn evaluate_internal_from_state_production(
        &self,
        position: &Position,
        state: &AccumulatorState,
    ) -> i32 {
        self.evaluate_internal_from_state_with_mode(position, state, production_forward_mode())
    }

    /// Fills the caller-owned feature-transform output and returns parity metadata.
    #[must_use]
    pub fn dump_features(&self, position: &Position, transformed: &mut [u8; L1]) -> EvaluationDump {
        let state = AccumulatorState::from_position(self, position);
        self.dump_features_from_state(position, &state, transformed)
    }

    /// Fills feature-transform output with an explicitly selected forward mode.
    #[must_use]
    pub fn dump_features_with_mode(
        &self,
        position: &Position,
        transformed: &mut [u8; L1],
        mode: ForwardMode,
    ) -> EvaluationDump {
        let state = AccumulatorState::from_position_with_backend(self, position, mode.backend())
            .expect("ForwardMode guarantees its accumulator backend is supported");
        self.dump_features_from_state_with_mode(position, &state, transformed, mode)
    }

    /// Fills feature-transform output with the production forward mode.
    #[must_use]
    pub fn dump_features_production(
        &self,
        position: &Position,
        transformed: &mut [u8; L1],
    ) -> EvaluationDump {
        self.dump_features_with_mode(position, transformed, production_forward_mode())
    }

    /// Fills feature-transform output from a caller-supplied complete accumulator state.
    #[must_use]
    pub fn dump_features_from_state(
        &self,
        position: &Position,
        state: &AccumulatorState,
        transformed: &mut [u8; L1],
    ) -> EvaluationDump {
        let (us, them, bucket, psqt_internal, stm) = evaluation_inputs(position, state);
        feature_transform(&us.values, &them.values, transformed);
        let stack = self
            .layer_stack(bucket)
            .expect("legal chess occupancy must select an NNUE layer stack");
        let eval_internal = forward(stack, transformed, psqt_internal);

        EvaluationDump {
            bucket,
            stm,
            psqt_internal,
            eval_internal,
        }
    }

    /// Fills supplied-state feature output with an explicit forward mode.
    #[must_use]
    pub fn dump_features_from_state_with_mode(
        &self,
        position: &Position,
        state: &AccumulatorState,
        transformed: &mut [u8; L1],
        mode: ForwardMode,
    ) -> EvaluationDump {
        if mode.backend() == SimdBackend::Scalar {
            return self.dump_features_from_state(position, state, transformed);
        }
        let (us, them, bucket, psqt_internal, stm) = evaluation_inputs(position, state);
        let chunk_mask =
            feature_transform_simd(mode.backend(), &us.values, &them.values, transformed);
        let stack = self
            .layer_stack(bucket)
            .expect("legal chess occupancy must select an NNUE layer stack");
        let eval_internal = forward_simd(stack, transformed, psqt_internal, mode, &chunk_mask);

        EvaluationDump {
            bucket,
            stm,
            psqt_internal,
            eval_internal,
        }
    }

    /// Fills supplied-state feature output with the production forward mode.
    #[must_use]
    pub fn dump_features_from_state_production(
        &self,
        position: &Position,
        state: &AccumulatorState,
        transformed: &mut [u8; L1],
    ) -> EvaluationDump {
        self.dump_features_from_state_with_mode(
            position,
            state,
            transformed,
            production_forward_mode(),
        )
    }
}

#[inline]
fn normalize_internal(internal: i32) -> i32 {
    ((i64::from(internal) * 100) / NORMALIZE_TO_PAWN_VALUE)
        .clamp(i64::from(-EVAL_MAX), i64::from(EVAL_MAX)) as i32
}

#[inline]
fn evaluation_inputs<'state>(
    position: &Position,
    state: &'state AccumulatorState,
) -> (
    &'state crate::accumulator::Accumulator,
    &'state crate::accumulator::Accumulator,
    usize,
    i32,
    Color,
) {
    let white = state.accumulator(Color::White);
    let black = state.accumulator(Color::Black);
    let stm = position.side_to_move();
    let (us, them) = match stm {
        Color::White => (white, black),
        Color::Black => (black, white),
    };
    let bucket = (position.occupancy().count() as usize - 1) / 4;
    let psqt_internal = us.psqt[bucket].wrapping_sub(them.psqt[bucket]) / 2;
    (us, them, bucket, psqt_internal, stm)
}

#[inline]
fn feature_transform(us: &[i16; L1], them: &[i16; L1], output: &mut [u8; L1]) {
    for feature in 0..HALF {
        output[feature] = product(us[feature], us[feature + HALF]);
        output[feature + HALF] = product(them[feature], them[feature + HALF]);
    }
}

#[inline]
fn product(left: i16, right: i16) -> u8 {
    let left = i32::from(left).clamp(0, FT_MAX);
    let right = i32::from(right).clamp(0, FT_MAX);
    ((left * right) / 512) as u8
}

#[inline]
fn blend_output(fwd: i32, psqt_internal: i32) -> (i32, i32) {
    let output_value = ((i64::from(fwd) * 9_600) / 16_384) as i32;
    let psqt_value = psqt_internal / 16;
    let positional_value = output_value / 16;
    let eval_internal = (125 * psqt_value + 131 * positional_value) / 128;
    (output_value, eval_internal)
}

#[inline]
fn build_concatenated_activations(fc0: &[i32; FC0_OUT], output: &mut [u8; FC1_IN]) {
    for index in 0..31 {
        let value = i64::from(fc0[index]);
        output[index] = ((value * value) >> 21).min(127) as u8;
        output[31 + index] = (fc0[index] >> 7).clamp(0, 127) as u8;
    }
    output[62] = 0;
    output[63] = 0;
}

fn forward(stack: &LayerStack, transformed: &[u8; L1], psqt_internal: i32) -> i32 {
    let mut fc0 = [0; FC0_OUT];
    affine(
        transformed,
        stack.fc0_weights(),
        stack.fc0_biases(),
        &mut fc0,
    );

    let mut concatenated = [0; FC1_IN];
    build_concatenated_activations(&fc0, &mut concatenated);

    let mut fc1 = [0; FC1_OUT];
    affine(
        &concatenated,
        stack.fc1_weights(),
        stack.fc1_biases(),
        &mut fc1,
    );

    let mut activation = [0; FC2_IN];
    for (output, value) in activation.iter_mut().zip(fc1) {
        *output = (value >> 6).clamp(0, 127) as u8;
    }

    let fc2 = dot(&activation, stack.fc2_weights(), stack.fc2_bias());
    let fwd = fc2.wrapping_add(fc0[FC0_OUT - 1]);
    blend_output(fwd, psqt_internal).1
}

fn forward_simd(
    stack: &LayerStack,
    transformed: &[u8; L1],
    psqt_internal: i32,
    mode: ForwardMode,
    nonzero_chunks: &[u64; L1 / 256],
) -> i32 {
    let mut fc0 = [0; FC0_OUT];
    if mode.sparse_fc0() {
        affine_sparse_fc0(
            mode.backend(),
            transformed,
            stack.fc0_sparse_weights(),
            stack.fc0_biases(),
            nonzero_chunks,
            &mut fc0,
        );
    } else {
        affine_dense(
            mode.backend(),
            transformed,
            stack.fc0_weights(),
            stack.fc0_biases(),
            &mut fc0,
        );
    }

    let mut concatenated = [0; FC1_IN];
    build_concatenated_activations(&fc0, &mut concatenated);

    let mut fc1 = [0; FC1_OUT];
    affine_dense(
        mode.backend(),
        &concatenated,
        stack.fc1_weights(),
        stack.fc1_biases(),
        &mut fc1,
    );

    let mut activation = [0; FC2_IN];
    for (output, value) in activation.iter_mut().zip(fc1) {
        *output = (value >> 6).clamp(0, 127) as u8;
    }

    let mut fc2 = [0_i32; 1];
    affine_dense(
        mode.backend(),
        &activation,
        stack.fc2_weights(),
        &[stack.fc2_bias()],
        &mut fc2,
    );
    let fwd = fc2[0].wrapping_add(fc0[FC0_OUT - 1]);
    blend_output(fwd, psqt_internal).1
}

#[inline]
fn affine<const INPUTS: usize, const OUTPUTS: usize>(
    input: &[u8; INPUTS],
    weights: &[i8],
    biases: &[i32; OUTPUTS],
    output: &mut [i32; OUTPUTS],
) {
    debug_assert_eq!(weights.len(), INPUTS * OUTPUTS);
    for neuron in 0..OUTPUTS {
        output[neuron] = dot(
            input,
            &weights[neuron * INPUTS..(neuron + 1) * INPUTS],
            biases[neuron],
        );
    }
}

#[inline]
fn dot(input: &[u8], weights: &[i8], bias: i32) -> i32 {
    input
        .iter()
        .zip(weights)
        .fold(bias, |sum, (&activation, &weight)| {
            sum.wrapping_add(i32::from(activation) * i32::from(weight))
        })
}

#[cfg(test)]
mod tests {
    use super::{blend_output, build_concatenated_activations, feature_transform};
    use crate::{FC0_OUT, FC1_IN, HALF, L1};

    #[test]
    fn output_formula_truncates_negative_operands_toward_zero() {
        let vectors = [
            (-1, 0, 0, 0),
            (-16_383, 0, -9_599, -613),
            (-16_384, 0, -9_600, -614),
            (0, -31, 0, 0),
            (0, 31, 0, 0),
            (0, -32, 0, -1),
            (0, 32, 0, 1),
            (-20_000, -1_001, -11_718, -809),
        ];

        for (fwd, psqt_internal, output_value, eval_internal) in vectors {
            assert_eq!(
                blend_output(fwd, psqt_internal),
                (output_value, eval_internal)
            );
        }
    }

    #[test]
    fn feature_transform_clamps_multiplies_and_orders_stm_first() {
        let mut us = [0_i16; L1];
        let mut them = [0_i16; L1];
        let mut ft = [0_u8; L1];

        us[0] = -1;
        us[HALF] = 255;
        us[1] = 256;
        us[HALF + 1] = 256;
        us[2] = 128;
        us[HALF + 2] = 255;

        them[0] = 255;
        them[HALF] = 255;
        them[1] = 64;
        them[HALF + 1] = 8;
        them[2] = i16::MAX;
        them[HALF + 2] = i16::MIN;

        feature_transform(&us, &them, &mut ft);

        assert_eq!(ft[0], 0);
        assert_eq!(ft[1], 127);
        assert_eq!(ft[2], 63);
        assert_eq!(ft[HALF], 127);
        assert_eq!(ft[HALF + 1], 1);
        assert_eq!(ft[HALF + 2], 0);
        assert!(ft[3..HALF].iter().all(|&value| value == 0));
        assert!(ft[HALF + 3..].iter().all(|&value| value == 0));
    }

    #[test]
    fn concatenated_activation_uses_only_fc0_outputs_zero_through_thirty() {
        let mut fc0 = [0_i32; FC0_OUT];
        let mut conc = [99_u8; FC1_IN];
        fc0[0] = -20_000;
        fc0[1] = -127;
        fc0[2] = 128;
        fc0[3] = 16_383;
        fc0[31] = i32::MAX;

        build_concatenated_activations(&fc0, &mut conc);

        assert_eq!(conc[0], 127);
        assert_eq!(conc[1], 0);
        assert_eq!(conc[2], 0);
        assert_eq!(conc[3], 127);
        assert_eq!(conc[31], 0);
        assert_eq!(conc[32], 0);
        assert_eq!(conc[33], 1);
        assert_eq!(conc[34], 127);
        assert_eq!(conc[62], 0);
        assert_eq!(conc[63], 0);
    }
}
