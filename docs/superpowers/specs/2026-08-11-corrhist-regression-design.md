# Corrhist Regression Research Tool Design

## Goal

Implement Stage 7 EXP-D as isolated research tooling: fit
`search_value - raw_static_eval` against Manifold's six correction-history
components and compare the fitted model with the shipped hand blend.

The normal engine binary and default search behavior must remain unchanged.

## Architecture

### Feature-gated search sampler

Add an off-by-default `corrhist-regression` feature to `mf-search`.
Under the feature, expose:

- `CorrectionFeatures`: pawn, minor, major, material, continuation-2, continuation-4;
- `CorrectionSample`: features, raw static evaluation, exact searched value, depth,
  ply, and position key;
- a single-threaded search entry point using caller-owned `SharedHistory` and a sample
  callback.

Feature extraction stays inside `search.rs`, where the continuation stack is available.
Predictors are snapshotted at node entry, before descendants or the current node update
can change history. Emit only completed, exact, ordinary PVS nodes:

- no qsearch;
- not in check;
- no excluded/verification move;
- exact bound;
- non-mate, evaluation-range score.

Default builds compile out the callback and sample structures.

### Dataset procedure

`mf-lab corrhist-regression` reads EPD/FEN roots and runs deterministic single-thread,
fixed-node searches:

1. deterministically select warmup and measured roots;
2. replay warmup roots into separate train and test histories without recording;
3. assign measured roots to train/test by stable root hash, never by individual node;
4. clear TT between roots but retain each split's history;
5. collect a bounded deterministic reservoir of node samples.

Separate histories prevent later training roots from modifying test predictors.

### Regression

Use a standard-library implementation:

- intercept plus six standardized predictors;
- 7×7 normal equations;
- Gaussian elimination with partial pivoting;
- ridge penalty excluding the intercept;
- deterministic candidate-lambda selection on training-root folds.

Report:

- sample/root counts;
- means and standard deviations;
- fitted raw-unit coefficients and intercept;
- test R², MAE, RMSE;
- the same metrics for the exact shipped integer blend;
- coefficient sign/stability across folds.

The shipped comparator uses the current enabled defaults:

```text
(15341*pawn + 10569*minor + 8761*continuation_2
 + 8761*continuation_4) / 131072
```

Major and material are maintained but default-disabled, so their shipped coefficients
are zero.

## CLI

Dedicated binary, not a `manifold` subcommand:

```text
cargo run --release -p mf-lab --features corrhist-regression -- \
  corrhist-regression \
  --epd tools/books/UHO_4060_v4.epd \
  --nodes 10000 \
  --warm-roots 2000 \
  --roots 8000 \
  --max-samples 1000000 \
  --seed 1 \
  --ridge 0,0.01,0.1,1,10 \
  --output experiments/EXP-D-corrhist-regression
```

`--eval-file` is optional; normal network resolution is the fallback.

Output directory:

- `samples-summary.csv`: aggregate feature/sample statistics, not every node;
- `coefficients.csv`: shipped and fitted coefficients;
- `report.md`: configuration, metrics, coefficients, fold stability, conclusions.

## Constraints

- No external linear-algebra/statistics dependency.
- No changes to `mf-uci` or the `manifold` runtime dependency graph.
- Default release bench remains 40,705 nodes.
- Deterministic for fixed corpus, network, seed, and arguments.
- No unbounded sample storage.
- Tests skip cleanly when an NNUE test network is unavailable.

## EXP-C decision gate

Do not implement EXP-C automatically unless EXP-D shows:

- positive out-of-sample R²;
- fitted model materially improves test RMSE/MAE over the shipped blend;
- coefficients are reasonably sign-stable across folds.

If those conditions fail, report the dead end instead of adding search complexity.
