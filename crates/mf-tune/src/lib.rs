//! Parameter tuning and statistical match orchestration.
//!
//! Implements fishtest-style SPSA over the search hyperparameters `mf-search` advertises
//! as UCI spin options. One iteration perturbs every parameter by `±c_k`, plays a small
//! batch of paired games between the two perturbed engines, and steps theta along the
//! measured direction. Over thousands of iterations that converges on a better point
//! without ever needing a gradient, which is the only thing available when the objective
//! is "how often does this win".
//!
//! The crate is a **binary** (`mf-tune`) rather than a `manifold` subcommand. The engine
//! binary is what fastchess launches sixteen at a time during a tuning batch; keeping the
//! tuner out of it means the tuner cannot grow the engine's startup cost or its
//! dependencies, and the tuner is free to spawn processes and write files that have no
//! business being reachable from a UCI session.
//!
//! Layering, outermost first:
//!
//! * [`run`] — the loop, generic over what plays a batch, so the resume path is testable
//!   without games.
//! * [`batch`] — fastchess invocation and PGN scoring, including the affinity guardrail.
//! * [`spsa`] — the update itself: no I/O, no chess, testable against a synthetic
//!   objective in milliseconds.
//! * [`config`] / [`checkpoint`] — what a run reads and what it must survive being killed
//!   with, both on [`document`]'s small shared TOML subset.

pub mod batch;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod document;
pub mod interrupt;
pub mod run;
pub mod spsa;

pub use batch::BatchResult;
pub use checkpoint::Checkpoint;
pub use cli::run_cli;
pub use config::{MatchSettings, TuningConfig, starter_config};
pub use run::{Arena, FastchessArena, RunOutcome, RunPaths, Stop};
pub use spsa::{Dimension, Schedule, Spsa};
