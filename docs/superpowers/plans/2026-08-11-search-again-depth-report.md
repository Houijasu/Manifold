# Search-Again Depth Implementation Report

## Outcome

Implemented the complete search-again depth plan in the existing dirty workspace without
reverting, cleaning, or committing any dependent work.

## Implementation

### Search model and activation

- Added `SearchOptions::use_search_again_depth`.
- The option defaults to `false`.
- Activation is strictly:

  ```rust
  options.use_search_again_depth
      && limits.use_clock_management
      && options.multi_pv == 1
  ```

- No `SEARCH_PARAMETERS` entry was added.
- Added the saturating effective-depth model:

  ```text
  max(1, nominal_depth - 3 * (search_again_counter + 1) / 4)
  ```

### Search-local shared state

- Every search creates a fresh `Arc<AtomicBool>` initialized to `true`.
- `SearchJob` carries the same `Arc<AtomicBool>` to every participating SMP worker.
- `WorkerParameters::with_increase_depth(...)` exposes the shared decision to the
  worker.
- Workers use relaxed atomic loads and stores.
- Worker 0 is the only writer.
- Each worker starts with an independent `search_again_counter` of zero.
- A worker increments its local counter only when the feature is active and the shared
  decision is false.
- The counter is never reset during the search.

### Iterative deepening

- Nominal iterative depth continues advancing normally.
- The calculated effective depth is passed into the existing root/aspiration search.
- Existing aspiration fail-high reductions were not changed and therefore continue to
  compose with the adjusted base depth.
- Fixed-depth, fixed-node, infinite, movetime, and MultiPV searches remain outside the
  activation gate.

### Worker-0 publication

After worker 0 completes line 1, it publishes the decision for the next nominal
iteration:

```text
pondering || elapsed <= current_effective_soft_time / 2
```

The current effective soft time is read through the existing `scaled_soft_time()` path,
so the decision consumes whichever legacy or interpolated governor is active without
modifying either governor. Ponder elapsed time continues to use the existing
`ponderhit` rebase.

### UCI

- Advertised:

  ```text
  option name UseSearchAgainDepth type check default false
  ```

- Parsing is case-insensitive.
- Valid values persist across `ucinewgame`.
- Malformed values preserve the previous setting.

## Files changed for this feature

- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\src\search.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\src\thread_pool.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\tests\search_invariants.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-uci\src\lib.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-uci\tests\bench_cli.rs`
- `C:\Users\Samaritan\Projects\Manifold\crates\mf-uci\tests\uci_protocol.rs`

## Test coverage added

- Pure effective-depth progression and floor.
- Disabled-path nominal-depth identity.
- Actual two-worker `SearchPool`/`SearchJob` dispatch observing the same shared atomic
  decision while each dispatched worker advances an independently seeded local counter.
- Immediate transition ordering: a false decision increments the local counter before
  the same nominal iteration computes its effective depth.
- Strict timed/single-PV activation gate.
- Ponder always permitting depth growth.
- Half-soft-budget decision boundary.
- Fixed-depth and fixed-node node-for-node identity.
- MultiPV inertness across toggle arms.
- Live timed activation showing repeated effective depths.
- Default UCI advertisement.
- Case-insensitive UCI parsing.
- Persistence across `ucinewgame`.
- Malformed-value preservation.
- Movetime inertness.
- Default, explicit-off, and both-Stage-6-off bench identity.

The tests were developed red-green. Focused failures were observed for the missing
effective-depth helper, actual dispatched-worker observation seam, transition helper,
`SearchOptions` field, depth-decision helper, UCI advertisement, and UCI parser before
their implementations were added.

## Validation

### Focused search and protocol validation

- `cargo test -p mf-search search_again -- --nocapture` — passed.
- `cargo test -p mf-search --test search_invariants` — 36 passed.
- `cargo test -p mf-search --test smp` — 11 passed.
- `cargo test -p mf-uci --test uci_protocol` — 67 passed.
- `cargo test -p mf-uci --test bench_cli` — 29 passed.

An initial full-workspace run exposed a timing-sensitive exact-depth comparison in the
new MultiPV test and an unrelated interpolated-TM timing observation under load. The new
MultiPV test was changed to the repository's existing balanced-arm elapsed comparison.
The focused invariant target and the complete workspace were then rerun successfully.

### Full workspace

- `cargo test --workspace` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.

### Formatting

- Targeted `rustfmt --check` for all six touched Rust files — passed.
- Targeted `git diff --check` for all six touched Rust files — passed.
- `cargo fmt --all -- --check` — failed only on the pre-existing unrelated formatting
  differences in:
  - `C:\Users\Samaritan\Projects\Manifold\crates\mf-core\src\see.rs`
  - `C:\Users\Samaritan\Projects\Manifold\crates\mf-search\examples\see_profile.rs`

Those unrelated files were not modified for this task.

### Release bench

Command:

```powershell
cargo run --release -p mf-uci --bin manifold -- bench
```

Result:

```text
Positions: 6
Nodes searched: 40705
Time (ms): 57
NPS: 708861
```

The required 40,705-node release signature is unchanged.

### Exclusive timed smoke

An exclusive release UCI session validated enabled/disabled clock searches, fixed
depth, movetime, MultiPV, ponder, and `ponderhit`.

Completed timed depths:

```text
off: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
on:  [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 11, 11, 11]
```

Additional smoke observations:

- Enabled timed search completed in 215 ms within its hard bound.
- Fixed-depth enabled/off arms both reported depths 1 through 7.
- Enabled `go movetime 80` reported 80 ms.
- Enabled MultiPV completed within its clock bound and emitted line 2.
- Ponder followed by `ponderhit` returned normally; the existing protocol integration
  test also passed its clock-rebase assertion.

## Follow-up verification fixes

The original worker-sharing unit test manually constructed two `WorkerParameters`.
That test did not prove the `SearchPool` → `SearchJob` → `worker_loop` path cloned and
attached the shared `Arc<AtomicBool>` to helpers. It was replaced with a deterministic
actual-dispatch test.

### Actual dispatch seam

- Added a test-only, pool-local control guarded by a `Mutex`; no global state is used.
- The control sets the search-local atomic's initial decision, independently seeds each
  worker's local counter, and supplies an observation channel.
- A real two-worker `SearchPool` dispatches real `SearchJob`s through both worker
  threads.
- Each worker reports its loaded decision, shared atomic address, and counter after its
  first transition.
- The test proves:
  - worker 0 and helper both read `false`;
  - both observe the same atomic address;
  - independently seeded counters `0` and `4` transition independently to `1` and `5`.
- The assertion fails if a helper stops receiving the job's shared atomic, receives a
  separately allocated atomic, or shares counter state with worker 0.

### Transition ordering

Extracted `search_again_iteration(...)`, which is now used by the production iterative
deepening loop. It performs the transition in one pure operation:

1. read the already-loaded shared decision;
2. increment the local counter when active and false;
3. immediately compute the effective depth from the incremented counter.

The deterministic regression assertion pins:

```text
nominal=8, counter=0, active=true, increase_depth=false
    -> counter=1, effective_depth=7
```

It fails if the increment moves after depth calculation or is deferred by one nominal
iteration.

### Follow-up validation

- Both new deterministic tests passed 20/20 consecutive repetitions.
- `cargo test -p mf-search --test smp` — 11 passed.
- `cargo test -p mf-search --test search_invariants` — 36 passed.
- `cargo test --workspace` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo run --release -p mf-uci --bin manifold -- bench` — `Nodes searched: 40705`.
- Production search behavior was unchanged except for routing the existing transition
  through the pure helper.

## Optional-network test fix

The dispatched-worker regression test no longer unconditionally loads the gitignored
`nets/main.nnue`.

- It resolves `MF_NNUE_TEST_NET` first.
- Without that override, it falls back to `nets/main.nnue` relative to
  `CARGO_MANIFEST_DIR`.
- If the resolved path is not a file, it emits the repository's standard `SKIPPED:`
  diagnostic and returns before constructing the pool.
- When a network is available, the complete dispatch path and all original assertions
  still execute unchanged.

Validation:

- With the available local network, the actual two-worker dispatch test passed.
- With `MF_NNUE_TEST_NET` set to a deliberately nonexistent path, the test emitted its
  skip diagnostic and passed without accessing the local fallback.
- `cargo test -p mf-search` — passed: 134 unit tests passed, 1 ignored; all integration
  and doc-test targets passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo run --release -p mf-uci --bin manifold -- bench` — `Nodes searched: 40705`.

## Concerns

- Full-workspace formatting remains blocked by the two unrelated pre-existing files
  listed above.
- The workspace contains extensive pre-existing dirty and untracked dependent work.
  This implementation did not clean, revert, or commit any of it.
