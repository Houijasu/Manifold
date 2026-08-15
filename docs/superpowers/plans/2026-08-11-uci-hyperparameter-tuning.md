# UCI hyperparameter tuning implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the approved Tier 0, Tier 1, and Tier 2 search/clock hyperparameters
through a dedicated tuning engine, classify weaker runtime values as Tier 3, then make
`mf-tune` consume the live binary contract and run legal, reproducible,
crash-recoverable SPSA campaigns.

**Architecture:** `mf-search` owns one declarative registry for spins, checks, metadata, and constraints. `manifold` presents only production options; `manifold-tune-engine` presents the expanded tuning surface and a canonical manifest. `mf-tune` attests that exact binary, projects SPSA onto its integer constraint domain, and records each batch in an append-only journal before deriving checkpoint and CSV views.

**Tech Stack:** Rust 2024, existing `mf-core`/`mf-search`/`mf-uci`/`mf-tune`, fastchess, `sha2 = "0.10"` for fingerprints, `windows-sys = "0.59"` for Windows Job Objects, `libc = "0.2"` on non-Windows targets for process groups, and standard-library process/filesystem primitives.

## Global constraints

- Follow `docs/superpowers/specs/2026-08-11-uci-hyperparameter-tuning-design.md`.
- Use an isolated git worktree for implementation. The current working tree contains unrelated user changes.
- Do not create a second search implementation or a duplicate parameter list in `mf-tune`.
- Keep `manifold` production option names/defaults unchanged.
- Preserve the release bench signature at 40,705 nodes when every parameter/check is at its shipped default.
- `Hash`, `Threads`, and `Move Overhead` are fixed campaign settings, not SPSA
  dimensions. `EvalFile` may be a hashed setting. `MultiPV` is fixed at 1; `Ponder` and
  `UCI_Chess960` are false; `SyzygyPath` is rejected.
- Keep NNUE, TT encoding, score sentinels, memory-layout constants, and Tier 3 research constants outside the tuning manifest.
- No Elo measurement is part of this implementation. The final smoke proves mechanics only.
- Apply the repository affinity rule exactly: Threads=1 uses affinity/concurrency 8; Threads>1 uses no affinity/concurrency 1.
- Each task starts with a failing test and ends with focused verification plus a commit.

## File map

### New files

- `crates/mf-search/src/options.rs`: declarative spin/check registry, metadata, constraints, defaults, lookup, and clamping setters.
- `crates/mf-uci/src/tuning_manifest.rs`: canonical tuning-manifest serializer.
- `crates/mf-uci/src/bin/manifold-tune-engine.rs`: thin tuning-engine launcher.
- `crates/mf-tune/src/manifest.rs`: live engine query, UCI attestation, campaign manifest, fingerprints, and compatibility checks.
- `crates/mf-tune/src/domain.rs`: linear/log coordinates, lattice quantization, cross-parameter projection, and actual SPSA deltas.
- `crates/mf-tune/src/journal.rs`: framed append-only records, recovery, and derived-view rebuild input.
- `crates/mf-tune/src/process.rs`: timeout-capable child process adapter used by engine attestation and fastchess.

### Main modified files

- `crates/mf-search/src/search.rs`: consume registry fields instead of hidden constants; correct movetime gating.
- `crates/mf-search/src/history.rs`: consume Tier 2 history/correction weights.
- `crates/mf-search/src/move_ordering.rs`: consume Tier 2 ordering weights.
- `crates/mf-search/src/lib.rs`: export registry interfaces.
- `crates/mf-search/tests/search_invariants.rs`: metadata, default identity, activation, and constraint tests.
- `crates/mf-uci/src/lib.rs`: flavor-aware UCI presentation and clock-allocation parameters.
- `crates/mf-uci/src/main.rs`: retain production entry behavior.
- `crates/mf-uci/Cargo.toml`: declare the tuning binary.
- `crates/mf-uci/tests/uci_protocol.rs`: production/tuning handshake and manifest agreement.
- `crates/mf-uci/tests/bench_cli.rs`: default identity for both binaries.
- `crates/mf-tune/Cargo.toml`: add SHA-256, remove linked engine metadata dependencies after migration.
- `crates/mf-tune/src/config.rs`: parse campaign intent, checks, and live-manifest-resolved dimensions.
- `crates/mf-tune/src/document.rs`: add strict boolean values for `[[option]]`.
- `crates/mf-tune/src/spsa.rs`: schedule only; delegate feasible points and actual deltas to `ParameterDomain`.
- `crates/mf-tune/src/batch.rs`: captured evidence, exact PGN/console validation, memory preflight.
- `crates/mf-tune/src/run.rs`: journal-driven state machine and recovery.
- `crates/mf-tune/src/checkpoint.rs`: derived cache with safe rotation.
- `crates/mf-tune/src/cli.rs`: live `init`, attested `run`, group/tier selection, strict resume.
- `crates/mf-tune/src/lib.rs`: export the new modules.
- `crates/mf-tune/tests/smoke_run.rs`: real release creation/resume/rebuild smoke.
- `README.md`: tuning-engine and campaign workflow.

---

### Task 1: Pin exact movetime behavior

**Files:**
- Modify: `crates/mf-search/src/search.rs` around `effective_soft_limit` and `time_scale_percent`
- Test: `crates/mf-search/tests/search_invariants.rs`
- Test: `crates/mf-uci/tests/uci_protocol.rs`

**Interfaces:**
- Consumes: existing `SearchLimits::use_clock_management`
- Produces: one predicate used by every adaptive time governor

```rust
fn adaptive_time_management_active(limits: &SearchLimits) -> bool {
    limits.use_clock_management && limits.soft_time.is_some()
}
```

- [ ] **Step 1: Add a failing search invariant**

Add `movetime_soft_limit_is_not_scaled_by_between_iteration_governors`. Build limits with equal soft/hard durations and `use_clock_management=false`, enable each of `UseTimeEffort`, `UseInterpolatedTimeManagement`, and `UseSearchAgainDepth`, and assert the effective soft limit remains the requested duration.

