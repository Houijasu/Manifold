# Corrhist Regression Implementation Report

## Status

DONE_WITH_CONCERNS

The complete EXP-D corrhist regression tool is implemented and the authoritative run
completed. The only verification failures are pre-existing workspace issues outside the
touched implementation:

- `cargo fmt --all -- --check` reports formatting differences only in
  `crates/mf-core/src/see.rs` and `crates/mf-search/examples/see_profile.rs`.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` fails because
  `crates/mf-uci/tests/train_cli.rs` references the absent unrelated
  `mf_uci::parse_train_config`.

## Implementation

- Added the off-by-default `mf-search/corrhist-regression` feature.
- Added feature-gated `CorrectionFeatures`, `CorrectionSample`, and
  `search_with_correction_samples`.
- Predictor entries are copied before descendant searches and correction-history
  updates. The callback receives only completed exact ordinary PVS nodes that are not
  in check, verification/excluded-move nodes, mate scores, tablebase scores, or qsearch.
- Default builds compile out the sample types, reporter storage, snapshot reads, and
  callback branch.
- Added the feature-gated `mf-lab` binary and optional `mf-search` dependency; no
  `mf-uci` source or dependency changes were made.
- Added standard-library EPD parsing, deterministic root selection and train/test
  assignment, SplitMix64 Algorithm R reservoirs, standardized seven-column OLS/ridge,
  partial-pivot Gaussian elimination, root-fold lambda selection, exact shipped
  integer-blend comparison, and deterministic Markdown/CSV reporting.
- Train and test use separate `SharedHistory` and transposition tables. Warm roots are
  replayed into both histories without recording; each measured root belongs to one
  split; its split TT is cleared while its split history is retained.
- Output storage is bounded by `--max-samples`.

## TDD Coverage

Red tests were observed before implementation for:

- feature extraction and independent missing continuation predictors;
- immutable predictor snapshots after history updates;
- EPD/FEN parsing and invalid-line diagnostics;
- deterministic root selection and split isolation;
- deterministic fixed-capacity reservoirs;
- complete CLI parsing and validation;
- known-coefficient OLS recovery;
- singular OLS and collinear ridge behavior;
- R², MAE, and RMSE;
- exact truncating shipped integer blend;
- deterministic root-fold ridge selection;
- byte-identical fixed-seed end-to-end outputs.

## Verification

Passed:

- `cargo test -p mf-search --features corrhist-regression`
- `cargo test -p mf-lab --features corrhist-regression`
- `cargo clippy -p mf-search --all-targets --features corrhist-regression -- -D warnings`
- `cargo clippy -p mf-lab --all-targets --features corrhist-regression -- -D warnings`
- `cargo check -p mf-search`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- targeted `rustfmt --edition 2024 --check` for every touched Rust file
- task-scoped `git diff --check`
- `cargo run --release -p mf-uci --bin manifold -- bench`
  - nodes: **40,705**

Expected unrelated failures:

- full workspace format check: only `see.rs` and `see_profile.rs`;
- all-features workspace clippy: unrelated missing `parse_train_config`.

## EXP-D Run

Command:

```text
cargo run --release -p mf-lab --features corrhist-regression -- corrhist-regression --epd tools/books/UHO_4060_v4.epd --nodes 10000 --warm-roots 2000 --roots 8000 --max-samples 1000000 --seed 1 --ridge 0,0.01,0.1,1,10 --output experiments/EXP-D-corrhist-regression
```

Output:

- path: `C:\Users\Samaritan\Projects\Manifold\experiments\EXP-D-corrhist-regression`
- files: 3
- total size: 4,592 bytes
- parsed roots: 241,670
- measured roots: 6,388 train / 1,612 test
- eligible/stored samples: 244,351 train / 61,324 test
- selected lambda: 10

Test metrics:

| Model | R² | MAE | RMSE |
|---|---:|---:|---:|
| Fitted | 0.071651 | 26.082117 | 45.436294 |
| Shipped integer blend | -0.036966 | 25.103467 | 48.020799 |

The fitted model improves RMSE by 5.38% but worsens MAE by 3.90%. Coefficient signs
were 100% stable across the five training-root folds, but both continuation
coefficients reverse sign relative to the shipped blend.

The generated `report.md` therefore records the explicit recommendation:

> **STOP; DO NOT PROCEED TO EXP-C.**

The full run was repeated. Output hashes were byte-identical:

- `coefficients.csv`:
  `A3DE21B18FD422ADE8E775FE57453D89EDDEEA7672658DFA82FFBF9F116831C2`
- `report.md`:
  `F306717D0CD7C447F3B9AE912E6107211CCA84DA6990F3BAD334B677F0DAB084`
- `samples-summary.csv`:
  `F73422D067BDDF6222C5BD6FCF74A400937DBDEDEB235280434027210CEDAADF`

No commit was created.
