# Reliability Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Make Manifold's tests, CI, datagen, UCI error handling, measurement provenance,
release builds, and search profiling reliable before attempting further strength changes.

**Architecture:** Land independent reliability fixes in dependency order. Use standard-library
channels and append-only checkpoints for datagen, explicit join results for asynchronous UCI
errors, repository-relative PowerShell helpers for artifact provenance, dedicated Cargo target
directories for experimental builds, and the existing default-off instrumentation feature for
search counters.

**Tech Stack:** Rust 2024, Rust standard library, Cargo workspace tests, PowerShell 7,
GitHub Actions, GitHub release assets, existing fastchess and benchmark harnesses.

## Global Constraints

- Work only in
  `C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation` on branch
  `execution/reliability-foundation`.
- Keep the local ignored fixtures `nets/main.nnue` and `tools/testdata/` present.
- The NNUE fixture is 111,261,604 bytes with SHA-256
  `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A`.
- Use TDD for every production behavior change: observe the regression test fail, add the
  smallest implementation, then rerun focused and neighboring tests.
- Add no runtime dependency or async runtime.
- Add no search heuristic and change no search default.
- Keep the native Windows default `target-cpu=native`.
- Do not push, create a GitHub release, or upload the network without explicit authorization
  for that exact external action.
- Every commit uses a short imperative sentence-case subject and this exact trailer:
  `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`.
- Before every commit, run `git status --short`, `git diff --check`, inspect the staged diff,
  and stage exact paths only.

---

## File map

### Test and CI reliability

- `crates/mf-uci/tests/uci_protocol.rs`
  - remove the machine-sized Hash allocation;
  - retain legal-move coverage for a 50 ms clock without scheduler timing assertions.
- `crates/mf-uci/tests/bench_cli.rs`
  - fail when an explicitly configured NNUE path is absent.
- `crates/mf-search/tests/search_invariants.rs`
  - fail rather than skip for a missing explicit NNUE path.
- `crates/mf-search/tests/smp.rs`
  - honor `MF_NNUE_TEST_NET` and fail when it is missing.
- `crates/mf-search/src/thread_pool.rs`
  - same explicit-fixture contract for its unit tests.
- `crates/mf-datagen/tests/bulletformat_round_trip.rs`
  - same explicit-fixture contract.
- `.github/workflows/ci.yml`
  - Windows and Ubuntu gates with checksum-pinned network download.
- `README.md`, `plans/007-ci-and-repo-hygiene.md`, `plans/README.md`
  - provisioning and CI status.

### Datagen

- `crates/mf-datagen/src/generate.rs`
  - cancellation-aware ordered coordinator;
  - `generate_from` restart index;
  - cumulative progress callback.
- `crates/mf-uci/src/datagen_cli.rs`
  - self-play `--resume`;
  - append-only checkpoint log.

### UCI

- `crates/mf-uci/src/lib.rs`
  - emergency finite budget for malformed finite values;
  - asynchronous writer/search-thread errors returned through join.
- `crates/mf-uci/tests/uci_protocol.rs`
  - malformed finite `go` process regressions.

### Harness and releases

- `harness/provenance.ps1`
  - repository root and binary source-attestation helpers.
- `harness/provenance.tests.ps1`
  - framework-free PowerShell assertions.
- `harness/run_match.ps1`
  - repository-relative paths and unambiguous metadata.
- `harness/build_pgo.ps1`
  - dedicated target directories and non-shipping PGO artifact.
- `harness/build_portable.ps1`
  - baseline x86-64 artifact and verification.
- `harness/README.md`, `README.md`
  - current build and provenance contracts.

### Search profiling

- `crates/mf-search/src/instrumentation.rs`
  - thread-local `SearchCounters`.
- `crates/mf-search/src/lib.rs`
  - instrumentation-only public reset/snapshot exports.
- `crates/mf-search/src/search.rs`
  - counter increments at existing decisions.
- `crates/mf-search/examples/search_profile.rs`
  - stable `key=value` output.
- `crates/mf-search/Cargo.toml`
  - register the feature-gated example.

---

### Task 1: Remove unsafe and scheduler-sensitive test behavior

**Files:**
- Modify: `crates/mf-uci/tests/uci_protocol.rs:1030-1066`
- Modify: `crates/mf-uci/tests/uci_protocol.rs:1665-1721`
- Modify: `crates/mf-uci/tests/bench_cli.rs:210-225`
- Modify: `crates/mf-search/tests/search_invariants.rs:15-37`
- Modify: `crates/mf-search/tests/smp.rs:65-79`
- Modify: `crates/mf-search/src/thread_pool.rs:684-699`
- Modify: `crates/mf-datagen/tests/bulletformat_round_trip.rs:147-162`

