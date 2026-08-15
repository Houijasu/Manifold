# Corrhist Regression Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a deterministic `mf-lab corrhist-regression` research tool that fits and evaluates six correction-history predictors without changing the normal engine.

**Architecture:** Add a compile-time-gated sampler to `mf-search`, then implement EPD selection, bounded reservoir sampling, standardized ridge/OLS fitting, and Markdown/CSV reporting in a standalone `mf-lab` binary.

**Tech Stack:** Rust 2024, standard library only for parsing/statistics, existing `mf-core`, `mf-nnue`, and `mf-search`.

## Global Constraints

- `corrhist-regression` is off by default.
- Default engine behavior and the 40,705-node bench signature remain unchanged.
- No `mf-uci` dependency on `mf-lab`.
- No external math/statistics crate.
- Deterministic for fixed corpus, network, seed, and arguments.
- Bounded sample storage.
- Do not commit from the shared dirty working tree.

---

### Task 1: Feature-gated search sampling API

**Files:**
- Modify: `crates/mf-search/Cargo.toml`
- Modify: `crates/mf-search/src/lib.rs`
- Modify: `crates/mf-search/src/search.rs`

**Interfaces:**

```rust
pub struct CorrectionFeatures {
    pub pawn: i16,
    pub minor: i16,
    pub major: i16,
    pub material: i16,
    pub continuation_2: i16,
    pub continuation_4: i16,
}

pub struct CorrectionSample {
    pub features: CorrectionFeatures,
    pub raw_static_eval: i32,
    pub search_value: i32,
    pub depth: u32,
    pub ply: usize,
    pub position_key: u64,
}
```

Expose one feature-gated single-thread search entry point accepting caller-owned
`SharedHistory` and `FnMut(CorrectionSample)`.

- [ ] Add tests proving feature extraction reproduces the exact existing
  `correction_value`, including independent missing continuation values.
- [ ] Snapshot features at ordinary PVS node entry.
- [ ] Emit only completed exact, non-check, non-verification, non-mate nodes.
- [ ] Prove descendant/current-node updates cannot alter the emitted snapshot.
- [ ] Confirm the non-feature build has no callback branch or bench change.

Run:

```powershell
cargo test -p mf-search --features corrhist-regression
```

---

### Task 2: Deterministic corpus and reservoir layer

**Files:**
- Modify: `crates/mf-lab/Cargo.toml`
- Replace: `crates/mf-lab/src/lib.rs`
- Create: `crates/mf-lab/src/main.rs`
- Create: `crates/mf-lab/src/corpus.rs`
- Create: `crates/mf-lab/src/reservoir.rs`

**Interfaces:**

- Parse each non-empty EPD line by taking the FEN before the first `;`.
- Deterministically select warmup and measured root indices.
- Deterministically assign measured roots to train/test.
- Provide a fixed-capacity reservoir sampler driven by a local SplitMix64 RNG.

- [ ] Add parser tests for plain FEN, semicolon EPD, blank/comment lines, invalid FEN.
- [ ] Add deterministic split tests proving no root crosses train/test.
- [ ] Add reservoir reproducibility/capacity tests.
- [ ] Add CLI parsing for all design arguments and clear validation errors.

Run:

```powershell
cargo test -p mf-lab --features corrhist-regression
```

---

### Task 3: Standardized ridge/OLS solver

**Files:**
- Create: `crates/mf-lab/src/regression.rs`

**Interfaces:**

- Seven columns: intercept + six predictors.
- Standardize predictors using training means/stddevs.
- Solve normal equations with partial-pivot Gaussian elimination.
- Ridge penalty applies only to predictor diagonals.
- Convert coefficients back to raw history-entry units.

- [ ] Add synthetic recovery test with known coefficients and intercept.
- [ ] Add singular OLS failure test.
- [ ] Add collinear ridge finite-solution test.
- [ ] Add metric tests for R², MAE, RMSE.
- [ ] Add exact shipped-integer-blend metric test.

Run:

```powershell
cargo test -p mf-lab --features corrhist-regression regression
```

---

### Task 4: Search collection and report generation

**Files:**
- Create: `crates/mf-lab/src/corrhist.rs`
- Create: `crates/mf-lab/src/report.rs`
- Modify: `crates/mf-lab/src/main.rs`

**Interfaces:**

- Resolve network using existing `mf-nnue` facilities.
- Use separate `SharedHistory` and TT instances for train/test.
- Replay warm roots without recording.
- Clear TT between roots, retain split history.
- Run fixed-node, single-thread searches with tablebases/root filters absent.
- Store at most `max_samples` through deterministic reservoirs.

- [ ] Add a small integration test using a temporary EPD and optional test network;
  skip cleanly if no network is available.
- [ ] Write `samples-summary.csv`, `coefficients.csv`, and `report.md`.
- [ ] Include full command/config, network source, corpus path/hash, sample/root counts,
  lambda selection, fitted/shipped metrics, and fold stability.
- [ ] Ensure output is byte-identical across two fixed-seed runs.

---

### Task 5: Full verification and real EXP-D run

- [ ] Run:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p mf-search --features corrhist-regression
cargo test -p mf-lab --features corrhist-regression
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench
```

Known unrelated formatting failures in `mf-core/src/see.rs` and
`mf-search/examples/see_profile.rs` may be attributed without modification; touched
files still require targeted rustfmt and `git diff --check`.

- [ ] Run a real corpus experiment. Start with a smaller validation run, then use the
  design's 2,000 warm / 8,000 measured / 10,000 nodes settings if runtime is practical.
- [ ] Review the generated report for the EXP-C gate:
  positive test R², meaningful RMSE/MAE improvement over shipped blend, and stable
  coefficient signs.
- [ ] Record a recommendation in `report.md`: proceed to EXP-C or stop.
