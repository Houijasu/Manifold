# Recommended Execution Order Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the approved reliability, deterministic-test, tablebase-coverage,
repository-hygiene, and same-binary measurement work in the required order, then apply
only evidence-backed option defaults.

**Architecture:** Land narrowly scoped correctness changes first, each through a red/green
test cycle and focused commit. Freeze one release binary only after correctness and
hygiene gates pass, use that exact binary in both arms of three sequential fixed-time
matches, then make any default-only changes in a separate commit and run the complete
gate/review set.

**Tech Stack:** Rust 2024, standard library synchronization and process APIs, existing
`mf-core`/`mf-datagen`/`mf-nnue`/`mf-search`/`mf-tb`/`mf-uci` tests, PowerShell 7,
fastchess through `harness/run_match.ps1`.

## Global Constraints

- Work only in
  `C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order` on branch
  `execution/recommended-order`.
- The copied baseline assets `tools/testdata/` and `nets/main.nnue` must remain present.
- Baseline fact: `cargo test --workspace` passes in this isolated worktree.
- Use TDD for every production behavior change: failing test, observed failure, minimal
  implementation, passing focused test.
- Add no new search heuristic.
- Do not broadly refactor `crates/mf-search/src/search.rs`.
- Do not change any default before match evidence satisfies Task 12.
- Keep code-correctness tasks separate from long-running matches.
- Same-thread-count comparisons use fixed time, never fixed nodes.
- Both engines at `Threads=1` require `-use-affinity -concurrency 8`; the harness must
  derive those values.
- Do not run the three matches concurrently.
- Do not push without explicit instruction.
- Every commit uses a short imperative sentence-case subject and this exact trailer:
  `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`.
- Before each commit, inspect `git status --short` and `git diff --check`; stage only the
  files named by that task.

---

## File map

### Production behavior

- `crates/mf-search/src/search.rs`
  - ponder latch/wakeup and analysis-ceiling continuation;
  - no heuristic or unrelated search cleanup.
- `crates/mf-uci/src/lib.rs`
  - fallible engine construction;
  - automatic NNUE reset error handling;
  - ponder protocol integration tests already live in its test target.
- `crates/mf-uci/src/main.rs`
  - concise nonzero startup error.
- `crates/mf-tb/src/probe.rs`
  - trim one `SyzygyPath` segment before `PathBuf` construction.
- `crates/mf-datagen/src/record.rs`
  - preflight untrusted material counts before `Position::place_piece`.

### Tests and deterministic fixtures

- `crates/mf-uci/tests/uci_protocol.rs`
  - ceiling-saturated ponder conversion and condition-based UCI behavior.
- `crates/mf-uci/tests/movetime_budget.rs`
  - budget/legality assertions instead of minimum-depth assertions.
- `crates/mf-tb/tests/path_lists.rs`
  - whitespace-normalized semicolon path lists.
- `crates/mf-search/tests/tablebase_integration.rs`
  - root DTZ, interior WDL, and public `tbhits` behavior using vendored tiny tables.
- `crates/mf-search/src/search.rs` test module
  - private TT depth-bonus and floor/ceiling assertions.
- `crates/mf-search/tests/data/syzygy/`
  - only if Task 7's fixture feasibility gate passes: minimal WDL/DTZ files plus
    provenance/license README.

### Repository hygiene and evidence

- `AGENTS.md`, `README.md`
  - live crate and UCI maps; experiment run-state policy.
- `.gitignore`
  - root `config.json` and experiment `games.pgn` only; never ignore
    `experiments/` itself.
- `plans/007-ci-and-repo-hygiene.md`, `plans/README.md`
  - targeted subset completion and remaining CI/net-provisioning work.
- `experiments/2026-08-18-recommended-order/<toggle>/`
  - result write-up, console, fastchess log, and run metadata for each admissible match.

---

### Task 1: Confirm the isolated baseline

**Files:**
- No changes

**Interfaces:**
- Consumes: copied `tools/testdata/` and `nets/main.nnue`
- Produces: a clean, known-good starting point for every later red/green result

- [ ] **Step 1: Verify branch, cleanliness, and assets**

Run:

```powershell
$root = 'C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order'
git -C $root branch --show-current
git -C $root status --short
Test-Path "$root\tools\testdata"
Test-Path "$root\nets\main.nnue"
```

Expected:

```text
execution/recommended-order
<no status output>
True
True
```

- [ ] **Step 2: Reconfirm the supplied baseline**

Run:

```powershell
Set-Location 'C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order'
cargo test --workspace
```

Expected: exit 0. A failure is a STOP: record the exact failing target and do not begin
implementation.

---

### Task 2: Park and resume a saturated clocked ponder search

**Files:**
- Modify: `crates/mf-search/src/search.rs:514-577`
- Modify: `crates/mf-search/src/search.rs:1260-1652`
- Modify: `crates/mf-uci/tests/uci_protocol.rs:1857-1922`

**Interfaces:**
- Consumes: `PonderState::{is_pondering, ponderhit, abort, rebased_start}`
- Produces:
  - `PonderState::wait_until_released(&self)`
  - a clocked-ponder saturation path that blocks at the analysis ceiling, wakes on
    `ponderhit`/`abort`, and continues until the converted budget stops it

- [ ] **Step 1: Add the failing protocol regression**

Add this test beside the existing ponder tests in
`crates/mf-uci/tests/uci_protocol.rs`:

```rust
#[test]
fn a_clocked_ponder_that_reaches_the_analysis_ceiling_spends_the_converted_clock() {
    let mut engine = InteractiveUci::spawn_exclusive();
    engine.send("position fen 7k/8/6QK/8/8/8/8/8 w - - 0 1");
    engine.send("go ponder wtime 3000 btime 3000");

    assert!(
        engine
            .receive_until(watchdog(Duration::from_secs(10)), |line| {
                is_completed_iteration(line) && field(line, "depth") == 128
            })
            .is_some(),
        "the forced-mate ponder must reach the bounded analysis ceiling"
    );
    assert!(
        engine
            .receive_until(Duration::from_millis(200), |line| {
                line.starts_with("bestmove ")
            })
            .is_none(),
        "reaching the ceiling must not answer before ponderhit or stop"
    );

    engine.send("ponderhit");
    let mut rebased_time = None;
    let bestmove = engine.receive_until(watchdog(Duration::from_secs(5)), |line| {
        if is_completed_iteration(line)
            && field(line, "depth") == 128
            && let Some(time) = optional_field(line, "time")
        {
            rebased_time = Some(time);
        }
        line.starts_with("bestmove ")
    });

    assert!(bestmove.is_some(), "ponderhit must eventually release a bestmove");
    assert!(
        rebased_time.is_some_and(|time| (40..1000).contains(&time)),
        "the post-hit ceiling iteration must spend the rebased budget, got {rebased_time:?}"
    );
    engine.send("quit");
    assert!(engine.wait_for_exit(watchdog(Duration::from_secs(2))));
}
```