**Interfaces:**
- Consumes: `MF_NNUE_TEST_NET`, local `nets/main.nnue`
- Produces: explicit fixture failures in CI and no process test that allocates the machine
  maximum Hash

- [x] **Step 1: Prove explicit missing fixtures currently skip**

Run:

```powershell
Set-Location 'C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation'
$env:MF_NNUE_TEST_NET = "$PWD\missing.nnue"
cargo test -p mf-search --test search_invariants deterministic_single_thread -- --exact
$before = $LASTEXITCODE
Remove-Item Env:MF_NNUE_TEST_NET
if ($before -ne 0) { throw 'Expected the old helper to skip and exit zero' }
```

Expected: exit 0 with a `SKIPPED:` diagnostic. That zero exit is the defect.

- [x] **Step 2: Make every explicit path authoritative**

Use this pattern in each helper that currently skips an explicit missing path:

```rust
let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
let path = explicit_path.clone().map_or_else(
    || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
    PathBuf::from,
);
if !path.is_file() {
    assert!(
        explicit_path.is_none(),
        "MF_NNUE_TEST_NET requires an existing network file: {}",
        path.display()
    );
    eprintln!("SKIPPED: search tests need {}", path.display());
    return None;
}
```

In `bench_cli.rs`, make `bench_network_path()` panic on a missing explicit path rather than
returning it:

```rust
if let Some(path) = std::env::var_os("MF_NNUE_TEST_NET").map(PathBuf::from) {
    assert!(
        path.is_file(),
        "MF_NNUE_TEST_NET requires an existing network file: {}",
        path.display()
    );
    return Some(path);
}
```

- [x] **Step 3: Verify the explicit path now fails**

Run the Step 1 command again.

Expected: nonzero exit with `MF_NNUE_TEST_NET requires an existing network file`.

- [x] **Step 4: Remove the dangerous Hash process test**

Delete
`the_advertised_hash_maximum_is_accepted_rather_than_refused` from
`crates/mf-uci/tests/uci_protocol.rs`. Keep the unit test
`the_advertised_hash_maximum_is_the_one_the_engine_enforces`, which proves the handshake and
resize path share `max_hash_mebibytes()` without allocating it.

- [x] **Step 5: Convert the 50 ms stress loop to one legality check**

Rename the test to `fifty_millisecond_clock_returns_a_legal_move`, remove the `for sample in
0..50` loop and the `reported < 80` assertion, then send one search:

```rust
engine.send(&format!("position fen {fen}"));
engine.send("go wtime 50 btime 50 winc 0 binc 0");
let bestmove = engine
    .receive_until(Duration::from_secs(10), |line| line.starts_with("bestmove "))
    .expect("the 50 ms clock should answer within the hang watchdog");
let mv = bestmove
    .strip_prefix("bestmove ")
    .expect("bestmove prefix")
    .split_whitespace()
    .next()
    .expect("bestmove carries a move");
assert!(legal_moves.iter().any(|legal| legal == mv));
```

- [x] **Step 6: Run focused and full UCI tests**

Run:

```powershell
cargo test -p mf-uci --test uci_protocol fifty_millisecond_clock_returns_a_legal_move -- --exact
cargo test -p mf-uci --test bench_cli
cargo test -p mf-search --test search_invariants deterministic_single_thread -- --exact
cargo test -p mf-search --test smp
cargo test -p mf-datagen --test bulletformat_round_trip
```

Expected: all pass with the real local network.

- [x] **Step 7: Commit**

Commit subject: `Make NNUE tests explicit and scheduler independent`

---

### Task 2: Add checksum-pinned CI provisioning

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `README.md:64-81`
- Modify: `plans/007-ci-and-repo-hygiene.md`
- Modify: `plans/README.md`

**Interfaces:**
- Consumes: GitHub release
  `nnue-e8449b6/manifold-main-e8449b6.nnue`
- Produces: `MF_NNUE_TEST_NET` on Windows and Ubuntu CI jobs

- [x] **Step 1: Obtain explicit authorization for the network release**

Ask permission to create tag/release `nnue-e8449b6` and upload the local
`nets/main.nnue` as `manifold-main-e8449b6.nnue`. Do not perform the upload without that
authorization.

- [x] **Step 2: Create and verify the release asset**

After authorization, create the release and upload the asset. Download it to a temporary path
and verify:

```powershell
$asset = Join-Path $env:TEMP 'manifold-main-e8449b6.nnue'
Invoke-WebRequest `
  -Uri 'https://github.com/Houijasu/Manifold/releases/download/nnue-e8449b6/manifold-main-e8449b6.nnue' `
  -OutFile $asset
(Get-FileHash -Algorithm SHA256 $asset).Hash
(Get-Item $asset).Length
```

