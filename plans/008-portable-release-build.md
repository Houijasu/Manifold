# Plan 008: Produce a portable release build (baseline x86-64)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If
> anything in the "STOP conditions" section occurs, stop and report. When
> done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- .cargo/config.toml harness/ crates/mf-core/src/sliding.rs crates/mf-core/Cargo.toml`
> Written against commit `b9d15bf` **plus its uncommitted working tree**. Excerpt mismatch = STOP.

## Status

- **Priority**: P2 (direction item — release engineering, not strength)
- **Effort**: S-M
- **Risk**: LOW to the engine (same code, different target features); MED to release process if the wrong flag slips through — hence the disassembly gate in step 4
- **Depends on**: none (independent of 001-007)
- **Category**: direction
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15
- **Completion**: DONE locally on 2026-08-19; native/portable bench 37,420,
  portable perft 4,865,609, force-magic green, controlled scanner fixture green,
  portable scan zero matches, staged binary stable through publication, embedded
  network hash stable through final validation, encoded flags isolated/restored,
  repository-pinned toolchain isolated and recorded, exact-sysroot LLVM lookup verified,
  committed publication survives backup-cleanup failure, ordinary release unchanged

## Why this matters

`.cargo/config.toml` compiles the Windows MSVC target with `target-cpu=native`. Release
binaries built on BMI2-capable machines may therefore contain BMI2 instructions and
fail on older CPUs. The codebase already contains everything needed for a portable
build: the sliding-attack backend auto-selects black-magics when BMI2 is absent at
compile time, and the NNUE SIMD backend dispatches at **runtime**
(Scalar/Avx2/Avx2Vnni), so only the sliding layer and generic codegen need a baseline
target. This plan adds a portable build path and a check that keeps it honest.

## Current state

- `.cargo/config.toml` (repo root, applies to `x86_64-pc-windows-msvc`):
```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-cpu=native"]
```
  (AGENTS.md documents the consequence: native builds on BMI2-capable hosts select the
  PEXT backend; `force-magic` selects black-magics for testing.)
- `crates/mf-core/Cargo.toml:9` — the `force-magic` feature exists precisely to exercise the non-PEXT backend.
- `crates/mf-core/src/sliding.rs` — PEXT code is gated on `target_feature = "bmi2"` with a debug cross-check; without BMI2 at compile time the black-magic backend is selected automatically (per AGENTS.md "Non-Obvious Build & Test Behavior").
- `crates/mf-nnue/src/simd.rs` — `SimdBackend` dispatch is runtime-detected (`is_x86_feature_detected!` style; backends Scalar/Avx2/Avx2Vnni) — no portability work needed there.
- `harness/build_pgo.ps1` exists with a bench-signature gate; it builds the native binary (PGO is a native-machine optimization and stays that way).
- Repo release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` (root `Cargo.toml`).
- Bench determinism: the bench node signature is backend-independent in **node count** (move ordering does not depend on the sliding backend) — the portable build must print the same signature as the native build.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Native bench (reference) | dedicated native build inside `harness/build_portable.ps1` | 37,420 |
| Portable build | `pwsh -NoProfile -File harness/build_portable.ps1` | exit 0, artifact under `target/portable/` |
| Portable bench | `target/portable/manifold.exe bench` | **same** node signature as native |
| Magic-backend tests | `cargo test -p mf-core --features force-magic` | all pass |
| No-BMI2 scan | instruction-token scan inside `harness/build_portable.ps1` | zero `pext`/`pdep`/`bzhi`/`mulx`/`sarx`/`shlx`/`shrx`/`rorx` mnemonics |

## Scope

**In scope**:
- `harness/build_portable.ps1` (create)
- `README.md` (Build section: document both artifacts)
- `harness/README.md` (one paragraph)
- `.github/workflows/ci.yml` (optional extra job, only if plan 007 already landed)

**Out of scope**:
- `.cargo/config.toml` — the default stays native; portability is an explicit build path, not a default change.
- Multiple-architecture targets (ARM64 etc.).
- Installers/packages; the default network is embedded, so this plan produces a
  self-contained runnable `.exe` and requires no adjacent network file.
- PGO for the portable build (contradiction in terms).

## Git workflow

- Keep each implementation or review follow-up focused, with a sentence-case imperative
  subject.

## Steps

### Step 1: Create `harness/build_portable.ps1`

Model the script's structure on `harness/build_pgo.ps1` (read it first; match its parameter style, logging, and metadata stamping). The body:

1. Clear higher-precedence `CARGO_ENCODED_RUSTFLAGS` and inherited
   `RUSTUP_TOOLCHAIN`, set
   `$env:RUSTFLAGS = '-C target-cpu=x86-64'`, and set `$env:CARGO_TARGET_DIR` to
   dedicated `target\portable-build`; restore or remove all four caller values in
   `finally` according to their exact initial state. Treat `rust-toolchain.toml` as a
   tracked build input before build and publication.