- [ ] **Step 2: Add a failing UCI regression**

Send `go movetime 120` under each time-manager check combination and assert elapsed
wall time remains inside the existing protocol tolerance. Do not try to observe
`IterationInfo.time_scale_percent`; UCI does not serialize that internal field.

- [ ] **Step 3: Run the focused failures**

```powershell
cargo test -p mf-search --test search_invariants movetime_soft_limit_is_not_scaled_by_between_iteration_governors -- --exact
cargo test -p mf-uci --test uci_protocol movetime_does_not_enter_adaptive_clock_management -- --exact
```

Expected: at least the legacy path fails because it currently scales any soft limit.

- [ ] **Step 4: Apply the minimal gate**

Route the legacy, effort, interpolated, and search-again decisions through `adaptive_time_management_active`. Do not change clock-derived searches.

- [ ] **Step 5: Verify time-management regressions**

```powershell
cargo test -p mf-search time_
cargo test -p mf-uci --test uci_protocol movetime
cargo test --release -p mf-uci --test bench_cli bench_reports_deterministic_nodes_time_and_nps -- --exact
```

- [ ] **Step 6: Commit**

```powershell
git add crates/mf-search/src/search.rs crates/mf-search/tests/search_invariants.rs crates/mf-uci/tests/uci_protocol.rs
git commit -m "Keep movetime outside adaptive management"
```

---

### Task 2: Create the declarative Tier 0 registry

**Files:**
- Create: `crates/mf-search/src/options.rs`
- Modify: `crates/mf-search/src/search.rs:263-435` and `SearchOptions`
- Modify: `crates/mf-search/src/lib.rs`
- Modify: `crates/mf-uci/src/lib.rs` handshake/setoption code
- Test: `crates/mf-search/tests/search_invariants.rs`
- Test: `crates/mf-uci/tests/uci_protocol.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionVisibility { Production, Tuning }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterScale { Linear, Log }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterTier { Tier0, Tier1, Tier2 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationExpr {
    Always,
    Check { name: &'static str, value: bool },
    Setting { name: &'static str, value: i32 },
    ClockManaged,
    All(&'static [ActivationExpr]),
    Any(&'static [ActivationExpr]),
}

pub struct SearchParameterSpec {
    pub name: &'static str,
    pub default: i32,
    pub legal_min: i32,
    pub legal_max: i32,
    pub tune_min: i32,
    pub tune_max: i32,
    pub quantum: i32,
    pub scale: ParameterScale,
    pub tier: ParameterTier,
    pub group: &'static str,
    pub visibility: OptionVisibility,
    pub unit: &'static str,
    pub activation: ActivationExpr,
    pub provenance: &'static str,
    get: fn(&SearchParameters) -> i32,
    set: fn(&mut SearchParameters, i32),
}

pub struct SearchCheckSpec {
    pub name: &'static str,
    pub default: bool,
    pub group: &'static str,
    pub provenance: &'static str,
    get: fn(&SearchOptions) -> bool,
    set: fn(&mut SearchOptions, bool),
}

pub enum SearchConstraintSpec {
    LessEqual { left: &'static str, right: &'static str },
    StrictLess { left: &'static str, right: &'static str },
    Positive { name: &'static str },
}

impl SearchConstraintSpec {
    pub fn is_satisfied_by(&self, parameters: &SearchParameters) -> bool;
}

pub fn validate_search_registry() -> Result<(), String>;
```

- [ ] **Step 1: Write registry completeness tests**

Add these exact tests:

```rust
#[test]
fn search_registry_names_are_unique_case_insensitively() {
    let mut names = std::collections::HashSet::new();
    for parameter in SEARCH_PARAMETERS {
        assert!(names.insert(parameter.name.to_ascii_lowercase()));
    }
    for check in SEARCH_CHECKS {
        assert!(names.insert(check.name.to_ascii_lowercase()));
    }
}

#[test]
fn every_parameter_has_a_valid_legal_and_recommended_domain() {
    for parameter in SEARCH_PARAMETERS {
        assert!(parameter.legal_min <= parameter.tune_min);
        assert!(parameter.tune_min <= parameter.default);
        assert!(parameter.default <= parameter.tune_max);
        assert!(parameter.tune_max <= parameter.legal_max);
        assert!(parameter.quantum > 0);
        assert_eq!((parameter.default - parameter.legal_min) % parameter.quantum, 0);
    }
}

#[test]
fn every_activation_and_constraint_reference_resolves() {
    validate_search_registry().expect("all metadata references should resolve");
}

#[test]
fn compound_activation_supports_any_all_settings_and_clock_mode() {
    validate_search_registry().expect(
        "singular OR, clock-managed single-PV, LMR effective-depth, and legacy-time metadata",
    );
}

#[test]
fn shipped_defaults_satisfy_every_search_constraint() {
    let parameters = SearchParameters::default();
    for constraint in SEARCH_CONSTRAINTS {
        assert!(constraint.is_satisfied_by(&parameters));
    }
}
```

Delete the arbitrary `20..=40` parameter-count assertion only after the new tests fail for missing metadata.

- [ ] **Step 2: Define the registry macro and move Tier 0 declarations**

Use one declaration entry per current spin:

```rust
spin! {
    rfp_margin_per_depth: "RfpMarginPerDepth" = 95,
    legal 20..=300,
    tune 60..=160,
    quantum 1,
    scale Linear,
    tier Tier0,
    group "rfp",
    visibility Production,
    unit "cp/ply",
    activation Check { name: "UseRFP", value: true },
    provenance "RFP_MARGIN_PER_DEPTH";
}
```

The macro must generate `SearchParameters`, `Default`, `SEARCH_PARAMETERS`, lookup, getter, and clamping setter. Copy the exact Tier 0 recommended ranges from the design document.

- [ ] **Step 3: Move search checks into metadata**

Move `SearchOptions` to `options.rs` and generate `SEARCH_CHECKS` for every `Use*` field. Leave `multi_pv` and non-tuning runtime data as ordinary fields.

