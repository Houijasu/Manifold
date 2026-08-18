# `harness/` — tracked measurement drivers

**Why this directory is tracked.** `experiments/`, `baselines/`, and `tools/` are all listed
in `.git/info/exclude`, so a script living there does not survive a fresh clone and is one
`git clean` away from being lost. The flag rules these drivers encode were each paid for with
a wrong conclusion; they belong in version control.

| file | purpose |
|---|---|
| `run_match.ps1` | Guard-railed fastchess match driver. **Use it for every match.** |
| `nps_compare.py` | Fair two-engine NPS/nodes comparison with warmup handling. |
| `build_pgo.ps1` | Reproducible PGO build with a hard node-signature gate. |
| `build_portable.ps1` | Verified baseline x86-64 release build and BMI2 scan. |

## `run_match.ps1`

Enforces, mechanically, what mission `AGENTS.md` §4.451 says must not be left to a comment:

1. **Affinity and concurrency are DERIVED from the thread counts and are not overridable.**
   - Both engines `Threads=1` → `-use-affinity` + `-concurrency 8`.
   - Any engine `Threads>1` → **no** `-use-affinity`, `-concurrency 1`.

   Passing a `-Concurrency` that contradicts the mandated value makes the script **refuse to
   run** (exit 2). Both directions are refused: pinning a multi-threaded engine manufactured
   a ~600 Elo artifact with 69 forfeits in 140 games, and *omitting* affinity when both
   engines are single-threaded invalidates every M1–M5 SPRT.

2. **Per-player time-forfeit, crash, and illegal-move accounting.** Forfeits are attributed
   from the PGN (the forfeiting side is the loser of a `[Termination "time forfeit"]` game)
   and cross-checked against the fastchess console `Player:`/`Timeouts:`/`Crashed:` summary.
   Any non-zero count **aborts loudly** (exit 3) with "not admissible evidence".

   A per-player forfeit count read off the console was the *only* signal that distinguished
   an invalid harness configuration from a genuine time-manager bug. It is now automatic.

3. **`Illegal PV move` warnings are counted SEPARATELY from illegal moves played.** The
   former is fastchess objecting to a printed PV line and is cosmetic; the latter is a fatal
   engine defect. Conflating them invents findings.

4. **Writes the mandatory §4.7 `run-metadata.txt` BEFORE the match starts**, so even a
   killed run carries provenance: the driver commit plus each binary's source commit,
   attestation mode, and SHA-256; TC, seed, book, affinity/concurrency/threads/hash, SPRT
   bounds, pre-run CPU load (max of 5 samples, since `Win32_Processor` intermittently
   returns `null` on this machine), purpose, date, and the full command line. An exact
   40-hex `<binary>.source-commit` sidecar is authoritative. Without one, only binaries
   under this worktree's `target/` directory infer the current worktree HEAD; other binaries
   remain `unknown`/`unattested`.

5. `-ForfeitsAllowedFor <name>` is the only escape hatch, for third-party opponents whose
   failures the validation contract says to record separately rather than charge to Manifold
   (A-EONEGO-001). It still warns prominently and writes the counts into `run-metadata.txt`.

Guardrail self-tests, including a verified forfeit abort whose PGN attribution matched the
console counts exactly, are in `experiments/M4-F4-harness-selftest/`.

The default fastchess executable and opening book are resolved relative to the checkout
containing `harness/run_match.ps1`, so the driver works unchanged from linked worktrees.

### Examples

```powershell
# Single-threaded SPRT. Affinity on and concurrency 8 are chosen for you.
.\harness\run_match.ps1 -OutDir experiments\my-run -Purpose 'what this is evidence FOR' `
    -AName new  -ACmd .\target\release\manifold.exe `
    -BName base -BCmd .\baselines\M4\manifold.exe `
    -TC '8+0.08' -Rounds 2000 -Seed 20260901 `
    -Sprt 'elo0=0 elo1=5 alpha=0.05 beta=0.05'

# Multi-thread match. Affinity is refused and concurrency forced to 1 for you.
.\harness\run_match.ps1 -OutDir experiments\smp -Purpose '8T vs 1T at equal time' `
    -AName T8 -ACmd .\target\release\manifold.exe -AThreads 8 `
    -BName T1 -BCmd .\target\release\manifold.exe -BThreads 1 `
    -TC '10+0.1' -Hash 128 -Rounds 150 -Seed 20260902
