// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

#[cfg(test)]
use std::cell::Cell;
use std::fmt;
use std::sync::OnceLock;

use crate::network::{L1, Network, PSQT_BUCKETS};

#[cfg(target_arch = "x86")]
use std::arch::x86::{
    __m128i, __m256i, _mm_add_epi32, _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi16,
    _mm256_add_epi32, _mm256_castsi256_si128, _mm256_cmpeq_epi8, _mm256_cvtepi8_epi16,
    _mm256_dpbusd_avx_epi32, _mm256_extracti128_si256, _mm256_hadd_epi32, _mm256_loadu_si256,
    _mm256_madd_epi16, _mm256_maddubs_epi16, _mm256_max_epi16, _mm256_min_epi16,
    _mm256_movemask_epi8, _mm256_mullo_epi16, _mm256_packus_epi16, _mm256_permute4x64_epi64,
    _mm256_set1_epi16, _mm256_set1_epi32, _mm256_setzero_si256, _mm256_srli_epi16,
    _mm256_storeu_si256, _mm256_sub_epi16, _mm256_sub_epi32,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    __m128i, __m256i, _mm_add_epi32, _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi16,
    _mm256_add_epi32, _mm256_castsi256_si128, _mm256_cmpeq_epi8, _mm256_cvtepi8_epi16,
    _mm256_dpbusd_avx_epi32, _mm256_extracti128_si256, _mm256_hadd_epi32, _mm256_loadu_si256,
    _mm256_madd_epi16, _mm256_maddubs_epi16, _mm256_max_epi16, _mm256_min_epi16,
    _mm256_movemask_epi8, _mm256_mullo_epi16, _mm256_packus_epi16, _mm256_permute4x64_epi64,
    _mm256_set1_epi16, _mm256_set1_epi32, _mm256_setzero_si256, _mm256_srli_epi16,
    _mm256_storeu_si256, _mm256_sub_epi16, _mm256_sub_epi32,
};

/// Runtime NNUE kernel implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdBackend {
    /// Portable scalar implementation.
    Scalar,
    /// AVX2 implementation.
    Avx2,
    /// AVX2 implementation with AVX-VNNI available to forward kernels.
    Avx2Vnni,
}

impl SimdBackend {
    /// Reports whether this process may safely execute the backend.
    #[must_use]
    pub fn is_supported(self) -> bool {
        match self {
            Self::Scalar => true,
            Self::Avx2 => avx2_supported(),
            Self::Avx2Vnni => avx2_supported() && avx_vnni_supported(),
        }
    }
}

/// A validated forward-kernel choice for tests, benchmarks, or production.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForwardMode {
    backend: SimdBackend,
    sparse_fc0: bool,
}

impl ForwardMode {
    /// Validates an explicit backend without executing any target-feature code.
    ///
    /// Sparse FC0 is a SIMD-only mode, so requesting it with the scalar backend
    /// is rejected as unsupported too.
    pub fn new(backend: SimdBackend, sparse_fc0: bool) -> Result<Self, UnsupportedBackend> {
        if sparse_fc0 && backend == SimdBackend::Scalar {
            return Err(UnsupportedBackend {
                backend,
                reason: UnsupportedBackendReason::SparseFc0RequiresSimd,
            });
        }
        if !backend.is_supported() {
            return Err(UnsupportedBackend {
                backend,
                reason: UnsupportedBackendReason::CpuFeatureUnavailable,
            });
        }
        Ok(Self {
            backend,
            sparse_fc0,
        })
    }

    /// Returns the always-supported dense scalar mode.
    #[must_use]
    pub const fn scalar() -> Self {
        Self {
            backend: SimdBackend::Scalar,
            sparse_fc0: false,
        }
    }

    /// Returns the validated backend.
    #[must_use]
    pub const fn backend(self) -> SimdBackend {
        self.backend
    }

    /// Reports whether sparse FC0 was selected.
    #[must_use]
    pub const fn sparse_fc0(self) -> bool {
        self.sparse_fc0
    }
}

/// Reason an explicitly requested NNUE mode cannot run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedBackendReason {
    /// The process does not expose the CPU features required by the backend.
    CpuFeatureUnavailable,
    /// Sparse FC0 has no scalar implementation.
    SparseFc0RequiresSimd,
}

/// Error returned when an explicitly requested backend or mode cannot run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedBackend {
    backend: SimdBackend,
    reason: UnsupportedBackendReason,
}

impl UnsupportedBackend {
    /// Returns the backend that was rejected.
    #[must_use]
    pub const fn backend(self) -> SimdBackend {
        self.backend
    }

    /// Returns why the requested backend or mode was rejected.
    #[must_use]
    pub const fn reason(self) -> UnsupportedBackendReason {
        self.reason
    }
}

impl fmt::Display for UnsupportedBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            UnsupportedBackendReason::CpuFeatureUnavailable => write!(
                formatter,
                "NNUE SIMD backend {:?} is not supported by this process",
                self.backend
            ),
            UnsupportedBackendReason::SparseFc0RequiresSimd => {
                formatter.write_str("sparse FC0 is not supported by the scalar NNUE backend")
            }
        }
    }
}

impl std::error::Error for UnsupportedBackend {}

