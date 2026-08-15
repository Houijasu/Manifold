# Stage 6 flaky timed-activation test fix

## Failure

`interpolated_tm_changes_a_timed_search_scale_when_enabled` compared the final
`(depth, nodes)` of enabled and disabled timed searches over soft budgets of 50, 100,
200, 400, and 800 ms. A fresh full-workspace run failed because every pair stopped on
the same completed-iteration boundary.

## Diagnosis

The production path was active. Temporary diagnostics at the point where the search
sets its next soft-time scale observed deterministic differences from the first
completed iteration:

```text
depth                 1    2    3    4    5    6
interpolated scale  180  173   83  180  180   83
legacy scale        110  105  100  110  110  105
```

The data flow is:

1. an iteration completes;
2. the selected governor computes and stores `time_scale_percent`;
3. the interpolated path immediately recomputes whether the scaled soft limit was
   reached;
4. the search publishes the completed iteration;
5. the between-iteration stop check decides whether another iteration begins.

The scale is continuous, but the old assertion observed only step 5 through final
depth/node counts. Two different soft budgets can therefore select different scales and
still land on the same completed depth.

### Reproduction

- Exact isolated test: **20/20 passed**.
- One isolated diagnostic run passed and showed the scale sequences above.
- Full `search_invariants` target, three consecutive runs: **2 passed, 1 failed**.
- A concurrent full-workspace run passed, confirming the failure is intermittent rather
  than feature-configuration-specific.
- Additional 32-process and 96-process CPU-contention runs passed but stretched
  iteration times substantially; the same-target parallel run was the reliable
  reproduction because it also creates the suite's realistic NNUE, TT, and shared-history
  memory pressure.

The reproduced failing run reported:

```text
soft  enabled depth/nodes/elapsed   disabled depth/nodes/elapsed
  50  6 / 3,866 / 79 ms            6 / 3,866 / 85 ms
 100  7 / 9,920 / 207 ms           7 / 9,920 / 187 ms
 200  9 / 20,810 / 395 ms          9 / 20,810 / 306 ms
 400 10 / 51,429 / 786 ms         10 / 51,429 / 739 ms
 800 11 / 96,783 / 1,537 ms       11 / 96,783 / 1,266 ms
```

Every enabled search used interpolated scales, but scheduler and memory contention made
all five final depth/node pairs equal.

Stable timing tests in the repository avoid this mistake:

- hard-limit and movetime tests use broad safety bounds rather than exact completed
  depths;
- MultiPV timing controls balance arm order and compare aggregate elapsed time;
- activation predicates and transition ordering are pinned by deterministic unit tests.

## Root cause

The flaky test treated a quantized, scheduler-dependent consequence of the governor as
proof that the governor ran. Under full-suite CPU and memory contention, independent
enabled and disabled searches often completed the same last iteration even though their
between-iteration soft scales differed. Production logic was correct; the test's
wall-clock proxy was not.

## Fix

`IterationInfo` now publishes the `time_scale_percent` selected after that completed
iteration. This extends the existing production iteration callback instead of adding a
test-only global hook.

The integration test now:

1. runs a real clock-managed search with real soft and hard limits;
2. observes the production iteration callback;
3. stops after the first completed iteration so later wall-clock scheduling cannot
   affect the assertion;
4. asserts the enabled path reports scale `180` and the disabled path reports `110`;
5. asserts each timed search still returns a move.

This fails if the toggle, clock-management guard, scale computation, context storage, or
callback publication stops reaching production logic. It does not depend on which later
iteration happens to cross a short wall-clock budget.

The test was written first and failed with the expected missing
`IterationInfo::time_scale_percent` field before the production field was added.

## Verification

- Changed test repeated: **50/50 passed**.
- `cargo test -p mf-search --test search_invariants`: **37 passed**.
- `cargo test --workspace`: **passed**.
- `cargo clippy --workspace --all-targets -- -D warnings`: **passed**.
- Release bench: **40,705 nodes**.

Release bench output:

```text
Positions: 6
Nodes searched: 40705
Time (ms): 59
NPS: 681533
```
