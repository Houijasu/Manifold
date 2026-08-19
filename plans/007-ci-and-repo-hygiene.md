# Plan 007: Add CI and fix the stale repo map (AGENTS.md, README, config.json, harness paths)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If
> anything in the "STOP conditions" section occurs, stop and report. When
> done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- AGENTS.md README.md config.json harness/ .github/`
> Written against commit `b9d15bf` **plus its uncommitted working tree** (note: `crates/mf-tb/` and parts of `harness/` are untracked). Excerpt mismatch = STOP.

## Status

- **Execution status**: DONE
- **Priority**: P2
- **Effort**: M
- **Risk**: MED (the fresh-clone build problem in step 1 must be solved before any CI can go green; CPU-flag and test-duration caveats below)
- **Depends on**: none
- **Category**: dx + docs
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

### Targeted hygiene update (2026-08-18)

The original findings and steps below are retained as planning history.

- [x] Refresh the live crate, UCI command, and option maps.
- [x] Preserve the legacy root match snapshot as historical experiment evidence and
      document `experiments/<run-name>/` as the live run-state contract.
- [x] Add narrow tracked ignores for root `config.json` and per-run `games.pgn`.
- [x] Add and validate the CI workflow.
- [x] Decide and document fresh-clone network provisioning.
- [ ] Clean up absolute harness paths; this was not requested by the targeted execution.

The checksum-pinned workflow and fresh-clone provisioning instructions are validated
remotely: PR #2's Windows and Ubuntu jobs both pass on the pinned release asset.

## Why this matters

The repo declares three "authoritative gates" (`cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`) and enforces **none of them**: there is no CI at all (no `.github/`), and the current tree demonstrates the failure mode — `crates/mf-tb/` is a fully wired, untracked crate that exists on exactly one machine. Meanwhile the repo's own instruction file is materially wrong: AGENTS.md calls `mf-tune` and `mf-lab` "stubs containing only a crate-level doc comment" (mf-tune is a ~100 KB SPSA tuner that spawns engine processes; mf-lab is a working corrhist-regression CLI), omits `mf-tb` from the crate list entirely, and lists six UCI commands where the engine implements the full set. Every agent and human entering this repo is misdirected. Three smaller hygiene items ride along: root `config.json` is leftover scratch pointing at a deleted temp worktree, and `harness/run_match.ps1` hardcodes this machine's absolute paths.

**Critical discovery this plan must handle first**: a fresh clone cannot build the default feature set. `crates/mf-nnue/build.rs` panics unless `nets/main.nnue` exists, and that 106 MB file is gitignored by policy. CI therefore needs an explicit net-provisioning step (step 2), and the README needs to say how a new contributor obtains the net.

## Current state

- No `.github/` directory exists (verified by listing the repo root).
- `AGENTS.md:10` — "`mf-tune`, `mf-lab`: planned tuning and experiment layers. These are currently stubs containing only a crate-level doc comment stating each crate's intended responsibility." — false for the current tree; crate list omits `mf-tb` (a workspace member, `Cargo.toml` members list, wired into mf-uci/mf-search/mf-datagen).
- `AGENTS.md:43` — "mf-uci currently implements `uci`, `isready`, `quit`, `setoption`, `position`, and `go perft`; unsupported commands are silently ignored." — the engine implements `ucinewgame`, `go depth|nodes|movetime|wtime/btime|infinite`, `stop`, `bench`, `mtbench`, MultiPV, Ponder, SyzygyPath (see the option table at `crates/mf-uci/src/lib.rs:38-80`).
- `README.md` — crate map calls mf-lab a stub and omits mf-tb; options table omits `SyzygyPath`; the Development section's gate commands are correct.
- `crates/mf-nnue/build.rs:13-27` — the embedded-net build gate requiring `nets/main.nnue`:
```rust
    if std::env::var_os("CARGO_FEATURE_EMBEDDED_NET").is_none() {
        return;
    }
    ...panic!("mf-nnue feature `embedded-net` requires a network file at {}, but that path is not a file", ...)
```
- `crates/mf-nnue/src/provision.rs:131-137` — runtime discovery (`nets/main.nnue` beside the executable/working directory and ancestors, then embedded). mf-nnue tests skip gracefully when the net is absent (`src/test_support.rs`).
- `config.json` — absolute paths into `C:/Users/Samaritan/AppData/Local/Temp/manifold-validation-readiness-20260812-0110/isolated-worktree/...` for `opening.file`, `pgn.file`, and both `engines[].cmd`; embedded 2-game stats block. AGENTS.md describes it as "a fastchess tournament config".
- `harness/run_match.ps1:80` — `$root = 'C:\Users\Samaritan\Projects\Manifold'`; `:91` — default `-Book 'C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd'`; `harness/build_pgo.ps1:105` — `py -3.14` launcher pin.
- `.cargo/config.toml` sets `target-cpu=native` for `x86_64-pc-windows-msvc` only — Linux CI runners are unaffected; Windows runners compile native to the runner (fine for correctness gates, binaries non-representative — keep benchmarks out of CI).
- Test-duration facts for CI budgeting: debug `cargo test --workspace` is minutes; `cargo test --release` perft is 13+ minutes (keep out of per-push CI; optional nightly).
- Repo commit style: short imperative sentence-case subjects (`Implement bitboards and sliding attacks`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Gates locally | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` | exit 0 |
| No-CI sanity | `ls .github` (PowerShell: `Test-Path .github`) | False before this plan, True after |

## Scope