The `128` anchor is `MAX_SEARCH_PLY`/`MAX_ITERATIVE_DEEPENING_DEPTH` in the current
engine. If that constant changes before execution, use the live exported/search constant
value and keep the assertion exact.

- [ ] **Step 2: Run the regression and observe the current failure**

Run:

```powershell
cargo test -p mf-uci --test uci_protocol `
  a_clocked_ponder_that_reaches_the_analysis_ceiling_spends_the_converted_clock `
  -- --nocapture
```

Expected: FAIL because current code emits `bestmove` immediately after `ponderhit` and
does not produce a post-hit ceiling iteration with rebased elapsed time.

- [ ] **Step 3: Add a blocking, idempotent ponder wakeup**

In `crates/mf-search/src/search.rs`, extend `PonderState` with a condition variable:

```rust
pub struct PonderState {
    pondering: AtomicBool,
    rebased_start: Mutex<Option<Instant>>,
    released: Condvar,
}
```

Initialize it in `new`, notify it at the end of both `ponderhit` and `abort`, and add:

```rust
fn wait_until_released(&self) {
    let mut rebased = self
        .rebased_start
        .lock()
        .expect("ponder clock lock should not be poisoned");
    while self.is_pondering() {
        rebased = self
            .released
            .wait(rebased)
            .expect("ponder clock lock should not be poisoned");
    }
}
```

Import `std::sync::Condvar`. Preserve the existing release/acquire ordering and
idempotent first-`ponderhit` clock base.

- [ ] **Step 4: Continue a saturated clocked ponder without flooding `info`**

Make the iterative-deepening loop distinguish ordinary completion from exhausting the
analysis ceiling while all of these are true:

```rust
ponder.is_some()
    && limits.depth.is_none()
    && limits.nodes.is_none()
    && !limits.infinite
    && context.is_pondering()
```

At that boundary:

1. call `wait_until_released`;
2. if `rebased_start()` is `None`, the release was `abort`; return the best completed
   result;
3. if a rebased start exists, repeat only the maximum-depth search until
   `should_stop_after_iteration()` or the hard check stops the context;
4. suppress intermediate duplicate ceiling callbacks;
5. publish exactly one refreshed ceiling `IterationInfo` when the converted search
   finishes, so its `time` is measured from `ponderhit`;
6. retain the latest completed score/PV as the returned result.

Keep normal `go depth`, `go nodes`, `go infinite`, non-ponder time controls, mate exits,
and MultiPV loop behavior byte-for-byte outside this branch.

- [ ] **Step 5: Add focused `PonderState` unit coverage**

In the existing `search.rs` test module use `std::thread::scope` and a channel:

```rust
#[test]
fn ponder_wait_wakes_on_ponderhit_and_records_one_clock_base() {
    let ponder = PonderState::new();
    thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        scope.spawn(|| {
            ponder.wait_until_released();
            tx.send(()).expect("wake should be observable");
        });
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        ponder.ponderhit();
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
    });
    let first = ponder.rebased_start().expect("ponderhit records a base");
    ponder.ponderhit();
    assert_eq!(ponder.rebased_start(), Some(first));
}

#[test]
fn ponder_wait_wakes_on_abort_without_recording_a_clock_base() {
    let ponder = PonderState::new();
    thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        scope.spawn(|| {
            ponder.wait_until_released();
            tx.send(()).expect("wake should be observable");
        });
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        ponder.abort();
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
    });
    assert_eq!(ponder.rebased_start(), None);
}
```

- [ ] **Step 6: Run ponder-focused tests**

Run:

```powershell
cargo test -p mf-search ponder_ -- --nocapture
cargo test -p mf-uci --test uci_protocol ponder -- --nocapture
```

Expected: all pass, including:

- no `bestmove` before `ponderhit`/`stop`;
- `stop` still releases the deferred result;
- `ponderhit` after saturation emits a post-hit timed iteration;
- replacement `go` still produces one answer per `go`.

- [ ] **Step 7: Commit**

```powershell
git add crates/mf-search/src/search.rs crates/mf-uci/tests/uci_protocol.rs
git diff --cached --check
$message = @"
Fix saturated clocked ponder searches

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 3: Trim every `SyzygyPath` segment

**Files:**
- Modify: `crates/mf-tb/src/probe.rs:453-461`
- Create: `crates/mf-tb/tests/path_lists.rs`

**Interfaces:**
- Consumes: semicolon-separated `Tablebases::new(&str)`
- Produces: each non-empty path segment normalized with `str::trim`

- [ ] **Step 1: Write the failing integration test**

Create `crates/mf-tb/tests/path_lists.rs`:

```rust
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use mf_tb::Tablebases;

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should follow the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("manifold-{name}-{}-{nonce}", std::process::id()))
}

#[test]
fn every_semicolon_delimited_path_segment_is_trimmed() {
    let first = unique_temp_dir("syzygy-first");
    let second = unique_temp_dir("syzygy-second");
    fs::create_dir_all(&first).expect("first fixture directory should be created");
    fs::create_dir_all(&second).expect("second fixture directory should be created");

    let paths = format!("  {}  ;\t{}\t", first.display(), second.display());
    let opened = Tablebases::new(&paths);

    fs::remove_dir_all(&first).expect("first fixture directory should be removed");
    fs::remove_dir_all(&second).expect("second fixture directory should be removed");
    assert!(opened.is_ok(), "trimmed existing directories should be accepted");
}
```

- [ ] **Step 2: Run it and observe failure**

Run:

```powershell
cargo test -p mf-tb --test path_lists -- --nocapture
```

Expected: FAIL because the untrimmed first/second `PathBuf` values do not exist.

- [ ] **Step 3: Apply the one-line normalization**

Change the path collection in `Store::new` to:

```rust
let dirs: Vec<PathBuf> = paths
    .split(';')
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .map(PathBuf::from)
    .collect();
