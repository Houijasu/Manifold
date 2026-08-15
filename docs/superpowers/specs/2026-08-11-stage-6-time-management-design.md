# Stage 6 Time Management Design

## Goal

Complete the remaining Stage 6 search work without changing shipped playing behavior
before match testing. Add two independent, default-off systems:

1. a continuous between-iteration time governor;
2. `increaseDepth` / `searchAgainCounter`.

The existing UCI clock allocator remains the control. Fixed-depth, fixed-node,
infinite, and MultiPV searches are unaffected.

## Decision

### Continuous governor

Add `UseInterpolatedTimeManagement` (default `false`). When enabled for a timed,
single-PV search, replace the current additive percentage governor with a
Stockfish-style multiplicative budget:

```text
effective soft time =
    base soft time
    × falling-eval factor
    × best-move-stability factor
    × best-move-instability factor
    × root-effort factor
```

The factors use the reference interpolation anchors and clamps:

- falling eval: score trend, clamped to `[0.576, 1.728]`;
- stability: compute the reference `timeReduction` from depth since the root best
  move last changed, clamp it to `[0.629, 1.544]`, then multiply time by its
  reciprocal. This preserves the reference denominator semantics: a long-stable best
  move receives less time;
- instability: intra-iteration root-best-move changes;
- root effort: fraction of nodes spent on the current best root move, clamped to
  `[0.693, 0.838]`.

Manifold will not add Stockfish's cross-move `previousTimeReduction` or
`originalTimeAdjust` state in this iteration. Those require mutable state crossing the
detached UCI search-thread boundary and alter the start-of-move allocator. The current
base soft/hard budgets stay intact, and the new governor only scales the soft budget.
This keeps the feature isolated and directly comparable to the existing governor.

Root effort becomes cumulative across aspiration re-searches while the feature is
enabled. The default-off path preserves today's reset behavior exactly.

### Search-again depth

Add `UseSearchAgainDepth` (default `false`). For timed, single-PV searches:

- worker 0 publishes a shared `increase_depth` decision after each completed
  iteration;
- pondering always permits depth increase;
- otherwise depth increases normally only while elapsed time is at most half of the
  current effective soft budget;
- each worker keeps a `search_again_counter`;
- when `increase_depth` is false, the nominal iteration still increments but effective
  depth is reduced by `3 * (counter + 1) / 4`, floored at one ply.

The existing aspiration fail-high depth reduction remains unchanged and composes with
the adjusted base depth. Fixed-depth/node/infinite/MultiPV controls bypass this system.

## State and boundaries

- `SearchLimits` gains an explicit clock-management discriminator. Normal clock
  controls set it; `movetime`, depth, nodes, and infinite searches do not. Equal
  soft/hard durations are not used as an implicit mode test.
- New toggles live in `SearchOptions` and are advertised as UCI check options.
- New numeric constants are plain Rust constants. `SEARCH_PARAMETERS` is already
  39 entries and must not be expanded.
- The shared depth decision follows existing `Arc<Atomic*>` pool state patterns.
- No allocations occur inside search hot paths.
- Ponder clock rebasing remains authoritative for elapsed time.

## Verification

- Defaults and explicit off/off reproduce the 40,705-node bench signature.
- Fixed-depth and fixed-node searches are node-for-node identical across toggle arms.
- Pure tests cover interpolation anchors, clamps, monotonicity, multiplier composition,
  and effective-depth progression.
- Timed integration tests prove both features activate, respect the hard limit, and do
  not act in MultiPV or non-time-managed searches.
- Full workspace tests and clippy pass.

## Deferred measurement

Both features ship default-off. Later match testing must evaluate them independently
and together. Single-thread testing uses affinity and concurrency 8; multi-thread
testing uses no affinity and concurrency 1. No Elo claim is made during implementation.

## Non-goals

- Replacing the UCI start-of-move soft/hard allocator.
- Cross-move `originalTimeAdjust` or `previousTimeReduction`.
- Ponder bonus tuning.
- MultiPV-aware time allocation.
- New SPSA parameters.