Expected hash:
`E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A`.
Expected length: `111261604`.

- [x] **Step 3: Create the CI workflow**

Use two explicit jobs rather than a shell-heavy matrix so checksum commands stay native:

```yaml
name: CI

on:
  push:
  pull_request:

env:
  NNUE_URL: https://github.com/Houijasu/Manifold/releases/download/nnue-e8449b6/manifold-main-e8449b6.nnue
  NNUE_SHA256: E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A

jobs:
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - name: Provision NNUE
        shell: pwsh
        run: |
          New-Item -ItemType Directory -Path nets -Force | Out-Null
          Invoke-WebRequest -Uri $env:NNUE_URL -OutFile nets/main.nnue
          $actual = (Get-FileHash -Algorithm SHA256 nets/main.nnue).Hash
          if ($actual -ne $env:NNUE_SHA256) { throw "NNUE checksum mismatch: $actual" }
          "MF_NNUE_TEST_NET=$PWD\nets\main.nnue" >> $env:GITHUB_ENV
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
      - run: cargo test -p mf-core --features force-magic

  ubuntu:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Provision NNUE
        shell: bash
        run: |
          mkdir -p nets
          curl --fail --location --retry 3 "$NNUE_URL" --output nets/main.nnue
          echo "$NNUE_SHA256  nets/main.nnue" | sha256sum --check -
          echo "MF_NNUE_TEST_NET=$PWD/nets/main.nnue" >> "$GITHUB_ENV"
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
```

- [x] **Step 4: Document provisioning**

Add the release URL, exact checksum, and download examples for PowerShell and POSIX shells to
README Development. Mark plan 007's CI and provisioning checkboxes complete only after both
jobs pass.

- [x] **Step 5: Validate locally**

Run:

```powershell
python -c "import pathlib, yaml; yaml.safe_load(pathlib.Path('.github/workflows/ci.yml').read_text())"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mf-core --features force-magic
```

If PyYAML is unavailable, parse with Ruby:

```powershell
ruby -e "require 'yaml'; YAML.load_file('.github/workflows/ci.yml')"
```

- [x] **Step 6: Commit**

Commit subject: `Add checksum-pinned CI gates`

Do not claim CI is green until the branch is pushed and both jobs finish successfully.

---

### Task 3: Make ordered datagen cancellation-aware

**Files:**
- Modify: `crates/mf-datagen/src/generate.rs:1-225`

**Interfaces:**
- Produces:
  - `pub fn generate_from<S, P>(config, first_game, network, tablebases, sink, progress)`
  - `pub fn GenerateStats::merge(&mut self, other: &Self)`
  - prompt worker, channel, and sink error propagation
- Preserves: canonical byte order across thread counts

- [x] **Step 1: Add failing coordinator tests**

Extract one private generic coordinator with this interface:

```rust
fn generate_with_worker<S, W, F, P, G>(
    config: GenerateConfig,
    first_game: u64,
    make_worker: F,
    play: P,
    sink: S,
    progress: G,
) -> Result<GenerateStats, String>
where
    S: FnMut(&[Record]) -> Result<(), String>,
    F: Fn() -> Result<W, String> + Sync,
    P: Fn(&mut W, u64) -> Result<GameOutput, String> + Sync,
    G: FnMut(u64, &GenerateStats) -> Result<(), String>,
```

First add tests against the wished-for interface:

```rust
#[test]
fn a_worker_error_is_returned_instead_of_waiting_for_the_missing_game() {
    let config = GenerateConfig { games: 8, threads: 4, ..GenerateConfig::default() };
    let error = generate_with_worker(
        config,
        0,
        || Ok(()),
        |_, index| {
            if index == 0 { Err("worker failed at game 0".to_string()) }
            else { Ok(empty_output(index)) }
        },
        |_| Ok(()),
        |_, _| Ok(()),
    )
    .expect_err("worker failure must end generation");
    assert_eq!(error, "worker failed at game 0");
}

#[test]
fn a_sink_error_cancels_before_the_run_claims_every_game() {
    let claimed = AtomicU64::new(0);
    let sink_failed = AtomicBool::new(false);
    let config = GenerateConfig { games: 10_000, threads: 4, ..GenerateConfig::default() };
    let error = generate_with_worker(
        config,
        0,
        || Ok(()),
        |_, index| {
            claimed.fetch_add(1, Ordering::Relaxed);
            if index != 0 {
                while !sink_failed.load(Ordering::Relaxed) {
                    std::thread::yield_now();
                }
            }
            Ok(empty_output(index))
        },
        |_| {
            sink_failed.store(true, Ordering::Relaxed);
            Err("disk full".to_string())
        },
        |_, _| Ok(()),
    )
    .expect_err("sink failure must end generation");
    assert_eq!(error, "disk full");
    assert!(claimed.load(Ordering::Relaxed) < config.games);
}
```

