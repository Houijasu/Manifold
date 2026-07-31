# Repository Guidelines

## Project Structure & Module Organization

Manifold is a Rust 2024 chess-engine workspace. Production code lives under `crates/`:

- `mf-core`: board representation, move generation, notation, hashing, and perft. Its public API is re-exported flat from `src/lib.rs`.
- `mf-uci`: the `manifold` executable and UCI protocol handling.
- `mf-search`, `mf-nnue`, `mf-datagen`: implemented search, NNUE evaluation, and data-generation layers.
- `mf-tune`, `mf-lab`: planned tuning and experiment layers. These are currently stubs containing only a crate-level doc comment stating each crate's intended responsibility.

Keep unit tests beside small implementation details and integration tests in each crate's `tests/` directory. Shared perft-suite helpers live in `crates/mf-core/tests/common/mod.rs` and load EPD suites from `tools/testdata/` via a path relative to `CARGO_MANIFEST_DIR`; moving crates or testdata breaks that coupling.

Other top-level directories:

- `research/`: design notes (`infra-readiness.md` and `search-and-eval-sota.md` carry roadmap context) and `research/_src/`, read-only Stockfish-derived C++ used as a reference for NNUE/search work. Do not modify it.
- `tools/testdata/`: perft EPD suites (ChessProgramming wiki, Ethereal, Fischer random). `tools/books/`: opening books for gauntlets. `tools/fastchess/` and `tools/lc0/` hold third-party artifacts only.
- `baselines/`, `experiments/`, `nets/reference/`: intentionally empty placeholders. Do not commit generated `target/` output or local `nets/*.nnue` files (both gitignored).
- Root `config.json` is a fastchess tournament config, not a Rust or engine config.

## Build, Test, and Development Commands

- `cargo build --workspace` builds every crate with development settings.
- `cargo test --workspace` runs the full unit and integration test suite.
- `cargo test -p mf-core --test perft` runs one focused integration-test target.
- `cargo run -p mf-uci --bin manifold` starts the engine in UCI mode.
- `cargo run -p mf-uci --bin manifold -- perft 5 [--fen <FEN>] [--chess960]` runs the perft CLI.
- `cargo fmt --all -- --check` verifies formatting.
- `cargo clippy --workspace --all-targets -- -D warnings` treats lint findings as failures.
- `cargo build --release` produces the optimized engine (workspace profile: fat LTO, one codegen unit, `panic = "abort"`).

## Non-Obvious Build & Test Behavior

- `.cargo/config.toml` sets `target-cpu=native` for `x86_64-pc-windows-msvc`. This enables BMI2, so `SlidingAttacks::new()` selects the PEXT backend on native builds and black-magics elsewhere. The `mf-core` feature `force-magic` forces the magic backend; test both when changing sliding attacks (`cargo test -p mf-core --features force-magic`). Binaries built with `target-cpu=native` are not portable across CPUs.
- `crates/mf-core/build.rs` generates both PEXT and black-magic sliding tables into `OUT_DIR` (included by `src/sliding.rs`). Magic generation runs a bounded randomized search at build time; it only re-runs when `build.rs` itself changes (`cargo:rerun-if-changed=build.rs`), so edits to `sliding.rs` do not regenerate tables.
- Perft test anchors are depth-gated on `cfg!(debug_assertions)`: debug runs shallow depths, release runs the full anchors (e.g. startpos depth 6 = 119,060,324 nodes, Ethereal suite to depth 6). `cargo test --release` is the authoritative perft validation and takes much longer. The `perft.rs` test target serializes its tests through a global mutex to avoid oversubscribing the machine.
- `cozy-chess` is a dev-dependency used only as a differential oracle (`tests/perft_differential.rs` compares move counts and perft-3 on random reachable positions, including Chess960). It must never become a runtime dependency.

## Architecture Notes

- `Position` (mf-core) keeps both a mailbox array and bitboards, plus reversible `Undo` state; `make_move`/`unmake_move` must restore the position bit-for-bit, and tests assert this. Zobrist keys are incrementally updated by every mutator, including a non-pawn material-count key in `ZobristKeys`.
- Chess960 is first-class: castling rights are stored as rook squares, and `Position::from_fen` (X-FEN rook-file rights), `format_uci_move`/`parse_uci_move` (king-takes-rook castling notation), and the UCI `UCI_Chess960` option all take a `chess960` flag. Always thread that flag through instead of assuming standard castling.
- `mf-uci` currently implements `uci`, `isready`, `quit`, `setoption`, `position`, and `go perft`; unsupported commands are silently ignored.

## Coding Style & Naming Conventions

Use standard `rustfmt` output and four-space indentation. Follow Rust conventions: `snake_case` for modules, functions, and test names; `UpperCamelCase` for types and traits; `SCREAMING_SNAKE_CASE` for constants. Keep crate names in the existing `mf-*` pattern. Prefer explicit domain names such as `castling_rights` or `zobrist_key` over abbreviations, and keep public APIs documented when behavior or invariants are non-obvious.

## Testing Guidelines

Use Rust's built-in `#[test]` framework. Name tests as behavioral statements, for example `make_unmake_restores_every_special_move_bit_for_bit`. Add regression coverage for move legality, Chess960 castling, hashing, protocol parsing, and make/unmake symmetry when touching those areas. Perft changes should include exact node-count anchors or differential checks against the existing `cozy-chess` test oracle.

### Match-harness rules for multi-thread measurements

Two harness rules govern every fastchess measurement in this repo. Both were established by single-variable controls, not by argument; see `experiments/M5-smp/M5-smp-results.md` for the evidence.

- **`-use-affinity` is mandatory when both engines run `Threads=1`, and forbidden when either engine runs `Threads>1`.** Windows migrates engine threads onto E-cores mid-search (1.7x P-vs-E NPS gap), so unpinned single-threaded results are invalid rather than merely noisy; run those with `-use-affinity -concurrency 8`. But `-use-affinity` pins each engine process into a CPU subset too small to hold 8 threads, so a `Threads=8` engine oversubscribes, its clock-owning worker 0 is descheduled, and it forfeits on time. A 20-game control differing only in that flag measured **+214.85 Elo with zero forfeits unpinned** versus **-381.70 Elo pinned** for the same 8-thread engine — a roughly 600 Elo artifact. Multi-thread matches use **no `-use-affinity` and `-concurrency 1`**.
- **Never compare different thread counts at a fixed node budget.** Lazy SMP threads search overlapping trees, so an aggregate node budget divides by the thread count. On `go nodes 100000`, Stockfish 18 itself drops from depth 15 at 1 thread to depth 10 at 8 threads. Compare thread counts at **fixed time**. Fixed-node comparisons stay valid only between builds at the *same* thread count, which is what the deterministic `bench` signature is for.

## Commit & Pull Request Guidelines

Recent commits use short, imperative, sentence-case subjects such as `Implement bitboards and sliding attacks`; follow that style and keep each commit focused. Pull requests should explain the change and its engine impact, list commands run, link relevant issues or research notes, and call out performance or node-count changes. Include reproducible benchmark or perft results for search, move-generation, and evaluation changes.
