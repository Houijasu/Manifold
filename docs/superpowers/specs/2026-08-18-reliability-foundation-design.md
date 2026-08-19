# Reliability Foundation Design

## Purpose

Execute the approved improvement audit in dependency order: make the test suite safe and
non-silent, establish CI with a pinned NNUE fixture, repair datagen and UCI failure handling,
make measurement and release artifacts trustworthy, then add default-off telemetry before
attempting strength changes.

The isolated worktree is
`C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation` on branch
`execution/reliability-foundation`. Local ignored fixtures copied into it are:

- `nets/main.nnue`, 111,261,604 bytes, SHA-256
  `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A`;
- `tools/testdata/`, six perft fixtures.

After those fixtures were copied, `cargo test --workspace` passed on the unmodified branch.

## Considered approaches

### 1. Reliability first, then measurement, then experiments

Fix unsafe and scheduler-sensitive tests, make required fixtures explicit, add CI, repair
failure propagation, and make artifacts reproducible. Add telemetry only after those gates
are dependable. This is the selected approach because every later performance or training
decision depends on trustworthy tests and provenance.

### 2. Strength work first

Immediately test checked-node evaluation removal and threshold SEE. This may find Elo or NPS
sooner, but it leaves silent fixture skips, machine-sized test allocations, and unreliable
metadata underneath the evidence. Rejected.

### 3. Build the complete training flywheel first

Add chunk storage, exact shuffle, holdouts, training orchestration, and candidate promotion
as one project. This is too broad before Manifold has a first locally trained candidate and
would create infrastructure whose requirements have not been exercised. Rejected. This
design implements only the current data-loss and liveness fixes.

## Fixed execution order

1. remove the machine-sized Hash allocation test and the remaining scheduler-sensitive
   50 ms timing assertion;
2. make NNUE-dependent tests fail when an explicitly configured fixture is missing;
3. add Windows and Linux CI with checksum-pinned NNUE provisioning;
4. make datagen worker and sink failures cancel generation promptly;
5. make self-play output crash-safe and resumable;
6. keep malformed finite `go` requests finite and propagate search-thread output failures;
7. repair match-source provenance and repository-relative harness paths;
8. keep experimental PGO output separate from the ordinary release binary;
9. add and verify a portable baseline x86-64 release build;
10. add default-off single-thread search telemetry;
11. use telemetry to decide whether checked-node evaluation and threshold SEE merit measured
    same-binary experiments.

Items 1-10 are implementation work. Item 11 is an experiment gate, not permission to change
search defaults. Candidate-model promotion, opening curricula, and a general training
orchestrator remain deferred until resumable self-play is proven and a candidate net exists.

## Design decisions

### 1. Safe, deterministic tests

Delete the process-level test that reads the advertised machine-derived Hash maximum and
hands it back to the engine. `TranspositionTable::new` eagerly writes the allocation, so on
this machine the test can touch 8 GiB. The contract is already covered without allocation by
unit tests proving that `hash_option_line()` and `resize_hash(..., maximum, ...)` use the same
maximum. Strengthen those unit assertions if needed; do not replace the dangerous allocation
with a smaller process allocation that no longer tests the advertised value.

Delete the repeated 50 ms clock test's `< 80 ms` scheduler assertion. Keep deterministic unit
coverage for computed clock limits and process-level checks for:

- a legal `bestmove`;
- at least one completed iteration where the budget permits it;
- engine-reported elapsed time within the engine's computed hard limit plus only a generous
  hang watchdog;
- prompt handling of zero and negative clocks.

No global `--test-threads=1` policy is added. Serializing the workspace would hide rather than
remove cross-process timing dependence and materially lengthen CI.

### 2. Required NNUE fixtures

The test helper contract becomes:

- if `MF_NNUE_TEST_NET` is set, the path must exist and load or the test fails;
- local developer runs may continue using `nets/main.nnue`;
- tests that exercise NNUE/search behavior may skip only when neither source exists;
- CI always sets `MF_NNUE_TEST_NET`, so missing or malformed provisioning is a hard failure.

