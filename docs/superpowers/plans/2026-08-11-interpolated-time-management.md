# Interpolated Time Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a default-off continuous between-iteration time governor that jointly uses score trend, best-move stability, root instability, and root effort.

**Architecture:** Keep the existing UCI soft/hard allocator unchanged. For timed, single-PV searches only, compute a multiplicative scale after line 1 completes and apply it through the existing `SearchContext::scaled_soft_time()` seam. Preserve the old additive governor whenever the new toggle is disabled.

**Tech Stack:** Rust 2024, standard library atomics, existing `mf-search`/`mf-uci` test harnesses.

## Global Constraints

- `UseInterpolatedTimeManagement` defaults to `false`.
- MultiPV, fixed-depth, fixed-node, movetime-equals-hard-limit behavior, and infinite searches must remain unchanged.
- The 40,705-node release bench signature must remain unchanged.
- Add no `SEARCH_PARAMETERS` entries; the table is already 39 entries.
- No external dependencies or hot-path allocations.
- Do not commit from the shared dirty working tree.

---

### Task 1: Pure interpolation and factor model

**Files:**
- Modify: `crates/mf-search/src/search.rs`

**Interfaces:**
- Produces: `interpolate(x, x0, x1, y0, y1) -> f64`
- Produces: a pure factor function returning a bounded soft-time scale from iteration statistics

- [ ] **Step 1: Add failing unit tests**

Cover:

```rust
#[test]
fn interpolation_reaches_both_anchor_values() { /* exact anchors */ }

#[test]
fn interpolated_time_factors_are_clamped_to_reference_ranges() { /* extremes */ }

#[test]
fn falling_scores_receive_more_time_than_rising_scores() { /* monotonic */ }

#[test]
fn stable_best_moves_receive_less_time_than_recent_changes() { /* monotonic */ }

#[test]
fn concentrated_root_effort_receives_less_time() { /* 75_800..104_510 anchors */ }
```

- [ ] **Step 2: Run focused tests and confirm failure**

Run:

```powershell
cargo test -p mf-search interpolation_ -- --nocapture
```

- [ ] **Step 3: Implement the pure model**

Use the reference anchors:

```text
falling_eval =
  clamp((11.48 + 2.30*(previous_average-best) + 1.1*(older-best))/100,
        0.576, 1.728)

time_reduction =
  clamp(interpolate(depth_since_change, 4.96, 18.79, 0.639, 1.712),
        0.629, 1.544)

stability = 1 / time_reduction

instability =
  1.077 + 2.229 * best_move_changes / worker_count

root_effort =
  clamp(interpolate(nodes_effort, 75_800, 104_510, 0.969, 0.714),
        0.693, 0.838)
```

Compose the factors in `f64`, convert once to a bounded percentage for the existing
soft-time scaling seam, and cap the effective soft limit at the hard limit.

- [ ] **Step 4: Run focused tests**

Run:

```powershell
cargo test -p mf-search interpolation_ time_factor -- --nocapture
```

---

### Task 2: Collect cumulative root statistics

**Files:**
- Modify: `crates/mf-search/src/search.rs`

**Interfaces:**
- Consumes: pure factor model from Task 1
- Produces: per-iteration cumulative root effort and best-move-change statistics

- [ ] **Step 1: Add failing tests**

Add tests around a small statistics helper:

```rust
#[test]
fn aspiration_researches_accumulate_root_effort_when_interpolated_tm_is_enabled() {}

#[test]
fn legacy_mode_resets_root_effort_for_each_root_search() {}

#[test]
fn root_best_move_replacements_increment_instability_only_for_line_one() {}
```

- [ ] **Step 2: Implement minimal collection**

- Preserve the existing effort reset when the toggle is off.
- When enabled, reset cumulative effort once at the start of the nominal iteration,
  not for every aspiration re-search.
- Count line-1 root best-move replacements during the iteration.
- Do not count MultiPV secondary lines.
- Keep worker-local state; worker 0 is the only reporter.

- [ ] **Step 3: Run focused tests**

```powershell
cargo test -p mf-search root_effort root_best_move -- --nocapture
```

---

### Task 3: Integrate the default-off governor

**Files:**
- Modify: `crates/mf-search/src/search.rs`
- Modify: `crates/mf-search/tests/search_invariants.rs`

**Interfaces:**
- Produces: `SearchOptions::use_interpolated_time_management: bool`

- [ ] **Step 1: Add the option field with default false**

The new path is active only when:

```rust
options.use_interpolated_time_management
    && limits.use_clock_management
    && options.multi_pv == 1
```

Add `SearchLimits::use_clock_management: bool`. Normal clock controls set it to true;
`movetime`, depth, nodes, and infinite searches set it to false.

- [ ] **Step 2: Integrate after the primary line completes**

- Maintain enough score history for the factor inputs.
- Track the depth at which the best root move last changed, and feed
  `current_depth - last_change_depth` into the stability interpolation.
- Compute the new scale after each completed line 1.
- Recompute the soft-time-reached decision immediately using the new scale before the
  between-iteration stop check.
- Leave hard-limit checks unchanged.
- When disabled, execute the existing stability/score/optional-effort code exactly.

- [ ] **Step 3: Add identity and activation tests**

```rust
#[test]
fn interpolated_tm_is_inert_for_fixed_depth_and_fixed_nodes() {}

#[test]
fn interpolated_tm_is_inert_for_multipv() {}

#[test]
fn interpolated_tm_changes_a_timed_search_scale_when_enabled() {}
```

- [ ] **Step 4: Run search tests**

```powershell
cargo test -p mf-search --test search_invariants
cargo test -p mf-search
```

---

### Task 4: Add the UCI toggle

**Files:**
- Modify: `crates/mf-uci/src/lib.rs`
- Modify: `crates/mf-uci/tests/bench_cli.rs`
- Modify: `crates/mf-uci/tests/uci_protocol.rs`

**Interfaces:**
- Consumes: `SearchOptions::use_interpolated_time_management`

- [ ] **Step 1: Advertise and parse**

Add:

```text
option name UseInterpolatedTimeManagement type check default false
```

Use the existing `UseTimeEffort` check-option pattern.

- [ ] **Step 2: Add protocol/unit coverage**

Test default advertisement, mixed-case parsing, state persistence through
`ucinewgame`, and malformed check values.

- [ ] **Step 3: Prove control identity**

Default and explicit `false` must both report:

```text
Nodes searched: 40705
```

Run:

```powershell
cargo test -p mf-uci --test bench_cli
cargo test -p mf-uci --test uci_protocol
```

---

### Task 5: Full verification

**Files:**
- No new files

- [ ] **Step 1: Run formatting and lint**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

If full formatting still fails solely on the unrelated dirty files
`crates/mf-core/src/see.rs` and `crates/mf-search/examples/see_profile.rs`, run targeted
rustfmt checks on every touched file and `git diff --check`; do not modify those files.

- [ ] **Step 2: Run all tests**

```powershell
cargo test --workspace
```

- [ ] **Step 3: Run the release bench**

```powershell
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: `Nodes searched: 40705`.

- [ ] **Step 4: Run an exclusive timed smoke**

Compare default-off and enabled searches on the same clock command. Confirm:

- enabled path changes reported time/depth behavior;
- neither arm exceeds the hard limit;
- no forfeits/crashes;
- MultiPV=2 remains on the legacy time path.
