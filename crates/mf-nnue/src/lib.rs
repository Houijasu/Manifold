//! NNUE feature accumulation, network loading, and inference.

mod accumulator;
mod eval;
mod format;
pub mod halfka;
#[cfg(feature = "instrumentation")]
mod instrumentation;
mod network;
mod provision;
mod simd;
pub mod threats;

#[cfg(feature = "instrumentation")]
pub use accumulator::UpdateProfile;
pub use accumulator::{
    ACCUMULATOR_STACK_CAPACITY, AccumulatorStack, AccumulatorStackError, AccumulatorState,
};
pub use eval::EvaluationDump;
#[cfg(feature = "instrumentation")]
pub use instrumentation::{UpdateCounters, reset_update_counters, update_counters};
pub use network::{
    FC0_OUT, FC1_IN, FC1_OUT, FC2_IN, HALF, HALF_KA_DIMS, HalfKaWeights, L1, LAYER_STACKS,
    LayerStack, LoadError, Network, PSQT_BUCKETS, THREAT_DIMS, ThreatWeights, VERSION,
};
pub use provision::{NetworkSource, ResolveError, ResolvedNetwork, resolve_network};
pub use simd::{
    ForwardMode, SimdBackend, UnsupportedBackend, UnsupportedBackendReason, production_forward_mode,
};