`empty_output(index)` returns a `GameOutput` with no records and `stats.games = 1`.

- [x] **Step 2: Run the tests and observe failure**

Run:

```powershell
cargo test -p mf-datagen a_worker_error_is_returned_instead_of_waiting_for_the_missing_game
```

Expected: compile failure because `generate_with_worker` does not exist.

- [x] **Step 3: Implement the standard-library coordinator**

Use `std::sync::mpsc`, `BTreeMap`, and `Arc<AtomicBool>`. Each worker:

1. checks cancellation;
2. claims an index;
3. calls `play(index)`;
4. sends `Result<GameOutput, String>`;
5. exits on send failure or an error result.

The coordinator inserts successes by index, emits consecutive ready games, and returns the
first error. On any error it sets cancellation before leaving the receive loop. A channel
disconnect before `config.games` is complete returns
`"datagen workers stopped before every game was produced"`.

- [x] **Step 4: Wrap real game state and add restart index**

Production `W` is `(TranspositionTable, SharedHistory)`. `play_game` remains unchanged and is
wrapped in `Ok(...)`.

Make the existing `GenerateStats::merge(&mut self, other: &Self)` public so the CLI can add
checkpointed statistics to the resumed segment without duplicating the field logic.

Keep:

```rust
pub fn generate<S>(...) -> Result<GenerateStats, String>
```

as a wrapper calling `generate_from(config, 0, ..., sink, |_| Ok(()))`.

Add:

```rust
pub fn generate_from<S, P>(
    config: GenerateConfig,
    first_game: u64,
    network: &Network,
    tablebases: Option<&Tablebases>,
    sink: S,
    progress: P,
) -> Result<GenerateStats, String>
where
    S: FnMut(&[Record]) -> Result<(), String>,
    P: FnMut(u64, &GenerateStats) -> Result<(), String>,
```

`progress(completed_game_count, &stats)` runs after each emitted whole game.

- [x] **Step 5: Run datagen tests**

Run:

```powershell
cargo test -p mf-datagen
cargo test -p mf-datagen --test bulletformat_round_trip
```

Expected: all pass, including existing thread-count byte identity.

- [x] **Step 6: Commit**

Commit subject: `Cancel datagen promptly on worker and sink errors`

---

### Task 4: Add crash-safe self-play resume

**Files:**
- Modify: `crates/mf-uci/src/datagen_cli.rs:61-390`
- Modify: `crates/mf-uci/src/datagen_cli.rs:680-733`
- Modify: `crates/mf-datagen/src/generate.rs`

**Interfaces:**
- Consumes: `generate_from(..., first_game, ..., progress)`
- Produces:
  - self-play `--resume`;
  - `<out>.progress` append-only checkpoint log;
  - uninterrupted/resumed byte identity

- [x] **Step 1: Add checkpoint codec tests**

Define:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
struct GenerationCheckpoint {
    games_completed: u64,
    output_bytes: u64,
    games: u64,
    nodes: u64,
    seed: u64,
    score_bound: i32,
    syzygy_path: String,
    stats: GenerateStats,
}
```

Use one tab-separated `key=value` line. Add tests proving:

- round trip preserves every field and both statistics arrays;
- `read_generation_checkpoint` returns the last valid line;
- a final truncated line is ignored;
- a config mismatch is rejected with the mismatched key named.

- [x] **Step 2: Observe the parser tests fail**

Run:

```powershell
cargo test -p mf-uci datagen_cli::tests::generation_checkpoint -- --nocapture
```

Expected: compile failure because the checkpoint type/functions do not exist.

- [x] **Step 3: Implement append-only checkpoints**

Add:

```rust
const GENERATION_CHECKPOINT_GAMES: u64 = 100;

