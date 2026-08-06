# M4-F1 — Integration audit and mission-build consolidation

**Purpose:** consolidate the mission build. Promote the M3 baseline, audit every toggle
default against its recorded keep/revert decision, run every gate, re-check SMP scaling and
memory behaviour, run the authoritative release perft suite once, and fix the two known test
flakes plus one piece of dead scaffolding.

**Decision: SHIP.** All gates green, all toggle defaults match their results docs, SMP
scaling is at the top of its pre-mission band, the 8T large-Hash smoke match is clean with
correct memory behaviour, and the full release perft suite passes.
`baselines/mission-final/manifold.exe` exists and benches 44,737.

This feature changed **no engine code**. The four commits touch test harnesses, one doc
comment, and the match driver. The mission-final binary is functionally identical to
`baselines/m3-search` and benches identically.

---

## 0. Baseline promotion

`baselines/m3-search/manifold.exe` + `build-metadata.txt`, built from `c43dcdf`, bench
**44,737**, SHA-256 `D080358B...FB5D`.

This is the promotion M3-F4's results doc called for and no earlier feature performed.
M3-F4 shipped `UsePostLMRDepth` **ON**, which moved the shipped signature 45,036 → 44,737,
so `baselines/m2-nnue` is no longer the previous *kept* build and is the wrong thing for
M4-F1b and M4-F2 to measure against. Three M3 features before it (qsearch checks, capture
LMR, TM effort) shipped OFF and correctly promoted nothing; two more (leaper LUTs, thread
history) were behaviour-neutral at 1T. M3-F4 is the one that moved.

## 1. Toggle audit — every default matches its results doc

Read from the **live `uci` handshake** against the release binary, not from the source, so
this checks what the engine actually advertises. Cross-checked against
`SearchOptions::default()` in `crates/mf-search/src/search.rs`.

| Toggle | Decision (results doc) | Measured | Default |
|---|---|---|---:|
| `UsePostLMRDepth` | **KEEP** — `MSN-S4-postlmr` | +3.47 ± 21.91 Elo, better fixed-depth nodes | `true` |
| `UseQSearchChecks` | REVERT — `MSN-S1-qchecks` | -12.75 ± 23.01 Elo | `false` |
| `UseCaptureLMR` | REVERT — `MSN-S2-capture-lmr` | -8.11 ± 20.67 Elo | `false` |
| `UseTimeEffort` | REVERT — `MSN-S3-tm-effort`(`-ltc`) | -17.39 ± 18.99 STC, -34.86 ± 44.35 LTC | `false` |
| `UsePostLMRContHist` | REVERT — `MSN-S4-postlmr` | +5.9% nodes, no depth gained | `false` |

Every loser defaults OFF. Pre-mission settled negatives are also still OFF:
`UsePawnHistory`, `UseHistoryPruning`, `UseCorrHistMajor`, `UseCorrHistMaterial`.

M3-F5 (leaper LUTs) and M3-F6 (thread-invariant history sizing) ship unconditionally with no
toggle, which is correct — both are behaviour-neutral refactors verified bit-exact across the
full anchor vector by their own features.

## 2. Gates and anchors

| Gate | Result |
|---|---|
| `cargo test --workspace` (debug) | **exit 0**, 62 test targets, 0 failures (15m18s) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo test --release -p mf-uci --test bench_cli` | **20/20** (54.8s) |
| `manifold bench` × 2 consecutive | **44,737 / 44,737** — identical |

The bench_cli suite is the anchor check: it re-verifies the full 34-value ablation vector
against the final defaults, including `post_lmr_depth_ships_enabled_and_reproduces_the_m3_signature`
(`UsePostLMRDepth=false` → 45,036 bit-for-bit), the three disabled-technique signatures
(41,588 capture LMR / 48,017 qsearch checks / 46,541 post-LMR conthist), and
`the_time_effort_term_cannot_move_the_fixed_depth_bench_signature`.

## 3. Threads-independence

`uci_protocol::fixed_depth_output_is_identical_at_every_thread_count` passes in release.
`cargo test --release -p mf-search --test smp` 11/11.

Per M3-F6 this is now a real guard rather than a tripwire — the inherited
`SharedHistory::new(thread_count)` bucket-sizing coupling is fixed, so a failure here would be
a genuine regression.

## 4. SMP scaling — `mtbench --threads 1,2,4,8`

Five alternating repeats, medians (M3-F6's lesson: **one mtbench run is jitter** — its first
sweep read -10% at 1T and -12% at 8T, and five repeats put 8T at +8.5%). Raw output in
`mtbench.txt`. Machine idle, 4% pre-run CPU.

| Threads | median NPS | samples (sorted) | speedup | efficiency |
|---:|---:|---|---:|---:|
| 1 | 560,920 | 555,027 – 573,343 | 1.00x | — |
| 2 | 1,117,462 | 1,072,738 – 1,134,284 | 1.99x | **99.6%** |
| 4 | 2,010,399 | 1,919,048 – 2,169,789 | 3.58x | **89.6%** |
| 8 | 3,765,267 | 3,329,553 – 4,028,336 | 6.71x | **83.9%** |

**No regression flagged.** The M6 pre-mission note recorded 65–85% NPS efficiency at 8T;
83.9% sits at the top of that band. Node counts at 1T are identical across all five repeats
(300,272), which is the determinism cross-check.

Note this is an NPS measurement at fixed depth 10, not a strength measurement, and it is
compared only within one build — AGENTS.md forbids cross-thread-count comparison at a fixed
node budget, which is a different thing.

## 5. 8T smoke match at the largest guardrail-permitted Hash

Directory: `smoke-8t-hash4096/`. Command in `run-metadata.txt`.

```
harness\run_match.ps1 -OutDir experiments\MSN-M4-F1-integration\smoke-8t-hash4096 `
  -AName final-8t     -ACmd .\target\release\manifold.exe          -AThreads 8 `
  -BName m3-search-8t -BCmd .\baselines\m3-search\manifold.exe     -BThreads 8 `
  -Hash 4096 -Rounds 10 -Seed 20260806
```

