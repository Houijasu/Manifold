//! Self-play data generation and training-record export.
//!
//! Produces `bulletformat::ChessBoard` records — the 32-byte format bullet's
//! `DirectSequentialDataLoader` reads — from deterministic self-play.
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
pub mod record;
pub mod rng;
pub mod validate;

pub use filter::{DEFAULT_SCORE_BOUND, Filter, Rejection};
pub use generate::{GenerateConfig, GenerateStats, generate};
pub use record::{EncodeError, Outcome, RECORD_BYTES, Record, StructuralError};
pub use rng::Rng;
pub use validate::{ValidationError, ValidationReport, validate_file};