fn append_generation_checkpoint(
    path: &Path,
    checkpoint: &GenerationCheckpoint,
) -> Result<(), String>
```

Open with `OpenOptions::new().create(true).append(true)`, write one line plus `\n`, then
`flush()`. `read_generation_checkpoint` scans lines and keeps the last line that parses
completely.

- [x] **Step 4: Permit `--resume` for self-play**

Add `resume: bool` to `GenerateOptions`. Remove the parser rejection that says resume applies
only to JSONL. Keep conversion behavior unchanged.

On resume:

1. require the sidecar;
2. validate games, nodes, seed, score bound, and exact Syzygy path string;
3. truncate output to `output_bytes`;
4. set `first_game = games_completed`;
5. start with checkpoint cumulative stats.

Thread count is deliberately not part of the identity.

- [x] **Step 5: Checkpoint only complete games**

Use `generate_from`'s progress callback. Maintain cumulative statistics by merging the prior
checkpoint stats with current-run stats. After every 100 completed games, and at normal
completion:

1. flush the output;
2. append a checkpoint naming the current byte length;
3. flush the sidecar.

After a successful complete run, remove the sidecar. On error, retain it.

- [x] **Step 6: Add an end-to-end interrupted/resumed test**

Extract:

```rust
struct GenerationRunPolicy {
    checkpoint_every: u64,
    stop_after: Option<u64>,
}
```

Production uses `{ checkpoint_every: 100, stop_after: None }`. The test uses one-game
checkpoints and stops after two games.

Generate the same six-game fixed-seed corpus twice:

1. uninterrupted to `whole.bullet`;
2. interrupted after game two to `resumed.bullet`, then rerun with `--resume`.

Assert byte identity, equal final summary counts, and absence of the progress sidecar after
completion.

- [x] **Step 7: Run focused tests**

Run:

```powershell
cargo test -p mf-uci datagen_cli::tests -- --nocapture
cargo test -p mf-datagen
```

- [x] **Step 8: Commit**

Commit subject: `Make self-play generation resumable`

---

### Task 5: Keep malformed finite go requests finite and surface async errors

**Files:**
- Modify: `crates/mf-uci/src/lib.rs:236-645`
- Modify: `crates/mf-uci/src/lib.rs:1491-1655`
- Modify: `crates/mf-uci/tests/uci_protocol.rs`

**Interfaces:**
- Produces:
  - one-node emergency budget for malformed finite values;
  - `ActiveSearch::stop_and_join(self) -> io::Result<()>`;
  - `JoinHandle<io::Result<()>>`

- [x] **Step 1: Add malformed finite parser tests**

Add unit tests:

```rust
#[test]
fn invalid_finite_values_use_an_emergency_node_budget_instead_of_infinite_search() {
    for arguments in [
        &["depth", "banana"][..],
        &["nodes", "banana"][..],
        &["movetime", "banana"][..],
        &["mate", "banana"][..],
    ] {
        let (parameters, ignored) =
            GoParameters::parse(arguments).expect("finite keyword is recognized");
        assert_eq!(parameters.nodes, Some(1), "{arguments:?}");
        assert!(!parameters.infinite, "{arguments:?}");
        assert!(!ignored.is_empty(), "{arguments:?}");
    }
}

