# Search-Again Depth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a default-off shared `increaseDepth` decision and per-worker `searchAgainCounter` that slow nominal depth growth when the timed search should re-search at roughly the current effective depth.

**Architecture:** Worker 0 publishes a shared boolean after completed timed iterations. Each worker retains a local counter and derives an effective base depth from the nominal iterative-deepening depth. Existing aspiration fail-high reduction composes with this adjusted base depth.

**Tech Stack:** Rust 2024, `Arc<AtomicBool>`, existing lazy-SMP worker plumbing.

## Global Constraints

- `UseSearchAgainDepth` defaults to `false`.
- Fixed-depth, fixed-node, infinite, and MultiPV searches are unaffected.
- Pondering always permits depth growth.
- Existing aspiration fail-high depth reduction remains unchanged.
- The 40,705-node release bench signature must remain unchanged.
- Add no `SEARCH_PARAMETERS` entries.
- Do not commit from the shared dirty working tree.

---

### Task 1: Pure effective-depth model

**Files:**
- Modify: `crates/mf-search/src/search.rs`

**Interfaces:**
- Produces: a pure effective-depth helper from nominal depth and counter

- [ ] **Step 1: Add failing tests**

```rust
#[test]
fn search_again_counter_repeats_depth_before_advancing() {
    // nominal depths continue increasing, effective depth follows
    // max(1, nominal - 3*(counter+1)/4)
}

#[test]
fn effective_depth_never_falls_below_one() {}

#[test]
fn disabled_search_again_depth_returns_the_nominal_depth() {}
```

- [ ] **Step 2: Implement the helper**

Use:

```text
effective = max(1, nominal - 3 * (search_again_counter + 1) / 4)
```

Use saturating integer arithmetic.

- [ ] **Step 3: Run focused tests**

```powershell
cargo test -p mf-search search_again -- --nocapture
```

---

### Task 2: Thread the shared depth decision through SMP

**Files:**
- Modify: `crates/mf-search/src/search.rs`
- Modify: `crates/mf-search/src/thread_pool.rs`
- Modify: affected `mf-search` tests/call sites

**Interfaces:**
- Produces: `Arc<AtomicBool>` shared by workers for one search
- Produces: `WorkerParameters::with_increase_depth(...)`

- [ ] **Step 1: Add failing worker-plumbing test**

Prove worker 0 and a helper observe the same decision while keeping independent
`search_again_counter` values.

- [ ] **Step 2: Add the shared state**

- Initialize the shared decision to `true` for every search.
- Thread it through `SearchJob` and `WorkerParameters`, following existing ponder and
  node-counter patterns.
- Worker 0 is the only writer; all workers read with relaxed ordering.
- The state is search-local, not retained between `go` commands.

- [ ] **Step 3: Run SMP tests**

```powershell
cargo test -p mf-search --test smp
```

---

### Task 3: Integrate effective-depth progression

**Files:**
- Modify: `crates/mf-search/src/search.rs`
- Modify: `crates/mf-search/tests/search_invariants.rs`

**Interfaces:**
- Produces: `SearchOptions::use_search_again_depth: bool`

- [ ] **Step 1: Add the option with default false**

Activation requires:

```rust
options.use_search_again_depth
    && limits.use_clock_management
    && options.multi_pv == 1
```

- [ ] **Step 2: Add per-worker counter behavior**

At each nominal iteration:

- if the shared decision is `false`, increment the local counter;
- derive the effective base depth;
- pass that depth to the existing aspiration/root search;
- if disabled or in an excluded search mode, use nominal depth and do not increment.

Do not reset the counter mid-search.

- [ ] **Step 3: Publish the decision**

After worker 0 completes line 1:

- if pondering, publish `true`;
- otherwise calculate the current effective soft budget using whichever governor is
  active;
- publish `elapsed <= effective_soft_time / 2`.

The decision affects the next nominal iteration.

- [ ] **Step 4: Add behavior tests**

```rust
#[test]
fn search_again_depth_is_inert_for_fixed_depth_and_fixed_nodes() {}

#[test]
fn search_again_depth_is_inert_for_multipv() {}

#[test]
fn ponder_searches_always_allow_depth_growth() {}

#[test]
fn late_timed_iterations_repeat_effective_depth() {}
```

- [ ] **Step 5: Run search tests**

```powershell
cargo test -p mf-search
cargo test -p mf-search --test search_invariants
cargo test -p mf-search --test smp
```

---

### Task 4: Add the UCI toggle

**Files:**
- Modify: `crates/mf-uci/src/lib.rs`
- Modify: `crates/mf-uci/tests/bench_cli.rs`
- Modify: `crates/mf-uci/tests/uci_protocol.rs`

**Interfaces:**
- Consumes: `SearchOptions::use_search_again_depth`

- [ ] **Step 1: Advertise and parse**

Add:

```text
option name UseSearchAgainDepth type check default false
```

- [ ] **Step 2: Add option tests**

Cover default advertisement, case-insensitive parsing, persistence across
`ucinewgame`, and malformed values.

- [ ] **Step 3: Prove control identity**

Default, explicit off, and both Stage 6 toggles off must preserve:

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

Attribute any known unrelated formatting failures without modifying those files.

- [ ] **Step 2: Run all tests**

```powershell
cargo test --workspace
```

- [ ] **Step 3: Run bench**

```powershell
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: `Nodes searched: 40705`.

- [ ] **Step 4: Timed smoke**

In an exclusive UCI session, compare off/on arms with the same clock:

- enabled arm shows repeated effective depths late in the budget;
- hard limit remains authoritative;
- `ponderhit` rebases elapsed time correctly;
- MultiPV and fixed-depth outputs are unchanged.
