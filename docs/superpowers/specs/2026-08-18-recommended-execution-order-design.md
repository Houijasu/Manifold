# Recommended Execution Order Design

## Purpose

Execute the approved reliability, test-quality, tablebase-coverage, repository-hygiene,
and measurement work in an order that keeps correctness changes separate from strength
experiments.

The isolated worktree is
`C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order` on branch
`execution/recommended-order`. Its copied local assets include `tools/testdata/` and
`nets/main.nnue`. The baseline `cargo test --workspace` passes in that worktree.

## Fixed scope and constraints

The work is ordered and single-purpose:

1. fix clocked ponder saturation;
2. trim each `SyzygyPath` segment;
3. reject malformed datagen material counts before position reconstruction;
4. make automatic NNUE startup failures graceful without changing runtime `EvalFile`;
5. replace scheduler-dependent depth/timing assertions with condition and budget checks;
6. add deterministic `mf-search` tablebase integration coverage, subject to the explicit
   fixture STOP condition below;
7. finish only the targeted plan-007 repository hygiene;
8. run three fixed-time same-binary option matches;
9. apply only justified defaults, run all gates, and review the resulting branch.

The execution must not:

- add a new search heuristic;
- broadly refactor `crates/mf-search/src/search.rs`;
- change any option default before its same-binary match evidence satisfies the policy in
  this design;
- compare thread counts at a fixed node budget;
- use `-use-affinity` when either engine has more than one thread;
- omit `-use-affinity` when both engines have one thread;
- push the branch without explicit user instruction.

Correctness and test-maintenance commits precede all strength experiments. Match results
must not be used to excuse a failing correctness gate.

## Design decisions

### 1. Clocked ponder saturation

#### Current failure

A clocked `go ponder` receives ordinary soft/hard limits but ignores them while the
ponder latch is armed. Because it is not marked `infinite`, iterative deepening stops at
the ordinary `DEFAULT_MAX_DEPTH`. The search worker returns, the pool marks the shared
stop flag, and the UCI search thread merely waits for `ponderhit` or `stop`. If
`ponderhit` arrives after this saturation point, there is no active search left to spend
the converted clock; the already-computed answer is emitted immediately.

#### Required behavior

While pondering, reaching the analysis ceiling must park without completing the search.
It must continue to suppress `bestmove`. A later:

- `ponderhit` rebases the clock and resumes/continues useful search until the converted
  clock budget expires;
- `stop` releases the best completed result as a ponder miss;
- `quit` or a new command still joins the search cleanly.

The smallest change is to treat an attached, armed `PonderState` as an analysis-depth
mode for the iterative-deepening ceiling and saturation loop, without changing normal
timed, node-limited, fixed-depth, or infinite behavior. The search must not spin through
duplicate ceiling iterations or emit an unbounded backlog of `info` lines. A condition
variable or equivalent blocking primitive on `PonderState` should wake the parked
worker on `ponderhit`/`abort`; a polling sleep is acceptable only if the existing
architecture makes the blocking primitive materially larger.

The clock base recorded by `ponderhit` remains authoritative. No ponder bonus or other
time-allocation tuning is introduced.

### 2. `SyzygyPath` normalization

`mf-tb::Store::new` already ignores whitespace-only segments but constructs non-empty
segments from the untrimmed text. Normalize each semicolon-delimited segment with
`str::trim` before `PathBuf::from`. Preserve semicolon splitting and all table discovery
semantics. Do not canonicalize paths or change error policy.

### 3. Datagen material-count validation

`Record::to_position` reconstructs an untrusted record by repeatedly calling
`Position::place_piece`. The non-pawn Zobrist material table supports at most
`MAX_MATERIAL_COUNT` (16) pieces of one color and non-pawn kind. A corrupt record can
contain 17 same-color knights, bishops, rooks, or queens; the seventeenth placement
indexes past the material table before validation can report the record.

