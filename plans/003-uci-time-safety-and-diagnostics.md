# Plan 003: Give `go nodes`/`go depth` clock safety, make `go movetime` honor Move Overhead, and diagnose invalid check-option values

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If
> anything in the "STOP conditions" section occurs, stop and report. When
> done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- crates/mf-uci/src/lib.rs`
> Written against commit `b9d15bf` **plus its uncommitted working tree**. Excerpt mismatch = STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW (touches only time-limit plumbing; determinism paths must be preserved)
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

## Why this matters

Three small UCI defects, one function apart:

1. **`go nodes N wtime X btime Y` runs with zero time safety** (prior review #19, still present): when `nodes` or `depth` is set, both clock bounds are discarded, so a large node budget on a slow node rate can flag the engine.
2. **`go movetime T` ignores Move Overhead**: the clock path subtracts the configured overhead before budgeting; movetime spends the full requested time plus I/O latency, exceeding the budget the sender asked for.
3. **Invalid check-option values fail silently** while invalid numeric values print `info string invalid ...`: a GUI or tuner speaking the numeric-bool dialect (`setoption name UseNMP value 1`) tunes nothing and hears nothing — violating the repo's own stated philosophy that "a silently ignored setoption leaves the tuner measuring a value the engine never adopted."

## Current state

- `crates/mf-uci/src/lib.rs` lines ~1618-1635 — `GoParameters::search_limits`, the single function where all three time behaviors live:

```rust
    fn search_limits(&self, position: &Position, move_overhead_millis: u64) -> SearchLimits {
        let (soft_time, hard_time, use_clock_management) =
            if self.infinite || self.depth.is_some() || self.nodes.is_some() {
                (None, None, false)
            } else if let Some(millis) = self.movetime {
                (
                    Some(Duration::from_millis(millis)),
                    Some(Duration::from_millis(millis)),
                    false,
                )
            } else {
                let (soft_time, hard_time) = self.clock_limits(position, move_overhead_millis);
                (soft_time, hard_time, soft_time.is_some())
            };
```

  (`clock_limits`, reached only in the third branch, is where `move_overhead_millis` is honored.)

- `crates/mf-uci/src/lib.rs` lines ~1305-1314 — `parse_check_option` returns `None` for anything but `true`/`false`; every `Use*` branch then silently drops the write. Contrast: numeric option branches print diagnostics, e.g. `info string invalid MultiPV value '{value}'` (~line 952).
- Determinism constraint: the `bench` subcommand and node-limited determinism tests rely on node/depth-limited runs being reproducible. They send **no clock tokens**, so deriving clock bounds *only when clock tokens are present* preserves them.
- Test exemplars: `movetime_search_limits_use_the_requested_duration` (`lib.rs` ~2661) pins current movetime behavior — it must be updated to the overhead-subtracted expectation; `tests/uci_protocol.rs` holds the integration-level `go` behavior tests.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Unit tests | `cargo test -p mf-uci --lib` | all pass |
| Integration | `cargo test -p mf-uci` | all pass |
| Gates | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Bench (must NOT move) | `cargo run --release -p mf-uci --bin manifold -- bench` | same node signature as before this plan |

## Scope

**In scope**:
- `crates/mf-uci/src/lib.rs`
- `crates/mf-uci/tests/uci_protocol.rs` (new assertions)

**Out of scope**:
- `crates/mf-search/` — `SearchLimits` semantics (soft vs hard, governor) stay as-is.
- The `go <garbage>` silent-ignore policy (documented, deliberate).
- Time-management tuning parameters.

## Git workflow

- One commit: `Give node-limited searches clock safety and movetime its overhead`.

## Steps

### Step 1: Clock bounds for node/depth-limited searches

In `search_limits`, when `self.depth.is_some() || self.nodes.is_some()` and **clock tokens are present** (`self.wtime.is_some() || self.btime.is_some()`), compute `clock_limits(position, move_overhead_millis)` and keep only its `hard_time` as a safety deadline. Leave `soft_time = None` and `use_clock_management = false` so the soft-limit governor stays out of node-limited searches (the node counter remains the primary stop). No clock tokens → exactly today's behavior.

**Verify**: new unit test — `GoParameters::parse(["nodes", "100000", "wtime", "2000", "btime", "2000"])` then `search_limits(...)` yields `hard_time = Some(...)`, `soft_time = None`; and `parse(["nodes", "100000"])` yields `hard_time = None`. `cargo test -p mf-uci --lib` → pass.

### Step 2: Movetime minus overhead

In the `movetime` branch, subtract the configured overhead before building both durations: `let budget = millis.saturating_sub(move_overhead_millis).max(1);`. Update `movetime_search_limits_use_the_requested_duration` to pass a nonzero overhead and assert the subtraction; add a companion test asserting `movetime 5` with overhead 10 clamps to 1 ms (never zero).

**Verify**: `cargo test -p mf-uci --lib` → pass including both updated/new tests.

### Step 3: Diagnose invalid check-option values

Where a `Use*` (check-type) branch receives a `value` token that `parse_check_option` rejects, print `info string invalid <Name> value '<v>' (expected true|false)` — mirror the MultiPV diagnostic's exact phrasing style (`lib.rs` ~952). The existing test at `lib.rs` ~2302-2310 pins today's silence for `UseRFP value banana`; update it to expect the diagnostic. Keep **absence of a value token** silent (some GUIs send bare `setoption name X`).

**Verify**: `cargo test -p mf-uci --lib` → pass; then `cargo test -p mf-uci` → all pass.

### Step 4: Confirm no strength or determinism change

**Verify**: bench signature unchanged from pre-plan value; `cargo test --workspace` → all pass.

## Test plan

- Unit: the three new/updated tests above, modeled on `movetime_search_limits_use_the_requested_duration`.
- Integration (`tests/uci_protocol.rs`): one test sending `go nodes 50000 wtime 1000 btime 1000 movestogo 40` asserting a `bestmove` arrives well under 1 s (a smoke check that the hard bound exists and does not fire early).
- Regression net: the full uci_protocol + analysis_stop suites.

## Done criteria

- [ ] All gates exit 0; `cargo test -p mf-uci` green
- [ ] `go nodes` + clock yields a hard deadline; without clock, behavior identical to before
- [ ] `go movetime` respects Move Overhead, clamped at 1 ms
- [ ] Invalid check values print `info string invalid ...`; bare setoption stays silent
- [ ] Bench node signature unchanged

## STOP conditions

- Making the node-limited hard bound fire requires touching `SearchLimits` consumers in mf-search (out of scope) — report instead.
- Any determinism test (`fixed_depth_output_is_identical_at_every_thread_count` or bench anchors) breaks — diagnose before proceeding; do not relax the test.

## Maintenance notes

- Plan 015-class future work on interpolated time management must keep the "node limit primary, clock only a safety net" split.
- Reviewers: the only acceptable behavioral deltas are the three described; anything else moving (node counts!) means a gate leaked into a decision path — investigate.
