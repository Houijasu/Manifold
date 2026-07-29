# Lazy SMP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement persistent, lock-free-hot-path Lazy SMP search with real `Threads` support, worker-0 UCI reporting, root voting, and measurable 1/2/4/8-thread scaling.

**Architecture:** `mf-search` gains worker-aware search parameters, a persistent standard-library worker pool, and a pure root-voting module. Workers own all mutable search state and share only the lockless transposition table, stop flag, and relaxed node counters. `mf-uci` keeps its existing per-`go` driver thread and delegates the actual search to the pool.

**Tech Stack:** Rust 2024, `std::thread`, `std::sync::mpsc`, relaxed atomics, existing `mf-core`, `mf-search`, and `mf-uci` APIs.

## Global Constraints

- Keep `Threads=1` search and `bench` node counts deterministic.
- Only worker 0 may emit iteration callbacks or inspect time limits.
- Search workers must never hold a mutex or channel lock while searching.
- Use no new third-party dependencies and add no new `unsafe`.
- Helpers use the shared stop flag; worker 0 terminates finite time-limited searches.
- Node limits are exact at `Threads=1` and may overshoot by the publication interval at `Threads>1`.
- `go infinite` ignores depth and node limits until `stop`.
- Public UCI `go depth N` must remain deterministic by dispatching only worker 0; helper workers stay parked and cannot perturb the shared TT.
- Fixed-depth SMP is available only through a separate explicitly named internal/`mtbench` path.
- Exact node-count assertions, `bench`, and deterministic node-budget tests must explicitly use `Threads=1`.
- Do not modify `research/_src/`.
- Leave the user-owned untracked `AGENTS.md` untouched and untracked.
- Do not touch the untracked `docs/reviews/` directory.
- Before every commit, run `git status`, review `git diff --cached`, and scan staged changes for secrets.

---

## File Structure

- Create `crates/mf-search/src/history.rs`: per-worker killer and quiet-history ownership.
- Create `crates/mf-search/src/vote.rs`: pure best-worker selection.
- Create `crates/mf-search/src/thread_pool.rs`: persistent workers, job dispatch, progress/result collection, parallel TT clear.
- Modify `crates/mf-search/src/search.rs`: worker parameters, generation, node publication, clock cadence.
- Modify `crates/mf-search/src/lib.rs`: internal module registration and public `SearchPool` export.
- Modify `crates/mf-uci/src/lib.rs`: pool lifecycle, `Threads` resizing, SMP search dispatch, selected-result reporting.
- Modify `crates/mf-uci/src/main.rs`: add `mtbench` subcommand.
- Create `crates/mf-search/tests/smp.rs`: pool stress, node accounting, stop, and legal-PV tests.
- Modify `crates/mf-search/tests/search_invariants.rs`: worker-0 and generation regression coverage.
- Modify `crates/mf-uci/tests/uci_protocol.rs`: live multi-thread UCI assertions.
- Create `crates/mf-uci/tests/mtbench_cli.rs`: scaling harness output contract.
- Create `docs/superpowers/specs/2026-07-29-m5-lazy-smp-design.md`: persist the approved design.
- Create `experiments/M5-smp/run-metadata.txt`: exact validation commands after implementation.

---

### Task 1: Persist the Approved Design and Capture Baselines

**Files:**
- Create: `docs/superpowers/specs/2026-07-29-m5-lazy-smp-design.md`
- Reference: `C:\Users\Samaritan\.factory\specs\2026-07-29-lazy-smp-multi-thread-search-support.md`

**Interfaces:**
- Produces: the repository-local approved design used by later review checkpoints.

- [ ] **Step 1: Copy the approved design into the repository**

Copy the approved spec verbatim. Do not expand scope or add implementation decisions that were not approved.

- [ ] **Step 2: Record the pre-change release bench**

Run:

```powershell
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected:

```text
Positions: 6
Nodes searched: 175944
Time (ms): <machine-dependent>
NPS: <machine-dependent>
```

- [ ] **Step 3: Record baseline protocol behavior**

Run:

```powershell
cargo test -p mf-uci --test uci_protocol iterative_search_emits_well_formed_monotone_info_and_legal_pv -- --exact
cargo test -p mf-uci --test uci_protocol node_limited_search_is_repeatable_at_exact_budget -- --exact
cargo test -p mf-uci --test uci_protocol quit_during_infinite_search_exits_cleanly -- --exact
```

Expected: all three tests pass before production code changes.

- [ ] **Step 4: Commit the approved design only**

Stage only the new design file, review the staged diff, then commit:

```powershell
git add docs/superpowers/specs/2026-07-29-m5-lazy-smp-design.md
git diff --cached
git status
git commit -m "Document Lazy SMP design"
```

---

### Task 2: Wire Search Generation into TT Stores

**Files:**
- Modify: `crates/mf-search/src/search.rs`
- Test: `crates/mf-search/src/search.rs`
- Test: `crates/mf-search/tests/search_invariants.rs`

**Interfaces:**
- Produces: `WorkerParameters { worker_id, generation, node_counters }`.
- Produces: `search_worker_with_history_callback_options(...)`, the internal entry point used by the pool.
- Preserves: existing public `search*` functions as worker-0, generation-0 wrappers.

- [ ] **Step 1: Write a failing unit test for generation storage**

Add a unit test in `search.rs` that runs a shallow worker search with generation `9`, probes the root TT entry using `tt_key`, and asserts `entry.age == 9`.

```rust
#[test]
fn worker_generation_is_written_to_root_tt_entries() {
    let position = Position::startpos();
    let table = TranspositionTable::new(1).unwrap();
    let stop = AtomicBool::new(false);
    let counters = [AtomicU64::new(0)];
    let history = [position.repetition_key()];

    search_worker_with_history_callback_options(
        &position,
        &history,
        &table,
        SearchLimits {
            depth: Some(2),
            ..SearchLimits::default()
        },
        SearchOptions::default(),
        &stop,
        WorkerParameters::new(0, 9, &counters),
        |_| {},
    );

    assert_eq!(table.probe(tt_key(&position, 2)).unwrap().age, 9);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```powershell
cargo test -p mf-search worker_generation_is_written_to_root_tt_entries -- --exact
```

Expected: compile failure because `WorkerParameters` and the worker entry point do not exist.

- [ ] **Step 3: Add worker parameters and generation**

Add:

```rust
pub(crate) struct WorkerParameters<'a> {
    worker_id: usize,
    generation: u8,
    node_counters: &'a [AtomicU64],
}

impl<'a> WorkerParameters<'a> {
    pub(crate) fn new(
        worker_id: usize,
        generation: u8,
        node_counters: &'a [AtomicU64],
    ) -> Self {
        assert!(worker_id < node_counters.len());
        Self {
            worker_id,
            generation: generation & 31,
            node_counters,
        }
    }
}
```

Thread `generation` through `SearchContext::new`, replace the three production `age: 0` stores with `age: context.generation`, and leave test fixtures unchanged.

- [ ] **Step 4: Add an internal worker search entry point**

Move the body of `search_with_history_callback_options` into:

```rust
pub(crate) fn search_worker_with_history_callback_options<F>(
    position: &Position,
    history: &[u64],
    transposition_table: &TranspositionTable,
    limits: SearchLimits,
    options: SearchOptions,
    stop: &AtomicBool,
    worker: WorkerParameters<'_>,
    on_iteration: F,
) -> SearchResult
where
    F: FnMut(&IterationInfo),
```

The existing public function creates one `AtomicU64`, passes worker id `0`, generation `0`, and otherwise behaves identically.

- [ ] **Step 5: Run focused and invariant tests**

Run:

```powershell
cargo test -p mf-search worker_generation_is_written_to_root_tt_entries -- --exact
cargo test -p mf-search --test search_invariants
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: tests pass; bench nodes become the Phase 0 signature and are recorded for later bit-identity checks.

- [ ] **Step 6: Commit**

```powershell
git add crates/mf-search/src/search.rs crates/mf-search/tests/search_invariants.rs
git diff --cached
git status
git commit -m "Wire search generations into the hash table"
```

---

### Task 3: Extract Per-Worker History State

**Files:**
- Create: `crates/mf-search/src/history.rs`
- Modify: `crates/mf-search/src/lib.rs`
- Modify: `crates/mf-search/src/search.rs`
- Test: `crates/mf-search/src/history.rs`

**Interfaces:**
- Produces: `HistoryTables::new`, `killers`, `record_killer`, `quiet_score`, and `update_quiet`.
- Consumes: `Move`, `Color`, and `MAX_SEARCH_PLY`.
- Preserves: exact single-thread move ordering and history gravity formula.

- [ ] **Step 1: Write failing history unit tests**

Cover killer rotation, duplicate killer suppression, color-specific quiet history, and bounded gravity updates.

```rust
#[test]
fn killers_rotate_without_duplicates() {
    let mut history = HistoryTables::new(4);
    history.record_killer(3, first_move());
    history.record_killer(3, second_move());
    history.record_killer(3, second_move());
    assert_eq!(history.killers(3), [Some(second_move()), Some(first_move())]);
}

#[test]
fn quiet_history_is_color_specific_and_bounded() {
    let mut history = HistoryTables::new(4);
    history.update_quiet(Color::White, first_move(), HISTORY_MAX);
    assert_eq!(history.quiet_score(Color::White, first_move()), HISTORY_MAX);
    assert_eq!(history.quiet_score(Color::Black, first_move()), 0);
}
```

- [ ] **Step 2: Run the tests and verify failure**

Run:

```powershell
cargo test -p mf-search history::tests --lib
```

Expected: compile failure because `history` is not registered.

- [ ] **Step 3: Implement `HistoryTables`**

Use the existing exact field types:

```rust
pub(crate) struct HistoryTables {
    killers: [[Option<Move>; 2]; MAX_SEARCH_PLY],
    quiet: Box<[[[i16; 64]; 64]; 2]>,
}
```

`HistoryTables::new(thread_count)` must validate `thread_count > 0`; the argument is a deliberate sizing seam for M4's `BASE_SIZE × next_power_of_two(thread_count)` tables. Move the existing killer and gravity-update logic without changing formulas.

- [ ] **Step 4: Replace direct `SearchContext` fields**

Replace:

```rust
killers: [[Option<Move>; 2]; MAX_SEARCH_PLY],
quiet_history: Box<[[[i16; 64]; 64]; 2]>,
```

with:

```rust
history_tables: HistoryTables,
```

Update all move-picker, LMR, cutoff, and malus call sites to use the new interface.

- [ ] **Step 5: Verify exact single-thread behavior**

Run:

```powershell
cargo test -p mf-search
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: bench node count is bit-identical to Task 2's recorded Phase 0 signature.

- [ ] **Step 6: Commit**

```powershell
git add crates/mf-search/src/history.rs crates/mf-search/src/lib.rs crates/mf-search/src/search.rs
git diff --cached
git status
git commit -m "Isolate per-worker search history"
```

---

### Task 4: Add Worker-Aware Limits, Jitter, and Node Publication

**Files:**
- Modify: `crates/mf-search/src/search.rs`
- Test: `crates/mf-search/src/search.rs`
- Test: `crates/mf-search/tests/search_invariants.rs`

**Interfaces:**
- Produces: aggregate node reporting through `WorkerParameters::node_counters`.
- Produces: worker-0-only clock checks at a 512-node cadence.
- Produces: deterministic `aspiration_delta(worker_id)` with worker 0 returning `25`.

- [ ] **Step 1: Write failing unit tests**

```rust
#[test]
fn worker_zero_preserves_the_original_aspiration_delta() {
    assert_eq!(aspiration_delta(0), ASPIRATION_INITIAL_DELTA);
}

#[test]
fn helpers_receive_distinct_bounded_aspiration_deltas() {
    let values: Vec<_> = (0..8).map(aspiration_delta).collect();
    assert_eq!(values[0], ASPIRATION_INITIAL_DELTA);
    assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn aggregate_nodes_sum_published_worker_counts() {
    let counters = [AtomicU64::new(10), AtomicU64::new(20)];
    assert_eq!(published_node_total(&counters), 30);
}
```

- [ ] **Step 2: Run focused tests and verify failure**

Run:

```powershell
cargo test -p mf-search worker_zero_preserves_the_original_aspiration_delta -- --exact
cargo test -p mf-search aggregate_nodes_sum_published_worker_counts -- --exact
```

Expected: compile failure for missing helpers.

- [ ] **Step 3: Implement node publication**

Add constants:

```rust
const TIME_CHECK_INTERVAL: u64 = 512;
const NODE_PUBLISH_INTERVAL: u64 = 1_024;
```

`visit_node` must:

1. Read `stop` every node.
2. Increment the local plain `u64`.
3. Publish the local counter with `Ordering::Relaxed` every 1024 nodes.
4. Check aggregate node limits against the sum of published counters.
5. Check `Instant::elapsed()` only on worker 0 and only every 512 nodes.

Publish the final local count before returning `SearchResult`.

- [ ] **Step 4: Preserve exact worker-0 semantics**

For a single counter:

- Check node limits against the local count each node, preserving exact `go nodes N`.
- Use aspiration delta `25`.
- Report local node totals.

Helpers receive `soft_time = None` and `hard_time = None`; worker 0 receives the original limits.

- [ ] **Step 5: Verify single-thread contracts**

Run:

```powershell
cargo test -p mf-uci --test uci_protocol iterative_search_emits_well_formed_monotone_info_and_legal_pv -- --exact
cargo test -p mf-uci --test uci_protocol node_limited_search_is_repeatable_at_exact_budget -- --exact
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: both tests pass and bench matches Task 2's signature exactly.

- [ ] **Step 6: Commit**

```powershell
git add crates/mf-search/src/search.rs crates/mf-search/tests/search_invariants.rs
git diff --cached
git status
git commit -m "Parameterize search workers"
```

---

### Task 5: Implement Pure Root Voting

**Files:**
- Create: `crates/mf-search/src/vote.rs`
- Modify: `crates/mf-search/src/lib.rs`
- Test: `crates/mf-search/src/vote.rs`

**Interfaces:**
- Produces: `pub(crate) fn select_best_result(results: &[SearchResult]) -> usize`.
- Consumes: completed `SearchResult` values from all workers.
- Returns: worker 0 for empty/all-depth-zero voting input. Public UCI fixed-depth searches bypass multi-worker dispatch and voting entirely.

- [ ] **Step 1: Write failing voting tests**

Construct small `SearchResult` fixtures and test:

- Depth-zero results are excluded.
- Relative votes combine workers selecting the same move.
- A decisive result overrides a non-decisive vote winner.
- Larger absolute decisive score wins among consistent decisive results.
- Equal vote totals prefer the longer PV.

```rust
#[test]
fn depth_zero_workers_do_not_vote() {
    let results = [result(0, 500, move_a(), 1), result(4, 20, move_b(), 2)];
    assert_eq!(select_best_result(&results), 1);
}
```

- [ ] **Step 2: Run the tests and verify failure**

Run:

```powershell
cargo test -p mf-search vote::tests --lib
```

Expected: compile failure because `vote` is not registered.

- [ ] **Step 3: Implement score-relative voting**

Use `std::collections::HashMap<Move, i64>` and the exact weighting:

```rust
*votes.entry(best_move).or_default() += i64::from(result.score - min_score + 14);
```

Use the comparison order from the approved design and Stockfish reference:

1. Completed decisive result.
2. Larger absolute decisive score.
3. Higher total move vote, excluding decisive losses from ordinary vote selection.
4. Longer PV.
5. Lower worker index for deterministic final ties.

- [ ] **Step 4: Run voting tests**

Run:

```powershell
cargo test -p mf-search vote::tests --lib
```

Expected: all voting tests pass.

- [ ] **Step 5: Commit**

```powershell
git add crates/mf-search/src/vote.rs crates/mf-search/src/lib.rs
git diff --cached
git status
git commit -m "Add Lazy SMP root voting"
```

---

### Task 6: Build the Persistent Search Pool

**Files:**
- Create: `crates/mf-search/src/thread_pool.rs`
- Modify: `crates/mf-search/src/lib.rs`
- Create: `crates/mf-search/tests/smp.rs`

**Interfaces:**
- Produces:

```rust
pub struct SearchPool;
pub struct PoolSearchResult {
    pub result: SearchResult,
    pub selected_worker: usize,
}
#[derive(Debug)]
pub enum PoolError {
    Busy,
    WorkerUnavailable,
}

impl SearchPool {
    pub fn new(thread_count: usize) -> io::Result<Self>;
    pub fn thread_count(&self) -> usize;
    pub fn clear(&self, table: Arc<TranspositionTable>) -> Result<(), PoolError>;
    pub fn search_with_history_callback_options<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo);

    pub fn search_fixed_depth_with_history_callback_options<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo);

    pub fn search_fixed_depth_smp_with_history_callback_options<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo);
}
```

- Consumes: Task 2 worker entry point and Task 5 root voting.
- `search_fixed_depth_with_history_callback_options` dispatches only worker 0 and is the public UCI fixed-depth path.
- `search_fixed_depth_smp_with_history_callback_options` dispatches every configured worker and exists only for `mtbench` scaling.

- [ ] **Step 1: Write failing pool integration tests**

Add tests for:

1. `SearchPool::new(4).unwrap().thread_count() == 4`.
2. The worker-0-only fixed-depth path returns a legal move and legal PV from a 4-worker pool without dispatching helpers.
3. The explicit 4-thread fixed-depth SMP path returns a legal move and legal PV.
4. External `stop` ends an infinite 4-thread search.
5. `clear()` removes entries from a shared TT.
6. An 8-thread stress loop over representative FENs never panics.

- [ ] **Step 2: Run the tests and verify failure**

Run:

```powershell
cargo test -p mf-search --test smp
```

Expected: compile failure because `SearchPool` does not exist.

- [ ] **Step 3: Implement persistent workers**

Each `WorkerHandle` owns:

```rust
struct WorkerHandle {
    sender: mpsc::Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
}
```

Commands:

```rust
enum WorkerCommand {
    Search(SearchJob),
    Clear {
        table: Arc<TranspositionTable>,
        start_cluster: usize,
        end_cluster: usize,
        done: mpsc::Sender<()>,
    },
    Shutdown,
}
```

`SearchJob` owns cloned root state and shares `Arc` values:

```rust
struct SearchJob {
    worker_id: usize,
    generation: u8,
    position: Position,
    history: Arc<[u64]>,
    table: Arc<TranspositionTable>,
    limits: SearchLimits,
    options: SearchOptions,
    stop: Arc<AtomicBool>,
    counters: Arc<[AtomicU64]>,
    events: mpsc::Sender<WorkerEvent>,
}
```

Workers block on `Receiver::recv()`. They hold no lock while calling the search.

`SearchPool` also owns:

```rust
active: AtomicBool,
generation: AtomicU8,
```

Every search and clear operation acquires the active state with `compare_exchange(false, true, ...)` and releases it through an RAII guard. Concurrent operations return `PoolError::Busy` instead of interleaving events. This preserves a lock-free hot path and makes the public API safe against accidental concurrent callers.

Use a private dispatch mode shared by the pool entry points:

```rust
enum DispatchMode {
    WorkerZeroOnly,
    AllWorkers,
}
```

The public fixed-depth method selects `WorkerZeroOnly`; the explicitly named `mtbench` method selects `AllWorkers`.

- [ ] **Step 4: Implement dispatch and completion**

The shared pool implementation must:

1. Reset stop and all counters.
2. Increment a five-bit generation counter for the job.
3. Select the participating workers from the dispatch mode.
4. Clone the position/history once per participating worker.
5. Give time limits only to worker 0.
6. Dispatch only the selected jobs. `WorkerZeroOnly` must not send a job to any helper.
7. Forward only worker-0 progress to the caller.
8. On worker-0 completion for finite searches, set stop.
9. Collect one result per participating worker.
10. Return worker 0 directly for `WorkerZeroOnly`; call `select_best_result` for `AllWorkers`.
11. Replace selected `nodes`, `hashfull`, and `elapsed` with aggregate values from participating workers.
12. Preserve worker-0 iterations unchanged and return `selected_worker` separately in `PoolSearchResult`.

- [ ] **Step 5: Implement synchronous parallel TT clear**

Expose a crate-private cluster-range clear method from `TranspositionTable` and partition clusters into contiguous ranges. Send one `Clear` command per worker and wait for all acknowledgements.

The public `TranspositionTable::clear()` remains single-threaded for existing callers and tests.

- [ ] **Step 6: Ensure clean drop**

`Drop for SearchPool` sends `Shutdown` to every worker and joins every handle. Channel disconnection must also terminate a worker.

- [ ] **Step 7: Run pool tests**

Run:

```powershell
cargo test -p mf-search --test smp
cargo test -p mf-search --test transposition_table
```

Expected: all tests pass with no hangs.

- [ ] **Step 8: Commit**

```powershell
git add crates/mf-search/src/thread_pool.rs crates/mf-search/src/transposition_table.rs crates/mf-search/src/lib.rs crates/mf-search/tests/smp.rs crates/mf-search/tests/transposition_table.rs
git diff --cached
git status
git commit -m "Implement persistent Lazy SMP workers"
```

---

### Task 7: Wire the Pool into UCI

**Files:**
- Modify: `crates/mf-uci/src/lib.rs`
- Modify: `crates/mf-uci/tests/uci_protocol.rs`
- Modify: `crates/mf-uci/tests/bench_cli.rs`

**Interfaces:**
- Consumes: `Arc<SearchPool>` and its search/clear methods.
- Preserves: existing asynchronous `ActiveSearch` driver and writer serialization.

- [ ] **Step 1: Write failing UCI tests**

Add tests that:

- `setoption name Threads value 4` creates a real 4-worker search and still exits cleanly.
- Invalid thread values leave the existing pool usable.
- `Threads=1` depth search emits exactly one line per completed depth.
- Repeating the same `Threads=4`, position, and `go depth N` sequence produces identical deterministic `info` fields and `bestmove`. Canonicalize away machine-dependent `time` and `nps` fields before comparing the `info` lines.
- `Threads=4` `stop` returns one legal `bestmove`.
- `ucinewgame` clears the shared TT through the pool.

- [ ] **Step 2: Run focused tests and verify failure**

Run:

```powershell
cargo test -p mf-uci --test uci_protocol -- Threads
```

Expected: new assertions fail because `EngineState::threads` remains write-only.

- [ ] **Step 3: Replace dead thread state**

Replace:

```rust
threads: usize,
```

with:

```rust
search_pool: Arc<SearchPool>,
```

Initialize with one worker. `setoption Threads` must:

1. Parse and clamp to `1..=256`.
2. Stop the active search first (already guaranteed by the command loop).
3. Allocate a replacement pool.
4. Replace the old pool only after successful construction.

- [ ] **Step 4: Dispatch UCI searches through the pool**

Pass an `Arc<SearchPool>` into `start_search`. Keep the existing driver thread and callback writer behavior.

For public UCI `go depth N`, call `search_fixed_depth_with_history_callback_options`. It dispatches only worker 0 even when the configured pool has more threads. Helper workers remain parked, perform no TT writes, and cannot perturb the emitted `info` lines or `bestmove`.

All other SMP-enabled UCI searches call `search_with_history_callback_options`.

For finite search, write `bestmove` when the pool returns.

For infinite search, keep the existing UCI rule: wait after the search result until `stop` is observed before writing `bestmove`, including terminal positions.

If `selected_worker != 0`, emit one terminal helper-selection line before `bestmove`:

```text
info score <score> nodes <nodes> nps <nps> hashfull <hashfull> time <time> pv <pv>
```

This line intentionally omits `depth`, so the existing strict monotonicity contract for `info depth ...` remains unchanged while the GUI still sees the score and PV corresponding to `bestmove`.

- [ ] **Step 5: Use parallel clear on `ucinewgame`**

Replace direct TT clear with:

```rust
self.search_pool.clear(Arc::clone(&self.transposition_table));
```

Hash resizing still creates a fresh empty TT and does not require a clear.

- [ ] **Step 6: Run the protocol suite**

Run:

```powershell
cargo test -p mf-uci --test uci_protocol
cargo test -p mf-uci --test bench_cli
```

Expected: all tests pass. In particular:

- `iterative_search_emits_well_formed_monotone_info_and_legal_pv` still sees exactly six lines.
- The repeated `Threads=4` fixed-depth regression sees identical canonicalized `info` output and `bestmove` on every run; only machine-dependent `time` and `nps` fields are excluded.
- `node_limited_search_is_repeatable_at_exact_budget` still sees exactly 20,000 nodes.
- `quit_during_infinite_search_exits_cleanly` exercises real four-thread shutdown.
- `interrupted_iteration_does_not_duplicate_a_completed_depth` remains strictly monotone.

- [ ] **Step 7: Commit**

```powershell
git add crates/mf-uci/src/lib.rs crates/mf-uci/tests/uci_protocol.rs crates/mf-uci/tests/bench_cli.rs
git diff --cached
git status
git commit -m "Wire UCI Threads into Lazy SMP"
```

---

### Task 8: Add the `mtbench` Scaling Harness

**Files:**
- Modify: `crates/mf-uci/src/main.rs`
- Modify: `crates/mf-uci/src/lib.rs`
- Create: `crates/mf-uci/tests/mtbench_cli.rs`

**Interfaces:**
- Produces: `manifold mtbench [--threads 1,2,4,8] [--depth 10]`.
- Uses: the existing six deterministic bench positions and one shared TT per run.

- [ ] **Step 1: Write failing CLI tests**

Test:

- Default output has rows for 1, 2, 4, and 8 threads.
- `--threads 1,4 --depth 8` prints only those rows.
- Invalid zero/duplicate/malformed thread lists fail helpfully.
- Output columns are `Threads`, `Depth`, `Nodes`, `Time (ms)`, and `NPS`.

- [ ] **Step 2: Run the tests and verify failure**

Run:

```powershell
cargo test -p mf-uci --test mtbench_cli
```

Expected: failure because `mtbench` is an unknown command.

- [ ] **Step 3: Implement argument parsing**

Add:

```rust
pub fn run_mtbench_subcommand<I, S, W>(arguments: I, writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
```

Defaults:

```text
threads = [1, 2, 4, 8]
depth = 10
hash = 64 MiB
```

- [ ] **Step 4: Run one fixed-depth search per position and thread count**

For each row:

1. Create `SearchPool::new(threads)` and propagate construction errors.
2. Create a 64 MiB TT.
3. Clear between positions.
4. Call the explicit `search_fixed_depth_smp_with_history_callback_options` path so all configured workers participate despite the fixed-depth limit.
5. Sum aggregate nodes and elapsed wall time.
6. Print NPS.

Keep the existing `bench` subcommand unchanged and deterministic at `Threads=1`. Never route public UCI `go depth N` through this SMP-only benchmark path.

- [ ] **Step 5: Run CLI tests and a release smoke run**

Run:

```powershell
cargo test -p mf-uci --test mtbench_cli
cargo run --release -p mf-uci --bin manifold -- mtbench --threads 1,2,4,8 --depth 8
```

Expected: all rows complete; throughput generally increases with thread count, without making a strict timing assertion in tests.

- [ ] **Step 6: Commit**

```powershell
git add crates/mf-uci/src/main.rs crates/mf-uci/src/lib.rs crates/mf-uci/tests/mtbench_cli.rs
git diff --cached
git status
git commit -m "Add multi-thread search benchmark"
```

---

### Task 9: Full Correctness and Performance Validation

**Files:**
- Create: `experiments/M5-smp/run-metadata.txt`
- Create after validation: `experiments/M5-smp/M5-smp-results.md`
- Create after validation: `baselines/M5/manifold.exe`
- Create after validation: `baselines/M5/build-metadata.txt`

**Interfaces:**
- Consumes: completed Lazy SMP implementation.
- Produces: reproducible correctness, scaling, and Elo evidence.

- [ ] **Step 1: Run formatting, lint, tests, and builds**

Run:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mf-core --features force-magic
cargo build --release
```

Expected: every command succeeds with no warnings.

- [ ] **Step 2: Confirm deterministic single-thread signature**

Run twice:

```powershell
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: identical node count on both runs and equal to Task 2's Phase 0 signature.

- [ ] **Step 3: Record scaling**

Run:

```powershell
target\release\manifold.exe mtbench --threads 1,2,4,8 --depth 10
```

Record per-thread Nodes, Time, NPS, speedup, and efficiency:

```text
speedup(T) = NPS(T) / NPS(1)
efficiency(T) = speedup(T) / T
```

If 8-thread efficiency is below 60%, stop and diagnose before match testing.

- [ ] **Step 4: Run fixed-node equivalence**

Use fastchess with equal node limits and `Hash=64`, one engine at `Threads=1` and one at `Threads=8`. Record the exact executable paths, opening book, seed, game count, and result in `run-metadata.txt`.

Expected: no large Elo gap at equal aggregate nodes. Treat a substantial gap as a correctness/selectivity problem.

- [ ] **Step 5: Run the full LTC SMP measurement**

Use:

```powershell
fastchess.exe `
  -engine cmd=target\release\manifold.exe name=Manifold-8T option.Hash=64 option.Threads=8 `
  -engine cmd=target\release\manifold.exe name=Manifold-1T option.Hash=64 option.Threads=1 `
  -each proto=uci tc=60+0.6 `
  -openings file=tools\books\UHO_Lichess_4852_v1.epd format=epd order=random `
  -games 2 -rounds 100 -repeat -concurrency 1 -use-affinity `
  -srand 20260729 -report penta=true
```

Resolve the actual fastchess executable and book path from the verified local infrastructure before running. Do not silently substitute a different book or time control.

- [ ] **Step 6: Tune aspiration jitter only if scaling evidence requires it**

Compare the approved baseline jitter against at most two bounded alternatives. Use the `mtbench` scaling curve as the free screen; only run games for a candidate that materially improves scaling without harming `Threads=1`.

- [ ] **Step 7: Archive reproducibility artifacts**

Record:

- Git commit.
- Rust toolchain.
- CPU and OS.
- Exact build command.
- Bench signature.
- Scaling table.
- Fastchess commands and seed.
- Timeout/crash count.
- Match result and confidence interval.

Confirm the tracked design and implementation plan retain the testing rule: multi-thread search is nondeterministic, so exact node-count assertions, `bench`, and deterministic node-budget tests must explicitly use `Threads=1`.

- [ ] **Step 8: Final implementation review**

Invoke `verification-before-completion`, then `requesting-code-review`. Address all high-confidence findings and rerun the relevant commands.

- [ ] **Step 9: Commit validation artifacts**

```powershell
git add experiments/M5-smp baselines/M5
git diff --cached
git status
git commit -m "Validate Lazy SMP scaling"
```
