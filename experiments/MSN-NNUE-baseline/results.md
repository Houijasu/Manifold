# MSN-NNUE-baseline — NNUE performance baseline and incremental-update profile

**Feature:** M2-F1-nnue-profiling (milestone M2, NNUE inference speed)
**Date:** 2026-08-05
**Decision:** KEEP the instrumentation (default-off, zero production cost). **Flagged to the
orchestrator: the profile does not support the lazy-update plan as scoped.** See
[What this means for M2-F2 and M2-F3](#what-this-means-for-m2-f2-and-m2-f3).

## Purpose

Establish the currency for M2. No optimization is attempted here; this feature only measures,
so that M2-F2 (lazy updates) and M2-F3 (Finny tables) are aimed at real headroom rather than
at the roadmap's guess. The four required measurements are: per-mode forward-pass ns/eval, the
push/pop microbenchmark, a profile of incremental-update cost split by phase, and absolute 1T
NPS against `baselines/mission-start/manifold.exe`.

## Provenance

| | |
|---|---|
| Repo commit at measurement | `d2036f4` (branch `feature/nnue-optimizations`), working tree carrying only this feature's instrumentation |
| Machine | i9-13980HX (8 P-cores + 16 E-cores, 32 logical), 31.6 GB RAM, Windows 11 |
| Toolchain | rustc 1.97.1, `--release` (fat LTO, 1 CGU, `panic=abort`), `target-cpu=native` |
| Net | `nets/main.nnue`, 111,261,604 bytes |
| Production forward mode | `Avx2Vnni`, sparse FC0 (no AVX-512 on this CPU) |
| Core pinning | Every measurement ran in a shell pinned to the 8 P-cores (`ProcessorAffinity = 0xFFFF`), per the repo's P-vs-E rule |
| Baseline compared against | `baselines/mission-start/manifold.exe` (commit `0012b36`, bench 45,036) |

Raw console output for each run is committed beside this document:
`forward-throughput.txt`, `push-pop-throughput.txt`, `update-profile.txt`,
`bench-control.txt`, `nps-depth12.json`.

## 1. Forward-pass throughput per backend

`cargo run --release -p mf-nnue --example forward_throughput -- 50000 9`
(50,000 iterations x 9 samples, median reported; state supplied, so this times the forward pass
only — feature transform output through FC0/FC1/FC2 — and not accumulation.)

| Backend | Sparse FC0 | ns/eval (median) | eval/s | vs scalar | vs dense same backend |
|---|---|---|---|---|---|
| Scalar | no | 1670.42 | 598,651 | 1.00x | — |
| Avx2 | no | 314.07 | 3,183,983 | 5.32x | — |
| Avx2 | **yes** | 203.15 | 4,922,471 | 8.22x | **1.55x** |
| Avx2Vnni | no | 279.67 | 3,575,592 | 5.97x | — |
| Avx2Vnni | **yes** | **189.61** | **5,274,039** | **8.81x** | **1.47x** |

Checksums are identical across all five modes (121500000), so the SIMD paths are bit-exact
against the scalar oracle at this position.

Reading: the production mode (Avx2Vnni + sparse) is already 8.8x the scalar oracle. VNNI buys
11% over plain AVX2 in the dense path but only 7% in the sparse path — sparse FC0 is the larger
of the two wins and it is already on. **There is no cheap backend switch left to make.**

## 2. Push/pop microbenchmark

`cargo run --release -p mf-nnue --features instrumentation --example push_pop_throughput -- 200000`

| Case | legal moves | push ns | eval ns | combined ns | changed edges (min/mean/med/max) | sliders scanned |
|---|---|---|---|---|---|---|
| quiet (startpos) | 20 | 272.29 | 176.06 | 452.44 | 0 / 3.6 / 4 / 8 | 0 / 2.5 / 2 / 5 |
| busy (kiwipete) | 48 | 643.05 | 202.24 | 841.75 | 3 / 11.0 / 10 / 25 | 0 / 5.2 / 4 / 14 |

Reading: **the incremental push already costs more than the forward pass it feeds** — 1.5x at
startpos, 3.2x at kiwipete. In a busy middlegame the accumulator update is 76% of the
push+eval pair. That inverts the usual assumption that NNUE cost is dominated by inference.

## 3. Incremental-update profile over real searches

New example `crates/mf-search/examples/nnue_update_profile.rs`, built with the `instrumentation`
feature, which drives real `mf-search` searches and reads the new thread-local counters:

`cargo run --release -p mf-search --features instrumentation --example nnue_update_profile -- 7`

Positions: the six `manifold bench` FENs at depth 7 (their node total is exactly the bench
signature, 45,036, so this arm is directly comparable to an uninstrumented `manifold bench`),
plus three deeper searches (startpos d14, kiwipete d13, a quiet middlegame d13).

```
position     d      nodes     pushes      nulls      thr%      upd%     rbld%      fwd%     nnue%  kingR/kn   ovfR/kn  edges/pu         nps
bench1       7       8884       8786         15     22.4%     58.4%      5.7%     19.2%     45.1%     21.72      0.00      6.25      666042
bench2       7      15926      15202        653     18.9%     66.3%     18.2%     14.7%     43.7%    107.06      0.00     10.39      394334
bench3       7       3121       3011         94     16.3%     65.5%     27.5%     18.3%     40.6%    308.88      0.00      2.30      756533
bench4       7       6101       5890        178     18.9%     65.6%     11.0%     15.5%     46.8%     54.91      0.00      9.00      479710
bench5       7       2971       2913         45     18.9%     68.3%     21.2%     12.8%     44.7%    135.31      0.00      6.63      529571
bench6       7       8033       7503        498     20.4%     63.4%     14.3%     16.1%     43.9%     70.21      0.00      9.57      527197
BENCH        7      45036      43305       1483     19.6%     64.7%     15.2%     15.8%     44.3%     92.44      0.00      8.40      492646
deep-d14    14     225979     218293       5055     20.2%     59.2%      9.8%     20.6%     45.2%     50.71      0.00      7.98      559046
deep-d13    13     303157     278242      17376     18.4%     66.1%     19.6%     15.4%     44.3%    135.45      0.00      9.54      452486
deep-d13    13      69240      66751       1395     22.6%     58.9%      6.5%     18.5%     44.8%     30.94      0.00      9.61      555355
TOTAL        0     643412     606591      25309     19.5%     63.1%     14.9%     17.4%     44.6%     91.43      0.00      8.91      498654
```

Columns: `thr%`/`upd%`/`fwd%` are shares **of counted NNUE time**; `rbld%` is the full-rebuild
subset of `upd%`; `nnue%` is the share of **wall time** the counted regions account for.
`kingR/kn` and `ovfR/kn` are rebuilds per 1000 searched nodes.

### The headline numbers

**Share of NNUE time (aggregate over 643,412 nodes):**

| Phase | Share of NNUE time | Share of total wall time |
|---|---|---|
| Threat-edge discovery (`discover_changed_threats`) | **19.5%** | 8.7% |
| Accumulator update (fused add/sub + rebuilds) | **63.1%** | 28.1% |
| — of which full rebuilds | 14.9% | 6.6% |
| — of which genuine incremental update | 48.2% | 21.5% |
| Forward pass | **17.4%** | 7.8% |
| **Total counted NNUE** | 100% | **44.6%** |

**Full-rebuild frequency per 1000 searched nodes:**

| Source | Count | Per 1000 nodes |
|---|---|---|
| King move (single perspective rebuilt) | 58,826 | **91.43** |
| `MAX_CHANGED` overflow (both perspectives rebuilt) | **0** | **0.000** |

**Absolute per-operation costs** (TSC 2.419 GHz; these include the counters' own rdtsc pairs,
see the caveat below, so treat them as upper bounds and the *ratios* as the real result):

| Operation | ns |
|---|---|
| Threat discovery, per real push | 185.1 |
| Accumulator update, per real push | 599.3 |
| — of which rebuild, amortized per push | 141.7 |
| — of which incremental, per push | 457.6 |
| One full perspective rebuild | 1461.0 |
| One forward pass (in-search) | 225.7 |

**Work per push:** 8.91 net changed threat edges, 4.38 slider candidates inspected.

**Unread pushes: 188,236 of 631,900 (29.8%).** This is the ceiling on what lazy updates can
skip: a deferred update only ever saves work on a pushed state whose accumulator is never read.

### Instrumentation overhead (measured, not assumed)

The `BENCH` row reproduces the bench signature exactly (45,036 nodes) at 492,646 NPS. The same
build's uninstrumented `manifold bench`, same shell pinning, three consecutive runs:
548,480 / 534,522 / 554,470 NPS (median 548,480). **The counters cost ~10% NPS.** The
*proportions* above are therefore reliable (all three phases pay the same rdtsc tax), but the
absolute ns figures are inflated by roughly one rdtsc pair each (~20-30 cycles) and should be
read as upper bounds. This is exactly why the feature ships default-off.

## 4. Absolute 1T NPS vs `baselines/mission-start`

`py -3.14 harness/nps_compare.py --engine current=.\target\release\manifold.exe --engine mission-start=.\baselines\mission-start\manifold.exe --depth 12 --hash 64 --warmup 1 --repeat 3`

| Position | nodes (both) | current NPS | mission-start NPS | ratio |
|---|---|---|---|---|
| startpos | 99,674 | 601,423 | 612,083 | 0.98x |
| kiwipete | 141,659 | 454,484 | 455,364 | 1.00x |
| midgame | 50,159 | 573,645 | 568,583 | 1.01x |
| endgame | 38,768 | 989,141 | 1,033,042 | 0.96x |
| **geometric mean** | | | | **0.99x** |

An earlier run of the identical command (before the release rebuild) gave 1.01x. The two runs
bracket 1.00x, so the current tree is NPS-identical to mission-start, as it must be: this
feature changes no production code path. Node counts to depth are identical at every position
(ratio 1.00x), confirming no functional change.

**Bench signature: 45,036 nodes, two consecutive runs identical.** Unchanged from the pinned
anchor, so `bench_cli` needed no re-pinning.

The mission-start reference point for M2's ≥10% target (A-NNUE-002) is therefore:
**startpos 612k / kiwipete 455k / midgame 569k / endgame 1,033k NPS at depth 12, Hash 64, 1T.**

## What this means for M2-F2 and M2-F3

**This is the flag the feature description asked for, and the answer is the awkward one.**

The feature said: *"if threat-update cost dominates and rebuild frequency is low, flag that to
the orchestrator before lazy-update work proceeds."* Both conditions hold, and a third finding
makes the picture worse for the plan as scoped:

1. **Threat/accumulator update cost dominates inference, decisively.** The forward pass is
   17.4% of NNUE time; getting to the accumulator is 82.6% of it. Even a *free* forward pass
   would buy under 8% of wall time. The roadmap's framing of M2 as "NNUE inference speed" is
   pointed at the smallest of the three phases.

2. **`MAX_CHANGED` overflow rebuilds are exactly zero** across 606,591 real pushes over ten
   searches, including two 13-ply searches from tactical positions. The 128-edge buffer is
   never close to full — the busiest observed push netted 25 edges. **Any work aimed at the
   overflow path is aimed at a path that never executes.** Raising or tuning `MAX_CHANGED`
   is dead work.

3. **King-move rebuilds, by contrast, are frequent and expensive: 91.4 per 1000 nodes at
   ~1461 ns each, 14.9% of all NNUE time and 6.6% of total wall time.** Endgames are far worse
   (bench3: 308.9 rebuilds per 1000 nodes, 27.5% of NNUE time) because the king is an active
   piece there. **This is the single largest identified, addressable block of waste in the
   NNUE path — and it is what M2-F3 (Finny tables / refresh cache) targets, not M2-F2.**

4. **The lazy-update ceiling is 29.8%, not 46%.** Only 188,236 of 631,900 pushed states were
   never evaluated. Lazy updates can, at absolute best, skip that fraction of the 82.6% of
   NNUE time spent reaching the accumulator — i.e. ~0.298 x 0.446 x 0.826 ≈ **11% of wall time
   as a hard upper bound**, before any bookkeeping cost for the deferred-update machinery
   itself, and before accounting for the fact that a deferred update usually still has to run
   later against a longer dirty chain. The roadmap's ~46% NPS estimate is not reachable from
   this profile.

### Recommended reordering

**Run M2-F3 (Finny tables) before M2-F2 (lazy updates).** The evidence:

- Finny tables attack a measured 14.9% of NNUE time / 6.6% of wall time with a *known*
  mechanism (cache the per-king-bucket accumulator so a king move refreshes from a nearby
  cached state instead of rebuilding from scratch), and the architecture note already warns
  that 32 king buckets *without* Finny tables can be a net loss — this profile is that warning
  cashing out at 91.4 rebuilds per 1000 nodes.
- Lazy updates attack a ≤11%-of-wall ceiling with machinery that adds per-push bookkeeping to
  the hottest loop in the engine, and whose benefit shrinks further once Finny tables reduce
  the cost of the updates being deferred.
- They interact: measuring lazy updates first would measure them against a rebuild-heavy
  baseline and then have that measurement invalidated by M2-F3.

If the orchestrator prefers to keep the stated order, M2-F2 should be scoped with an explicit
kill criterion at the ≤11% ceiling rather than the roadmap's ~46%, and should not touch the
`MAX_CHANGED` overflow path at all.

A secondary, cheaper target this profile surfaces: **threat discovery is 19.5% of NNUE time and
inspects 4.38 slider candidates per push** to net only 8.91 changed edges. That is a larger
share of NNUE time than the entire forward pass, and it is pure discovery overhead.

## Changes made by this feature

No production code path was altered. All additions are behind the default-off `instrumentation`
feature:

- `crates/mf-nnue/src/instrumentation.rs` (new) — thread-local `UpdateCounters` with
  `reset_update_counters()` / `update_counters()`. Thread-local rather than atomic because the
  accumulator stack is per-worker; sharing atomics across Lazy SMP workers would add contention
  to the hottest loop and average away per-worker behaviour. Timing uses `rdtsc`, not
  `Instant::now`, because a `QueryPerformanceCounter` pair costs as much as the region measured.
- `crates/mf-nnue/src/accumulator.rs` — counter/timer probes on `push_real`, `push_null`,
  `evaluate`, the king-move rebuild branch, and the overflow rebuild branch, each behind
  `#[cfg(feature = "instrumentation")]`.
- `crates/mf-nnue/src/threats.rs` — `discover_changed_threats` and the `_profiled` duplicate
  collapsed into one function returning the slider-scan count, with the counting itself gated by
  `const PROFILE: bool = cfg!(feature = "instrumentation")`. The production build monomorphizes
  to `PROFILE = false` and the `popcnt` disappears, so the merge costs nothing; the duplicate
  entry point is gone.
- `crates/mf-search/examples/nnue_update_profile.rs` (new) + an `instrumentation` feature on
  `mf-search` forwarding to `mf-nnue/instrumentation`.

Verification: `cargo test --workspace` green (including `bench_cli` 13/13 at the unchanged
45,036 anchor), `cargo clippy --workspace --all-targets -- -D warnings` green both with and
without the feature, `cargo fmt --all -- --check` green, bench deterministic across two
consecutive runs, and 1T NPS unchanged vs mission-start (geometric mean 0.99x / 1.01x across
two runs).