```

Do not canonicalize, change delimiters, or alter discovery/error semantics.

- [ ] **Step 4: Run all `mf-tb` tests**

```powershell
cargo test -p mf-tb
```

Expected: exit 0. Machine-local real-table tests may skip when `MF_SYZYGY_PATH` is not
set; the new path-list test must always run.

- [ ] **Step 5: Commit**

```powershell
git add crates/mf-tb/src/probe.rs crates/mf-tb/tests/path_lists.rs
git diff --cached --check
$message = @"
Trim Syzygy path segments

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 4: Reject datagen material-count overflow before reconstruction

**Files:**
- Modify: `crates/mf-datagen/src/record.rs:256-362`

**Interfaces:**
- Consumes: untrusted piece codes from `Record::iter_pieces`
- Produces:
  - `StructuralError::MaterialCountOverflow { opponent: bool, kind: PieceKind, found: u32, max: u32 }`
  - `Record::material_count_overflow(self) -> Option<StructuralError>`
  - `Record::to_position` returning `None` before `Position::place_piece` for malformed
    material

- [ ] **Step 1: Add a corrupt-record fixture and failing tests**

Inside the existing `record.rs` test module, add a helper that starts from a valid
32-byte record and writes an occupancy/piece array containing:

- one side-to-move king;
- one opponent king;
- seventeen side-to-move knights;
- no invalid piece codes.

Then add:

```rust
#[test]
fn structural_validation_reports_material_count_overflow() {
    let record = record_with_seventeen_stm_knights();
    assert!(record.structural_errors().contains(
        &StructuralError::MaterialCountOverflow {
            opponent: false,
            kind: mf_core::PieceKind::Knight,
            found: 17,
            max: 16,
        }
    ));
}

#[test]
fn malformed_material_is_rejected_before_position_reconstruction() {
    let record = record_with_seventeen_stm_knights();
    assert_eq!(record.to_position(), None);
}
```

Construct the piece nibbles explicitly so the test does not call `Position::place_piece`
while creating its fixture.

- [ ] **Step 2: Run the tests and confirm the panic/failure**

Run:

```powershell
cargo test -p mf-datagen malformed_material -- --nocapture
cargo test -p mf-datagen structural_validation_reports_material -- --nocapture
```

Expected: the `to_position` case currently panics/indexes out of bounds before returning;
the structural error variant does not yet exist.

- [ ] **Step 3: Implement count-first validation**

Add a private constant matching the `mf-core` invariant:

```rust
const MAX_ZOBRIST_MATERIAL_COUNT: u32 = 16;
```

Add `material_count_overflow` that:

1. iterates all record pieces without constructing a position;
2. ignores invalid kind codes, which existing structural validation reports separately;
3. counts only knight, bishop, rook, and queen by relative color;
4. returns the first count above 16 with exact side/kind/count/max data.

Call it from `structural_errors`, and make `to_position` begin with:

```rust
if self.material_count_overflow().is_some() {
    return None;
}
```

Do not modify `mf-core::Position::place_piece` or expose the crate-private Zobrist
constant.

- [ ] **Step 4: Run crate tests**

```powershell
cargo test -p mf-datagen
```

Expected: exit 0; validation reports the new defect instead of aborting.

- [ ] **Step 5: Commit**