**Hash was sized by the new guardrail, not by guesswork.** With ~16.2 GB free the budget is
11,358 MiB; at concurrency 1 (two engine processes) that permits Hash ≤ 5,679 MiB, so 4096 is
the largest power of two that fits. 8192 — the advertised maximum — would need 16,384 MiB and
is correctly refused.

| | |
|---|---|
| Games | 20 (10 rounds × 2, paired openings) |
| Conditions | Threads=8 both sides, **no affinity, concurrency 1** (harness-enforced) |
| **Forfeits / crashes / illegal moves** | **0 / 0 / 0 for both engines** |
| Script exit | **0** |
| Wall | 9m35s |
| Elo | -34.86 ± 110.23 (Ptnml [1,3,3,3,0]) |

The Elo figure is **not a result** and must not be quoted as one: 20 games against a
functionally identical binary carries a ±110 error bar. The pass bar for this match is zero
forfeits, which it met.

### Memory under 8-thread search

Sampled with `Get-Process` in a live UCI session (the in-match monitoring job returned no
samples, so this was measured directly and is the reported number):

| State | WorkingSet64 |
|---|---:|
| net loaded, default Hash 16 | 0.12 GB |
| after `Hash 4096` + `Threads 8`, `readyok` | 4.11 GB |
| during an 8-thread `go movetime 8000` | 4.12 GB |

**Delta over baseline = 4.00 GB for a 4096 MiB Hash** — exactly the M1-F2 design, and it does
not grow with the thread count, which is the property under test (the TT is shared across
workers, not per-worker). No leftover processes after the session or the match.

## 6. Authoritative release perft suite

`cargo test --release -p mf-core` — **all green, 13m07s.** Run once for the mission, per
AGENTS.md.

| Target | Result |
|---|---|
| `perft.rs` (8 tests, incl. startpos depth 6 = **119,060,324**) | ok, 27.8s |
| `perft_ethereal.rs` — standard suite to depth 6 | ok, 175.1s |
| `perft_fischer.rs` — Chess960 1–240 | ok, 147.7s |
| `perft_fischer_2.rs` — Chess960 241–480 | ok, 137.2s |
| `perft_fischer_3.rs` — Chess960 481–720 | ok, 146.8s |
| `perft_fischer_4.rs` — Chess960 721–960 | ok, 138.9s |
| `perft_differential.rs` (cozy-chess oracle) | ok |
| plus 10 unit + 32 other integration tests | ok |

## 7. mission-final

`baselines/mission-final/manifold.exe` + `build-metadata.txt`, from `6aa0f28`, bench 44,737.

**Its SHA-256 differs from `baselines/m3-search` while being functionally identical**, because
an MSVC release link is not byte-reproducible across invocations (embedded link timestamp).
Compare these two by bench signature and anchor vector, never by hash. Both bench 44,737 and
neither has any search/eval difference.

## 8. Dead scaffolding: `select_best_result` was never dead

`crates/mf-search/src/vote.rs` carried `#[allow(dead_code)]` on `select_best_result`. The
function is **live**: `ThreadPool::search` calls it for every `DispatchMode::AllWorkers`
dispatch (`thread_pool.rs:399`), i.e. it *is* the Lazy-SMP best-thread selection. So neither
branch of the feature's "wire it in or delete it" applied — the attribute was stale and
suppressing nothing. Removing it produces no warning, which is the proof; it was replaced by a
doc comment naming the caller. Commit `cd843cb`.

`#[allow(dead_code)]` now appears nowhere in mission code. The two remaining instances are
pre-existing and legitimate: `crates/mf-core/tests/common/mod.rs` (shared test helpers not
used by every consumer of the module) and a generated line in `crates/mf-core/build.rs`.

## 9. Memory pre-flight guardrail in `run_match.ps1`

Commit `6aa0f28`. Added as **RULE 3**, following RULE 1's refusal pattern: exit 2, print the
arithmetic, name the fix.

M1-F2 established that a match whose Hash does not fit in free physical memory **pages**, and
that the symptom of paging is engines losing on time — the exact signal RULE 2 reserves for
distinguishing a broken harness config from a genuine engine defect. A paging run is therefore
inadmissible whatever it prints, which makes it a refusal rather than a warning.