- [ ] **Step 4: Generate UCI check and Tier 0 spin lines**

Remove handwritten `Use*` entries from `UCI_RESPONSE`. Generate check lines from `SEARCH_CHECKS` and production-visible spin lines from `SEARCH_PARAMETERS`.

- [ ] **Step 5: Route setoption through registry setters**

Replace the large check-name match with case-insensitive registry lookup. Preserve existing diagnostics and clamping behavior.

- [ ] **Step 6: Prove the production handshake did not change**

Add a golden list test containing all current production option lines in current order. Compare the complete option subset, not only individual containment.

- [ ] **Step 7: Verify Tier 0 identity**

```powershell
cargo test -p mf-search --test search_invariants search_registry
cargo test -p mf-uci --test uci_protocol every_search_check_and_parameter_is_advertised_once
cargo test --release -p mf-uci --test bench_cli setting_every_tunable_parameter_to_its_default_reproduces_the_shipped_signature -- --exact
```

- [ ] **Step 8: Commit**

```powershell
git add crates/mf-search/src/options.rs crates/mf-search/src/search.rs crates/mf-search/src/lib.rs crates/mf-search/tests/search_invariants.rs crates/mf-uci/src/lib.rs crates/mf-uci/tests/uci_protocol.rs
git commit -m "Centralize search option metadata"
```

---

### Task 3: Wire Tier 1 direct parameters

**Files:**
- Modify: `crates/mf-search/src/options.rs`
- Modify: `crates/mf-search/src/search.rs`
- Modify: `crates/mf-search/src/move_ordering.rs`
- Modify: `crates/mf-uci/src/lib.rs` clock allocation
- Test: `crates/mf-search/tests/search_invariants.rs`
- Test: unit tests in `crates/mf-search/src/move_ordering.rs`
- Test: `crates/mf-uci/tests/bench_cli.rs`

**Interfaces:**
- Produces all Tier 1 entries listed in the design, with `visibility=Tuning`
- Adds fixed-point fields:

```rust
pub search_again_reduction_permille: i32,       // default 750
pub search_again_growth_cutoff_permille: i32,   // default 500
```

- [ ] **Step 1: Add a default-identity test for hidden constants**

For each Tier 1 parameter, construct `SearchOptions::default()`, assert its value equals the current literal, set it back through the registry, and confirm an aggregate fixed-depth node signature is unchanged.

- [ ] **Step 2: Add targeted reachability tests**

Use small positions/limits to prove at least one parameter from each group changes its
intended calculation: NMP depth, pruning max depth, qsearch margin, quiet-check SEE
threshold in `move_ordering.rs`, aspiration widening, legacy time factor, clock
allocation, and search-again depth.

- [ ] **Step 3: Replace hidden literals with registry fields**

Use `context.options.parameters.<field>` in search. For UCI clock allocation, pass `&SearchParameters` into `clock_limits` and use the clock-allocation fields there.

- [ ] **Step 4: Replace search-again fractions**

```rust
let reduction = search_again_counter
    .saturating_add(1)
    .saturating_mul(parameters.search_again_reduction_permille as u32)
    / 1000;

let increase = elapsed.as_millis().saturating_mul(1000)
    <= soft_time.as_millis().saturating_mul(
        parameters.search_again_growth_cutoff_permille as u128,
    );
```

Keep all arithmetic saturating and clamp the registered legal values before use.

- [ ] **Step 5: Add cross-parameter constraints**

Register positivity plus:

```text
NmpMinDepth < NmpVerificationDepth
AspirationInitialDelta <= AspirationMaxDelta
PostLmrShallowerMargin <= PostLmrDeeperMargin
TimeEffortLowPermille < TimeEffortHighPermille
TimeEffortHighPercent <= TimeEffortLowPercent
DefaultMovesToGo <= MaxMovesToGo
```

- [ ] **Step 6: Verify default bench and clock tests**

```powershell
cargo test -p mf-search --test search_invariants tier1_
cargo test -p mf-uci clock_limits
cargo test --release -p mf-uci --test bench_cli
```

Expected bench: 40,705 nodes.

- [ ] **Step 7: Commit**

```powershell
git add crates/mf-search/src/options.rs crates/mf-search/src/search.rs crates/mf-search/src/move_ordering.rs crates/mf-search/tests/search_invariants.rs crates/mf-uci/src/lib.rs crates/mf-uci/tests/bench_cli.rs
git commit -m "Expose direct search tuning parameters"
```

---

### Task 4: Wire Tier 2 grouped parameters

**Files:**
- Modify: `crates/mf-search/src/options.rs`
- Modify: `crates/mf-search/src/search.rs`
- Modify: `crates/mf-search/src/history.rs`
- Modify: `crates/mf-search/src/move_ordering.rs`
- Test: unit tests in those modules
- Test: `crates/mf-search/tests/search_invariants.rs`

**Interfaces:**
- Produces the Tier 2 groups from the design with the exact legal and recommended
  domains listed there
- Fixed-point conversion helpers:

```rust
fn permille(value: i32) -> f64 { f64::from(value) / 1000.0 }
fn hundredth(value: i32) -> f64 { f64::from(value) / 100.0 }
```

- [ ] **Step 1: Add formula-equivalence tests**

At shipped defaults, assert the parameterized forms equal the current formulas for:

- falling-eval factor;
- time-reduction interpolation;
- best-move instability;
- root-effort factor;
- ttMove-history margin;
- correction blend/update;
- continuation and ordering scores.

Use exact integer equality where the old formula is integer, and `abs(diff) <= f64::EPSILON` for values represented exactly by the chosen fixed-point conversion.

- [ ] **Step 2: Add group constraint tests**

Cover interpolation anchor ordering, clamp ordering, non-increasing continuation weights, and capture ordering score relationships.

- [ ] **Step 3: Parameterize interpolated time management**

Replace every literal in `falling_eval_factor`, `time_reduction_factor`, `best_move_instability_factor`, and `root_effort_factor` with registry fields. Preserve the existing `f64` calculation seam.

