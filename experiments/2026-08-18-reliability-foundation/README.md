# Reliability foundation validation

## Provenance

- Engine source commit profiled:
  `985c6af973df1d2ed284b408a1b8900a4cd71ac8`
- NNUE fixture:
  `C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation\nets\main.nnue`
- NNUE size: `111,261,604` bytes
- NNUE SHA-256:
  `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A`
- Working directory:
  `C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation`
- Shell: PowerShell 7

The captured command and environment were:

```powershell
$env:MF_NNUE_TEST_NET = 'C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation\nets\main.nnue'
cargo run --release -p mf-search --features instrumentation --example search_profile -- 7 |
    Tee-Object 'experiments\2026-08-18-reliability-foundation\search-profile.txt'
```

`search-profile.txt` contains exactly the six stable `key=value` profile rows emitted
on standard output. Cargo diagnostics were emitted separately on standard error.

## Final local gates

All commands below used the same `MF_NNUE_TEST_NET` value shown above.

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `cargo test --workspace` | Passed |
| `cargo test -p mf-core --features force-magic` | Passed |
| `cargo test --release -p mf-core --test perft` | Passed: 8/8 tests |
| First uninstrumented `cargo run --release -p mf-uci --bin manifold -- bench` | Passed: 37,420 nodes |
| Second uninstrumented `cargo run --release -p mf-uci --bin manifold -- bench` | Passed: 37,420 nodes |
| `pwsh -NoProfile -File harness/provenance.tests.ps1` | Passed |
| `pwsh -NoProfile -File harness/build_pgo.tests.ps1` | Passed |
| `pwsh -NoProfile -File harness/build_portable.tests.ps1` | Passed |
| `git diff --check` | Passed |

The branch has not been pushed. The Windows and Ubuntu CI jobs have therefore not run,
and their plan checkboxes remain open.

## Integration fixes included in this validation

This final evidence refresh includes:

- `1c8dc31` — complete explicit NNUE fixture enforcement;
- `8423434` — seed game-zero self-play checkpoints so pre-100-game resume is safe;
- `0d56665` — stabilize PGO network and NPS evidence;
- `447b4af` — honor the explicit NNUE fixture in UCI unit tests;
- `84a94c3` — pin PGO tools to the active Rust toolchain;
- `985c6af` — pin portable-build toolchain identity.

The authoritative workspace tests ran with the verified explicit fixture, so the
fixture contract, UCI unit cleanup, and pre-100-game resume checkpoint regressions were
part of the passing suite.

## Full PGO run

Command:

```powershell
pwsh -NoProfile -File harness/build_pgo.ps1 -BenchRuns 3 -MeasureNps
```

Result: exit `0`. The complete five-stage pipeline built a dedicated baseline, collected
three instrumented bench profiles, merged them, built the PGO binary, verified both
node signatures, and ran the machine-local NPS comparison.
The run completed on this exact clean HEAD immediately before the evidence refresh.
Independent checks then verified the published artifacts and metadata.

| PGO evidence | Value |
|---|---|
| Source commit | `985c6af973df1d2ed284b408a1b8900a4cd71ac8` |
| Baseline signature | 37,420 nodes |
| PGO signature | 37,420 nodes |
| Baseline artifact | `target\pgo\manifold-nopgo.exe` |
| Baseline SHA-256 | `ADD38AD73A587DDAF49400AAD532238C4E56E9233991350744834A716EE82821` |
| PGO artifact | `target\pgo\manifold-pgo.exe` |
| PGO SHA-256 | `816CD92734707D9A3CB74367A729C66BB01F0418C9BF529C3DDC14B1EE40B5A3` |
| Merged profile | `target\pgo\merged.profdata` |
| Profile SHA-256 | `B557008D62DF9DD54160AB1E197F2E4ECA92EB3E1E84EF6B9EF8E471C48F0673` |
| NPS evidence | `target\pgo\nps-verdict.txt` |
| NPS evidence SHA-256 | `9D3B69D7C7CA4A5B862AC967D7367BB1963E3F83C3785037DC28B632C9487E19` |
| NPS verdict | Passed, exit 0 |
| Network size / SHA-256 | `111,261,604` / `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A` |
| Baseline source sidecar | `985c6af973df1d2ed284b408a1b8900a4cd71ac8` |
| PGO source sidecar | `985c6af973df1d2ed284b408a1b8900a4cd71ac8` |
| Ordinary release SHA-256 before/after | `A47749622EB717ED92B95C44233BA119EF495A60E74060164285EF1F80B9A8FE` |
| Ordinary release preserved | Yes |

