//! Training-data acquisition and training-record export.
//!
//! Produces `bulletformat::ChessBoard` records — the 32-byte format bullet's
//! `DirectSequentialDataLoader` reads — from two input sources that share one encoder:
//!
//! * [`generate`] — deterministic **self-play**, for later refinement, where genuine
//!   win/draw/loss labels exist.
//! * [`jsonl`] — the **downloaded CC0 Lichess evaluation database**, which is the
//!   bootstrap corpus. Self-play runs at ~836 positions/second, so a rung-1-sized
//!   corpus is weeks of wall clock; the download is ~45 minutes.
//!
//! Both go through [`record`], so there is exactly one byte-identical-to-bullet
//! encoder and one place the format can be wrong.
//!
//! Three properties govern the design, all of them validation-contract requirements:
//!
//! * **The format must be exactly right.** A record that decodes to the wrong board
//!   trains the network on noise and there is no downstream signal that would catch
//!   it, so [`record`] is checked against `bulletformat` itself as a round-trip oracle
//!   in `tests/bulletformat_round_trip.rs`, the same way perft is checked against
//!   `cozy-chess`. Both are **dev-dependencies only**: this crate has zero runtime
//!   dependencies outside the workspace.
//! * **The filters must actually be applied**, and verifiably so from the emitted file
//!   rather than from the generator's own bookkeeping. See [`validate`].
//! * **Generation must be deterministic under a fixed seed**, independently of the
//!   thread count. See [`generate`].

pub mod filter;
pub mod generate;
pub mod jsonl;
pub mod record;
pub mod rng;
pub mod validate;

pub use filter::{DEFAULT_SCORE_BOUND, Filter, Rejection};
pub use generate::{GenerateConfig, GenerateStats, generate, generate_from};
pub use jsonl::{
    ConvertConfig, ConvertStats, MATE_SATURATION_CP, RUNG1_WDL_LAMBDA, SkipReason, TIE_BREAK_RULE,
    convert,
};
pub use record::{EncodeError, Outcome, RECORD_BYTES, Record, StructuralError};
pub use rng::Rng;
pub use validate::{ValidationError, ValidationReport, validate_file};