- [ ] **Step 4: Parameterize history/correction families**

Pass `&SearchParameters` into history read/update methods that need weights. Do not make table sizes or saturation bounds runtime fields.

- [ ] **Step 5: Parameterize move ordering**

Pass the needed weights through `OrderingContext`. Keep distinct groups for the
ordering-history multiplier/weights, LMR stat-history weights, quiet ordering scores,
and capture ordering weights. Pin current names/defaults exactly:

```text
butterfly ordering multiplier = 2
pawn ordering weight = 2
low-ply ordering weight = 8
LMR butterfly stat weight = 2048
LMR continuation stat weights = 1126, 1093
primary killer = 20000
secondary killer = 19000
castling = 1000
```

- [ ] **Step 6: Parameterize singular outcome constants**

Use the grouped fields for TT depth tolerance, verification divisor, extension amounts, and multicut/cut-node reductions. Keep recursion ceilings fixed.

- [ ] **Step 7: Verify default identity and feature-off identity**

```powershell
cargo test -p mf-search formula_equivalence
cargo test -p mf-search --test search_invariants tier2_
cargo test --release -p mf-uci --test bench_cli
```

Expected bench: 40,705 nodes. Existing feature toggle-off signatures must remain unchanged.

- [ ] **Step 8: Commit**

```powershell
git add crates/mf-search/src/options.rs crates/mf-search/src/search.rs crates/mf-search/src/history.rs crates/mf-search/src/move_ordering.rs crates/mf-search/tests/search_invariants.rs
git commit -m "Expose grouped search tuning parameters"
```

---

### Task 5: Add the dedicated tuning engine and manifest

**Files:**
- Create: `crates/mf-uci/src/tuning_manifest.rs`
- Create: `crates/mf-uci/src/bin/manifold-tune-engine.rs`
- Modify: `crates/mf-uci/src/lib.rs`
- Modify: `crates/mf-uci/Cargo.toml`
- Test: `crates/mf-uci/tests/uci_protocol.rs`
- Test: `crates/mf-uci/tests/bench_cli.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UciFlavor { Production, Tuning }

pub fn run_with_flavor<R: BufRead, W: Write>(
    reader: R,
    writer: W,
    flavor: UciFlavor,
) -> io::Result<()>;

pub fn write_tuning_manifest(mut writer: impl Write) -> io::Result<()>;
```

`run` remains and delegates to
`run_with_flavor(reader, writer, UciFlavor::Production)`.

- [ ] **Step 1: Write failing flavor tests**

Assert `manifold` omits `NmpMinDepth`, while the tuning flavor advertises it and every Tier 0/1/2 spin once.

- [ ] **Step 2: Write failing manifest tests**

Pin the header `manifold-tuning-manifest 1`, `registry-revision 1`,
registry-order records, terminal `end`, and byte-identical repeated output.

- [ ] **Step 3: Implement canonical serialization**

Use space-separated `key=value` fields with no free-form prose. Serialize parameters, checks, and constraints from the registry. Validate at startup that names/units/groups contain no whitespace.
Serialize activation trees with the exact manifest-v1 grammar from the design:
`always`, `clock`, `check(NAME,BOOL)`, `setting(NAME,INT)`,
`all(EXPR[;EXPR]*)`, and `any(EXPR[;EXPR]*)`.

- [ ] **Step 4: Add the tuning binary**

With no arguments, call
`run_with_flavor(stdin.lock(), stdout.lock(), UciFlavor::Tuning)`. Support only
`bench` and `tune-manifest` subcommands. Unknown commands fail with a concise hint.

- [ ] **Step 5: Make setoption flavor-aware**

Production accepts only `visibility=Production`. Tuning accepts Tier 0/1/2. Check options are accepted in both.

- [ ] **Step 6: Cross-check manifest and UCI**

Parse the tuning handshake in the test and assert every manifest parameter/check has the same default and legal bounds.

- [ ] **Step 7: Verify both binaries**

```powershell
cargo test -p mf-uci --test uci_protocol
cargo test --release -p mf-uci --test bench_cli
cargo build --release -p mf-uci --bin manifold --bin manifold-tune-engine
.\target\release\manifold-tune-engine.exe tune-manifest
.\target\release\manifold-tune-engine.exe bench
```

Both bench commands must report 40,705 nodes.

- [ ] **Step 8: Commit**

```powershell
git add crates/mf-uci/Cargo.toml crates/mf-uci/src/lib.rs crates/mf-uci/src/tuning_manifest.rs crates/mf-uci/src/bin/manifold-tune-engine.rs crates/mf-uci/tests/uci_protocol.rs crates/mf-uci/tests/bench_cli.rs
git commit -m "Add dedicated tuning engine manifest"
```

---

### Task 6: Make `mf-tune` consume the live engine contract

**Files:**
- Create: `crates/mf-tune/src/process.rs`
- Create: `crates/mf-tune/src/manifest.rs`
- Modify: `crates/mf-tune/src/config.rs`
- Modify: `crates/mf-tune/src/document.rs`
- Modify: `crates/mf-tune/src/cli.rs`
- Modify: `crates/mf-tune/src/lib.rs`
- Modify: `crates/mf-tune/Cargo.toml`

**Interfaces:**

```rust
pub struct ProcessRequest {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
}

pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait ProcessRunner {
    fn run(&mut self, request: &ProcessRequest) -> Result<ProcessOutput, String>;
}

pub struct EngineContract {
    pub registry_revision: u32,
    pub manifest_bytes: Vec<u8>,
    pub parameters: Vec<ManifestParameter>,
    pub checks: Vec<ManifestCheck>,
    pub constraints: Vec<ManifestConstraint>,
    pub uci_handshake: Vec<String>,
}

impl EngineContract {
    pub fn query(
        engine: &Path,
        runner: &mut dyn ProcessRunner,
        timeout: Duration,
    ) -> Result<Self, String>;
}
```

- [ ] **Step 1: Add parser and attestation failures**