The NPS comparison used depth 12, Hash 64, Threads 1, one discarded warmup, and three
timed repeats per position. It completed successfully with geometric mean
`baseline / PGO NPS = 0.96x` and nodes-to-depth ratio `1.00x`. Per-position NPS ratios
were `0.95x`, `0.97x`, `0.98x`, and `0.93x`. This is a machine-local observation, not
a portable speed claim or a strength result. The PGO binary remains experimental and
is not the shipping `target\release\manifold.exe`.

| Position | Baseline median NPS | PGO median NPS |
|---|---:|---:|
| startpos | 717,311 | 753,033 |
| kiwipete | 607,302 | 627,594 |
| midgame | 699,758 | 711,879 |
| endgame | 1,217,633 | 1,311,045 |

Independent checks confirmed every published hash, both exact source sidecars, the
passed NPS status/exit, and the network identity against the artifacts on disk. The
completed PGO run preserved the pre-existing ordinary release byte-for-byte.

Pinned toolchain evidence:

| Toolchain field | Value |
|---|---|
| rustc | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM `22.1.6` |
| sysroot | `C:\Users\Samaritan\.rustup\toolchains\1.97.1-x86_64-pc-windows-msvc` |
| host | `x86_64-pc-windows-msvc` |
| llvm-profdata | `C:\Users\Samaritan\.rustup\toolchains\1.97.1-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe` |

`target\pgo\pgo-metadata.txt` records the clean source tree, source commit, exact
rustc/sysroot/host/llvm-profdata identity, cleared `RUSTUP_TOOLCHAIN` and
`CARGO_ENCODED_RUSTFLAGS`, stable network, artifact/profile hashes, three bench runs,
the 37,420-node signature, and the successful NPS command exit.

## Full portable run

Command:

```powershell
pwsh -NoProfile -File harness/build_portable.ps1
```

Result: exit `0`.

| Portable evidence | Value |
|---|---|
| Source commit/sidecar | `985c6af973df1d2ed284b408a1b8900a4cd71ac8` |
| Native bench | 37,420 nodes, 725,528 NPS |
| Portable bench | 37,420 nodes, 725,493 NPS |
| Portable perft 5 | 4,865,609 nodes |
| Force-magic suite | Passed |
| Portable forbidden instruction scan | No `pext`, `pdep`, `bzhi`, `mulx`, `sarx`, `shlx`, `shrx`, or `rorx` |
| Native scan, informational | `bzhi`, `mulx`, `pext`, `rorx`, `shlx`, `shrx` |
| Portable artifact | `target\portable\manifold.exe` |
| Portable SHA-256 | `29BA0FA60A6C18A41E10059D04F69CFA9150E8480B897F8BDF1E1A1B0CE8A179` |
| NNUE SHA-256 before/after | `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A` |
| Ordinary release SHA-256 before/after | `A47749622EB717ED92B95C44233BA119EF495A60E74060164285EF1F80B9A8FE` |
| NNUE and ordinary release preserved | Yes |

The native and portable NPS values are observations from this run only. The gates are
the equal 37,420-node signatures, exact perft anchor, passing force-magic suite, clean
portable instruction scan, stable NNUE input, valid source sidecar, and preserved
ordinary release binary.

`target\portable\build-metadata.txt` records source HEAD, rustc/LLVM identity, native
and portable flags, cleared encoded flags, NNUE and binary hashes, bench/perft
signatures, force-magic status, disassembler path, instruction-scan results, and
ordinary release preservation.

This full portable run had completed immediately before the evidence refresh on the
same clean HEAD. Independent verification matched its exact sidecar, source and
toolchain identities, host, pinned `llvm-objdump`, artifact and network hashes, gate
results, instruction scan, and release-preservation record, so it was not rerun.

## Same-binary provenance smoke

Exact command:

```powershell
pwsh -NoProfile -File harness/run_match.ps1 `
    -OutDir 'experiments\2026-08-18-reliability-foundation\provenance-smoke-985c6af9' `
    -Purpose 'Final implementation same-binary provenance smoke at 985c6af9' `
    -AName 'Manifold-A' -ACmd 'target\release\manifold.exe' `
    -BName 'Manifold-B' -BCmd 'target\release\manifold.exe' `
    -TC '1+0.01' -Hash 16 -Rounds 1 -Seed 20260819 -RatingInterval 1