The process count is the whole point and is the part that is easy to get wrong: fastchess runs
`-concurrency` **games** at once and each game holds **two** engine processes, so a 1T match
at the mandated concurrency 8 has **sixteen** engines alive, each with its own Hash.

Verified three ways before the smoke match relied on it:

| Case | Arithmetic | Outcome |
|---|---|---|
| 1T, Hash 4096 | 16 × 4096 = 65,536 MiB vs 8,879 MiB budget | **refused, exit 2**, suggests Hash ≤ 554 or Threads≥2 |
| 1T, Hash 64 (default) | 16 × 64 = 1,024 MiB vs 8,880 MiB | passes, match completes, exit 0 |
| 8T, Hash 4096 | 2 × 4096 = 8,192 MiB vs 8,863 MiB | passes, match completes, exit 0 |

The 70% figure is a headroom allowance against the OS and any foreign process growing during
a match, not a measured cliff.

## 10. `bench_cli` harness flake — fixed

Commit `6f29ef0`. `run_uci_session` drained the child's pipes only via `wait_with_output`
*after* exit. A Windows pipe buffers ~64 KiB, so a session emitting more than that blocked the
engine inside its own `write` while the harness waited for an exit that could not arrive; the
3000 s watchdog then killed the child and it surfaced as
`assertion failed: output.status.success()` — which reads like an engine crash but is the
harness starving the engine of pipe space. Both pipes are now drained concurrently on their own
threads for the whole session.

**Verified red, not assumed.** Reverting only the drain order made the new test hang past
**1200 s**; with the fix it passes in **0.76 s**.

The regression test drives **2,500 `setoption name Threads` echoes (~75 KiB)** rather than
searches. That is a deliberate instrument choice: it clears the buffer in under a second in
*both* profiles, whereas reaching the same volume through searches costs tens of seconds per
run. Two candidate search-based versions were measured and rejected first — a single
`go movetime 4000` from kiwipete emits ~77 KiB in release but only ~40 KiB in debug (so the
test would silently stop exercising the deadlock in the very profile where it fires), and the
three-search version that cleared both (114 KiB debug / 203 KiB release) cost ~12 s.

## 11. `movetime_and_clock_go_forms_honor_bounded_budgets` flake — fixed

Commit `2f0d3c6`. Reproduced at **1 failure in 10** full `uci_protocol` runs before any
change.

**The bound was not loosened.** The diagnosis is what decided the fix: the failing arm is
`movestogo 40` inflating from its usual ~137 ms to **338 ms**, not `movestogo 2` shrinking. So
the engine genuinely overshoots when ~40 sibling engines are searching — this is real load, not
measurement error.

That was established by trying the cheaper fix first. Switching the measurement from wall clock
to the engine's own reported `time` field was implemented and stress-tested alone, and **still
failed 2 of 15 full runs** with an identical signature (405 ms vs 338 ms). Supporting probe
data, 6 samples under 40 competing engine processes: wall and engine-reported time agree to
within 3 ms on an idle machine and to within 40 ms under load, and the *difference* between the
two arms stays 271–283 ms by either instrument. The instrument was never the problem.

`perft.rs`'s global-mutex precedent does not transfer, and that is the interesting part: a
mutex held by one test excludes nothing when the contention is **the other 41 tests in the same
binary**, which cargo runs in parallel and which spawn engines of their own. So the exclusion
is inverted — a `static RwLock<()>`, where every ordinary engine (`InteractiveUci::spawn` and
`run_uci`) takes a **read** guard and shares the machine freely, and this one test takes the
**write** guard and gets the machine to itself for the length of its session.

The engine-reported timing is kept alongside the guard. It strips 32–40 ms per arm of pipe and
scheduler cost that belongs to the harness rather than the time manager, and reading the
engine's clock is the right instrument for an assertion about the engine's clock. It is
documented as *not* being the fix, so a future reader does not mistake it for one.

| | before | after |
|---|---|---|
| full-suite failures | 1/10, then 2/15 | **0/20** |
| suite runtime | 7.0 s | 7.9 s |

### Reusable lesson

**A flake is a measurement, and the first question is whether the instrument or the subject is
wrong.** Both fixes here looked like "loosen the assertion" problems and neither was: one was a
harness deadlock that the assertion was faithfully reporting, and the other was a real
load-dependent overshoot that a better instrument could not hide. In both cases the cheap
diagnosis (revert one line and watch it hang; check *which* arm moved) picked the fix, and in
both cases the red state was reproduced before the fix was trusted.

---

## Artifacts

- `mtbench.txt` — five raw `mtbench --threads 1,2,4,8` sweeps.
- `smoke-8t-hash4096/` — `run-metadata.txt`, `console.txt`, `fastchess.log`, `games.pgn`
  for the 8T Hash-4096 smoke match. (`games.pgn` is untracked per repo convention; the seed
  and command in `run-metadata.txt` reproduce it.)
- Commits: `cd843cb` (vote.rs), `6f29ef0` (bench_cli drain), `2f0d3c6` (timing test guard),
  `6aa0f28` (harness memory guardrail).