Test malformed headers, duplicate names, unknown fields, missing `end`, missing `uciok`,
missing `readyok`, mismatched defaults/bounds, timeout, nonzero exit, invalid UTF-8,
empty/unknown activation operators, bad arity, malformed nesting, and trailing
activation text.

- [ ] **Step 2: Implement timeout-capable process execution**

For captured output, drain stdout and stderr concurrently on reader threads while
polling `try_wait`. For artifact output, redirect directly to files. On timeout, kill
the whole process tree and reap the child. Return the exit status to the caller; the
batch layer writes its durable status sidecar only after syncing every evidence file.
Keep command execution behind `ProcessRunner` for deterministic tests.

- [ ] **Step 3: Parse the compact manifest**

Require exact schema version 1, parse the emitted registry revision, and use closed
field sets. Parse the complete activation expression recursively and preserve registry
order. Add serializer/parser round-trip coverage for
`all(any(check(UseLMR,true);check(UseFutility,true));setting(MultiPV,1))`.

- [ ] **Step 4: Parse and attest the UCI handshake**

Send `uci\nisready\nquit\n`. Require exactly one option line per manifest item and exact default/legal agreement.

- [ ] **Step 5: Split raw config from resolved config**

```rust
pub struct RequestedParameter {
    pub name: String,
    pub value: Option<i32>,
    pub min: Option<i32>,
    pub max: Option<i32>,
    pub c_end: Option<f64>,
    pub r_end: Option<f64>,
}

pub struct TuningConfig {
    pub schedule: Schedule,
    pub budget: u64,
    pub parameters: Vec<RequestedParameter>,
    pub checks: Vec<(String, bool)>,
    pub seed: u64,
    pub match_settings: MatchSettings,
}

pub struct ResolvedConfig {
    pub schedule: Schedule,
    pub budget: u64,
    pub dimensions: Vec<Dimension>,
    pub checks: Vec<(String, bool)>,
    pub start: Vec<f64>,
    pub seed: u64,
    pub match_settings: MatchSettings,
    pub engine_contract: EngineContract,
}
```

Resolve defaults/ranges/scale/quantum/activation only after querying the engine. Extend
`MatchSettings` with typed `move_overhead_millis`, `multi_pv`, `ponder`,
`uci_chess960`, and optional `eval_file`; apply the same values to both fastchess arms.
Store every resolved pinned check in `MatchSettings` and serialize
`option.<CheckName>=true|false` for both arms. Do not retain generic `extra_options`.

- [ ] **Step 6: Tighten config validation**

Add a `Value::Boolean(bool)` variant plus strict `boolean`/`optional_boolean` accessors
and rendering for `[[option]] value = true`. Reject unknown root keys/sections, negative
seeds, non-finite or non-positive schedule values, off-lattice initial values, widened
legal ranges, violated constraints, and dimensions inert under the full activation
expression. Reject `SyzygyPath`; if `EvalFile` is set, require and later fingerprint the
resolved network file. Resolve relative paths against the config file's parent.

- [ ] **Step 7: Change `mf-tune init`**

Require `--engine`. Add `--params`, `--group`, `--tier`, `--checks shipped`, `--set-check`, and `--allow-partial-group`. Generate recommended ranges from the queried manifest.
When `--out` is used, write canonical absolute engine/fastchess/book paths or paths
relative to the destination config's parent. Add a nested-output regression matching
the final smoke layout.

- [ ] **Step 8: Remove linked engine metadata**

Remove `mf-core`, `mf-nnue`, and `mf-search` dependencies from `mf-tune`. Retain `mf-datagen` only for the deterministic RNG for now.

- [ ] **Step 9: Verify**

```powershell
cargo test -p mf-tune manifest
cargo test -p mf-tune config
cargo test -p mf-tune cli
```

- [ ] **Step 10: Commit**

```powershell
git add crates/mf-tune/Cargo.toml crates/mf-tune/src/process.rs crates/mf-tune/src/manifest.rs crates/mf-tune/src/config.rs crates/mf-tune/src/document.rs crates/mf-tune/src/cli.rs crates/mf-tune/src/lib.rs
git commit -m "Resolve tuner settings from live engine"
```

---

### Task 7: Freeze campaign identity before games

**Files:**
- Modify: `crates/mf-tune/Cargo.toml`
- Modify: `crates/mf-tune/src/manifest.rs`
- Modify: `crates/mf-tune/src/cli.rs`
- Modify: `crates/mf-tune/src/run.rs`
- Test: module tests in `manifest.rs`

**Interfaces:**

```rust
pub struct CampaignManifest {
    pub schema_version: u32,
    pub registry_version: u32,
    pub spsa_version: u32,
    pub domain_version: u32,
    pub journal_version: u32,
    pub fingerprint: String,
    pub canonical_config: String,
    pub engine_contract: EngineContract,
    pub resolved_match_settings: MatchSettings,
    pub tuner_sha256: String,
    pub engine_sha256: String,
    pub fastchess_sha256: String,
    pub book_sha256: String,
    pub external_network_sha256: Option<String>,
    pub fastchess_version: String,
    pub git_revision: Option<String>,
    pub git_dirty: Option<bool>,
    pub affinity: bool,
    pub concurrency: u32,
    pub memory_fraction_permille: u32,
}

pub fn sha256_file(path: &Path) -> Result<String, String>;
pub fn campaign_fingerprint(manifest: &CampaignManifest) -> String;
```

- [ ] **Step 1: Add `sha2 = "0.10"` and known-vector tests**

Test empty bytes and `abc` against standard SHA-256 vectors before hashing files.

- [ ] **Step 2: Define canonical semantic rendering**

Render fields in fixed order with normalized absolute paths and LF line endings. Exclude
invocation budget; include algorithm versions, tuner/engine/fastchess/book hashes,
an external-network hash when enabled, fastchess version, horizon, gains, selected
order, checks, handshake, TC, Hash, Threads, Move Overhead, MultiPV, Ponder,
UCI_Chess960, affinity, and concurrency. Reject `SyzygyPath` in tuning campaigns.
Obtain the harness version with `<fastchess> --version`; the bundled binary currently
reports `fastchess alpha 1.8.1 compiled for windows-latest 20260405-1525c4b (CI)`.