Validate counts before constructing a `Position`. `Record::structural_errors` must gain a
specific error carrying color/opponent side, piece kind, found count, and supported
maximum. `Record::to_position` must return `None` for such a record before the first
`place_piece` call. Keep `Position::place_piece` unchanged: this task hardens the
untrusted datagen boundary, not the general `mf-core` mutator API.

### 4. Graceful automatic NNUE startup errors

Automatic resolution currently panics inside `EngineState::default`, which aborts UCI
startup in release builds when no usable network exists. Convert automatic resolution
and engine construction to `Result`:

- cache `Result<SharedNetworkResolution, String>` rather than panicking;
- construct `EngineState` through a fallible constructor;
- let `run` return an `io::Error` with the full network-resolution diagnostic;
- let `main` print a concise startup error to stderr and return failure.

The benchmark subcommands must continue returning their existing structured `String`
errors. Runtime `setoption name EvalFile` behavior is unchanged:

- a valid explicit file replaces the active network and clears evaluation-dependent
  search state;
- an invalid explicit file reports an `info string` and retains the previous network;
- resetting `EvalFile` to automatic resolution succeeds or reports an `info string`
  while retaining the previous network.

Automatic-resolution failure tests must use a binary built without `embedded-net` and
an isolated working directory with no discoverable `nets/main.nnue`; they must assert a
nonzero exit and a readable stderr diagnostic, never a panic/backtrace.

### 5. Condition- and budget-based tests

Tests may use generous wall-clock deadlines as hang watchdogs, but pass/fail assertions
must target engine conditions rather than scheduler outcomes.

Replace:

- `movetime_budget.rs` minimum depth assertions with proof that the engine reports
  iterations, returns a legal move, spends a meaningful fraction of the requested
  budget, and stays below a generous hard watchdog;
- startup-contaminated or pipe-latency timing assertions with a warmed UCI session and
  the engine-reported `time` field;
- tests that wait for a scheduler-dependent depth before issuing `stop` with a
  deterministic condition such as observing at least one completed iteration, a node
  budget, or a test-only state transition.

Do not weaken protocol properties: `bestmove` cardinality, legality, monotone completed
iterations, no answer before `stop`/`ponderhit`, and hard-budget enforcement remain
mandatory.

### 6. Deterministic tablebase integration tests

The required `mf-search` coverage is:

- root DTZ filtering keeps only root-verdict-preserving moves;
- interior WDL produces the expected exact/lower/upper result;
- successful probes increment and publish `tbhits`;
- WDL TT entries receive `SYZYGY_TT_DEPTH_BONUS`;
- non-cutting wins/losses install the expected floor/ceiling and constrain the returned
  score.

The preferred fixture is a tiny, redistributable in-repo WDL/DTZ table pair whose
provenance and license are documented. Tests must not depend on `MF_SYZYGY_PATH`,
`C:\Syzygy`, downloads, or machine-local tables.

#### Explicit STOP condition

The current search API accepts concrete `mf_tb::Tablebases`, and `RootProbe` cannot be
constructed outside `mf-tb`. If a genuine minimal WDL/DTZ fixture cannot be generated
from already-vendored, license-compatible material and kept small enough for the repo,
stop before altering production search code. Report:

1. the attempted fixture source/generation path;
2. the size and licensing blocker;
3. why the concrete `Tablebases`/private `RootProbe` boundary prevents deterministic
   injection.

The smallest seam requiring separate approval is a narrow tablebase-probe interface
owned at the `mf-search` boundary, implemented by `mf_tb::Tablebases`, returning only
the WDL verdict and root preserving moves needed by search. Tests could then supply a
deterministic fake. The seam must not alter probing semantics, search heuristics, or hot
path allocation behavior. Do not introduce it silently as part of test work.

### 7. Targeted plan-007 repository hygiene

Only the approved live-map and run-state cleanup is in scope; CI installation and broad
harness rewrites remain outside this execution.

- Update `AGENTS.md` and `README.md` to describe all live crates, including `mf-tb`,
  `mf-tune`, and `mf-lab`, and the real UCI command/option surface including pondering,
  MultiPV, `SyzygyPath`, bench, and mtbench.