static PRODUCTION_FORWARD_MODE: OnceLock<ForwardMode> = OnceLock::new();
#[cfg(test)]
thread_local! {
    static SPARSE_FC0_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// Returns the process-wide production mode selected on first use.
///
/// `MF_FORCE_SCALAR`, `MF_FORCE_NOVNNI`, and `MF_FORCE_DENSE` override the
/// corresponding automatic choices when set to a truthy value.
#[must_use]
pub fn production_forward_mode() -> ForwardMode {
    *PRODUCTION_FORWARD_MODE.get_or_init(detect_production_mode)
}

fn detect_production_mode() -> ForwardMode {
    select_production_mode(
        environment_flag("MF_FORCE_SCALAR"),
        environment_flag("MF_FORCE_NOVNNI"),
        environment_flag("MF_FORCE_DENSE"),
        SimdBackend::Avx2.is_supported(),
        SimdBackend::Avx2Vnni.is_supported(),
    )
}

fn select_production_mode(
    force_scalar: bool,
    force_no_vnni: bool,
    force_dense: bool,
    avx2_supported: bool,
    avx2_vnni_supported: bool,
) -> ForwardMode {
    let backend = if force_scalar {
        SimdBackend::Scalar
    } else if !force_no_vnni && avx2_vnni_supported {
        SimdBackend::Avx2Vnni
    } else if avx2_supported {
        SimdBackend::Avx2
    } else {
        SimdBackend::Scalar
    };
    let sparse_fc0 = backend != SimdBackend::Scalar && !force_dense;

    ForwardMode {
        backend,
        sparse_fc0,
    }
}

fn environment_flag(name: &str) -> bool {
    let Some(value) = std::env::var_os(name) else {
        return false;
    };
    let value = value.to_string_lossy();
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn avx2_supported() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
const fn avx2_supported() -> bool {
    false
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn avx_vnni_supported() -> bool {
    std::arch::is_x86_feature_detected!("avxvnni")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
const fn avx_vnni_supported() -> bool {
    false
}

#[inline]
pub(crate) fn add_i16_row(backend: SimdBackend, accumulator: &mut [i16; L1], row: &[i16; L1]) {
    match backend {
        SimdBackend::Scalar => add_i16_row_scalar(accumulator, row),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, and the function
            // accepts complete L1 rows suitable for its fixed-width loop.
            unsafe { add_i16_row_avx2(accumulator, row) };
        }
    }
}

#[inline]
pub(crate) fn subtract_i16_row(backend: SimdBackend, accumulator: &mut [i16; L1], row: &[i16; L1]) {
    match backend {
        SimdBackend::Scalar => subtract_i16_row_scalar(accumulator, row),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, and the function
            // accepts complete L1 rows suitable for its fixed-width loop.
            unsafe { subtract_i16_row_avx2(accumulator, row) };
        }
    }
}

#[inline]
pub(crate) fn add_i8_row(backend: SimdBackend, accumulator: &mut [i16; L1], row: &[i8; L1]) {
    match backend {
        SimdBackend::Scalar => add_i8_row_scalar(accumulator, row),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, and the function
            // accepts complete L1 rows suitable for its fixed-width loop.
            unsafe { add_i8_row_avx2(accumulator, row) };
        }
    }
}

#[inline]
pub(crate) fn subtract_i8_row(backend: SimdBackend, accumulator: &mut [i16; L1], row: &[i8; L1]) {
    match backend {
        SimdBackend::Scalar => subtract_i8_row_scalar(accumulator, row),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, and the function
            // accepts complete L1 rows suitable for its fixed-width loop.
            unsafe { subtract_i8_row_avx2(accumulator, row) };
        }
    }
}

#[inline]
pub(crate) fn add_psqt_row(
    backend: SimdBackend,
    accumulator: &mut [i32; PSQT_BUCKETS],
    row: &[i32; PSQT_BUCKETS],
) {
    match backend {
        SimdBackend::Scalar => add_psqt_row_scalar(accumulator, row),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, and both arrays
            // contain exactly one AVX2 register of i32 lanes.
            unsafe { add_psqt_row_avx2(accumulator, row) };
        }
    }
}

#[inline]
pub(crate) fn subtract_psqt_row(
    backend: SimdBackend,
    accumulator: &mut [i32; PSQT_BUCKETS],
    row: &[i32; PSQT_BUCKETS],
) {
    match backend {
        SimdBackend::Scalar => subtract_psqt_row_scalar(accumulator, row),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, and both arrays
            // contain exactly one AVX2 register of i32 lanes.
            unsafe { subtract_psqt_row_avx2(accumulator, row) };
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fused_accumulator_update(
    backend: SimdBackend,
    network: &Network,
    parent_values: &[i16; L1],
    child_values: &mut [i16; L1],
    parent_psqt: &[i32; PSQT_BUCKETS],
    child_psqt: &mut [i32; PSQT_BUCKETS],
    halfka_removals: &[usize],
    halfka_additions: &[usize],
    threat_removals: &[u32],
    threat_additions: &[u32],
) {
    match backend {
        SimdBackend::Scalar => fused_accumulator_update_scalar(
            network,
            parent_values,
            child_values,
            parent_psqt,
            child_psqt,
            halfka_removals,
            halfka_additions,
            threat_removals,
            threat_additions,
        ),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above. All fixed-size
            // accumulators and network rows cover the complete AVX2 loops.
            unsafe {
                fused_accumulator_update_avx2(
                    network,
                    parent_values,
                    child_values,
                    parent_psqt,
                    child_psqt,
                    halfka_removals,
                    halfka_additions,
                    threat_removals,
                    threat_additions,
                );
            }
        }
    }
}

#[inline]
fn assert_supported(backend: SimdBackend) {
    assert!(
        backend.is_supported(),
        "attempted to execute unsupported NNUE backend {backend:?}"
    );
}

#[inline]
pub(crate) fn feature_transform(
    backend: SimdBackend,
    us: &[i16; L1],
    them: &[i16; L1],
    output: &mut [u8; L1],
) -> [u64; L1 / 256] {
    match backend {
        SimdBackend::Scalar => feature_transform_scalar(us, them, output),
        SimdBackend::Avx2 | SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above. All fixed-size
            // inputs cover the complete unaligned AVX2 loads and stores.
            unsafe { feature_transform_avx2(us, them, output) }
        }
    }
}

#[inline]
pub(crate) fn affine_dense<const INPUTS: usize, const OUTPUTS: usize>(
    backend: SimdBackend,
    input: &[u8; INPUTS],
    weights: &[i8],
    biases: &[i32; OUTPUTS],
    output: &mut [i32; OUTPUTS],
) {
    debug_assert_eq!(weights.len(), INPUTS * OUTPUTS);
    debug_assert_eq!(INPUTS % 32, 0);
    match backend {
        SimdBackend::Scalar => affine_dense_scalar(input, weights, biases, output),
        SimdBackend::Avx2 => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above. All NNUE dense
            // layer input widths are multiples of one unaligned AVX2 load.
            unsafe { affine_dense_avx2(input, weights, biases, output) };
        }
        SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, including AVX-VNNI.
            unsafe { affine_dense_avx2_vnni(input, weights, biases, output) };
        }
    }
}