- [ ] **Step 3: Write incompatibility tests**

For each field, mutate only that field and assert resume refusal: tuner bytes and
algorithm versions, engine bytes, external network, book bytes, fastchess bytes/version,
handshake, seed, horizon, alpha/gamma/A, TC, Hash, Threads, checks, parameter order,
bounds, scale, quantum, `c_end`, and `r_end`. Assert a larger invocation budget remains
compatible.

- [ ] **Step 4: Persist before run state**

On a new empty directory, write and sync `session-manifest.txt` before checkpoint, history, journal, or PGN. Refuse a non-empty directory without a valid manifest.

- [ ] **Step 5: Re-attest on every resume**

Query the live engine again, hash every file again, build the current manifest, and compare fingerprints before reading mutable run state.

- [ ] **Step 6: Verify deterministic bytes**

```powershell
cargo test -p mf-tune campaign_manifest
cargo test -p mf-tune resume_identity
```

- [ ] **Step 7: Commit**

```powershell
git add Cargo.lock crates/mf-tune/Cargo.toml crates/mf-tune/src/manifest.rs crates/mf-tune/src/cli.rs crates/mf-tune/src/run.rs
git commit -m "Bind tuning runs to immutable campaigns"
```

---

### Task 8: Add constraint-aware quantized SPSA

**Files:**
- Create: `crates/mf-tune/src/domain.rs`
- Modify: `crates/mf-tune/src/spsa.rs`
- Modify: `crates/mf-tune/src/config.rs`
- Modify: `crates/mf-tune/src/lib.rs`
- Test: module tests in `domain.rs` and `spsa.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinateScale { Linear, Log }

pub struct CoordinateSpec {
    pub name: String,
    pub min: i32,
    pub max: i32,
    pub quantum: i32,
    pub scale: CoordinateScale,
}

pub struct FeasiblePerturbation {
    pub flips: Vec<i8>,
    pub plus: Vec<i32>,
    pub minus: Vec<i32>,
    pub signed_deltas: Vec<f64>,
}

pub enum Constraint {
    LessEqual { left: usize, right: usize },
    StrictLess { left: usize, right: usize },
    Positive { coordinate: usize },
}

pub struct ParameterDomain {
    coordinates: Vec<CoordinateSpec>,
    constraints: Vec<Constraint>,
}

impl ParameterDomain {
    pub fn perturb(
        &self,
        theta: &[f64],
        gains: &[Gains],
        seed: u64,
        iteration: u64,
    ) -> Result<FeasiblePerturbation, String>;

    pub fn bound_theta(&self, theta: &[f64]) -> Result<Vec<f64>, String>;
}
```

- [ ] **Step 1: Write lattice tests**

Cover negative linear values, non-unit quantum, exact half-step arm behavior, log
coordinates, lower/upper clipping, NaN/infinity rejection, and continuous theta between
lattice points. Config tests, not the live domain, reject off-lattice initial spins.

- [ ] **Step 2: Write constraint projection tests**

Cover all three constraint variants and coupled examples from the engine manifest. Both arms must satisfy the complete constraint set.

- [ ] **Step 3: Write collapsed-arm tests**

At a tight bound, prove one coordinate may have zero signed delta while another keeps
the two full arms distinct; the zero-delta coordinate receives no update. Redraw the
complete sign vector only when every coordinate collapses, and return an error after 32
redraws when no distinct legal pair exists.

- [ ] **Step 4: Move feasible-point logic out of `Spsa`**

`Spsa` retains schedule and theta. It requests gains, delegates arm construction to `ParameterDomain`, and applies an update using `signed_deltas`.

Use the central finite-difference form:

```rust
let delta = perturbation.signed_deltas[index];
if delta != 0.0 {
    theta[index] += 2.0 * gains[index].a_k * result / delta;
}
```

The sign is already encoded in `delta = plus_coordinate - minus_coordinate`.

- [ ] **Step 5: Bound continuous theta after every update and resume**

Call `domain.bound_theta` after applying a result and when loading checkpoint state.
Preserve sub-quantum updates in continuous theta; quantize only plus/minus arms.
`bound_theta` clamps continuous bounds and projects cross-parameter inequalities without
rounding to the engine lattice.

- [ ] **Step 6: Preserve deterministic resume**

Update the synthetic quadratic test so uninterrupted, split-resume, linear, log, clipped, and quantized runs produce identical arm sequences and theta.

- [ ] **Step 7: Verify**

```powershell
cargo test -p mf-tune domain
cargo test -p mf-tune spsa
cargo test -p mf-tune uninterrupted_and_resumed_runs_are_identical
```

- [ ] **Step 8: Commit**

```powershell
git add crates/mf-tune/src/domain.rs crates/mf-tune/src/spsa.rs crates/mf-tune/src/config.rs crates/mf-tune/src/lib.rs
git commit -m "Project SPSA onto legal parameter domains"
```

---

### Task 9: Make the journal authoritative

**Files:**
- Create: `crates/mf-tune/src/journal.rs`
- Modify: `crates/mf-tune/src/run.rs`
- Modify: `crates/mf-tune/src/checkpoint.rs`
- Modify: `crates/mf-tune/src/lib.rs`
- Test: module tests in `journal.rs`, `run.rs`, and `checkpoint.rs`

**Interfaces:**

```rust
pub enum JournalEvent {
    Prepared(PreparedIteration),
    Observed(ObservedIteration),
    Committed(CommittedIteration),
    AttemptFailed(FailedAttempt),
}

pub struct JournalRecord {
    pub sequence: u64,
    pub event: JournalEvent,
}

pub struct JournalState {
    pub completed: u64,
    pub games_played: u64,
    pub theta: Vec<f64>,
    pub pending: Option<PendingIteration>,
}

pub fn append_record(path: &Path, record: &JournalRecord) -> Result<(), String>;
pub fn read_journal(path: &Path, fingerprint: &str) -> Result<JournalState, String>;
```