#[test]
fn one_valid_finite_value_wins_over_an_invalid_sibling() {
    let (parameters, _) =
        GoParameters::parse(&["depth", "banana", "nodes", "20"]).unwrap();
    assert_eq!(parameters.nodes, Some(20));
    assert!(!parameters.infinite);
}
```

- [x] **Step 2: Observe parser failure**

Run:

```powershell
cargo test -p mf-uci invalid_finite_values_use_an_emergency_node_budget
```

Expected: `parameters.infinite` is true and `nodes` is `None`.

- [x] **Step 3: Implement the emergency budget**

Track `finite_keyword_seen` and `finite_value_parsed`. Set the latter only after a successful
parse of depth, nodes, movetime, either clock, or mate. Before the bare-go infinite fallback:

```rust
if finite_keyword_seen && !finite_value_parsed && !parameters.infinite {
    parameters.nodes = Some(1);
}
```

Valid mixed requests remain unchanged.

- [x] **Step 4: Add a failing writer regression**

In the `lib.rs` test module, implement a writer that fails every write:

```rust
struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "test pipe closed"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "test pipe closed"))
    }
}
```

Run `run` with:

```rust
let input = Cursor::new(b"position startpos\ngo depth 8\nquit\n");
let error = run(input, BrokenWriter).expect_err("search output failure must escape run");
assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
```

Current behavior incorrectly returns `Ok(())`.

- [x] **Step 5: Return search-thread results**

Change:

```rust
handle: JoinHandle<io::Result<()>>
```

and make the spawned closure return `io::Result<()>`. Store the first callback write failure
as `(io::ErrorKind, String)` in shared state, set `search_stop`, and return it after search
completion. Do not change the callback signatures.

Make `ActiveSearch::stop_and_join` and `stop_active_search` return `io::Result<()>`; propagate
`?` at every join site and at EOF. Convert a thread panic to `io::Error::other("search thread
panicked")`.

- [x] **Step 6: Add process coverage for malformed go**

Send:

```text
position startpos
go depth banana
quit
```

Require one legal `bestmove`, an `info string ignoring ... depth banana` diagnostic, and clean
exit inside the ordinary watchdog.

- [x] **Step 7: Run UCI tests**

Run:

```powershell
cargo test -p mf-uci invalid_finite_values_use_an_emergency_node_budget
cargo test -p mf-uci search_output_failure -- --nocapture
cargo test -p mf-uci --test uci_protocol
```

- [x] **Step 8: Commit**

Commit subject: `Propagate UCI search failures and bound malformed go`

---

### Task 6: Correct match provenance and portable paths

**Files:**
- Create: `harness/provenance.ps1`
- Create: `harness/provenance.tests.ps1`
- Modify: `harness/run_match.ps1`
- Modify: `harness/README.md`

**Interfaces:**
- Produces:
  - `Get-ManifoldRepositoryRoot`
  - `Get-BinarySourceAttestation`
  - metadata fields `Driver commit`, `Source A/B`, and `Source mode A/B`

- [x] **Step 1: Write failing PowerShell assertions**

`harness/provenance.tests.ps1` dot-sources `provenance.ps1` and throws unless:

```powershell
$root = Get-ManifoldRepositoryRoot
if ($root -ne (Resolve-Path (Join-Path $PSScriptRoot '..')).Path) { throw 'wrong root' }

$inside = Get-BinarySourceAttestation (Join-Path $root 'target\release\manifold.exe')
if ($inside.Mode -ne 'inferred-target-worktree') { throw 'target binary not inferred' }
if ($inside.Commit -ne (& git -C $root rev-parse HEAD).Trim()) { throw 'wrong commit' }

$outside = Get-BinarySourceAttestation (Join-Path $env:TEMP 'copied-engine.exe')
if ($outside.Mode -ne 'unattested' -or $outside.Commit -ne 'unknown') {
    throw 'outside binary must stay unattested'
}
```

- [x] **Step 2: Observe the script fail**

Run:

```powershell
pwsh -NoProfile -File harness/provenance.tests.ps1
```

Expected: failure because `provenance.ps1` does not exist.

- [x] **Step 3: Implement the minimal helpers**

`Get-ManifoldRepositoryRoot` resolves `Join-Path $PSScriptRoot '..'`.

`Get-BinarySourceAttestation` returns:

```powershell
[pscustomobject]@{ Commit = 'unknown'; Mode = 'unattested' }
```

unless the resolved binary is under the current root's `target` directory, in which case it
returns current worktree HEAD with mode `inferred-target-worktree`. If
`<binary>.source-commit` exists, its exact 40-hex SHA wins with mode `sidecar`.

- [x] **Step 4: Update `run_match.ps1`**

Dot-source the helper. Replace the hard-coded root and book with:

```powershell
[string]$Book = ''

# after param(...)
. (Join-Path $PSScriptRoot 'provenance.ps1')
$root = Get-ManifoldRepositoryRoot
$fc = Join-Path $root 'tools\fastchess\fastchess.exe'
if ([string]::IsNullOrWhiteSpace($Book)) {
    $Book = Join-Path $root 'tools\books\UHO_4060_v4.epd'
}
```

Write separate metadata:

```text
Driver commit:
Source A:
Source mode A:
SHA-256 A:
Source B:
Source mode B:
SHA-256 B:
```

Do not emit a generic `Commit:` label.

- [x] **Step 5: Verify helpers and a two-game smoke**

Copy the ignored local fastchess/book assets into the worktree, build release, then run one
paired round with the same binary in both arms. Require exit 0, zero forfeits, and metadata
whose source commit equals this worktree HEAD.

- [x] **Step 6: Commit**

Commit subject: `Make match provenance worktree aware`

---

### Task 7: Keep PGO experiments out of the shipping binary

**Files:**
- Modify: `harness/build_pgo.ps1`
- Modify: `harness/README.md`

**Interfaces:**
- Produces:
  - `target/pgo-build/baseline/release/manifold.exe`
  - `target/pgo-build/instrumented/release/manifold.exe`
  - `target/pgo-build/optimized/release/manifold.exe`
  - `target/pgo/manifold-pgo.exe`
- Preserves: `target/release/manifold.exe`

- [x] **Step 1: Add a failing preservation smoke**

Build the ordinary release binary and record its SHA-256. Run the current PGO script with one
bench run. Assert the ordinary release hash is unchanged. Current behavior fails because the
script rebuilds `target/release/manifold.exe`.

- [x] **Step 2: Parameterize the Cargo target directory**

Change `Invoke-CargoBuild` to accept `TargetDir`, set `CARGO_TARGET_DIR`, and return the
resulting executable path. Use separate directories for baseline, instrumented, and optimized
stages.

- [x] **Step 3: Publish only the experimental copy**

Copy baseline to `target/pgo/manifold-nopgo.exe` and optimized to
`target/pgo/manifold-pgo.exe`. Write `<artifact>.source-commit` sidecars and metadata including
both binary hashes and the profile hash.

- [x] **Step 4: Keep the node-signature and NPS gates**

Compare signatures using the dedicated paths. `-MeasureNps` compares the two `target/pgo`
copies. Update the completion message so it never calls the PGO artifact the shipping build.

- [x] **Step 5: Run the full PGO script**

Run:

```powershell
pwsh -NoProfile -File harness/build_pgo.ps1 -BenchRuns 3 -MeasureNps
```

Expected: signature 37,420 for both, ordinary release hash unchanged, and PGO result reported
without replacing the shipping binary.

- [x] **Step 6: Commit**

Commit subject: `Isolate PGO artifacts from release builds`

---

### Task 8: Add a portable baseline x86-64 build

**Files:**
- Create: `harness/build_portable.ps1`
- Modify: `harness/README.md`
- Modify: `README.md`
- Modify: `plans/008-portable-release-build.md`
- Modify: `plans/README.md`

**Interfaces:**
- Produces:
  - `target/portable/manifold.exe`
  - `target/portable/manifold.exe.source-commit`
  - `target/portable/build-metadata.txt`

- [x] **Step 1: Write the script around a dedicated target**

Set:

```powershell
$env:RUSTFLAGS = '-C target-cpu=x86-64'
$env:CARGO_TARGET_DIR = (Join-Path $root 'target\portable-build')
```

Build `mf-uci --release --bin manifold`, copy the executable to `target/portable`, and restore
both environment variables in `finally`.

- [x] **Step 2: Add deterministic gates**

The script must:

1. run native and portable `bench`;
2. parse `Nodes searched`;
3. require both equal 37,420;
4. run portable `perft 5` and require `4865609`;
5. run `cargo test -p mf-core --features force-magic`;
6. locate `llvm-objdump.exe` beside the pinned toolchain's `llvm-profdata.exe`;
7. disassemble the portable binary and reject BMI2 mnemonics matching
   `\b(pext|pdep|bzhi|mulx|sarx|shlx|shrx|rorx)\b`.

If `llvm-objdump` is absent, exit nonzero with the exact `rustup component add
llvm-tools-preview` instruction.

- [x] **Step 3: Write metadata and source sidecar**

Record full Git SHA, `rustc -vV`, flags, network checksum, binary checksum, bench signature,
perft signature, and disassembler path. Write the full SHA to
`manifold.exe.source-commit`.

- [x] **Step 4: Run the portable build**

Run:

```powershell
pwsh -NoProfile -File harness/build_portable.ps1
```

Expected: all gates pass and `target/release/manifold.exe` remains unchanged.

- [x] **Step 5: Document and update plan status**

Document native versus portable artifacts and mark plan 008 done only after the instruction
scan, bench identity, perft, and force-magic gates pass.

- [x] **Step 6: Commit**

Commit subject: `Add a verified portable x86-64 build`

---

### Task 9: Add default-off search telemetry

**Files:**
- Create: `crates/mf-search/src/instrumentation.rs`
- Create: `crates/mf-search/examples/search_profile.rs`
- Modify: `crates/mf-search/src/lib.rs`
- Modify: `crates/mf-search/src/search.rs`
- Modify: `crates/mf-search/Cargo.toml`

**Interfaces:**
- Produces under `feature = "instrumentation"`:
  - `SearchCounters`
  - `reset_search_counters()`
  - `search_counters()`

- [x] **Step 1: Write counter lifecycle tests**

Model the new module on `mf-core/src/instrumentation.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchCounters {
    pub interior_nodes: u64,
    pub qsearch_nodes: u64,
    pub checked_interior_nodes: u64,
    pub interior_static_evals: u64,
    pub qsearch_static_evals: u64,
    pub tt_cutoffs: u64,
    pub razoring_attempts: u64,
    pub razoring_cutoffs: u64,
    pub rfp_attempts: u64,
    pub rfp_cutoffs: u64,
    pub nmp_attempts: u64,
    pub nmp_cutoffs: u64,
    pub probcut_attempts: u64,
    pub probcut_cutoffs: u64,
    pub lmp_attempts: u64,
    pub lmp_cutoffs: u64,
    pub futility_attempts: u64,
    pub futility_cutoffs: u64,
    pub history_pruning_attempts: u64,
    pub history_pruning_cutoffs: u64,
    pub see_pruning_attempts: u64,
    pub see_pruning_cutoffs: u64,
    pub lmr_reductions: u64,
    pub reduced_fail_highs: u64,
    pub full_depth_researches: u64,
}
```

Test zero, accumulation, reset, and thread-local isolation.

- [x] **Step 2: Observe compile failure**

Run:

```powershell
cargo test -p mf-search --features instrumentation instrumentation -- --nocapture
```

Expected: module/types do not exist.

- [x] **Step 3: Implement the thread-local module**

Use `Cell<SearchCounters>` and:

```rust
pub(crate) fn record(update: impl FnOnce(&mut SearchCounters))
```

Export reset/snapshot from `lib.rs` only under the existing instrumentation feature.

- [x] **Step 4: Instrument existing decisions**

Add `#[cfg(feature = "instrumentation")]` record calls:

- at entry to `pvs` and `quiescence`;
- after `in_check` is known;
- immediately before an actual `context.static_eval(position)` call;
- immediately before each TT cutoff return;
- around razoring, RFP, NMP, and ProbCut eligibility/cutoff paths;
- in the move loop when history, LMP, futility, or SEE pruning is evaluated and when it
  continues;
- when a positive LMR reduction is searched;
- when the reduced result exceeds alpha;
- when a full-depth re-search is launched.

Do not rearrange conditions or compute values solely for counters.

- [x] **Step 5: Add a search smoke test**

With instrumentation enabled, run fixed-depth startpos and assert:

```rust
assert!(counters.interior_nodes > 0);
assert!(counters.qsearch_nodes > 0);
assert!(counters.interior_static_evals > 0);
assert!(counters.lmr_reductions > 0);
assert!(counters.checked_interior_nodes <= counters.interior_nodes);
assert!(counters.razoring_cutoffs <= counters.razoring_attempts);
```

Reset and verify a second search starts from zero.

- [x] **Step 6: Add `search_profile`**

Register:

```toml
[[example]]
name = "search_profile"
required-features = ["instrumentation"]
```

Profile the six bench positions at configurable depth and print one stable line per position:

```text
position=bench1 depth=7 nodes=... interior_nodes=... qsearch_nodes=... checked_interior_nodes=...
```

Print every `SearchCounters` field plus existing `SeeCounters` and `UpdateCounters`. Keep
wall-clock NPS informational and never use it in assertions.

- [x] **Step 7: Verify zero production impact**

Run:

```powershell
cargo test -p mf-search --features instrumentation
cargo run --release -p mf-search --features instrumentation --example search_profile -- 7
cargo run --release -p mf-uci --bin manifold -- bench
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: both uninstrumented benches report exactly 37,420 nodes. Inspect a release build
without the feature using `cargo tree -e features` and confirm instrumentation is absent.

- [x] **Step 8: Commit**

Commit subject: `Add default-off search event telemetry`

---

### Task 10: Run final gates, profile, and review

**Files:**
- Create: `experiments/2026-08-18-reliability-foundation/search-profile.txt`
- Create: `experiments/2026-08-18-reliability-foundation/README.md`
- Modify: plan status documents only if their done criteria were met

**Interfaces:**
- Consumes: all previous tasks
- Produces: validated branch and evidence-backed decision on whether to start checked-node
  evaluation or threshold-SEE experiments

- [x] **Step 1: Run formatting, lint, and workspace tests**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mf-core --features force-magic
```

- [x] **Step 2: Run authoritative release perft**

```powershell
cargo test --release -p mf-core --test perft
```

Expected: all eight tests pass.

- [x] **Step 3: Run bench twice**

```powershell
cargo run --release -p mf-uci --bin manifold -- bench
cargo run --release -p mf-uci --bin manifold -- bench
```

Expected: exactly 37,420 nodes both times.

- [x] **Step 4: Capture telemetry**

```powershell
cargo run --release -p mf-search --features instrumentation --example search_profile -- 7 |
    Tee-Object experiments/2026-08-18-reliability-foundation/search-profile.txt
```

The README records:

- commit and NNUE hash;
- command;
- checked interior nodes as a fraction of interior nodes;
- interior/qsearch static evaluations per 1,000 nodes;
- SEE calls/cycles and NNUE forward cycles;
- pruning attempt/cutoff rates;
- whether checked-node evaluation has enough measured ceiling to justify a toggle experiment.

- [x] **Step 5: Run standards and spec review**

Review the complete range from the design commit's parent through HEAD. Reject unsupported
claims, generated artifacts, default changes, and instrumentation present in default builds.

- [x] **Step 6: Commit evidence and bookkeeping**

Commit subject: `Record reliability foundation validation`

- [x] **Step 7: Stop before strength changes**

If telemetry justifies checked-node evaluation or threshold SEE, write a separate design and
implementation plan. Do not implement either experiment on this reliability branch without
that explicit plan and approval.