#[inline]
pub(crate) fn affine_sparse_fc0(
    backend: SimdBackend,
    input: &[u8; L1],
    weights: &[i8],
    biases: &[i32; 32],
    nonzero_chunks: &[u64; L1 / 256],
    output: &mut [i32; 32],
) {
    debug_assert_eq!(weights.len(), L1 * 32);
    #[cfg(test)]
    SPARSE_FC0_CALLS.with(|calls| calls.set(calls.get() + 1));
    match backend {
        SimdBackend::Scalar => {
            unreachable!("sparse FC0 is not available for the scalar backend")
        }
        SimdBackend::Avx2 => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above. The sparse
            // layout contains 32 complete four-byte output rows per chunk.
            unsafe { affine_sparse_fc0_avx2(input, weights, biases, nonzero_chunks, output) };
        }
        SimdBackend::Avx2Vnni => {
            assert_supported(backend);
            // SAFETY: support was checked immediately above, including AVX-VNNI.
            unsafe { affine_sparse_fc0_avx2_vnni(input, weights, biases, nonzero_chunks, output) };
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_sparse_fc0_calls() {
    SPARSE_FC0_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(crate) fn sparse_fc0_calls() -> usize {
    SPARSE_FC0_CALLS.with(Cell::get)
}

#[inline]
fn feature_transform_scalar(
    us: &[i16; L1],
    them: &[i16; L1],
    output: &mut [u8; L1],
) -> [u64; L1 / 256] {
    for feature in 0..(L1 / 2) {
        output[feature] = clipped_pair_product(us[feature], us[feature + L1 / 2]);
        output[feature + L1 / 2] = clipped_pair_product(them[feature], them[feature + L1 / 2]);
    }
    nonzero_chunk_mask(output)
}

#[inline]
fn clipped_pair_product(left: i16, right: i16) -> u8 {
    let left = i32::from(left).clamp(0, 255);
    let right = i32::from(right).clamp(0, 255);
    ((left * right) / 512) as u8
}

#[inline]
fn nonzero_chunk_mask(output: &[u8; L1]) -> [u64; L1 / 256] {
    let mut mask = [0_u64; L1 / 256];
    for (chunk, values) in output.chunks_exact(4).enumerate() {
        if values.iter().any(|&value| value != 0) {
            mask[chunk / 64] |= 1_u64 << (chunk % 64);
        }
    }
    mask
}

#[inline]
fn affine_dense_scalar<const INPUTS: usize, const OUTPUTS: usize>(
    input: &[u8; INPUTS],
    weights: &[i8],
    biases: &[i32; OUTPUTS],
    output: &mut [i32; OUTPUTS],
) {
    for neuron in 0..OUTPUTS {
        output[neuron] = input
            .iter()
            .zip(&weights[neuron * INPUTS..][..INPUTS])
            .fold(biases[neuron], |sum, (&activation, &weight)| {
                sum.wrapping_add(i32::from(activation) * i32::from(weight))
            });
    }
}

#[inline]
fn add_i16_row_scalar(accumulator: &mut [i16; L1], row: &[i16; L1]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value = value.wrapping_add(weight);
    }
}

#[inline]
fn subtract_i16_row_scalar(accumulator: &mut [i16; L1], row: &[i16; L1]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value = value.wrapping_sub(weight);
    }
}

#[inline]
fn add_i8_row_scalar(accumulator: &mut [i16; L1], row: &[i8; L1]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value = value.wrapping_add(i16::from(weight));
    }
}

#[inline]
fn subtract_i8_row_scalar(accumulator: &mut [i16; L1], row: &[i8; L1]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value = value.wrapping_sub(i16::from(weight));
    }
}

#[inline]
fn add_psqt_row_scalar(accumulator: &mut [i32; PSQT_BUCKETS], row: &[i32; PSQT_BUCKETS]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value = value.wrapping_add(weight);
    }
}

#[inline]
fn subtract_psqt_row_scalar(accumulator: &mut [i32; PSQT_BUCKETS], row: &[i32; PSQT_BUCKETS]) {
    for (value, &weight) in accumulator.iter_mut().zip(row) {
        *value = value.wrapping_sub(weight);
    }
}