2. Build `cargo build --release -p mf-uci --bin manifold`, verify the executable in
   `target\portable-build\release`, and never write `target\release\manifold.exe`.
3. Use the default embedded network, so no adjacent `nets` copy is required.
4. Copy the portable output once into unique staging immediately after build; run all
   portable gates and hash calculation against that staged file, then add the exact
   40-hex engine-source sidecar and metadata before rollback-safe publication to
   `target\portable`.

**Verify**: script exits 0; `target/portable/manifold.exe bench` prints the **same** node signature as the native build.

### Step 2: Runtime sanity on this machine

`target/portable/manifold.exe` on a BMI2-capable machine still works while retaining
baseline x86-64 compatibility: run `bench`, plus a 10-game smoke against the native
build if fastchess is configured (`harness/run_match.ps1`, honoring the AGENTS.md
affinity rules for 1T). Both binaries should be closely matched in strength (the
portable one simply lacks BMI2 speed).

**Verify**: bench signature identical; smoke match 0 forfeits (score irrelevant).

### Step 3: Prove the binary is actually baseline

Parse the host from the active pinned `rustc -vV`; resolve `llvm-profdata.exe` and
`llvm-objdump.exe` only under that exact host beneath the active `rustc --print sysroot`,
without falling back to another installed toolchain. Disassemble the staged portable
binary and match only decoded instruction tokens (never comments or metadata):

- Reject portable `pext`, `pdep`, `bzhi`, `mulx`, `sarx`, `shlx`, `shrx`, and `rorx`.
- Test all forbidden tokens through a controlled text fixture; native scan output is
  informational only and must not gate portable builds on non-BMI2 hosts.

**Verify**: zero specified instruction matches in portable; controlled fixture detects
every specified mnemonic without matching comments or metadata.

### Step 4: Magic-backend confidence

Without BMI2, the sliding layer auto-selects the black-magic backend — the same one `force-magic` tests. Run the full magic-backend test suite, and additionally run the portable binary against a perft anchor:

`target/portable/manifold.exe perft 5` → 4,865,609 (startpos depth 5; cross-check the anchor value against `crates/mf-core/tests/perft.rs` before using it).

**Verify**: `cargo test -p mf-core --features force-magic` → all pass; portable perft matches the anchor exactly.

### Step 5: Document

README Build section: two artifacts (native, tuned to the build machine, fastest; portable, runs on any x86-64), the one-command portable build, and the caveat that portable NPS is meaningfully lower on BMI2 machines. One paragraph in `harness/README.md` linking the script.

**Verify**: README instructions reproduce the build from a clean `target/`.

## Test plan

- Signature identity (native vs portable bench) — the core invariant.
- Deterministic instruction-scanner fixture plus the staged portable scan (step 3).
- Staged-binary identity after mutating the original build output.
- Embedded-network hash change rejection.
- `CARGO_ENCODED_RUSTFLAGS` clearing/restoration for initially present and absent states.
- Network revalidation inside the installed final directory's validation callback.
- Backup-cleanup failure preserves the validated final and leaves the backup remainder.
- Inherited `RUSTUP_TOOLCHAIN` is absent inside the build and restored/removed exactly.
- Dirty `rust-toolchain.toml` is rejected before build/publication.
- Rustc host parsing and exact-sysroot/host LLVM lookup are deterministic fixtures.
- Perft anchor through the portable binary (step 4).
- Optional 10-game smoke for runtime sanity.

## Done criteria

- [x] `harness/build_portable.ps1` produces `target/portable/manifold.exe` + metadata
- [x] Portable bench signature identical to native (37,420)
- [x] Zero specified BMI2-family instruction tokens in the staged portable binary; controlled scanner fixture green
- [x] Published binary is the exact staged file that passed every portable gate
- [x] Embedded-network hash remains stable through the publication callback
- [x] Higher-precedence encoded flags are cleared and exactly restored/removed
- [x] Validated publication commits before backup cleanup; cleanup failure does not roll back
- [x] `cargo test -p mf-core --features force-magic` green; portable perft 5 = 4,865,609
- [x] README documents both build paths

## STOP conditions

- `RUSTFLAGS` override does not suppress `.cargo/config.toml`'s native flag (check `--verbose` output; if config wins, STOP and report — the override mechanism differs by cargo version).
- The portable bench signature differs from native (means backend-dependent behavior leaked into search — a real bug, not a build issue; report).
- BMI2 instructions survive the baseline build (codegen path found another way in; report the symbols).

## Maintenance notes

- The instruction-scan is the guard that keeps this honest; re-run it whenever the toolchain changes (a new rustc can emit BMI2 from different intrinsics).
- If a distribution channel is added later (plan-007 CI artifact upload), build the portable artifact in CI with the same RUSTFLAGS and scan there.
- NNUE AVX-512 work (deferred investigate item) must remain runtime-dispatched or it breaks this contract.
