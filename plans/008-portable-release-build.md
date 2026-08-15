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

## Why this matters

`.cargo/config.toml` compiles the Windows MSVC target with `target-cpu=native`. Every release binary this repo has ever produced SIGILLs on any CPU without BMI2 — which currently blocks shipping the engine to anyone who did not build it. The codebase already contains everything needed for a portable build: the sliding-attack backend auto-selects black-magics when BMI2 is absent at compile time, and the NNUE SIMD backend dispatches at **runtime** (Scalar/Avx2/Avx2Vnni), so only the sliding layer and generic codegen need a baseline target. This plan adds a portable build path and a check that keeps it honest.

## Current state

- `.cargo/config.toml` (repo root, applies to `x86_64-pc-windows-msvc`):
```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-cpu=native"]
```
  (AGENTS.md documents the consequence: native builds select the PEXT backend via BMI2; `force-magic` selects black-magics for testing.)
- `crates/mf-core/Cargo.toml:9` — the `force-magic` feature exists precisely to exercise the non-PEXT backend.
- `crates/mf-core/src/sliding.rs` — PEXT code is gated on `target_feature = "bmi2"` with a debug cross-check; without BMI2 at compile time the black-magic backend is selected automatically (per AGENTS.md "Non-Obvious Build & Test Behavior").
- `crates/mf-nnue/src/simd.rs` — `SimdBackend` dispatch is runtime-detected (`is_x86_feature_detected!` style; backends Scalar/Avx2/Avx2Vnni) — no portability work needed there.
- `harness/build_pgo.ps1` exists with a bench-signature gate; it builds the native binary (PGO is a native-machine optimization and stays that way).
- Repo release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` (root `Cargo.toml`).
- Bench determinism: the bench node signature is backend-independent in **node count** (move ordering does not depend on the sliding backend) — the portable build must print the same signature as the native build.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Native bench (reference) | `cargo run --release -p mf-uci --bin manifold -- bench` | current pinned signature (e.g. 41,588 at time of writing — confirm the live pin) |
| Portable build | `pwsh harness/build_portable.ps1` (created by this plan) | exit 0, artifact under `target/portable/` |
| Portable bench | `target/portable/manifold.exe bench` | **same** node signature as native |
| Magic-backend tests | `cargo test -p mf-core --features force-magic` | all pass |
| No-BMI2 scan | disassembly grep (step 4) | zero `pext`/`bzhi`/`tzcnt`-encoded-as-BMI2 instructions in the portable binary's .text |

## Scope

**In scope**:
- `harness/build_portable.ps1` (create)
- `README.md` (Build section: document both artifacts)
- `harness/README.md` (one paragraph)
- `.github/workflows/ci.yml` (optional extra job, only if plan 007 already landed)

**Out of scope**:
- `.cargo/config.toml` — the default stays native; portability is an explicit build path, not a default change.
- Multiple-architecture targets (ARM64 etc.).
- Installers/packages; this plan produces a runnable .exe next to its net, nothing more.
- PGO for the portable build (contradiction in terms).

## Git workflow

- One commit: `Add a portable baseline-x86-64 release build`.

## Steps

### Step 1: Create `harness/build_portable.ps1`

Model the script's structure on `harness/build_pgo.ps1` (read it first; match its parameter style, logging, and metadata stamping). The body:

1. Override the native flag for one invocation: `$env:RUSTFLAGS = '-C target-cpu=x86-64'` (RUSTFLAGS overrides `.cargo/config.toml` rustflags entirely — verify with `cargo build --release -p mf-uci --bin manifold --verbose` showing `-C target-cpu=x86-64` and no `native`).
2. `cargo build --release -p mf-uci --bin manifold` into the normal target dir, then copy `target/release/manifold.exe` to `target/portable/manifold.exe`.
3. Copy or hard-link `nets/main.nnue` to `target/portable/nets/main.nnue` (runtime discovery looks beside the executable — `crates/mf-nnue/src/provision.rs` documents the lookup), or document the embedded-net default already covering this (the default build embeds the net — confirm, and if so skip the copy and say why in the script).
4. Stamp `target/portable/build-metadata.txt` with git SHA, RUSTFLAGS, date, and `rustc -vV`.

**Verify**: script exits 0; `target/portable/manifold.exe bench` prints the **same** node signature as the native build.

### Step 2: Runtime sanity on this machine

`target/portable/manifold.exe` on a BMI2-capable machine still works (baseline code runs everywhere): run `bench`, plus a 10-game smoke against the native build if fastchess is configured (`harness/run_match.ps1`, honoring the AGENTS.md affinity rules for 1T). Both binaries should be closely matched in strength (the portable one simply lacks BMI2 speed).

**Verify**: bench signature identical; smoke match 0 forfeits (score irrelevant).

### Step 3: Prove the binary is actually baseline

Disassemble and grep for BMI2 instructions (PowerShell-friendly options: `dumpbin /DISASM`, or `llvm-objdump -d` if on PATH, or the `iced_x86` crate as a one-off — pick what exists on this machine):

- Search the portable binary's .text for `pext`, `bzhi`, `andn`, `mulx`, `sarx`, `shlx`, `shrx`, `lzcnt`, `tzcnt` **as BMI2/BMI1/LZCNT/TZCNT encodings** (note: `rep bsf`-style `tzcnt` bytes can alias `bsf`; check for the `F3 0F BC` prefix form). Expect **zero** matches.
- Search the native binary for the same; expect many (this proves the grep works — a control).

**Verify**: zero BMI2+ instruction matches in portable; nonzero in native (control positive).

### Step 4: Magic-backend confidence

Without BMI2, the sliding layer auto-selects the black-magic backend — the same one `force-magic` tests. Run the full magic-backend test suite, and additionally run the portable binary against a perft anchor:

`target/portable/manifold.exe perft 5` → 4,865,609 (startpos depth 5; cross-check the anchor value against `crates/mf-core/tests/perft.rs` before using it).

**Verify**: `cargo test -p mf-core --features force-magic` → all pass; portable perft matches the anchor exactly.

### Step 5: Document

README Build section: two artifacts (native, tuned to the build machine, fastest; portable, runs on any x86-64), the one-command portable build, and the caveat that portable NPS is meaningfully lower on BMI2 machines. One paragraph in `harness/README.md` linking the script.

**Verify**: README instructions reproduce the build from a clean `target/`.

## Test plan

- Signature identity (native vs portable bench) — the core invariant.
- Instruction-scan control experiment (step 3).
- Perft anchor through the portable binary (step 4).
- Optional 10-game smoke for runtime sanity.

## Done criteria

- [ ] `harness/build_portable.ps1` produces `target/portable/manifold.exe` + metadata
- [ ] Portable bench signature identical to native
- [ ] Zero BMI2-family instructions in the portable binary; control positive on native
- [ ] `cargo test -p mf-core --features force-magic` green; portable perft 5 = anchor
- [ ] README documents both build paths

## STOP conditions

- `RUSTFLAGS` override does not suppress `.cargo/config.toml`'s native flag (check `--verbose` output; if config wins, STOP and report — the override mechanism differs by cargo version).
- The portable bench signature differs from native (means backend-dependent behavior leaked into search — a real bug, not a build issue; report).
- BMI2 instructions survive the baseline build (codegen path found another way in; report the symbols).

## Maintenance notes

- The instruction-scan is the guard that keeps this honest; re-run it whenever the toolchain changes (a new rustc can emit BMI2 from different intrinsics).
- If a distribution channel is added later (plan-007 CI artifact upload), build the portable artifact in CI with the same RUSTFLAGS and scan there.
- NNUE AVX-512 work (deferred investigate item) must remain runtime-dispatched or it breaks this contract.