- Replace the absent/stale root run-state concept by documenting
  `experiments/<run-name>/` as the location for harness-owned run state; ignore only the
  generated per-run PGN and any developer-owned root `/config.json`.
- Keep experiment evidence trackable. Do not ignore `experiments/` or broad extensions
  such as all `*.log`, `*.txt`, or `*.pgn` repository-wide.
- Update plan-007 bookkeeping to state that the targeted map/config/ignore subset is
  complete while CI/net provisioning remains pending.

### 8. Same-binary fixed-time matches

Build one release binary after all correctness and hygiene work. Every match uses that
same executable for both arms, one option difference, `Threads=1`, `Hash=64`,
`TC=8+0.08`, 150 paired rounds (300 games), the UHO book, and distinct recorded seeds.
`harness/run_match.ps1` must derive `-use-affinity -concurrency 8`.

Measure sequentially:

1. `UseTtMoveHistory=true` versus its shipped `false`;
2. `UseCorrplexity=true` versus its shipped `false`;
3. `UseCaptureLMR=false` versus its shipped `true`.

Do not run matches concurrently. Each run must complete with zero Manifold time
forfeits, crashes, illegal moves, and “No output” failures. Store the command,
binary hash, commit, seed, pentanomial result, Elo point estimate/error, and decision in
an experiment result document.

#### Default-change validation policy

The primary 300-game match is admissible only when the harness exits zero and all
guardrails pass.

- If the tested alternative's Elo point estimate is zero or negative, retain the
  current default.
- If it is positive, run a second independent 300-game validation with a new seed and
  otherwise identical settings.
- Change the default only when the validation point estimate is non-negative and the
  pooled point estimate across both runs is positive.
- Any invalid run is discarded and rerun from the beginning; it is never pooled.

For `UseCaptureLMR`, the tested alternative is `false`, so positive evidence supports
turning the current default off. For the other two toggles, positive evidence supports
turning them on.

Apply all justified default changes only after all three primary measurements and any
required validation matches finish. Re-pin bench/default-vector assertions from fresh
runs and explain every changed anchor. A match point estimate alone never justifies a
new heuristic or unrelated parameter change.

### 9. Final gates and review

After default decisions:

1. run focused tests for every touched crate;
2. run formatting and clippy;
3. run `cargo test --workspace`;
4. run `cargo test -p mf-core --features force-magic`;
5. run release bench twice and require identical signatures;
6. run release perft validation if the execution changes any move-generation behavior
   (none is currently planned);
7. review the complete branch diff for accidental production changes, stale
   documentation, generated artifacts, and unsupported claims.

The final report must distinguish:

- correctness changes;
- test-only changes;
- repository hygiene;
- experiment evidence;
- default decisions;
- commands and outcomes;
- any STOP condition encountered.

Local commits should be frequent, focused, sentence-case, and include the required
`Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`
trailer. Do not push.

## Risks and controls

- **Ponder deadlock:** use one latch owner, idempotent wakeups, and integration tests for
  `ponderhit`, `stop`, `quit`, and replacement `go`.
- **Ponder busy loop/info flood:** park at the ceiling; do not repeatedly search the
  same maximum depth.
- **Datagen panic before reporting:** count material first, then construct.
- **NNUE behavior regression:** isolate startup construction from runtime `EvalFile`
  replacement and retain-on-error semantics.
- **Flaky timing tests:** engine-reported budgets decide assertions; wall time is a
  watchdog only.
- **Tablebase test overreach:** honor the fixture STOP; do not smuggle in a broad search
  abstraction.
- **Invalid Elo evidence:** same binary, one option, fixed time, harness-enforced
  affinity/concurrency, zero-forfeit gate, independent validation before a default flip.
- **Repository bloat:** narrow root run-state ignore only; preserve experiment evidence.

## Completion criteria

The branch is ready for final review only when all non-stopped ordered tasks are
committed, all required gates are green, every default decision cites admissible match
evidence, and the diff contains no new heuristic, broad `search.rs` refactor, automatic
unsupported default change, or push.
