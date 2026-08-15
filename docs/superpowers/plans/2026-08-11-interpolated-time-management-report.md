# Interpolated Time Management Implementation Report

## Outcome

Implemented the default-off interpolated between-iteration governor described by
`2026-08-11-interpolated-time-management.md` and
`2026-08-11-stage-6-time-management-design.md`.

The existing allocator and legacy additive governor remain the control when the new
option is disabled. Fixed-depth, fixed-node, infinite, movetime, and MultiPV searches
remain outside the new governor. No `searchAgainCounter`/`increaseDepth` work and no
new `SEARCH_PARAMETERS` entries were added.

## Files changed

Production and test changes:

- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\src\search.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\src\thread_pool.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\tests\search_invariants.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\tests\smp.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-uci\src\lib.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-uci\tests\bench_cli.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-uci\tests\uci_protocol.rs`

Report:

- `C:\Users\Samaritan\Projects\Manifold\docs\superpowers\plans\2026-08-11-interpolated-time-management-report.md`

No commit was created.

## Formulas implemented

The pure linear interpolation helper is:

```text
interpolate(x, x0, x1, y0, y1) =
    y0 + (x - x0) * (y1 - y0) / (x1 - x0)
```

The new governor composes these `f64` factors:

```text
falling_eval =
  clamp((11.48
         + 2.30 * (previous_average - best)
         + 1.1 * (older - best)) / 100,
        0.576, 1.728)

time_reduction =
  clamp(interpolate(current_depth - last_best_move_change_depth,
                    4.96, 18.79, 0.639, 1.712),
        0.629, 1.544)

stability = 1 / time_reduction

instability =
  1.077 + 2.229 * best_move_changes / max(worker_count, 1)

root_effort =
  clamp(interpolate(nodes_effort,
                    75_800, 104_510, 0.969, 0.714),
        0.693, 0.838)

scale_percent =
  round(100 * falling_eval * stability * instability * root_effort)