```

## `nps_compare.py`

`py -3.14 harness/nps_compare.py --engine A=<exe> --engine B=<exe> --depth 12 --hash 64 --warmup 1 --repeat 3`

Keeps **one** engine process alive for every measurement, so process startup (for a .NET
NativeAOT engine, ~0.9 s of embedded-net loading) is paid once and is never inside a timed
region. Runs `--warmup` **discarded** searches per position before any timed search and
reports the **median** of `--repeat` timed ones, so one scheduling hiccup cannot decide the
number. A cold-start comparison is unfair and is explicitly disallowed by A-EONEGO-004.

Drives both engines with **stdin held open**, polling stdout until `bestmove`. Piping a
script that ends in `quit` aborts the search and manufactures convincing false positives.

## `build_pgo.ps1`

`.\harness\build_pgo.ps1 [-BenchRuns 3] [-MeasureNps]`

Runs the `research/rust-perf-and-nnue-training.md` §0.5 PGO round-trip as one command:
plain, instrumented `-Cprofile-generate`, and optimised `-Cprofile-use` builds isolated under
`target\pgo-build\baseline`, `target\pgo-build\instrumented`, and
`target\pgo-build\optimized`; `bench` profiling runs; `llvm-profdata` merge from the pinned
toolchain; and provenance in `target\pgo\pgo-metadata.txt`.

The verified copies are published as experimental artifacts at
`target\pgo\manifold-nopgo.exe` and `target\pgo\manifold-pgo.exe`, each with an exact 40-hex
`.source-commit` sidecar. The script never replaces `target\release\manifold.exe`; these PGO
outputs are not shipping/release artifacts. Publication is fixed to `target\pgo`: the script
validates both binaries, sidecars, hashes, profile, and metadata in a staging directory before
replacing the prior output, and restores the prior output if final validation fails. It also
restores the caller's `CARGO_TARGET_DIR` and `RUSTFLAGS` values on every exit path.

Two things it enforces mechanically:

1. **`-C target-cpu=native` is re-stated in every `RUSTFLAGS` stage.** Setting
   `RUSTFLAGS` replaces `.cargo/config.toml`'s rustflags wholesale; forgetting this loses
   BMI2/PEXT and the AVX-VNNI kernels and invalidates any before/after number.
2. **The deterministic bench node signature must be identical before and after PGO.**
   A drift means the optimiser changed the search, and the script aborts (exit 4).

Measured on this repo (2026-08, depth-12 `nps_compare.py` medians): geomean 1.00x --
parity, not a gain, because fat LTO + `codegen-units = 1` + `target-cpu=native` leave
PGO little headroom. Re-run after large source changes.

## `build_portable.ps1`

`pwsh -NoProfile -File harness/build_portable.ps1`

Builds native and baseline x86-64 references under `target\native-build` and
`target\portable-build`, then publishes `target\portable\manifold.exe` only after both
bench signatures equal 37,420, portable perft 5 equals 4,865,609, the force-magic tests
pass, and pinned-toolchain `llvm-objdump` finds no `pext`, `pdep`, `bzhi`, `mulx`,
`sarx`, `shlx`, `shrx`, or `rorx` instruction tokens. A controlled disassembly-text
fixture tests the scanner independently of host CPU features; native scan results are
informational only. The portable build output is copied once into unique staging, and
bench, perft, disassembly, and hashes all use those exact staged bytes. Publication
includes an exact source-commit sidecar and metadata with toolchain, flags, stable
network/binary hashes, signatures, and disassembler path; staging rollback prevents
partial artifacts. The script restores caller `RUSTFLAGS`/`CARGO_TARGET_DIR`, rejects
an embedded-network change during the run, and keeps `target\release\manifold.exe`
byte-for-byte unchanged. The default embedded network means no adjacent `nets` copy is
required.