```

Output directory:

```text
C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation\experiments\2026-08-18-reliability-foundation\provenance-smoke-985c6af9
```

The ignored opening-book fixture was verified at 16,226,151 bytes with SHA-256
`3F499996FF0B674A04F85F2634811D102DD53B5115841E8F11D18E1F550BA2CA`.

| Match evidence | Value |
|---|---|
| Exit code | 0 |
| Games | 2, paired opening |
| Result | Manifold-A 1 win, 0 losses, 1 draw |
| Time forfeits | 0 for both engines |
| Crashes | 0 for both engines |
| Illegal moves | 0 for both engines |
| Driver commit | `985c6af973df1d2ed284b408a1b8900a4cd71ac8` |
| Source A / B | `985c6af973df1d2ed284b408a1b8900a4cd71ac8` |
| Source mode A / B | `inferred-target-worktree` |
| SHA-256 A / B | `A47749622EB717ED92B95C44233BA119EF495A60E74060164285EF1F80B9A8FE` |
| Affinity / concurrency | Enabled / 8 |
| Threads / Hash | 1 per engine / 16 MiB |
| PGN SHA-256 | `8B6E344C4CBF428E4418CC1CF236D6EF0AE43F5C76DFB0F3C47823C8EE3E18DD` |
| Metadata SHA-256 | `7EE07A507FF48FCF0A1F00486E468D5B4AAA5DC3DEEEA1E129ED05E0BA499948` |

`run-metadata.txt` contains the exact command, driver and binary provenance, hashes,
TC, seed, book, affinity/concurrency/thread/hash settings, CPU-load sample, purpose,
and zero-forfeit self-check.

## Aggregate profile

The six searches produced exactly `37,420` nodes:

| Metric | Aggregate |
|---|---:|
| Interior nodes | 29,594 |
| Qsearch nodes | 19,155 |
| Checked interior nodes | 1,553 |
| Checked interior fraction | 5.248% of interior nodes |
| Interior static evaluations | 7,041 |
| Qsearch static evaluations | 14,714 |
| Total NNUE forward evaluations | 21,755 |
| TT cutoffs | 5,703 |
| SEE calls | 51,432 |
| SEE cycles | 13,495,962 |
| NNUE forward cycles | 11,869,334 |
| LMR reductions | 5,785 |
| Reduced-search fail-highs | 19 |
| Full-depth re-searches | 17 |

Static-evaluation densities were:

- interior static evaluations: `237.920` per 1,000 interior nodes and `188.161`
  per 1,000 reported total nodes;
- qsearch static evaluations: `768.155` per 1,000 qsearch nodes and `393.212`
  per 1,000 reported total nodes;
- all NNUE forward evaluations: `581.374` per 1,000 reported total nodes.

The event counters have deliberately different boundaries, so `interior_nodes +
qsearch_nodes` is not used as a substitute for the profile's reported `nodes` value.

## Per-position profile

`Int eval/int` and `Q eval/q` are evaluations per 1,000 corresponding nodes.
`Int eval/total` and `Q eval/total` use each row's reported `nodes`.

| Position | Nodes | Interior | Qsearch | Checked interior | Int eval/int | Int eval/total | Q eval/q | Q eval/total | SEE calls | SEE cycles | NNUE forwards | Forward cycles |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| bench1 | 6,085 | 5,759 | 3,170 | 74 (1.285%) | 148.984 | 141.002 | 814.511 | 424.322 | 3,791 | 719,619 | 3,440 | 1,700,755 |
| bench2 | 13,484 | 8,783 | 7,562 | 480 (5.465%) | 322.441 | 210.027 | 772.547 | 433.254 | 25,340 | 7,371,737 | 8,674 | 4,902,729 |
| bench3 | 3,125 | 2,940 | 1,379 | 223 (7.585%) | 143.878 | 135.360 | 622.190 | 274.560 | 2,443 | 328,104 | 1,281 | 757,084 |
| bench4 | 6,006 | 4,542 | 3,001 | 187 (4.117%) | 280.273 | 211.955 | 761.080 | 380.286 | 8,881 | 2,027,300 | 3,557 | 2,026,879 |
| bench5 | 2,972 | 2,775 | 1,295 | 263 (9.477%) | 151.351 | 141.319 | 815.444 | 355.316 | 2,200 | 477,353 | 1,476 | 826,452 |
| bench6 | 5,748 | 4,795 | 2,748 | 326 (6.799%) | 257.560 | 214.857 | 761.281 | 363.953 | 8,777 | 2,571,849 | 3,327 | 1,655,435 |

Additional search-event rows:

| Position | TT cutoffs | LMR reductions | Reduced fail-highs | Full-depth re-searches |
|---|---:|---:|---:|---:|
| bench1 | 1,475 | 937 | 10 | 7 |
| bench2 | 1,514 | 1,793 | 0 | 0 |
| bench3 | 835 | 480 | 1 | 0 |
| bench4 | 786 | 1,141 | 0 | 0 |
| bench5 | 316 | 450 | 4 | 5 |
| bench6 | 777 | 984 | 4 | 5 |

## Pruning attempts and cutoffs

Rates are `cutoffs / attempts`. A zero attempt denominator is reported as `N/A`, not
as zero percent.

| Counter | Aggregate cutoffs / attempts | Aggregate rate |
|---|---:|---:|
| Razoring | 215 / 12,938 | 1.662% |
| Reverse futility pruning | 8,295 / 13,945 | 59.484% |
| Null-move pruning | 652 / 1,977 | 32.979% |
| ProbCut | 320 / 1,329 | 24.078% |
| Late-move pruning | 19,690 / 53,577 | 36.751% |
| Futility pruning | 23,178 / 31,886 | 72.690% |
| History pruning | 0 / 0 | N/A |
| SEE pruning | 4,191 / 14,020 | 29.893% |

Per-position cutoff rates:

| Position | Razor | RFP | NMP | ProbCut | LMP | Futility | History | SEE pruning |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| bench1 | 5/1,709 (0.293%) | 289/1,741 (16.600%) | 8/140 (5.714%) | 0/132 (0.000%) | 4,340/8,313 (52.207%) | 371/2,782 (13.336%) | 0/0 (N/A) | 461/2,465 (18.702%) |
| bench2 | 41/4,621 (0.887%) | 3,360/5,017 (66.972%) | 189/846 (22.340%) | 200/657 (30.441%) | 8,609/21,746 (39.589%) | 10,690/13,105 (81.572%) | 0/0 (N/A) | 2,137/5,839 (36.599%) |
| bench3 | 34/964 (3.527%) | 442/1,020 (43.333%) | 78/152 (51.316%) | 1/78 (1.282%) | 675/3,079 (21.923%) | 825/2,014 (40.963%) | 0/0 (N/A) | 217/1,259 (17.236%) |
| bench4 | 43/2,355 (1.826%) | 1,875/2,536 (73.935%) | 89/301 (29.568%) | 36/212 (16.981%) | 2,295/6,976 (32.899%) | 3,402/4,609 (73.812%) | 0/0 (N/A) | 637/2,162 (29.463%) |
| bench5 | 19/1,117 (1.701%) | 927/1,249 (74.219%) | 11/73 (15.068%) | 16/62 (25.806%) | 2,059/4,372 (47.095%) | 1,542/2,135 (72.225%) | 0/0 (N/A) | 130/700 (18.571%) |
| bench6 | 73/2,172 (3.361%) | 1,402/2,382 (58.858%) | 277/465 (59.570%) | 67/188 (35.638%) | 1,712/9,091 (18.832%) | 6,348/7,241 (87.667%) | 0/0 (N/A) | 609/1,595 (38.182%) |

These rates describe the existing event boundaries and are not direct estimates of Elo
or net search benefit.

## Cycle-counter interpretation

The current run recorded `13,495,962` SEE cycles across `51,432` calls and
`11,869,334` NNUE forward cycles across `21,755` forward evaluations. These values are
useful for identifying work within this one run. They are not portable timing claims:
cycle values and NPS vary with CPU, clock behavior, scheduling, compiler, and build
environment, and should not be compared as absolute performance numbers across CPUs.

## Experiment decision

### Checked-node evaluation

Checked interior nodes were a nontrivial `5.248%` of interior nodes. The current
counters do not distinguish checked nodes that performed a real NNUE forward from
checked nodes that reused a TT static evaluation. Therefore `1,553` is only an upper
bound on avoidable checked-node forwards. That upper bound is `22.057%` of the 7,041
interior forwards and `7.139%` of all 21,755 forwards in this profile.

Decision: the measured ceiling is large enough to justify requesting approval for a
separate, default-off, same-binary toggle experiment. It does not justify changing the
default or claiming a speed or strength win. Any experiment still needs its own approved
design and plan, deterministic tests, bench/NPS evidence, and fixed-time paired matches.
No strength change or experiment plan is included here.

### Threshold SEE

The profile establishes substantial existing SEE activity: `51,432` calls and
`13,495,962` run-local cycles. It does not separate threshold-capable call sites, record
the threshold distribution, or estimate how much exchange work an early exit would
avoid.

Decision: a threshold-SEE implementation is not yet justified by this profile alone.
Further work requires explicit approval and a separate plan that first proves
`see_ge(position, move, threshold)` exactly equivalent to
`static_exchange_evaluation(position, move) >= threshold` across the existing exhaustive
and random oracle. No such plan or implementation was created on this branch.

## Current review range

The current pre-evidence range is:

```text
943617a045b9cdfd7c102debd59e67e579326da8...985c6af973df1d2ed284b408a1b8900a4cd71ac8
```

It contains 32 commits and changes 37 files, with 7,433 insertions and 407 deletions.
The final implementation fixes are the explicit-fixture, game-zero resume, PGO
network/NPS, UCI fixture, and pinned PGO and portable toolchain changes listed above.

This evidence refresh validates the current clean HEAD and updates the review range. It
does not claim that a final whole-range re-review has approved these fixes.
Remote Windows and Ubuntu CI execution also remains pending because the branch has not
been pushed.