```powershell
git add crates/mf-datagen/src/record.rs
git diff --cached --check
$message = @"
Reject overflowing datagen material counts

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 5: Return graceful automatic NNUE startup errors

**Files:**
- Modify: `crates/mf-uci/src/lib.rs:146-223`
- Modify: `crates/mf-uci/src/lib.rs:282-474`
- Modify: `crates/mf-uci/src/lib.rs:755-775`
- Modify: `crates/mf-uci/src/lib.rs:1225-1253`
- Modify: `crates/mf-uci/src/main.rs:1-39`

**Interfaces:**
- Consumes: `mf_nnue::resolve_network(None)`
- Produces:
  - `default_network_resolution() -> Result<SharedNetworkResolution, String>`
  - `EngineState::try_new() -> Result<Self, String>`
  - startup failure through `run`/`main`, not panic
  - unchanged retain-on-error behavior for runtime explicit `EvalFile`

- [ ] **Step 1: Add failing constructor tests**

In the existing `mf-uci` library test module, add:

```rust
#[test]
fn engine_construction_returns_automatic_network_errors() {
    let error = match EngineState::try_new_with_network(Err(
        "fixture automatic network failure".to_string(),
    )) {
        Ok(_) => panic!("construction must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        "manifold requires an NNUE network to evaluate: fixture automatic network failure"
    );
}

#[test]
fn automatic_evalfile_reset_keeps_the_previous_network_on_resolution_failure() {
    let mut state = EngineState::try_new().expect("copied automatic network should load");
    let previous_network = Arc::clone(&state.network);
    let previous_source = state.network_source.clone();
    let mut output = Vec::new();

    handle_eval_file_with_automatic_resolution(
        "<empty>",
        &mut state,
        &mut output,
        || Err("fixture automatic network failure".to_string()),
    )
    .expect("the diagnostic should be writable");

    assert!(Arc::ptr_eq(&state.network, &previous_network));
    assert_eq!(state.network_source, previous_source);
    assert!(
        String::from_utf8(output)
            .expect("diagnostic should be UTF-8")
            .starts_with("info string unable to load EvalFile automatic resolution:")
    );
}
```

Implement the second test with a private `handle_eval_file_with_automatic_resolution`
helper whose resolver argument is:

```rust
impl FnOnce() -> Result<SharedNetworkResolution, String>
```

The production `handle_eval_file` passes `default_network_resolution`.

- [ ] **Step 2: Run the constructor/reset tests and observe failure**

```powershell
cargo test -p mf-uci --lib automatic_network -- --nocapture
cargo test -p mf-uci --lib automatic_evalfile_reset -- --nocapture
```

Expected: FAIL because `EngineState` still uses infallible `Default` and automatic reset
still calls a panicking resolver.

- [ ] **Step 3: Make automatic resolution and construction fallible**

Replace the resolution cache with:

```rust
fn default_network_resolution() -> Result<SharedNetworkResolution, String> {
    static RESOLUTION: OnceLock<Result<SharedNetworkResolution, String>> = OnceLock::new();
    RESOLUTION
        .get_or_init(|| {
            resolve_network(None)
                .map(|resolved| {
                    let (network, source) = resolved.into_parts();
                    SharedNetworkResolution {
                        network: Arc::new(network),
                        source,
                    }
                })
                .map_err(|error| error.to_string())
        })
        .clone()
}
```

Replace `impl Default for EngineState` with:

```rust
impl EngineState {
    fn try_new() -> Result<Self, String> {
        Self::try_new_with_network(default_network_resolution())
    }

    fn try_new_with_network(
        network: Result<SharedNetworkResolution, String>,
    ) -> Result<Self, String> {
        let network = network.map_err(|error| {
            format!("manifold requires an NNUE network to evaluate: {error}")
        })?;
        let position = Position::startpos();
        Ok(Self {
            position_history: vec![position.repetition_key()],
            position,
            position_is_stale: false,
            chess960: false,
            network: network.network,
            network_source: network.source,
            search_pool: Arc::new(
                SearchPool::new(1)
                    .map_err(|error| format!("unable to start default search worker: {error}"))?,
            ),
            search_options: SearchOptions::default(),
            transposition_table: Arc::new(
                TranspositionTable::new(DEFAULT_HASH_MIB)
                    .map_err(|error| format!("unable to allocate default Hash: {error}"))?,
            ),
            tablebases: None,
            move_overhead_millis: TIME_OVERHEAD_MILLIS,
            ponder_enabled: false,
        })
    }
}
```

Update tests that currently call `EngineState::default()` to call
`EngineState::try_new().expect("copied automatic network should load")`.

- [ ] **Step 4: Propagate startup and automatic-reset errors**

At the beginning of `run`:

```rust
let mut state = EngineState::try_new().map_err(io::Error::other)?;
```

In the automatic `EvalFile` branch:

```rust
match automatic_resolution() {
    Ok(resolution) => {
        state.network = resolution.network;
        state.network_source = resolution.source;
        clear_eval_dependent_search_state(state, writer, "EvalFile")?;
        write_network_selection(writer, state, "automatic resolution")
    }
    Err(error) => writeln!(
        writer,
        "info string unable to load EvalFile automatic resolution: {error}; keeping {}",
        state.network_source
    ),
}
```

Keep explicit-path success/failure logic unchanged. Update
`benchmark_network_resolution` to propagate `default_network_resolution()` rather than
wrap an infallible value.

In `main.rs`, change the no-argument error label to:

```rust
eprintln!("UCI startup error: {error}");
```

and retain `ExitCode::FAILURE`.

- [ ] **Step 5: Run focused library and protocol tests**

```powershell
cargo test -p mf-uci --lib
cargo test -p mf-uci --test uci_protocol every_search_reports_an_nnue_evaluator
```

Expected: exit 0. Explicit runtime `EvalFile` failure still retains the old network.

- [ ] **Step 6: Verify a no-embedded binary fails gracefully outside the repo tree**

Run:

```powershell
$root = 'C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order'
cargo build -p mf-uci --bin manifold --no-default-features `
  --target-dir "$root\target\no-embedded"

$probe = Join-Path $env:TEMP "manifold-no-net-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path "$probe\bin" -Force | Out-Null
Copy-Item "$root\target\no-embedded\debug\manifold.exe" "$probe\bin\manifold.exe"
Push-Location $probe
try {
    $output = & "$probe\bin\manifold.exe" 2>&1
    $exit = $LASTEXITCODE
} finally {
    Pop-Location
}
$output
"exit=$exit"
```

Expected:

- nonzero exit;
- stderr contains `UCI startup error: manifold requires an NNUE network to evaluate`;
- stderr does not contain `panicked at`, `RUST_BACKTRACE`, or `index out of bounds`.

- [ ] **Step 7: Commit**

```powershell
git add crates/mf-uci/src/lib.rs crates/mf-uci/src/main.rs
git diff --cached --check
$message = @"
Return graceful NNUE startup errors

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 6: Replace scheduler-dependent depth and timing assertions

**Files:**
- Modify: `crates/mf-uci/tests/movetime_budget.rs`
- Modify: `crates/mf-uci/tests/uci_protocol.rs:1510-1725`
- Modify: `crates/mf-uci/tests/uci_protocol.rs:2284-2335`

**Interfaces:**
- Consumes: UCI `info ... time`, node counts, legal move generation, completed-iteration
  recognition
- Produces: tests whose wall-clock values are hang watchdogs only

- [ ] **Step 1: Make `timed_go` return conditions, not only depth**

Change the helper result to:

```rust
struct TimedGo {
    wall_elapsed: Duration,
    reported_elapsed: Duration,
    completed_iterations: u32,
    nodes: u64,
    bestmove: String,
}
```

During line collection:

- count completed `info depth` lines;
- retain the maximum reported `nodes`;
- retain the latest reported `time`;
- parse the move token from `bestmove`.

Wait for `readyok` before starting the timed region. Keep the 60-second receive deadline
only as a hang watchdog.

- [ ] **Step 2: Replace minimum-depth assertions**

Rewrite the three tests with these assertions:

```rust
#[test]
fn movetime_spends_a_meaningful_share_of_its_budget_and_returns_a_legal_move() {
    let sample = timed_go("go movetime 4000");
    assert!(sample.completed_iterations > 0);
    assert!(sample.nodes > 0);
    assert_legal_bestmove(&sample.bestmove);
    assert!(sample.reported_elapsed >= Duration::from_millis(2990));
    assert!(sample.wall_elapsed < Duration::from_secs(60));
}

#[test]
fn clock_management_spends_a_nontrivial_budget_and_returns_a_legal_move() {
    let sample = timed_go("go wtime 300000 btime 300000 winc 0 binc 0");
    assert!(sample.completed_iterations > 0);
    assert!(sample.nodes > 0);
    assert_legal_bestmove(&sample.bestmove);
    assert!(sample.reported_elapsed >= Duration::from_millis(500));
    assert!(sample.wall_elapsed < Duration::from_secs(60));
}

#[test]
fn fixed_depth_reports_the_requested_completed_iteration() {
    let sample = timed_go("go depth 14");
    assert_eq!(sample.completed_iterations, 14);
    assert!(sample.nodes > 0);
    assert_legal_bestmove(&sample.bestmove);
}
```

Add `assert_legal_bestmove` by parsing `FEN`, formatting every generated legal move,
and asserting the resulting vector contains the returned token. For the timed tests:

- `completed_iterations > 0`;
- `nodes > 0`;
- `bestmove` belongs to `generate_legal_moves` for `FEN`;
- engine-reported elapsed is at least 75% of exact `movetime` after overhead for
  `go movetime 4000`;
- engine-reported elapsed is at least 500 ms for the 300-second clock case;
- wall time is less than the 60-second watchdog.

Delete `assert!(depth >= 10, ...)`; search depth is scheduler/build dependent.

- [ ] **Step 3: Replace the startup-contaminated 50 ms wall assertion**

In `fifty_millisecond_clock_returns_legal_moves_without_overshoot`, retain the warmed
session and legality loop, but collect the latest engine-reported `time` field for each
sample. Assert:

```rust
assert!(reported < 80, "sample {sample} reported {reported} ms");
```

Use the 10-second receive deadline only as a hang watchdog. Remove the debug/release
wall-clock overshoot branches and their scheduler-specific commentary.

- [ ] **Step 4: Stop finite/interrupted searches on observed conditions**

For `finite_search_can_be_stopped_before_its_budget_expires`, issue `stop` after the
first completed iteration rather than requiring depth 2.

For `interrupted_iteration_does_not_duplicate_a_completed_depth`, wait until:

- at least two completed iterations have been observed; and
- the latest reported nodes are below the one-million-node budget.

Then send `stop` and retain the strict-increasing-depth assertion. The receive durations
remain generous hang watchdogs, not quality thresholds.

- [ ] **Step 5: Run the modified targets repeatedly**

Run:

```powershell
1..10 | ForEach-Object {
    cargo test -p mf-uci --test movetime_budget
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
1..10 | ForEach-Object {
    cargo test -p mf-uci --test uci_protocol fifty_millisecond_clock -- --nocapture
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo test -p mf-uci --test uci_protocol finite_search_can_be_stopped -- --nocapture
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo test -p mf-uci --test uci_protocol interrupted_iteration_does_not_duplicate -- --nocapture
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
```

Expected: all 20 target runs exit 0.

- [ ] **Step 6: Commit**

```powershell
git add crates/mf-uci/tests/movetime_budget.rs crates/mf-uci/tests/uci_protocol.rs
git diff --cached --check
$message = @"
Make UCI budget tests condition based

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 7: Add deterministic `mf-search` tablebase integration coverage

**Files:**
- Create if feasible: `crates/mf-search/tests/data/syzygy/README.md`
- Create if feasible: minimal `.rtbw` and `.rtbz` files under
  `crates/mf-search/tests/data/syzygy/`
- Create if feasible: `crates/mf-search/tests/tablebase_integration.rs`
- Modify if feasible: `crates/mf-search/src/search.rs` test module only

**Interfaces:**
- Consumes: concrete `mf_tb::Tablebases`, `search_with_callback`,
  `TranspositionTable::probe`
- Produces deterministic coverage for root DTZ, interior WDL, `tbhits`, TT depth bonus,
  and floor/ceiling behavior without environment variables

- [ ] **Step 1: Run the fixture feasibility gate**

Inspect the already-vendored Pyrrhic notice and the machine-local three-man files:

```powershell
$root = 'C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order'
Get-Content "$root\THIRD_PARTY_NOTICES\Pyrrhic.txt"
Get-ChildItem C:\Syzygy -Recurse -File -ErrorAction SilentlyContinue |
  Where-Object { $_.Name -in @('KQvK.rtbw','KQvK.rtbz','KRvK.rtbw','KRvK.rtbz') } |
  Select-Object FullName,Length
```

Pass only if one WDL/DTZ pair:

- exists or can be generated entirely from already-vendored code/data;
- has documented redistribution-compatible provenance;
- is small enough to review and commit (target: each file under 64 KiB).

If any condition fails, STOP this task before modifying production search code. Add a
section to the final execution report containing:

- attempted source/generation path;
- actual sizes and license gap;
- the concrete `Tablebases`/private `RootProbe` injection blocker;
- the separately approvable seam:

```rust
trait TablebaseProbe {
    fn max_pieces(&self) -> usize;
    fn probe_wdl(&self, position: &Position) -> Option<Wdl>;
    fn preserving_root_moves(&self, position: &Position) -> Option<Vec<Move>>;
}
```

Do not implement that trait in this execution without new approval.

- [ ] **Step 2: Vendor the minimal fixture and provenance if the gate passes**

Copy only the selected pair into `crates/mf-search/tests/data/syzygy/`. Write
`README.md` with:

- exact upstream/source path;
- SHA-256 of each file;
- material class;
- license basis;
- generation command if generated;
- statement that the files exist solely as deterministic tests.

Run:

```powershell
Get-FileHash crates/mf-search/tests/data/syzygy/* -Algorithm SHA256
```

Copy the hashes verbatim into the README.

- [ ] **Step 3: Write failing public integration tests**

Create `crates/mf-search/tests/tablebase_integration.rs` with a shared loader:

```rust
fn tables() -> Tablebases {
    Tablebases::new(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/syzygy")
            .display()
            .to_string(),
    )
    .expect("vendored Syzygy fixtures must load")
}
```

Add tests using the KQvK fixture:

```rust
#[test]
fn root_dtz_filters_the_search_to_verdict_preserving_moves() {
    let tables = tables();
    let position =
        Position::from_fen("8/8/8/8/8/2k5/8/KQ6 w - - 0 1", false).unwrap();
    let preserving: Vec<_> = tables
        .probe_root(&position)
        .expect("KQvK root DTZ fixture should probe")
        .preserving_moves()
        .map(|entry| entry.mv)
        .collect();
    let result = search_with_tables(&position, &tables, 4);
    assert!(result.best_move.is_some_and(|mv| preserving.contains(&mv)));
}

#[test]
fn interior_wdl_changes_the_search_result_and_increments_tbhits() {
    let tables = tables();
    let position =
        Position::from_fen("8/8/8/8/8/2k5/8/K2Q4 b - - 0 1", false).unwrap();
    let result = search_with_tables(&position, &tables, 4);
    assert!(result.tbhits > 0);
    assert!(result.score < 0);
}

#[test]
fn exact_draw_wdl_is_reported_as_a_tablebase_hit() {
    let tables = tables();
    let position =
        Position::from_fen("8/8/8/8/8/2k5/8/KB6 w - - 0 1", false).unwrap();
    let result = search_with_tables(&position, &tables, 4);
    assert!(result.tbhits > 0);
    assert_eq!(result.score, 0);
}
```

Define `search_with_tables` with `search_with_callback`, fixed depth, one thread, a
fresh 1 MiB TT, and the position's one-key history. Each test must:

- load the existing copied `nets/main.nnue` through the same `OnceLock` pattern as
  `search_invariants.rs`;
- use fixed depth, one thread, a fresh 1 MiB TT, and halfmove clock zero for interior WDL;
- compare the returned root move against `Tablebases::probe_root(...).preserving_moves()`;
- assert `result.tbhits > 0` for interior probes;
- avoid `MF_SYZYGY_PATH` and skip logic.

- [ ] **Step 4: Observe failures before adding private assertions**

Run:

```powershell
cargo test -p mf-search --test tablebase_integration -- --nocapture
```

Expected: FAIL until fixture FENs and exact assertions are correctly selected. Use the
vendored tables themselves to derive expected WDL/preserving moves; do not hard-code a
move without first proving it is verdict-preserving.

- [ ] **Step 5: Add private TT bonus and floor/ceiling tests**

In the `search.rs` test module, use the same fixture directory and add a private helper
that constructs the existing `SearchContext`, attaches `context.tablebases`, calls
`pvs` directly at `ply = 1`, and returns `(score, tt_entry, context.tb_hits)`. Then add:

```rust
const WIN_FEN: &str = "8/8/8/8/8/2k5/8/KQ6 w - - 0 1";
const LOSS_FEN: &str = "8/8/8/8/8/2k5/8/K2Q4 b - - 0 1";

#[test]
fn syzygy_wdl_entries_receive_the_six_ply_tt_depth_bonus() {
    let probe = direct_tablebase_pvs(WIN_FEN, 4, -1, 0, false);
    assert_eq!(probe.score, TABLEBASE_SCORE - 1);
    assert_eq!(probe.entry.bound, Bound::Lower);
    assert_eq!(
        tt_entry_depth(probe.entry.depth),
        4 + SYZYGY_TT_DEPTH_BONUS
    );
    assert_eq!(probe.tb_hits, 1);
}

#[test]
fn noncutting_syzygy_win_sets_a_search_floor() {
    let probe = direct_tablebase_pvs(WIN_FEN, 4, -INFINITY, INFINITY, true);
    assert!(probe.score >= TABLEBASE_SCORE - 1);
    assert_eq!(probe.tb_hits, 1);
}

#[test]
fn noncutting_syzygy_loss_sets_a_search_ceiling() {
    let probe = direct_tablebase_pvs(LOSS_FEN, 4, -INFINITY, INFINITY, true);
    assert!(probe.score <= -(TABLEBASE_SCORE - 1));
    assert_eq!(probe.tb_hits, 1);
}
```

For the depth test:

- use a halfmove-zero interior position;
- search at an exact known depth;
- probe the TT with `position.repetition_key()`;
- decode with private `tt_entry_depth`;
- assert `entry_depth == requested_depth + SYZYGY_TT_DEPTH_BONUS`.

For floor/ceiling:

- assert the returned score is respectively not below/not above the tablebase
  floor/ceiling;
- assert `tbhits > 0`;
- use the explicit wide PV windows in the code above so the WDL result cannot cut off
  immediately.

- [ ] **Step 6: Run all deterministic tablebase tests**

```powershell
cargo test -p mf-search tablebase -- --nocapture
cargo test -p mf-search --test tablebase_integration -- --nocapture
```

Expected: exit 0 with no environment-dependent skips.

- [ ] **Step 7: Commit, or record the STOP**

If feasible:

```powershell
git add crates/mf-search/src/search.rs `
  crates/mf-search/tests/tablebase_integration.rs `
  crates/mf-search/tests/data/syzygy
git diff --cached --check
$message = @"
Add deterministic search tablebase tests

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

If stopped, make no tablebase-test commit and proceed to Task 8 only after recording the
four required STOP facts for the final report.

---

### Task 8: Finish the targeted plan-007 repository hygiene

**Files:**
- Modify: `AGENTS.md`
- Modify: `README.md`
- Modify: `.gitignore`
- Modify: `plans/007-ci-and-repo-hygiene.md`
- Modify: `plans/README.md`

**Interfaces:**
- Consumes: live workspace members and `mf-uci` UCI option/command list
- Produces: accurate live maps and a tracked policy that keeps run state under
  `experiments/<run>/`

- [ ] **Step 1: Add failing documentation consistency checks**

Run these checks before editing and save the failing output in the task notes:

```powershell
rg -n "stubs|config\.json|unsupported commands|mf-tb|SyzygyPath|ponder|mtbench" `
  AGENTS.md README.md
git check-ignore -v experiments\probe\results.md experiments\probe\games.pgn config.json
```

Expected current defects:

- stale stub/config/unsupported-command claims;
- missing or incomplete `mf-tb`/`SyzygyPath` map;
- root `config.json` and experiment PGN policy not expressed in tracked ignore rules.

- [ ] **Step 2: Update the live crate and UCI maps**

In both `AGENTS.md` and `README.md`, state:

- `mf-tb`: Syzygy WDL/DTZ discovery and probing used by UCI/search/datagen;
- `mf-tune`: implemented SPSA tuner with checkpoint/resume and process-driven matches;
- `mf-lab`: implemented corrhist-regression/experiment tooling, not a stub;
- UCI commands include `uci`, `isready`, `ucinewgame`, `setoption`, `position`,
  `go` time/depth/nodes/mate/searchmoves/ponder/infinite/perft forms, `ponderhit`,
  `stop`, `d`, `eval`, `bench`, `mtbench`, and `quit`;
- options include `Threads`, `Hash`, `MultiPV`, `Ponder`, `UCI_Chess960`, `EvalFile`,
  and `SyzygyPath`, with the live defaults copied from the handshake;
- unsupported `go` arguments are diagnosed/ignored according to the current parser,
  not described as silently disabling pondering.

Document that match run state belongs under an explicit
`experiments/<run-name>/` output directory created by `harness/run_match.ps1`; there is
no live root `config.json` contract.

- [ ] **Step 3: Add only narrow tracked ignores**

Change `.gitignore` to:

```gitignore
/target/
/nets/*.nnue
/.aidex/
/config.json
/experiments/**/games.pgn
```

Do not add `/experiments/`, `*.log`, `*.txt`, or repository-wide `*.pgn`.

- [ ] **Step 4: Update plan-007 bookkeeping**

In `plans/007-ci-and-repo-hygiene.md`, mark the live-map/root-run-state/narrow-ignore
subset complete and explicitly leave these items pending:

- CI workflow;
- fresh-clone net provisioning decision;
- harness absolute-path cleanup not requested by this execution.

In `plans/README.md`, set plan 007 to:

```text
IN PROGRESS (targeted live-map/run-state/ignore hygiene complete; CI and net provisioning pending)
```

- [ ] **Step 5: Verify the documentation against live output**

Run:

```powershell
$uci = @"
uci
quit
"@ | cargo run -q -p mf-uci --bin manifold
$uci | Select-String 'option name (Threads|Hash|MultiPV|Ponder|UCI_Chess960|EvalFile|SyzygyPath)'
rg -n "stubs containing|Root `config\.json`|pondering is effectively limited" AGENTS.md README.md
git check-ignore -v experiments\probe\results.md experiments\probe\games.pgn config.json
```

Expected:

- all seven named options appear in live output and docs;
- stale-claim search has no matches;
- `results.md` is not ignored;
- only `games.pgn` and root `config.json` are ignored.

- [ ] **Step 6: Commit**

```powershell
git add AGENTS.md README.md .gitignore plans/007-ci-and-repo-hygiene.md plans/README.md
git diff --cached --check
$message = @"
Refresh the live repository map

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 9: Gate correctness work and freeze the match binary

**Files:**
- Create locally, do not commit: `baselines/recommended-order/manifold.exe`
- Create locally, do not commit:
  `experiments/2026-08-18-recommended-order/binary-sha256.txt`

**Interfaces:**
- Consumes: Tasks 2-8
- Produces: one immutable executable used in both arms of every match

- [ ] **Step 1: Run focused crate tests**

```powershell
cargo test -p mf-datagen
cargo test -p mf-tb
cargo test -p mf-search
cargo test -p mf-uci
```

Expected: exit 0. If Task 7 stopped, no environment-free tablebase target is expected.

- [ ] **Step 2: Run formatting, lint, and workspace tests**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mf-core --features force-magic
```

Expected: all exit 0.

- [ ] **Step 3: Build and verify deterministic release bench**

```powershell
cargo build --release -p mf-uci --bin manifold
$first = cargo run --release -q -p mf-uci --bin manifold -- bench |
  Select-String 'Nodes searched:'
$second = cargo run --release -q -p mf-uci --bin manifold -- bench |
  Select-String 'Nodes searched:'
$first
$second
if ("$first" -ne "$second") { throw "release bench signature changed between runs" }
```

Expected: the two signatures are identical.

- [ ] **Step 4: Freeze the exact binary and hash**

```powershell
New-Item -ItemType Directory baselines\recommended-order -Force | Out-Null
Copy-Item target\release\manifold.exe baselines\recommended-order\manifold.exe
New-Item -ItemType Directory experiments\2026-08-18-recommended-order -Force | Out-Null
Get-FileHash baselines\recommended-order\manifold.exe -Algorithm SHA256 |
  Format-List | Out-File `
    experiments\2026-08-18-recommended-order\binary-sha256.txt `
    -Encoding utf8
```

Do not rebuild or overwrite this binary until all primary and validation matches finish.
Both `-ACmd` and `-BCmd` below name this same file.

---

### Task 10: Measure `UseTtMoveHistory`

**Files:**
- Create: `experiments/2026-08-18-recommended-order/UseTtMoveHistory/results.md`
- Generated by harness:
  - `console.txt`
  - `fastchess.log`
  - `run-metadata.txt`
  - ignored `games.pgn`

**Interfaces:**
- Consumes: frozen `baselines/recommended-order/manifold.exe`
- Produces: admissible primary point estimate for enabling `UseTtMoveHistory`

- [ ] **Step 1: Run the primary same-binary match**

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseTtMoveHistory\primary `
  -Purpose 'Measure UseTtMoveHistory=true against the shipped false default in the recommended-order binary' `
  -AName tt-move-history-on `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseTtMoveHistory=true' `
  -BName tt-move-history-off `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081801
```

Expected: harness exit 0, 300 games, affinity enabled, concurrency 8, zero forfeits,
crashes, illegal moves, and “No output”.

- [ ] **Step 2: Write the primary result**

Create `results.md` with exact:

- binary SHA-256 and commit from `run-metadata.txt`;
- full command;
- W/L/D, pentanomial, Elo point estimate and error;
- guardrail counts;
- decision: retain default if point estimate `<= 0`; request Task 12 validation if `> 0`.

- [ ] **Step 3: Commit the primary evidence**

```powershell
git add experiments/2026-08-18-recommended-order/UseTtMoveHistory
git diff --cached --check
$message = @"
Record the ttMove-history match

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 11: Measure `UseCorrplexity`

**Files:**
- Create: `experiments/2026-08-18-recommended-order/UseCorrplexity/results.md`
- Generated by harness under the same directory

**Interfaces:**
- Consumes: the same frozen binary as Task 10
- Produces: admissible primary point estimate for enabling `UseCorrplexity`

- [ ] **Step 1: Run the primary same-binary match**

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCorrplexity\primary `
  -Purpose 'Measure UseCorrplexity=true against the shipped false default in the recommended-order binary' `
  -AName corrplexity-on `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseCorrplexity=true' `
  -BName corrplexity-off `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081802
```

Expected: the same admissibility conditions as Task 10.

- [ ] **Step 2: Write and commit the primary result**

Use the same result fields and decision rule as Task 10.

```powershell
git add experiments/2026-08-18-recommended-order/UseCorrplexity
git diff --cached --check
$message = @"
Record the corrplexity match

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

---

### Task 12: Measure `UseCaptureLMR` and validate positive alternatives

**Files:**
- Create: `experiments/2026-08-18-recommended-order/UseCaptureLMR/results.md`
- Optionally create validation subdirectories for any positive primary alternative
- Modify the corresponding Task 10/11 result documents if validation is required

**Interfaces:**
- Consumes: all three primary point estimates and the same frozen binary
- Produces: final evidence-backed default decisions

- [ ] **Step 1: Run the `UseCaptureLMR=false` primary match**

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCaptureLMR\primary `
  -Purpose 'Measure UseCaptureLMR=false against the shipped true default in the recommended-order binary' `
  -AName capture-lmr-off `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseCaptureLMR=false' `
  -BName capture-lmr-on `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081803
```

Expected: the same admissibility conditions as Tasks 10-11.

- [ ] **Step 2: Write and commit the primary capture-LMR result**

For this toggle, alternative `false`:

- point estimate `<= 0` means retain shipped `true`;
- point estimate `> 0` requires independent validation before turning it off.

```powershell
git add experiments/2026-08-18-recommended-order/UseCaptureLMR
git diff --cached --check
$message = @"
Record the capture LMR match

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

- [ ] **Step 3: Validate each positive primary alternative**

Run only the validation commands whose primary alternative point estimate was positive.
Use identical binaries/settings and the independent seeds below.

`UseTtMoveHistory`:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseTtMoveHistory\validation `
  -Purpose 'Validate the positive UseTtMoveHistory=true primary point estimate with an independent seed' `
  -AName tt-move-history-on `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseTtMoveHistory=true' `
  -BName tt-move-history-off `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081811
```

`UseCorrplexity`:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCorrplexity\validation `
  -Purpose 'Validate the positive UseCorrplexity=true primary point estimate with an independent seed' `
  -AName corrplexity-on `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseCorrplexity=true' `
  -BName corrplexity-off `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081812
```

`UseCaptureLMR` alternative off:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCaptureLMR\validation `
  -Purpose 'Validate the positive UseCaptureLMR=false primary point estimate with an independent seed' `
  -AName capture-lmr-off `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseCaptureLMR=false' `
  -BName capture-lmr-on `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081813
```

Any invalid validation run is discarded and rerun in full with the same assigned seed.

- [ ] **Step 4: Calculate pooled point estimates and finalize result documents**

For every validated toggle, combine primary and validation game counts and pentanomial
counts, then calculate/report the pooled Elo point estimate with the same fastchess
rating convention used by the individual runs.

Change a default only when:

```text
primary alternative point estimate > 0
AND validation alternative point estimate >= 0
AND pooled alternative point estimate > 0
```

Otherwise retain the current default. Record the exact three values and decision in the
toggle's `results.md`.

- [ ] **Step 5: Commit validation evidence**

```powershell
git add experiments/2026-08-18-recommended-order
git diff --cached --check
$message = @"
Validate positive option match results

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

If no primary alternative was positive, skip this commit and state that no validation
run was required.

---

### Task 13: Apply only evidence-backed defaults

**Files:**
- Modify only if justified: `crates/mf-search/src/search.rs`
- Modify only if justified: `crates/mf-uci/src/lib.rs`
- Modify only if justified: `crates/mf-uci/tests/bench_cli.rs`
- Modify only if justified: `crates/mf-search/tests/search_invariants.rs`
- Modify: affected experiment `results.md`

**Interfaces:**
- Consumes: Task 12 final decisions
- Produces: live defaults and deterministic anchors consistent with evidence

- [ ] **Step 1: Add failing default-vector/handshake assertions**

For each justified flip:

- update/add the default-vector assertion in `search_invariants.rs`;
- update/add the UCI advertised default assertion in `bench_cli.rs` or
  `uci_protocol.rs`;
- run the focused assertion before changing production defaults.

Example for an enabled false-default toggle:

```rust
assert!(SearchOptions::default().use_tt_move_history);
```

Expected: FAIL until the production default changes.

If no toggle passes the policy, do not edit production/default tests; record
“all defaults retained” and skip to Task 14.

- [ ] **Step 2: Flip only the justified default fields and UCI strings**

Change only:

- the relevant `SearchOptions::default()` boolean(s);
- the matching `UCI_RESPONSE` default string(s);
- comments that explicitly describe the old default.

Do not alter the heuristic implementation or any numeric parameter.

- [ ] **Step 3: Recollect deterministic bench anchors**

Run the existing bench CLI tests to list failures:

```powershell
cargo test -p mf-uci --test bench_cli -- --nocapture
```

For every changed anchor:

1. reproduce it twice with the exact option vector named by the test;
2. update the expected number only when both runs match;
3. explain in the test comment which default flip reaches that tree;
4. leave anchors outside the toggle's dependency path unchanged.

- [ ] **Step 4: Run focused default tests**

```powershell
cargo test -p mf-search --test search_invariants
cargo test -p mf-uci --test bench_cli
cargo test -p mf-uci --test uci_protocol
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```powershell
git add crates/mf-search/src/search.rs `
  crates/mf-uci/src/lib.rs `
  crates/mf-uci/tests/bench_cli.rs `
  crates/mf-search/tests/search_invariants.rs `
  experiments/2026-08-18-recommended-order
git diff --cached --check
$message = @"
Apply evidence-backed search defaults

Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>
"@
git commit -m $message
```

Stage only paths that actually changed. If all defaults are retained, make no empty
default commit.

---

### Task 14: Run full gates and final review

**Files:**
- No intended source changes

**Interfaces:**
- Consumes: complete branch
- Produces: evidence required to claim completion

- [ ] **Step 1: Run formatting and lint**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both exit 0.

- [ ] **Step 2: Run all correctness tests**

```powershell
cargo test --workspace
cargo test -p mf-core --features force-magic
```

Expected: both exit 0.

- [ ] **Step 3: Run release validations**

```powershell
cargo test --release -p mf-core --test perft
$bench1 = cargo run --release -q -p mf-uci --bin manifold -- bench |
  Select-String 'Nodes searched:'
$bench2 = cargo run --release -q -p mf-uci --bin manifold -- bench |
  Select-String 'Nodes searched:'
$bench1
$bench2
if ("$bench1" -ne "$bench2") { throw "release bench signature is not deterministic" }
```

Expected: release perft exits 0 and bench signatures match.

- [ ] **Step 4: Review the complete diff and commit history**

```powershell
git status --short
git diff --check
git diff --stat 5021efd..HEAD
git log --oneline 5021efd..HEAD
git diff 5021efd..HEAD -- `
  crates/mf-search/src/search.rs `
  crates/mf-uci/src/lib.rs `
  crates/mf-uci/src/main.rs `
  crates/mf-tb/src/probe.rs `
  crates/mf-datagen/src/record.rs `
  AGENTS.md README.md .gitignore plans
```

Confirm:

- no uncommitted source or documentation changes;
- ignored match PGNs are the only permitted untracked run artifacts;
- no new heuristic;
- no broad `search.rs` refactor;
- no unsupported default flip;
- no production tablebase seam if Task 7 stopped;
- no generated `target/` or local `.nnue` file staged;
- every commit has the required co-author trailer;
- no push occurred.

- [ ] **Step 5: Request final code review**

Invoke the repository's code-review workflow against fixed point `5021efd`, covering both:

- standards: repository instructions, TDD evidence, match-harness rules, focused commits;
- spec: all nine ordered deliverables and every STOP/default policy.

Resolve only high-confidence findings. Re-run the affected focused tests and then the
full gates after any fix.

- [ ] **Step 6: Prepare the completion report**

Report:

- each correctness change and commit;
- tests converted from scheduler quality to conditions/budgets;
- tablebase fixture tests or the exact STOP report;
- repository hygiene changes;
- all primary/validation match commands and point estimates;
- final defaults and policy rationale;
- full gate commands and outcomes;
- final branch HEAD;
- explicit statement: branch was not pushed.