```

`scale_percent` is converted once, clamped to `[1, 180]`, applied through the existing
`SearchContext::scaled_soft_time()` seam, and the effective soft limit is capped at the
unchanged hard limit.

`last_best_move_change_depth` is the depth at which the root best move most recently
changed. The interpolation input is the stable duration
`current_depth - last_best_move_change_depth`. The reference `timeReduction` is
preserved and time is multiplied by its reciprocal, so each additional stable interval
progressively lowers the soft-time scale.

## State and collection added

`SearchOptions` now contains:

```rust
pub use_interpolated_time_management: bool
```

Its default is `false`.

`SearchLimits` now contains:

```rust
pub use_clock_management: bool
```

Normal `wtime`/`btime` allocation sets it to `true`. `movetime`, fixed depth, fixed
nodes, infinite analysis, and helper workers set it to `false`. This explicit mode
discriminator replaces inference from the presence or equality of soft/hard durations,
so exact `movetime` cannot enter the interpolated governor.

Worker-local `RootTimeStatistics` contains:

- a flat `(Move, u64)` cumulative root-effort vector;
- cumulative root-effort total;
- current line-1 root best move;
- line-1 root-best-move replacement count.

When the new governor is active, root effort and root-best-move instability reset once
at the start of the nominal iteration and accumulate across aspiration re-searches.
When inactive, root effort retains the old reset-at-every-root-search behavior.
Secondary MultiPV lines do not increment instability.

The iterative-deepening worker also maintains:

- previous averaged score;
- a four-entry older-score ring;
- score-ring index and initialization state;
- depth of the latest completed-iteration best-move change.

The new path is gated by:

```text
UseInterpolatedTimeManagement
&& SearchLimits::use_clock_management
&& MultiPV == 1
```

Immediately after each completed primary line, the scale is updated and
`soft_time_reached` is recomputed before the between-iteration stop decision. The
recomputation explicitly remains false while pondering. The covering regression starts
the search clock 200 ms before `ponderhit` with a nonzero 100 ms soft limit, proves the
pre-hit interval is not charged immediately after the hit, and then proves a rebased
150 ms elapsed interval does reach the limit.

## Focused-review fix round

All five focused-review findings were implemented:

1. Added and threaded `SearchLimits::use_clock_management`; the UCI allocator sets it
   only for normal side-to-move clock controls, and both worker-limit stripping paths
   clear it for helpers.
2. Replaced absolute change depth with depth since change, preserved the reference
   interpolation/clamp as `time_reduction`, and multiplied time by
   `1 / time_reduction`.
3. Replaced the fixed-depth MultiPV control with balanced, genuinely timed MultiPV=2
   samples. A separate exact activation-predicate test directly fails if either the
   clock-management or single-PV guard is removed.
4. Replaced the zero-limit ponder test with the nonzero rebasing scenario described
   above.
5. Added both UCI limit-mode unit coverage and an exclusive protocol regression showing
   `UseInterpolatedTimeManagement=true` does not shorten `go movetime 80`.

## UCI option

Advertised:

```text
option name UseInterpolatedTimeManagement type check default false
```

The option name and boolean values are parsed case-insensitively. Malformed check
values preserve the current state, and `ucinewgame` preserves the option.

## TDD and targeted gates

The new tests were introduced red-first and observed failing for missing functions,
missing state, missing option wiring, or the ponder-rebase edge before implementation.

Focused tests covered:

- interpolation anchors;
- all reference clamps;
- falling/rising score monotonicity;
- stable/recent best-move behavior;
- progressive stable-duration time reduction;
- concentrated root-effort behavior;
- bounded one-time factor conversion;
- cumulative aspiration root effort;
- legacy root-effort reset behavior;
- line-1-only instability;
- fixed-depth/fixed-node identity;
- balanced timed MultiPV=2 legacy-path behavior;
- timed activation;
- exact activation gating on clock management and single PV;
- nonzero-limit ponder elapsed rebasing;
- normal-clock versus movetime/depth/nodes/infinite mode discrimination;
- exact movetime preservation with the interpolated toggle enabled;
- UCI default, mixed-case parsing, malformed values, and `ucinewgame` persistence;
- default/explicit-false/explicit-true bench identity.

Targeted gate results:

- `cargo test -p mf-search --test search_invariants`: **33 passed, 0 failed**.
- `cargo test -p mf-search`: **190 passed, 0 failed, 1 ignored**.
- `cargo test -p mf-uci --test bench_cli`: **28 passed, 0 failed**.
- `cargo test -p mf-uci --test uci_protocol`: **67 passed, 0 failed**.

The plan's two-filter command
`cargo test -p mf-search interpolation_ time_factor -- --nocapture` is not valid Cargo
test syntax, so the `interpolation_` and `time_factor` filters were run separately.

## Full verification

- `cargo test --workspace`: **678 passed, 0 failed, 3 ignored** across 67 test/doc-test
  result groups.
- `cargo clippy --workspace --all-targets -- -D warnings`: **passed**.
- Targeted `rustfmt --check` for all seven touched Rust files: **passed**.
- `git diff --check`: **passed**; only existing CRLF conversion warnings were printed.
- `cargo fmt --all -- --check`: **failed only on unrelated existing formatting in**
  `crates/mf-core/src/see.rs` and `crates/mf-search/examples/see_profile.rs`.
  Those files were not edited.

## Bench identity

Release baseline before edits:

```text
Positions: 6
Nodes searched: 40705
Time (ms): 75
NPS: 535878
```

Initial implementation release bench:

```text
Positions: 6
Nodes searched: 40705
Time (ms): 66
NPS: 610537
```

Final release bench after the focused-review fix round:

```text
Positions: 6
Nodes searched: 40705
Time (ms): 64
NPS: 631855
```

The required deterministic signature remained **40,705 nodes**. The UCI bench test
also proved default, explicit `false`, and explicit `true` all remain at 40,705 nodes,
because bench is fixed-depth and bypasses the governor.

## Exclusive timed smoke

The release engine was run serially with `Threads=1` and no concurrent test process:

```text
go wtime 3000 btime 3000 winc 0 binc 0 movestogo 20
```

That command allocates a 146 ms base soft limit and a 584 ms hard limit.

Observed after the focused-review fix round:

```text
legacy clock       enabled=false MultiPV=1 depth=12 reported=323 ms wall=328 ms
interpolated clock enabled=true  MultiPV=1 depth=11 reported=141 ms wall=147 ms
interpolated move  enabled=true  MultiPV=1 go movetime 80        wall=85 ms
MultiPV clock      enabled=true  MultiPV=2 depth=11 reported=138 ms wall=143 ms
```

The enabled single-PV clock path changed time/depth behavior. All clock searches stayed
well inside the 584 ms hard limit and completed without a crash or forfeit. Exact
`movetime 80` remained approximately 80 ms with the toggle enabled instead of taking a
scaled soft stop. MultiPV remained on the legacy governor by both the exact activation
predicate and the balanced timed MultiPV regression.

## Deviations and concerns

- Full workspace formatting remains blocked solely by the two pre-existing unrelated
  files named above; all touched files are formatted and whitespace-clean.
- The timed activation integration test sweeps five bounded soft budgets rather than
  relying on one exact duration, because two different soft scales can legitimately
  land on the same completed-iteration boundary at a single sampled budget.
- The genuinely timed MultiPV control uses balanced toggle order and aggregate elapsed
  tolerance because between-iteration stopping is intentionally quantized by completed
  depth; the exact predicate unit test is the deterministic guard-removal detector.
- No other deviations or implementation concerns remain.