Frame each record as:

```text
record <schema> <sequence> <kind> <payload-bytes> <sha256>\n
<exact payload bytes>\n
```

- [ ] **Step 1: Add framing tests**

Test round-trip, multiple records, a torn final header, torn final payload, checksum mismatch in the middle, sequence gaps, duplicate sequence, wrong fingerprint, and unknown event kind.

- [ ] **Step 2: Flush each append**

Open append-only, write header/payload/newline, call `sync_data`, then return success.

- [ ] **Step 3: Replace the run loop state transitions**

Before arena launch append `Prepared`; after admissible evidence append `Observed`;
after one update append `Committed`. On timeout, interruption, missing trustworthy exit
status, or invalid evidence, append `AttemptFailed` without changing theta. A later run
may prepare the next attempt number for the same iteration.

- [ ] **Step 4: Add recovery tests for every crash window**

Inject stops:

1. before `Prepared`;
2. after `Prepared` and before arena;
3. after complete evidence and before `Observed`;
4. after `Observed` and before update;
5. after `AttemptFailed` and before retry;
6. after `Committed` and before checkpoint/history rebuild.

Assert no double-application; clean `Observed` batches recover exactly once; a prepared
batch without trustworthy status is preserved, marked failed, and retried explicitly
rather than silently replayed.

- [ ] **Step 5: Make checkpoint/history derived views**

Build both from committed journal records. On resume, rebuild if absent, truncated, stale, or inconsistent. Keep current external filenames.

- [ ] **Step 6: Rotate checkpoint safely**

Write and sync `checkpoint.next.toml`. While current remains valid, remove an older
`checkpoint.previous.toml`; rename current to previous; then rename next to current.
Readers fall back to previous when current is absent. Journal state wins if rotation is
interrupted.

- [ ] **Step 7: Verify**

```powershell
cargo test -p mf-tune journal
cargo test -p mf-tune recovery_
cargo test -p mf-tune checkpoint
```

- [ ] **Step 8: Commit**

```powershell
git add crates/mf-tune/src/journal.rs crates/mf-tune/src/run.rs crates/mf-tune/src/checkpoint.rs crates/mf-tune/src/lib.rs
git commit -m "Journal tuning iterations before checkpointing"
```

---

### Task 10: Harden fastchess evidence and resource checks

**Files:**
- Modify: `crates/mf-tune/Cargo.toml`
- Modify: `crates/mf-tune/src/batch.rs`
- Modify: `crates/mf-tune/src/process.rs`
- Modify: `crates/mf-tune/src/run.rs`
- Modify: `crates/mf-tune/src/interrupt.rs`
- Test: module tests in `batch.rs`

**Interfaces:**

```rust
pub struct BatchDiagnostics {
    pub time_forfeits: u32,
    pub crashes: u32,
    pub illegal_moves: u32,
    pub illegal_pv_reports: u32,
    pub no_output_events: u32,
}

pub struct BatchEvidence {
    pub result: BatchResult,
    pub diagnostics: BatchDiagnostics,
    pub exit_code: i32,
    pub pgn_sha256: String,
    pub console_sha256: String,
    pub fastchess_log_sha256: String,
    pub status_sha256: String,
}

pub trait MemoryProbe {
    fn free_physical_memory_mib(&self) -> Result<u64, String>;
    fn cpu_load_percent(&self) -> Result<u32, String>;
}
```

- [ ] **Step 1: Replace permissive PGN fixtures with exact-game fixtures**

Add cases for valid colour-reversed pairs, short and extra PGNs, unknown White/Black, duplicate arm names, malformed/missing result, time forfeit attribution, illegal move annotation, adjudication, and incomplete last game.

- [ ] **Step 2: Add console diagnostics fixtures**

Parse `Player`, `Timeouts`, `Crashed`, `Illegal PV move`, and `No output from` using the same semantics as `harness/run_match.ps1`.

- [ ] **Step 3: Capture artifacts**

Pass:

```text
-pgnout file=<attempt>.pgn append=false
-log file=<attempt>.fastchess.log level=warn append=false
```

Capture stdout/stderr to `<attempt>.console.txt`. Never delete an existing uncommitted
artifact; increment `attempt-NN`. Write and sync `<attempt>.status.txt` immediately
after fastchess exits, but only after flushing and syncing the PGN, console, and
fastchess warning log. Recovery may promote a prepared attempt only when this status
exists, hashes correctly, records exit code zero, and every referenced evidence file
hashes correctly.

- [ ] **Step 4: Require admissible evidence**

Reject nonzero exit, timeout, missing status/evidence files, any game count other than
exact, unknown players, time forfeits, crashes, illegal moves played, or `No output
from`. Record illegal-PV warnings without failing. Rejections append `AttemptFailed`;
they never leave an unrecoverable pending record.

- [ ] **Step 5: Add memory preflight**

```rust
let (_, concurrency) = affinity_policy(settings.threads);
let engine_processes = 2_u64 * u64::from(concurrency);
let required = engine_processes * u64::from(settings.hash_mebibytes);
let allowed = free_memory_mib * 70 / 100;
if required > allowed {
    return Err(format!(
        "memory preflight failed: tuning needs {required} MiB of Hash across \
         {engine_processes} engine processes, but the 70% allowance is {allowed} MiB; \
         reduce Hash or free memory"
    ));
}
```

Sample CPU load and persist both measurements in prepared/observed evidence.

- [ ] **Step 6: Add timeout and Ctrl+C cleanup**

Teach `process.rs` to terminate and reap the process tree. On Windows, use
`CreateProcessW(CREATE_SUSPENDED)`, assign the process to a Job Object configured with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, then resume its primary thread. Timeout,
interruption, or dropping the runner closes the job and kills the contained tree. On
non-Windows, start fastchess in a new POSIX process group with `libc::setpgid` and
signal the negative process-group ID. Preserve artifacts and append `AttemptFailed`.
Add `libc = "0.2"` under
`[target.'cfg(not(windows))'.dependencies]` and `windows-sys = "0.59"` with
`Win32_Foundation`, `Win32_Security`, `Win32_System_JobObjects`, and `Win32_System_Threading` under
`[target.'cfg(windows)'.dependencies]`.