#[allow(clippy::too_many_arguments)]
fn fused_accumulator_update_scalar(
    network: &Network,
    parent_values: &[i16; L1],
    child_values: &mut [i16; L1],
    parent_psqt: &[i32; PSQT_BUCKETS],
    child_psqt: &mut [i32; PSQT_BUCKETS],
    halfka_removals: &[usize],
    halfka_additions: &[usize],
    threat_removals: &[u32],
    threat_additions: &[u32],
) {
    child_values.copy_from_slice(parent_values);
    child_psqt.copy_from_slice(parent_psqt);

    for &feature in halfka_removals {
        subtract_i16_row_scalar(
            child_values,
            network
                .half_ka_weights()
                .row(feature)
                .expect("removed HalfKA feature must be in range"),
        );
        subtract_psqt_row_scalar(
            child_psqt,
            network
                .psqt_row(feature)
                .expect("removed HalfKA PSQT feature must be in range"),
        );
    }
    for &feature in halfka_additions {
        add_i16_row_scalar(
            child_values,
            network
                .half_ka_weights()
                .row(feature)
                .expect("added HalfKA feature must be in range"),
        );
        add_psqt_row_scalar(
            child_psqt,
            network
                .psqt_row(feature)
                .expect("added HalfKA PSQT feature must be in range"),
        );
    }
    for &feature in threat_removals {
        let feature = feature as usize;
        subtract_i8_row_scalar(
            child_values,
            network
                .threat_weights()
                .row(feature)
                .expect("removed FullThreats feature must be in range"),
        );
        subtract_psqt_row_scalar(
            child_psqt,
            network
                .threat_psqt_row(feature)
                .expect("removed FullThreats PSQT feature must be in range"),
        );
    }
    for &feature in threat_additions {
        let feature = feature as usize;
        add_i8_row_scalar(
            child_values,
            network
                .threat_weights()
                .row(feature)
                .expect("added FullThreats feature must be in range"),
        );
        add_psqt_row_scalar(
            child_psqt,
            network
                .threat_psqt_row(feature)
                .expect("added FullThreats PSQT feature must be in range"),
        );
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn feature_transform_avx2(
    us: &[i16; L1],
    them: &[i16; L1],
    output: &mut [u8; L1],
) -> [u64; L1 / 256] {
    let mut nonzero_chunks = [0_u64; L1 / 256];
    for (input, output_offset) in [(us, 0), (them, L1 / 2)] {
        for feature in (0..(L1 / 2)).step_by(32) {
            // SAFETY: each fixed-size perspective contains both 32-lane source
            // ranges, while output contains the corresponding 32 bytes.
            unsafe {
                let left_0 = _mm256_loadu_si256(input.as_ptr().add(feature).cast::<__m256i>());
                let left_1 = _mm256_loadu_si256(input.as_ptr().add(feature + 16).cast::<__m256i>());
                let right_0 =
                    _mm256_loadu_si256(input.as_ptr().add(feature + L1 / 2).cast::<__m256i>());
                let right_1 =
                    _mm256_loadu_si256(input.as_ptr().add(feature + L1 / 2 + 16).cast::<__m256i>());
                let zero = _mm256_setzero_si256();
                let maximum = _mm256_set1_epi16(255);
                let left_0 = _mm256_max_epi16(zero, _mm256_min_epi16(left_0, maximum));
                let left_1 = _mm256_max_epi16(zero, _mm256_min_epi16(left_1, maximum));
                let right_0 = _mm256_max_epi16(zero, _mm256_min_epi16(right_0, maximum));
                let right_1 = _mm256_max_epi16(zero, _mm256_min_epi16(right_1, maximum));
                let product_0 = _mm256_srli_epi16::<9>(_mm256_mullo_epi16(left_0, right_0));
                let product_1 = _mm256_srli_epi16::<9>(_mm256_mullo_epi16(left_1, right_1));
                let packed =
                    _mm256_permute4x64_epi64::<0xd8>(_mm256_packus_epi16(product_0, product_1));
                let zero_bytes = _mm256_cmpeq_epi8(packed, zero);
                let nonzero_bytes = !(_mm256_movemask_epi8(zero_bytes) as u32);
                let chunk_bits = compress_dword_nonzero_bits(nonzero_bytes);
                let chunk = (output_offset + feature) / 4;
                nonzero_chunks[chunk / 64] |= u64::from(chunk_bits) << (chunk % 64);
                _mm256_storeu_si256(
                    output
                        .as_mut_ptr()
                        .add(output_offset + feature)
                        .cast::<__m256i>(),
                    packed,
                );
            }
        }
    }
    nonzero_chunks
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn feature_transform_avx2(
    _: &[i16; L1],
    _: &[i16; L1],
    _: &mut [u8; L1],
) -> [u64; L1 / 256] {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available. NNUE activations are at most
/// 127, so each `vpmaddubsw` pair is bounded by +/-32,512 and cannot saturate.
unsafe fn affine_dense_avx2<const INPUTS: usize, const OUTPUTS: usize>(
    input: &[u8; INPUTS],
    weights: &[i8],
    biases: &[i32; OUTPUTS],
    output: &mut [i32; OUTPUTS],
) {
    let grouped_outputs = OUTPUTS / 4 * 4;
    for neuron in (0..grouped_outputs).step_by(4) {
        let mut sums = [_mm256_setzero_si256(); 4];
        for offset in (0..INPUTS).step_by(32) {
            // SAFETY: INPUTS is a multiple of 32 and each of the four rows
            // contains a complete INPUTS-byte weight vector.
            unsafe {
                let activations = _mm256_loadu_si256(input.as_ptr().add(offset).cast::<__m256i>());
                for (lane, sum) in sums.iter_mut().enumerate() {
                    let row = _mm256_loadu_si256(
                        weights
                            .as_ptr()
                            .add((neuron + lane) * INPUTS + offset)
                            .cast::<__m256i>(),
                    );
                    let pairs = _mm256_maddubs_epi16(activations, row);
                    let quads = _mm256_madd_epi16(pairs, _mm256_set1_epi16(1));
                    *sum = _mm256_add_epi32(*sum, quads);
                }
            }
        }
        // SAFETY: the source and destination each contain four complete i32 lanes.
        unsafe {
            let reduced = horizontal_sum_four_avx2(sums);
            let bias = _mm_loadu_si128(biases.as_ptr().add(neuron).cast::<__m128i>());
            _mm_storeu_si128(
                output.as_mut_ptr().add(neuron).cast::<__m128i>(),
                _mm_add_epi32(reduced, bias),
            );
        }
    }

    for neuron in grouped_outputs..OUTPUTS {
        let mut sums = _mm256_setzero_si256();
        for offset in (0..INPUTS).step_by(32) {
            // SAFETY: INPUTS is a multiple of 32 and the row-major weights
            // contain a complete INPUTS-byte row for every output.
            unsafe {
                let activations = _mm256_loadu_si256(input.as_ptr().add(offset).cast::<__m256i>());
                let row = _mm256_loadu_si256(
                    weights
                        .as_ptr()
                        .add(neuron * INPUTS + offset)
                        .cast::<__m256i>(),
                );
                let pairs = _mm256_maddubs_epi16(activations, row);
                let quads = _mm256_madd_epi16(pairs, _mm256_set1_epi16(1));
                sums = _mm256_add_epi32(sums, quads);
            }
        }
        let mut lanes = [0_i32; 8];
        // SAFETY: `lanes` contains exactly one unaligned AVX2 register.
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().cast::<__m256i>(), sums) };
        output[neuron] = lanes.into_iter().fold(biases[neuron], i32::wrapping_add);
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn affine_dense_avx2<const INPUTS: usize, const OUTPUTS: usize>(
    _: &[u8; INPUTS],
    _: &[i8],
    _: &[i32; OUTPUTS],
    _: &mut [i32; OUTPUTS],
) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,avxvnni")]
/// # Safety
///
/// The caller must verify that AVX2 and AVX-VNNI are available.
unsafe fn affine_dense_avx2_vnni<const INPUTS: usize, const OUTPUTS: usize>(
    input: &[u8; INPUTS],
    weights: &[i8],
    biases: &[i32; OUTPUTS],
    output: &mut [i32; OUTPUTS],
) {
    let grouped_outputs = OUTPUTS / 4 * 4;
    for neuron in (0..grouped_outputs).step_by(4) {
        let mut sums = [_mm256_setzero_si256(); 4];
        for offset in (0..INPUTS).step_by(32) {
            // SAFETY: INPUTS is a multiple of 32 and each of the four rows
            // contains a complete INPUTS-byte weight vector.
            unsafe {
                let activations = _mm256_loadu_si256(input.as_ptr().add(offset).cast::<__m256i>());
                for (lane, sum) in sums.iter_mut().enumerate() {
                    let row = _mm256_loadu_si256(
                        weights
                            .as_ptr()
                            .add((neuron + lane) * INPUTS + offset)
                            .cast::<__m256i>(),
                    );
                    *sum = _mm256_dpbusd_avx_epi32(*sum, activations, row);
                }
            }
        }
        // SAFETY: the source and destination each contain four complete i32 lanes.
        unsafe {
            let reduced = horizontal_sum_four_avx2(sums);
            let bias = _mm_loadu_si128(biases.as_ptr().add(neuron).cast::<__m128i>());
            _mm_storeu_si128(
                output.as_mut_ptr().add(neuron).cast::<__m128i>(),
                _mm_add_epi32(reduced, bias),
            );
        }
    }

    for neuron in grouped_outputs..OUTPUTS {
        let mut sums = _mm256_setzero_si256();
        for offset in (0..INPUTS).step_by(32) {
            // SAFETY: INPUTS is a multiple of 32 and the row-major weights
            // contain a complete INPUTS-byte row for every output.
            unsafe {
                let activations = _mm256_loadu_si256(input.as_ptr().add(offset).cast::<__m256i>());
                let row = _mm256_loadu_si256(
                    weights
                        .as_ptr()
                        .add(neuron * INPUTS + offset)
                        .cast::<__m256i>(),
                );
                sums = _mm256_dpbusd_avx_epi32(sums, activations, row);
            }
        }
        let mut lanes = [0_i32; 8];
        // SAFETY: `lanes` contains exactly one unaligned AVX2 register.
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().cast::<__m256i>(), sums) };
        output[neuron] = lanes.into_iter().fold(biases[neuron], i32::wrapping_add);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify AVX2 support.
unsafe fn horizontal_sum_four_avx2(sums: [__m256i; 4]) -> __m128i {
    let first = _mm256_hadd_epi32(sums[0], sums[1]);
    let second = _mm256_hadd_epi32(sums[2], sums[3]);
    let quarters = _mm256_hadd_epi32(first, second);
    _mm_add_epi32(
        _mm256_castsi256_si128(quarters),
        _mm256_extracti128_si256::<1>(quarters),
    )
}

#[inline]
const fn compress_dword_nonzero_bits(byte_bits: u32) -> u8 {
    let mut bits = byte_bits;
    bits |= bits >> 1;
    bits |= bits >> 2;
    bits &= 0x1111_1111;
    bits = (bits | (bits >> 3)) & 0x0303_0303;
    bits = (bits | (bits >> 6)) & 0x000f_000f;
    bits = (bits | (bits >> 12)) & 0x0000_00ff;
    bits as u8
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify AVX2 support and provide the chunk-major FC0 layout.
unsafe fn affine_sparse_fc0_avx2(
    input: &[u8; L1],
    weights: &[i8],
    biases: &[i32; 32],
    nonzero_chunks: &[u64; L1 / 256],
    output: &mut [i32; 32],
) {
    // SAFETY: the fixed-size bias and output arrays contain four AVX2 registers.
    let mut sums = unsafe {
        [
            _mm256_loadu_si256(biases.as_ptr().cast::<__m256i>()),
            _mm256_loadu_si256(biases.as_ptr().add(8).cast::<__m256i>()),
            _mm256_loadu_si256(biases.as_ptr().add(16).cast::<__m256i>()),
            _mm256_loadu_si256(biases.as_ptr().add(24).cast::<__m256i>()),
        ]
    };
    let ones = _mm256_set1_epi16(1);

    for (word_index, &word) in nonzero_chunks.iter().enumerate() {
        let mut remaining = word;
        while remaining != 0 {
            let chunk = word_index * 64 + remaining.trailing_zeros() as usize;
            let activation = i32::from_le_bytes(
                input[chunk * 4..chunk * 4 + 4]
                    .try_into()
                    .expect("input chunk contains four bytes"),
            );
            let activations = _mm256_set1_epi32(activation);
            for (group, sum) in sums.iter_mut().enumerate() {
                // SAFETY: each chunk occupies 128 bytes, with each eight-output
                // group occupying one complete unaligned AVX2 register.
                unsafe {
                    let rows = _mm256_loadu_si256(
                        weights
                            .as_ptr()
                            .add(chunk * 128 + group * 32)
                            .cast::<__m256i>(),
                    );
                    let pairs = _mm256_maddubs_epi16(activations, rows);
                    let quads = _mm256_madd_epi16(pairs, ones);
                    *sum = _mm256_add_epi32(*sum, quads);
                }
            }
            remaining &= remaining - 1;
        }
    }

    // SAFETY: every store writes one of the four complete eight-output groups.
    unsafe {
        for (group, sum) in sums.into_iter().enumerate() {
            _mm256_storeu_si256(output.as_mut_ptr().add(group * 8).cast::<__m256i>(), sum);
        }
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn affine_sparse_fc0_avx2(
    _: &[u8; L1],
    _: &[i8],
    _: &[i32; 32],
    _: &[u64; L1 / 256],
    _: &mut [i32; 32],
) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,avxvnni")]
/// # Safety
///
/// The caller must verify AVX2 and AVX-VNNI support and provide the
/// chunk-major FC0 layout.
unsafe fn affine_sparse_fc0_avx2_vnni(
    input: &[u8; L1],
    weights: &[i8],
    biases: &[i32; 32],
    nonzero_chunks: &[u64; L1 / 256],
    output: &mut [i32; 32],
) {
    // SAFETY: the fixed-size bias and output arrays contain four AVX2 registers.
    let mut sums = unsafe {
        [
            _mm256_loadu_si256(biases.as_ptr().cast::<__m256i>()),
            _mm256_loadu_si256(biases.as_ptr().add(8).cast::<__m256i>()),
            _mm256_loadu_si256(biases.as_ptr().add(16).cast::<__m256i>()),
            _mm256_loadu_si256(biases.as_ptr().add(24).cast::<__m256i>()),
        ]
    };

    for (word_index, &word) in nonzero_chunks.iter().enumerate() {
        let mut remaining = word;
        while remaining != 0 {
            let chunk = word_index * 64 + remaining.trailing_zeros() as usize;
            let activation = i32::from_le_bytes(
                input[chunk * 4..chunk * 4 + 4]
                    .try_into()
                    .expect("input chunk contains four bytes"),
            );
            let activations = _mm256_set1_epi32(activation);
            for (group, sum) in sums.iter_mut().enumerate() {
                // SAFETY: each chunk occupies 128 bytes, with each eight-output
                // group occupying one complete unaligned AVX2 register.
                unsafe {
                    let rows = _mm256_loadu_si256(
                        weights
                            .as_ptr()
                            .add(chunk * 128 + group * 32)
                            .cast::<__m256i>(),
                    );
                    *sum = _mm256_dpbusd_avx_epi32(*sum, activations, rows);
                }
            }
            remaining &= remaining - 1;
        }
    }

    // SAFETY: every store writes one of the four complete eight-output groups.
    unsafe {
        for (group, sum) in sums.into_iter().enumerate() {
            _mm256_storeu_si256(output.as_mut_ptr().add(group * 8).cast::<__m256i>(), sum);
        }
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn affine_sparse_fc0_avx2_vnni(
    _: &[u8; L1],
    _: &[i8],
    _: &[i32; 32],
    _: &[u64; L1 / 256],
    _: &mut [i32; 32],
) {
    unreachable!("AVX-VNNI dispatch is unavailable on non-x86 targets")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn affine_dense_avx2_vnni<const INPUTS: usize, const OUTPUTS: usize>(
    _: &[u8; INPUTS],
    _: &[i8],
    _: &[i32; OUTPUTS],
    _: &mut [i32; OUTPUTS],
) {
    unreachable!("AVX-VNNI dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn fused_accumulator_update_avx2(
    network: &Network,
    parent_values: &[i16; L1],
    child_values: &mut [i16; L1],
    parent_psqt: &[i32; PSQT_BUCKETS],
    child_psqt: &mut [i32; PSQT_BUCKETS],
    halfka_removals: &[usize],
    halfka_additions: &[usize],
    threat_removals: &[u32],
    threat_additions: &[u32],
) {
    const TILE_REGISTERS: usize = 8;
    const LANES_PER_REGISTER: usize = 16;
    const TILE_LANES: usize = TILE_REGISTERS * LANES_PER_REGISTER;

    for tile in (0..L1).step_by(TILE_LANES) {
        let mut values = [_mm256_setzero_si256(); TILE_REGISTERS];
        for (register, value) in values.iter_mut().enumerate() {
            let offset = tile + register * LANES_PER_REGISTER;
            // SAFETY: each tile contains eight complete 16-lane registers.
            *value =
                unsafe { _mm256_loadu_si256(parent_values.as_ptr().add(offset).cast::<__m256i>()) };
        }

        for &feature in halfka_removals {
            let row = network
                .half_ka_weights()
                .row(feature)
                .expect("removed HalfKA feature must be in range");
            for (register, value) in values.iter_mut().enumerate() {
                let offset = tile + register * LANES_PER_REGISTER;
                // SAFETY: every HalfKA row has L1 complete lanes.
                let weights =
                    unsafe { _mm256_loadu_si256(row.as_ptr().add(offset).cast::<__m256i>()) };
                *value = _mm256_sub_epi16(*value, weights);
            }
        }
        for &feature in halfka_additions {
            let row = network
                .half_ka_weights()
                .row(feature)
                .expect("added HalfKA feature must be in range");
            for (register, value) in values.iter_mut().enumerate() {
                let offset = tile + register * LANES_PER_REGISTER;
                // SAFETY: every HalfKA row has L1 complete lanes.
                let weights =
                    unsafe { _mm256_loadu_si256(row.as_ptr().add(offset).cast::<__m256i>()) };
                *value = _mm256_add_epi16(*value, weights);
            }
        }
        for &feature in threat_removals {
            let feature = feature as usize;
            let row = network
                .threat_weights()
                .row(feature)
                .expect("removed FullThreats feature must be in range");
            for (register, value) in values.iter_mut().enumerate() {
                let offset = tile + register * LANES_PER_REGISTER;
                // SAFETY: every FullThreats row has L1 complete lanes.
                let bytes = unsafe { _mm_loadu_si128(row.as_ptr().add(offset).cast::<__m128i>()) };
                *value = _mm256_sub_epi16(*value, _mm256_cvtepi8_epi16(bytes));
            }
        }
        for &feature in threat_additions {
            let feature = feature as usize;
            let row = network
                .threat_weights()
                .row(feature)
                .expect("added FullThreats feature must be in range");
            for (register, value) in values.iter_mut().enumerate() {
                let offset = tile + register * LANES_PER_REGISTER;
                // SAFETY: every FullThreats row has L1 complete lanes.
                let bytes = unsafe { _mm_loadu_si128(row.as_ptr().add(offset).cast::<__m128i>()) };
                *value = _mm256_add_epi16(*value, _mm256_cvtepi8_epi16(bytes));
            }
        }

        for (register, value) in values.into_iter().enumerate() {
            let offset = tile + register * LANES_PER_REGISTER;
            // SAFETY: each tile contains eight complete 16-lane registers.
            unsafe {
                _mm256_storeu_si256(
                    child_values.as_mut_ptr().add(offset).cast::<__m256i>(),
                    value,
                );
            }
        }
    }

    // SAFETY: both arrays contain exactly one complete AVX2 register.
    let mut psqt = unsafe { _mm256_loadu_si256(parent_psqt.as_ptr().cast::<__m256i>()) };
    for &feature in halfka_removals {
        let row = network
            .psqt_row(feature)
            .expect("removed HalfKA PSQT feature must be in range");
        // SAFETY: each PSQT row contains exactly one complete AVX2 register.
        let weights = unsafe { _mm256_loadu_si256(row.as_ptr().cast::<__m256i>()) };
        psqt = _mm256_sub_epi32(psqt, weights);
    }
    for &feature in halfka_additions {
        let row = network
            .psqt_row(feature)
            .expect("added HalfKA PSQT feature must be in range");
        // SAFETY: each PSQT row contains exactly one complete AVX2 register.
        let weights = unsafe { _mm256_loadu_si256(row.as_ptr().cast::<__m256i>()) };
        psqt = _mm256_add_epi32(psqt, weights);
    }
    for &feature in threat_removals {
        let feature = feature as usize;
        let row = network
            .threat_psqt_row(feature)
            .expect("removed FullThreats PSQT feature must be in range");
        // SAFETY: each PSQT row contains exactly one complete AVX2 register.
        let weights = unsafe { _mm256_loadu_si256(row.as_ptr().cast::<__m256i>()) };
        psqt = _mm256_sub_epi32(psqt, weights);
    }
    for &feature in threat_additions {
        let feature = feature as usize;
        let row = network
            .threat_psqt_row(feature)
            .expect("added FullThreats PSQT feature must be in range");
        // SAFETY: each PSQT row contains exactly one complete AVX2 register.
        let weights = unsafe { _mm256_loadu_si256(row.as_ptr().cast::<__m256i>()) };
        psqt = _mm256_add_epi32(psqt, weights);
    }
    // SAFETY: the child PSQT array contains one complete AVX2 register.
    unsafe {
        _mm256_storeu_si256(child_psqt.as_mut_ptr().cast::<__m256i>(), psqt);
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
unsafe fn fused_accumulator_update_avx2(
    _: &Network,
    _: &[i16; L1],
    _: &mut [i16; L1],
    _: &[i32; PSQT_BUCKETS],
    _: &mut [i32; PSQT_BUCKETS],
    _: &[usize],
    _: &[usize],
    _: &[u32],
    _: &[u32],
) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn add_i16_row_avx2(accumulator: &mut [i16; L1], row: &[i16; L1]) {
    for offset in (0..L1).step_by(16) {
        // SAFETY: the fixed-size arrays contain 16 lanes from every offset,
        // and unaligned loads/stores impose no stronger alignment requirement.
        unsafe {
            let values = _mm256_loadu_si256(accumulator.as_ptr().add(offset).cast::<__m256i>());
            let weights = _mm256_loadu_si256(row.as_ptr().add(offset).cast::<__m256i>());
            _mm256_storeu_si256(
                accumulator.as_mut_ptr().add(offset).cast::<__m256i>(),
                _mm256_add_epi16(values, weights),
            );
        }
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn add_i16_row_avx2(_: &mut [i16; L1], _: &[i16; L1]) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn subtract_i16_row_avx2(accumulator: &mut [i16; L1], row: &[i16; L1]) {
    for offset in (0..L1).step_by(16) {
        // SAFETY: the fixed-size arrays contain 16 lanes from every offset,
        // and unaligned loads/stores impose no stronger alignment requirement.
        unsafe {
            let values = _mm256_loadu_si256(accumulator.as_ptr().add(offset).cast::<__m256i>());
            let weights = _mm256_loadu_si256(row.as_ptr().add(offset).cast::<__m256i>());
            _mm256_storeu_si256(
                accumulator.as_mut_ptr().add(offset).cast::<__m256i>(),
                _mm256_sub_epi16(values, weights),
            );
        }
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn subtract_i16_row_avx2(_: &mut [i16; L1], _: &[i16; L1]) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn add_i8_row_avx2(accumulator: &mut [i16; L1], row: &[i8; L1]) {
    for offset in (0..L1).step_by(16) {
        // SAFETY: the fixed-size arrays contain 16 input and accumulator lanes
        // from every offset, and all memory accesses are unaligned.
        unsafe {
            let values = _mm256_loadu_si256(accumulator.as_ptr().add(offset).cast::<__m256i>());
            let bytes = _mm_loadu_si128(row.as_ptr().add(offset).cast::<__m128i>());
            let weights = _mm256_cvtepi8_epi16(bytes);
            _mm256_storeu_si256(
                accumulator.as_mut_ptr().add(offset).cast::<__m256i>(),
                _mm256_add_epi16(values, weights),
            );
        }
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn add_i8_row_avx2(_: &mut [i16; L1], _: &[i8; L1]) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn subtract_i8_row_avx2(accumulator: &mut [i16; L1], row: &[i8; L1]) {
    for offset in (0..L1).step_by(16) {
        // SAFETY: the fixed-size arrays contain 16 input and accumulator lanes
        // from every offset, and all memory accesses are unaligned.
        unsafe {
            let values = _mm256_loadu_si256(accumulator.as_ptr().add(offset).cast::<__m256i>());
            let bytes = _mm_loadu_si128(row.as_ptr().add(offset).cast::<__m128i>());
            let weights = _mm256_cvtepi8_epi16(bytes);
            _mm256_storeu_si256(
                accumulator.as_mut_ptr().add(offset).cast::<__m256i>(),
                _mm256_sub_epi16(values, weights),
            );
        }
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn subtract_i8_row_avx2(_: &mut [i16; L1], _: &[i8; L1]) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn add_psqt_row_avx2(accumulator: &mut [i32; PSQT_BUCKETS], row: &[i32; PSQT_BUCKETS]) {
    // SAFETY: both fixed-size arrays contain exactly eight i32 lanes, and the
    // unaligned load/store intrinsics impose no stronger alignment requirement.
    unsafe {
        let values = _mm256_loadu_si256(accumulator.as_ptr().cast::<__m256i>());
        let weights = _mm256_loadu_si256(row.as_ptr().cast::<__m256i>());
        _mm256_storeu_si256(
            accumulator.as_mut_ptr().cast::<__m256i>(),
            _mm256_add_epi32(values, weights),
        );
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn add_psqt_row_avx2(_: &mut [i32; PSQT_BUCKETS], _: &[i32; PSQT_BUCKETS]) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
/// # Safety
///
/// The caller must verify that AVX2 is available in the current process.
unsafe fn subtract_psqt_row_avx2(accumulator: &mut [i32; PSQT_BUCKETS], row: &[i32; PSQT_BUCKETS]) {
    // SAFETY: both fixed-size arrays contain exactly eight i32 lanes, and the
    // unaligned load/store intrinsics impose no stronger alignment requirement.
    unsafe {
        let values = _mm256_loadu_si256(accumulator.as_ptr().cast::<__m256i>());
        let weights = _mm256_loadu_si256(row.as_ptr().cast::<__m256i>());
        _mm256_storeu_si256(
            accumulator.as_mut_ptr().cast::<__m256i>(),
            _mm256_sub_epi32(values, weights),
        );
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn subtract_psqt_row_avx2(_: &mut [i32; PSQT_BUCKETS], _: &[i32; PSQT_BUCKETS]) {
    unreachable!("AVX2 dispatch is unavailable on non-x86 targets")
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    use super::horizontal_sum_four_avx2;
    use super::{
        ForwardMode, SimdBackend, UnsupportedBackendReason, add_i8_row, add_i16_row, add_psqt_row,
        affine_dense, affine_sparse_fc0, feature_transform, nonzero_chunk_mask,
        production_forward_mode, select_production_mode, subtract_i8_row, subtract_i16_row,
        subtract_psqt_row,
    };
    use crate::network::build_fc0_sparse_weights;
    use crate::{FC0_OUT, HALF, L1};
    #[cfg(target_arch = "x86")]
    use std::arch::x86::{_mm_storeu_si128, _mm256_loadu_si256};
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::{_mm_storeu_si128, _mm256_loadu_si256};
    fn supported_backends() -> impl Iterator<Item = SimdBackend> {
        [
            SimdBackend::Scalar,
            SimdBackend::Avx2,
            SimdBackend::Avx2Vnni,
        ]
        .into_iter()
        .filter(|backend| backend.is_supported())
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn four_way_horizontal_reduction_wraps_like_scalar() {
        if !SimdBackend::Avx2.is_supported() {
            return;
        }

        let lanes = [
            [1, 2, 3, 4, 5, 6, 7, 8],
            [i32::MAX, 1, -3, 5, 7, 11, 13, 17],
            [i32::MIN, -1, 3, -5, -7, -11, -13, -17],
            [99, -99, 101, -101, 103, -103, 107, -107],
        ];
        let registers = lanes.map(|values| {
            // SAFETY: each lane array contains one complete unaligned AVX2 register.
            unsafe { _mm256_loadu_si256(values.as_ptr().cast()) }
        });
        let sums = unsafe { horizontal_sum_four_avx2(registers) };
        let mut actual = [0_i32; 4];
        // SAFETY: the destination contains one complete unaligned SSE register.
        unsafe { _mm_storeu_si128(actual.as_mut_ptr().cast(), sums) };
        let expected = lanes.map(|values| values.into_iter().fold(0_i32, i32::wrapping_add));

        assert_eq!(actual, expected);
    }

    #[test]
    fn update_backends_match_scalar_with_wrapping_extremes() {
        let accumulator = std::array::from_fn(|index| match index % 6 {
            0 => i16::MAX,
            1 => i16::MIN,
            2 => -1,
            3 => 0,
            4 => 1,
            _ => (index as i16).wrapping_mul(127),
        });
        let i16_row = std::array::from_fn(|index| match index % 6 {
            0 => 1,
            1 => -1,
            2 => i16::MIN,
            3 => i16::MAX,
            4 => -32_000,
            _ => 32_000,
        });
        let i8_row = std::array::from_fn(|index| match index % 5 {
            0 => i8::MIN,
            1 => i8::MAX,
            2 => -1,
            3 => 0,
            _ => 1,
        });
        let psqt = [
            i32::MAX,
            i32::MIN,
            -1,
            0,
            1,
            1_000_000_000,
            -1_000_000_000,
            42,
        ];
        let psqt_row = [1, -1, i32::MIN, i32::MAX, -2, i32::MAX, i32::MIN, -42];

        let mut expected_i16_add = accumulator;
        add_i16_row(SimdBackend::Scalar, &mut expected_i16_add, &i16_row);
        let mut expected_i16_sub = accumulator;
        subtract_i16_row(SimdBackend::Scalar, &mut expected_i16_sub, &i16_row);
        let mut expected_i8_add = accumulator;
        add_i8_row(SimdBackend::Scalar, &mut expected_i8_add, &i8_row);
        let mut expected_i8_sub = accumulator;
        subtract_i8_row(SimdBackend::Scalar, &mut expected_i8_sub, &i8_row);
        let mut expected_psqt_add = psqt;
        add_psqt_row(SimdBackend::Scalar, &mut expected_psqt_add, &psqt_row);
        let mut expected_psqt_sub = psqt;
        subtract_psqt_row(SimdBackend::Scalar, &mut expected_psqt_sub, &psqt_row);

        for backend in supported_backends() {
            let mut actual = accumulator;
            add_i16_row(backend, &mut actual, &i16_row);
            assert_eq!(actual, expected_i16_add, "{backend:?} i16 add");

            actual = accumulator;
            subtract_i16_row(backend, &mut actual, &i16_row);
            assert_eq!(actual, expected_i16_sub, "{backend:?} i16 subtract");

            actual = accumulator;
            add_i8_row(backend, &mut actual, &i8_row);
            assert_eq!(actual, expected_i8_add, "{backend:?} i8 add");

            actual = accumulator;
            subtract_i8_row(backend, &mut actual, &i8_row);
            assert_eq!(actual, expected_i8_sub, "{backend:?} i8 subtract");

            let mut actual_psqt = psqt;
            add_psqt_row(backend, &mut actual_psqt, &psqt_row);
            assert_eq!(actual_psqt, expected_psqt_add, "{backend:?} PSQT add");

            actual_psqt = psqt;
            subtract_psqt_row(backend, &mut actual_psqt, &psqt_row);
            assert_eq!(actual_psqt, expected_psqt_sub, "{backend:?} PSQT subtract");
        }
    }

    #[test]
    fn explicit_modes_reject_every_unsupported_choice_safely() {
        assert!(ForwardMode::new(SimdBackend::Scalar, false).is_ok());
        let sparse_scalar = ForwardMode::new(SimdBackend::Scalar, true)
            .expect_err("scalar sparse FC0 must be rejected");
        assert_eq!(sparse_scalar.backend(), SimdBackend::Scalar);
        assert_eq!(
            sparse_scalar.reason(),
            UnsupportedBackendReason::SparseFc0RequiresSimd
        );
        assert_eq!(
            sparse_scalar.to_string(),
            "sparse FC0 is not supported by the scalar NNUE backend"
        );

        for backend in [SimdBackend::Avx2, SimdBackend::Avx2Vnni] {
            for sparse_fc0 in [false, true] {
                let result = ForwardMode::new(backend, sparse_fc0);
                assert_eq!(result.is_ok(), backend.is_supported());
                if let Err(error) = result {
                    assert_eq!(error.backend(), backend);
                    assert_eq!(
                        error.reason(),
                        UnsupportedBackendReason::CpuFeatureUnavailable
                    );
                }
            }
        }
    }

    #[test]
    fn production_mode_is_always_executable() {
        let mode = production_forward_mode();
        eprintln!("detected production NNUE mode: {mode:?}");
        assert!(mode.backend().is_supported());
        assert!(!mode.sparse_fc0() || mode.backend() != SimdBackend::Scalar);
    }

    #[test]
    fn production_overrides_control_automatic_selection() {
        assert_eq!(
            select_production_mode(false, false, false, true, true),
            ForwardMode {
                backend: SimdBackend::Avx2Vnni,
                sparse_fc0: true,
            }
        );
        assert_eq!(
            select_production_mode(false, true, false, true, true),
            ForwardMode {
                backend: SimdBackend::Avx2,
                sparse_fc0: true,
            }
        );
        assert_eq!(
            select_production_mode(false, false, true, true, true),
            ForwardMode {
                backend: SimdBackend::Avx2Vnni,
                sparse_fc0: false,
            }
        );
        assert_eq!(
            select_production_mode(true, false, false, true, true),
            ForwardMode::scalar()
        );
        assert_eq!(
            select_production_mode(false, false, false, false, false),
            ForwardMode::scalar()
        );
    }

    #[test]
    fn dense_feature_transform_matches_scalar_edges_and_chunk_mask() {
        let mut us = [0_i16; L1];
        let mut them = [0_i16; L1];
        us[0] = -1;
        us[HALF] = 255;
        us[1] = 256;
        us[HALF + 1] = 256;
        us[4] = 128;
        us[HALF + 4] = 255;
        them[0] = 255;
        them[HALF] = 255;
        them[511] = i16::MAX;
        them[L1 - 1] = i16::MAX;

        let mut expected = [0_u8; L1];
        let expected_mask = feature_transform(SimdBackend::Scalar, &us, &them, &mut expected);
        assert_eq!(expected[0], 0);
        assert_eq!(expected[1], 127);
        assert_eq!(expected[4], 63);
        assert_eq!(expected[HALF], 127);
        assert_eq!(expected[L1 - 1], 127);
        assert_eq!(expected_mask[0] & 0b11, 0b11);
        assert_eq!(expected_mask[2], 1);
        assert_eq!(expected_mask[3] >> 63, 1);

        for backend in supported_backends().filter(|backend| *backend != SimdBackend::Scalar) {
            let mut actual = [0_u8; L1];
            let actual_mask = feature_transform(backend, &us, &them, &mut actual);
            assert_eq!(actual, expected, "{backend:?} feature transform");
            assert_eq!(actual_mask, expected_mask, "{backend:?} chunk mask");
        }

        let zero = [0_i16; L1];
        let mut output = [99_u8; L1];
        for backend in supported_backends() {
            assert_eq!(
                feature_transform(backend, &zero, &zero, &mut output),
                [0; 4],
                "{backend:?} all-zero chunk mask"
            );
            assert_eq!(output, [0; L1], "{backend:?} all-zero transform");
        }
    }

    #[test]
    fn dense_affine_backends_match_scalar_with_negative_weights_and_max_activations() {
        let input = [127_u8; L1];
        let weights = vec![i8::MIN; L1 * FC0_OUT];
        let biases = std::array::from_fn(|index| if index % 2 == 0 { i32::MAX } else { i32::MIN });
        let mut expected = [0_i32; FC0_OUT];
        affine_dense(
            SimdBackend::Scalar,
            &input,
            &weights,
            &biases,
            &mut expected,
        );

        for backend in supported_backends().filter(|backend| *backend != SimdBackend::Scalar) {
            let mut actual = [0_i32; FC0_OUT];
            affine_dense(backend, &input, &weights, &biases, &mut actual);
            assert_eq!(actual, expected, "{backend:?} dense affine");
        }
    }

    #[test]
    fn sparse_fc0_matches_dense_for_zero_heavy_and_dense_inputs() {
        let weights = (0..L1 * FC0_OUT)
            .map(|index| index.wrapping_mul(37).wrapping_add(11) as i8)
            .collect::<Vec<_>>();
        let sparse_weights = build_fc0_sparse_weights(&weights);
        let biases =
            std::array::from_fn(|index| (index as i32).wrapping_mul(97).wrapping_sub(1_000));

        let mut zero_heavy = [0_u8; L1];
        for chunk in [0, 3, 17, 128, 255] {
            zero_heavy[chunk * 4..chunk * 4 + 4].copy_from_slice(&[
                1,
                127,
                0,
                (chunk as u8).wrapping_add(3),
            ]);
        }
        let zero_heavy_mask = nonzero_chunk_mask(&zero_heavy);
        let dense = [127_u8; L1];
        let dense_mask = [u64::MAX; L1 / 256];

        for (name, input, mask) in [
            ("zero-heavy", &zero_heavy, zero_heavy_mask),
            ("dense", &dense, dense_mask),
        ] {
            let mut expected = [0_i32; FC0_OUT];
            affine_dense(SimdBackend::Scalar, input, &weights, &biases, &mut expected);
            for backend in supported_backends().filter(|backend| *backend != SimdBackend::Scalar) {
                let mut actual = [0_i32; FC0_OUT];
                affine_sparse_fc0(backend, input, &sparse_weights, &biases, &mask, &mut actual);
                assert_eq!(actual, expected, "{backend:?} {name} sparse FC0");
            }
        }
    }
}