This preserves convenient no-net development for fixture-independent crates while preventing
a green CI run from silently omitting search, NNUE, and bench coverage. Real Syzygy-file tests
remain optional because the repository has no redistributable table set; deterministic search
tablebase semantics are already covered through the private probe seam.

### 3. CI and network provisioning

Create `.github/workflows/ci.yml` with Windows and Ubuntu jobs. Both jobs:

1. check out the repository;
2. create `nets/`;
3. download the exact network from a versioned GitHub release asset;
4. verify SHA-256
   `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A`;
5. export `MF_NNUE_TEST_NET` to that path;
6. run `cargo fmt --all -- --check`;
7. run `cargo clippy --workspace --all-targets -- -D warnings`;
8. run `cargo test --workspace`.

One job additionally runs `cargo test -p mf-core --features force-magic`. CI does not run
benchmarks, release perft, fastchess, or hardware-performance assertions.

The release asset must exist before the workflow is pushed. Uploading the 106 MiB network is
an external publishing action and therefore requires explicit approval at that step. The
asset name, release tag, size, and checksum are documented in `README.md`.

### 4. Datagen cancellation and ordered output

Replace the shared `PendingGames` mutex plus `yield_now` polling loop with a scoped standard
library MPSC channel:

- workers claim canonical game indices from the existing atomic counter;
- each worker sends `Result<GameOutput, String>`;
- the coordinator stores out-of-order successes in a `BTreeMap<u64, GameOutput>`;
- the coordinator emits only `next_to_emit`, preserving byte-identical output across thread
  counts;
- an `Arc<AtomicBool>` cancellation flag is checked before claiming and after finishing each
  game;
- the first worker, queue, or sink error sets cancellation and is returned after all scoped
  workers join;
- a disconnected channel before all games arrive is an explicit error, never a spin loop.

No async runtime, crossbeam dependency, or reusable worker framework is introduced.

### 5. Crash-safe self-play resume

Extend only self-play generation with `--resume`, reusing the JSONL converter's existing
checkpoint principles:

- the output is checkpointed only after a whole canonical game batch is flushed;
- checkpoint every 100 completed games and once at normal completion, bounding replay after
  a crash without growing a per-game sidecar;
- the sidecar records completed game count, output byte length, total games, node budget,
  thread-independent seed inputs, score bound, Syzygy path string, and cumulative
  `GenerateStats` needed for the final summary;
- the sidecar is an append-only log with one complete checkpoint per line; resume uses the
  last valid complete line and ignores a crash-truncated tail;
- resume refuses a mismatched command/configuration;
- resume truncates the output to the checkpointed byte boundary;
- generation restarts at the checkpointed canonical game index, so fixed seed output equals
  an uninterrupted run;
- a successful complete run removes the sidecar.

Add an internal `generate_from(config, first_game, ...)` entry point; keep the current public
`generate(...)` behavior as the zero-based wrapper. Do not add chunk directories, compression,
shuffling, or model promotion in this phase.

### 6. Finite `go` parsing and asynchronous failures

Track whether a finite-budget keyword was present separately from whether its value parsed.
If at least one finite keyword was present, no finite value parsed, and `infinite` was not
explicitly requested, diagnose the invalid argument and use a one-node emergency budget. The
engine therefore returns a legal move promptly instead of silently entering analysis. Valid
clock or node/depth values continue to control mixed requests.

Search-thread writes must no longer discard errors. Keep the callback signatures unchanged:
record the first `io::Error` in shared search state, set the stop flag, and have the search
thread return `io::Result<()>`. `ActiveSearch::stop_and_join` returns that result, and the UCI
loop propagates it whenever it joins a search. A panic becomes `io::ErrorKind::Other`.

This design does not add an input multiplexer merely to interrupt a blocking `BufRead`; output
failure stops search promptly and is surfaced at the next join point or EOF.