**In scope**:
- `.github/workflows/ci.yml` (create)
- `AGENTS.md`, `README.md`
- `config.json`
- `harness/run_match.ps1`, `harness/build_pgo.ps1` (path/launcher lines only)
- `.gitignore` (only if the net-provisioning approach requires it)

**Out of scope**:
- Any Rust source file.
- `docs/plans/`, `docs/reviews/` (historical records).
- Release-mode perft in CI (duration; add later as a scheduled job if wanted).

## Git workflow

- Suggested: one commit for docs/config hygiene, one for CI. Style: `Add CI enforcing the three gates`, `Refresh the stale repo map and harness paths`.

## Steps

### Step 1: Decide net provisioning for fresh clones (human decision point)

The build requires `nets/main.nnue` (106 MB, gitignored). Options, in increasing setup cost: (a) GitHub release asset downloaded by CI + README instructions for humans; (b) Git LFS; (c) CI builds `--no-default-features` and skips net-dependent tests (mf-nnue tests already skip, but mf-uci engine-spawning tests will fail without a net — verify with `rg -ln "main.nnue|embedded" crates/mf-uci/tests/`). Recommended: (a). Record the chosen mechanism in README's Development section.

**Verify**: chosen mechanism documented; if (a), the release asset exists and `curl -L -o nets/main.nnue <asset-url>` on a fresh clone makes `cargo check --workspace` exit 0.

### Step 2: Create `.github/workflows/ci.yml`

Two jobs, `ubuntu-latest` and `windows-latest`, each: checkout → provision the net (per step 1) → `rustup component add clippy rustfmt` (toolchain is pinned by `rust-toolchain.toml`, 1.97.1) → run the three gates (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`). Add caching (`Swatinem/rust-cache@v2` or equivalent) — the workspace builds heavy tables via build.rs. Do not run benchmarks or release perft in CI. Add a `cargo test -p mf-core --features force-magic` step to one of the two jobs (cheap, exercises the black-magic backend).

**Verify**: workflow file passes `actionlint` if available, else YAML parse; first run on the branch goes green on both runners.

### Step 3: Fix AGENTS.md

1. Crate list: replace the mf-tune/mf-lab stub bullet with accurate one-liners (mf-tune: SPSA tuner, spawns fastchess/manifold processes, checkpoint/resume; mf-lab: corrhist-regression experiment CLI), add `mf-tb`: Syzygy tablebase prober wired into mf-uci/mf-search/mf-datagen.
2. mf-uci command list: replace the six-command sentence with the real surface (see Current state) and drop "unsupported commands are silently ignored" if no longer accurate for the listed set (verify against `crates/mf-uci/src/lib.rs` dispatch).
3. Leave the harness rules and match-harness sections untouched — they are load-bearing and current.

**Verify**: every factual claim in the diffed lines is backed by the code (spot-check each crate's `src/` exists and does what the line says).

### Step 4: Fix README

Update the crate map (mf-tb, real mf-tune/mf-lab), add `SyzygyPath` to the options table, and add the net-provisioning instructions from step 1 to Development.

**Verify**: `cargo run --release -p mf-uci --bin manifold` prints an option list consistent with the README table (spot-check `SyzygyPath` and the crate descriptions).

### Step 5: Clean config.json and harness paths

1. Replace `config.json`'s temp-worktree paths with repo-relative ones (`tools/books/UHO_4060_v4.epd`, `target/release/manifold.exe`) or delete the file and drop its AGENTS.md mention — smaller diff wins; keep it as a valid gauntlet template if kept.
2. `run_match.ps1`: `$root = Join-Path $PSScriptRoot '..']` → resolve; default book from `$root\tools\books\...`.
3. `build_pgo.ps1`: `py -3.14` → `py -3` (or `python`) with a comment naming the minimum version.

**Verify**: from the repo root, `pwsh harness/run_match.ps1 -EngineA <a> -EngineB <b> -Games 2` (or the script's real parameters — read its `param()` block first) starts and completes a 2-game smoke with 0 forfeits, **with the AGENTS.md harness rules honored** (1T → `-use-affinity -concurrency 8`).

## Test plan

- CI itself is the test: green run on both runners.
- The 2-game harness smoke (step 5) validates the path changes end-to-end.

## Done criteria

- [x] `.github/workflows/ci.yml` exists and is green on ubuntu + windows (PR #2)
- [x] Fresh clone + documented net provisioning → all three gates pass locally (the CI
      job is exactly this: checkout, pinned asset download, fmt/clippy/test)
- [x] AGENTS.md/README contain zero stale claims (each diffed line code-backed)
- [x] `config.json` has no `Temp\` paths (no tracked root `config.json`; the ignored
      local file is per-run scratch); `run_match.ps1` has no `C:\Users` literals
      (`rg -n "C:\\\\Users" harness/` → no matches)
- [x] All local gates still exit 0

## STOP conditions

- CI cannot go green because tests require assets or machine properties that cannot be provisioned sanely (report the specific failing test — do not delete or weaken tests to make CI pass).
- The net-provisioning decision needs owner input (file size/licensing of the net) — present options and stop.
- The AGENTS.md/README corrections reveal more stale claims than this plan covers — fix what is in scope, list the rest in the report.

## Maintenance notes

- CI binaries are `target-cpu=native` to the runners (Windows) — never use CI for strength or NPS claims.
- When the net changes (retraining), the provisioning asset must be re-uploaded in the same change or CI goes red.
- Release perft (13+ min) remains a manual/local gate; consider a nightly job later.