- [ ] **Step 7: Verify command policy**

Pin exact affinity/concurrency arguments for Threads=1 and Threads=8, paired openings,
even game count, deterministic seed, and identical fixed settings on both arms:
`Hash`, `Threads`, `Move Overhead`, `MultiPV=1`, `Ponder=false`,
`UCI_Chess960=false`, plus `EvalFile` when configured. Assert every pinned `Use*` check
appears explicitly and identically on both arms.

- [ ] **Step 8: Run focused tests**

```powershell
cargo test -p mf-tune batch
cargo test -p mf-tune memory_preflight
cargo test -p mf-tune process_timeout
```

- [ ] **Step 9: Commit**

```powershell
git add Cargo.lock crates/mf-tune/Cargo.toml crates/mf-tune/src/batch.rs crates/mf-tune/src/process.rs crates/mf-tune/src/run.rs crates/mf-tune/src/interrupt.rs
git commit -m "Reject suspect tuning batches"
```

---

### Task 11: Finish CLI, documentation, and real smoke

**Files:**
- Modify: `crates/mf-tune/src/cli.rs`
- Modify: `crates/mf-tune/src/config.rs`
- Modify: `crates/mf-tune/tests/smoke_run.rs`
- Modify: `README.md`
- Modify: `crates/mf-tune/src/lib.rs` documentation

**Interfaces:**

Final commands:

```text
mf-tune init --engine <path> (--params <list> | --group <name> | --tier 0) [--out <file>]
mf-tune run --config <file> --out <directory> [--iterations N]
```

- [ ] **Step 1: Pin help and generated-config tests**

Help must explain live engine discovery, tiers/groups, checks, strict resume identity, journal authority, and output files. Generated configs must use recommended ranges and include pinned check values.

- [ ] **Step 2: Add session-directory refusal tests**

Cover a non-empty directory without manifest, malformed manifest, incompatible manifest, valid prefix resume, and already-complete invocation.

- [ ] **Step 3: Upgrade the ignored real smoke**

The test must:

1. build/use release `manifold-tune-engine`;
2. query manifest and handshake;
3. generate a two-parameter Tier 0 config;
4. run two real fastchess iterations;
5. resume to iteration 3;
6. delete checkpoint/history and rerun with the same budget to rebuild views;
7. assert journal/manifest bytes are unchanged and no arena process runs during rebuild.

- [ ] **Step 4: Add artifact assertions**

Assert one session manifest, one journal, three committed iterations, three unique
attempt PGNs/consoles/logs/status sidecars, valid hashes, exact colour reversal, zero
fatal diagnostics, and no orphaned processes.

- [ ] **Step 5: Update README**

Document:

- the distinction between `manifold` and `manifold-tune-engine`;
- Tier 0/1/2/3 and forbidden values;
- config generation examples;
- output and recovery semantics;
- fastchess affinity/memory guardrails;
- candidate validation through `harness/run_match.ps1`;
- that generated campaign directories should not be committed unless intentionally preserved as experiment evidence.

- [ ] **Step 6: Run focused release smoke**

```powershell
cargo build --release -p mf-uci --bin manifold --bin manifold-tune-engine
cargo test --release -p mf-tune --test smoke_run -- --ignored --nocapture
```

- [ ] **Step 7: Run the completion matrix**

```powershell
cargo test -p mf-search
cargo test -p mf-uci
cargo test -p mf-tune
cargo test --release -p mf-search
cargo test --release -p mf-uci --test bench_cli
cargo test --release -p mf-uci --test uci_protocol
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Known baseline issues must be reported faithfully rather than hidden: full rustfmt currently encounters unrelated formatting in `crates/mf-core/src/see.rs` and `crates/mf-search/examples/see_profile.rs`; `--all-features` clippy is separately blocked by the unfinished `mf-uci/train` surface. All touched files still need targeted rustfmt, default-feature clippy, and `git diff --check` clean.

- [ ] **Step 8: Run a no-Elo mechanics campaign**

```powershell
.\target\release\mf-tune.exe init `
  --engine .\target\release\manifold-tune-engine.exe `
  --params LmrCoefficient,RfpMarginPerDepth `
  --out .\target\tune-smoke\campaign.toml

.\target\release\mf-tune.exe run `
  --config .\target\tune-smoke\campaign.toml `
  --out .\target\tune-smoke\session `
  --iterations 2

.\target\release\mf-tune.exe run `
  --config .\target\tune-smoke\campaign.toml `
  --out .\target\tune-smoke\session `
  --iterations 3
```

Acceptance: exact resume at iteration 3, no replay, no overwritten artifacts, no fatal diagnostics, manifest unchanged, and all hashes valid.

- [ ] **Step 9: Commit**

```powershell
git add README.md crates/mf-tune/src/cli.rs crates/mf-tune/src/config.rs crates/mf-tune/src/lib.rs crates/mf-tune/tests/smoke_run.rs
git commit -m "Document reproducible tuning campaigns"
```

## Final review checklist

- [ ] Every design requirement maps to a task.
- [ ] Normal `manifold` advertises only the original production surface.
- [ ] `manifold-tune-engine` and `tune-manifest` derive from the same registry.
- [ ] `mf-tune` has no linked copy of the engine parameter contract.
- [ ] Every selected dimension is active, legal, on-lattice, and constraint-valid before a batch.
- [ ] SPSA uses actual quantized arm separation.
- [ ] Session fingerprint detects changed binaries, files, settings, and arithmetic.
- [ ] Journal recovery cannot double-apply or lose a completed batch.
- [ ] Suspect PGN/console/process evidence never updates theta.
- [ ] Memory and affinity policy match `AGENTS.md` and `harness/run_match.ps1`.
- [ ] Release defaults retain the 40,705-node signature.
- [ ] README and real smoke reflect the final workflow.