### 7. Match provenance and portable harness paths

`harness/run_match.ps1` derives the repository root from `$PSScriptRoot`. Default book and
fastchess paths derive from that root.

Metadata distinguishes:

- driver repository commit;
- binary SHA-256;
- source commit for each binary when its path is inside a Git worktree;
- `unknown` source when a copied third-party/baseline binary has no containing repository.

The driver commit must never be labelled as the binaries' source commit. Keep all existing
affinity, concurrency, memory, forfeit, crash, and illegal-move guardrails unchanged.

### 8. PGO isolation

The ordinary `target/release/manifold.exe` remains the verified non-PGO shipping build.
Instrumented and profile-use builds use a dedicated Cargo target directory. The final
experimental result is copied to `target/pgo/manifold-pgo.exe` with metadata and its NPS
verdict. Running the PGO script must not replace the shipping binary even when PGO passes the
node-signature gate.

### 9. Portable x86-64 release

Add `harness/build_portable.ps1`:

- use a dedicated Cargo target directory;
- compile with `RUSTFLAGS=-C target-cpu=x86-64`;
- copy the embedded-net executable to `target/portable/manifold.exe`;
- record Git commit, rustc identity, flags, binary and embedded-net SHA-256, and bench
  signature;
- require the portable and native binaries to produce the same bench node signature;
- run portable start-position perft 5 and require 4,865,609 nodes;
- run `cargo test -p mf-core --features force-magic`;
- inspect disassembly with the pinned toolchain's `llvm-objdump` when available and fail on
  BMI2 instructions; absence of a disassembler is a reported verification failure, not a
  silently skipped proof.

The native default remains unchanged.

### 10. Default-off search telemetry

Extend the existing default-off `mf-search` `instrumentation` feature, which already forwards
to the thread-local `mf-core` SEE and `mf-nnue` update counters. Production builds therefore
continue to contain no counters or atomics.

The initial single-thread counter set is deliberately small:

- interior nodes and qsearch nodes;
- checked interior nodes;
- static NNUE evaluations at interior and qsearch nodes;
- TT cutoffs;
- razoring, RFP, NMP, ProbCut, LMP, futility, history, and SEE-pruning attempts/cutoffs;
- LMR reductions, reduced-search fail-highs, and full-depth re-searches.

Provide reset/snapshot functions and a `search_profile` example that runs fixed positions,
prints stable `key=value` rows, and also reports the existing SEE/NNUE counters. Do not expose
telemetry through UCI or change `SearchResult`.

### 11. Experiment gate

After telemetry is validated:

1. measure the share and cost of checked interior nodes;
2. only if meaningful, add a same-binary toggle that replaces checked-node static evaluation
   and stored TT static eval with `UNEVALUATED_STATIC_EVAL`;
3. require deterministic tests, bench/NPS measurement, and fixed-time paired matches before a
   default change;
4. re-plan threshold SEE without the rejected king-only legality shortcut;
5. require `see_ge(position, move, threshold)` to be exactly equivalent to
   `static_exchange_evaluation(position, move) >= threshold` across the exhaustive/random
   oracle before measuring it.

No search default changes are part of the reliability implementation plan.

## Testing and commit policy

Every production behavior fix follows red, observed failure, minimal green implementation,
and focused regression verification. Configuration-only CI steps are validated with YAML
parsing and an actual GitHub Actions run.

Each numbered design area is a focused commit or small reviewable commit pair. Before every
commit:

- inspect `git status --short`;
- run `git diff --check`;
- stage exact paths only;
- include
  `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`.

Final local gates are:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mf-core --features force-magic
cargo test --release -p mf-core --test perft
release bench twice with identical 37,420-node signatures
```

The final review must separate correctness, test/CI reliability, artifact provenance,
performance instrumentation, and deferred work. No push, release upload, or other remote
write occurs without explicit authorization for that action.
