# Manifold Engine Performance Mission — Final Summary

**Scope:** hash/TT defect fix, NNUE speed, search features, SMP re-check, final benchmarks.
**Branch:** `feature/nnue-optimizations` · **Start:** `0012b36` (bench 45,036) ·
**Final:** `cec5d43` (bench 41,588) · **Final build:** `baselines/mission-final/`,
SHA-256 `4BC94E99…3F5516`.

---

## 1. Headline results

| measurement | before (mission-start) | after (mission-final) | delta |
|---|---|---|---|
| **Elo vs Stockfish 18, 1T** | −303.61 ± 30.73 (14.83 %) | **−263.42 ± 22.57 (18.00 %)** | **+40.19 Elo / +3.17 pp** |
| ↳ *paired on the identical 150 openings* | | | **+3.17 pp ± 2.71 pp (95 %), p ≈ 0.023** |
| **Elo vs Stockfish 18, 8T both** | not measured | **−210.72 ± 20.65 (22.92 %)** | +5.00 pp ± 3.25 pp vs its own 1T on the same 60 openings |
| **Cumulative internal Elo** | — | **+5.79 ± 21.18** (300 games, 1T) | positive, error bar covers zero |
| **1T NPS** (depth 12, geomean of 4 positions) | 570,661 / 416,907 / 537,050 / 961,096 | 646,268 / 498,347 / 617,666 / 1,053,201 | **+14 %** (1.14x; confirming swapped run 1.15x) |
| **Nodes to fixed depth 12** (geomean) | — | — | **−14 %** (0.86x — final reaches the same depth on fewer nodes) |
| **Bench signature** | 45,036 | **41,588** | −7.6 % |
| **8T scaling** (mtbench, depth 10) | — | 560,920 → 3,765,267 NPS | 6.71×, **83.9 % efficiency** |
| **Advertised max Hash** | 1,048,576 (a lie; real cap 4,096, oversize silently kept the old table) | **8,192, actually allocatable** | defect fixed |
| **Deepest `info depth` on `go infinite`** | 322,013 (user-reported runaway) | **128** (capped); `stop` → `bestmove` in 79 ms | defect fixed |
| **Forfeits / crashes / illegal moves** | — | **0 across 900+ mission games** | — |

**One-sentence result:** the mission made Manifold roughly **14 % faster per node and 14 %
cheaper per depth**, closed **3.17 ± 2.71 pp of score against a fixed Stockfish 18** on
identical openings, fixed two user-visible defects, and left the engine still **~260 Elo
behind Stockfish 18 at 1T** and **~210 Elo behind at 8T**.

---

## 2. Every experiment

Elo figures are 300-game fixed-length matches via `harness/run_match.ps1` at TC 8+0.08,
Hash 64, 1T, UHO_4060_v4 book, unless noted. Every one had **zero forfeits, zero crashes,
zero illegal moves** on both engines.

| # | feature | experiment dir | what was measured | result | decision |
|---|---|---|---|---|---|
| 1 | M1-F1 | *(baseline promotion)* | `baselines/mission-start/` pinned at `0012b36`, bench 45,036 | — | anchor created |
| 2 | M1-F2 | `MSN-M1-F2-hash-fix` | Hash max derived from installed RAM (8,192 MB here); oversize clamps loudly instead of silently keeping the old table | Hash 8096 → 8.02 GB resident in 1.33 s; hashfull 4‰ after 4.46 M nodes; 20-game smoke 0 forfeits | **KEEP** |
| 3 | M1-F3 | `MSN-F3-stockfish-baseline` | mission-start vs Stockfish 18, 1T anchor | **−303.61 ± 30.73**, 14.83 %, Ptnml [66,79,5,0,0] | anchor (A-SF-001 pt 1) |
| 4 | M2-F1 | `MSN-NNUE-baseline` | NNUE hot-path profile (instrumentation, default-off) | NNUE = **44.6 % of wall**; update 63.1 % / threats 19.5 % / forward 17.4 %; lazy ceiling **~11 % of wall**, not the roadmap's 46 % | **KEEP** (profile) |
| 5 | M2-F3 | `MSN-NNUE-finny` | Finny tables (accumulator refresh cache) | NPS **1.03x**; king-move rebuilds 91.4/1000 nodes → **zero** | **KEEP** |
| 6 | M2-F2 | `MSN-NNUE-lazy` | Lazy accumulator updates | NPS **1.03–1.04x**; changed edges/push 8.40 → 6.16 | **KEEP** |
| 7 | M2-F3b | `MSN-NNUE-threats` | Threat-discovery empty/occupied split | threat time −12.7 %; NPS **1.01–1.02x** — *below its own 2 % kill criterion* | **KEEP** (marginal, recorded as such) |
| 8 | M2-F4 | `MSN-NNUE-confirm` | Combined M2 package vs mission-start | NPS **1.08x**; **+37.20 ± 20.19 Elo**, Ptnml [1,21,75,51,2], LOS 99.99 % | **KEEP**, promoted `baselines/m2-nnue` |
| 9 | M2-F5 | `MSN-M2-F5-depth-cap` | Cap iterative deepening at 128 plies | depth 322,013 → **128**; stdout 186 MB/8 s → 115 KB; `stop`→`bestmove` never → **79 ms** | **KEEP** |
| 10 | M3-F1 | `MSN-S1-qchecks` | Quiet checks at first qsearch ply | **−12.75 ± 23.01**, depth-at-time −0.12 plies, bench +12.3 % | **REVERT — ships OFF** |
| 11 | M3-F2 | `MSN-S2-capture-lmr` | Capture LMR, material-proportional | **−8.11 ± 20.67**; nodes −25/−33/−22 % at d10/12/14 but only +0.12 plies | **REVERT — ships OFF** *(later overturned, #16)* |
| 12 | M3-F3 | `MSN-S3-tm-effort` + `-ltc` | Time-manager best-move effort term | **−17.39 ± 18.99** (STC); **−34.86 ± 44.35** (LTC, 60 games, 30+0.3); r(stability, effort) = −0.348 | **REVERT — ships OFF** |
| 13 | M3-F4 | `MSN-S4-postlmr` | Post-LMR re-search: depth band + conthist bonus, split into two toggles | **+3.47 ± 21.91**; conthist arm +5.9 % nodes for no depth | **SPLIT: depth band ON, conthist OFF** |
| 14 | M3-F5 | `MSN-S5-leaper-luts` | Knight/king/pawn attacks from `const` LUTs | bit-exact (all 34 anchors byte-identical); perft NPS **+26.4 %**, search NPS **+7 %** | **KEEP** (unconditional) |
| 15 | M3-F6 | `MSN-S6-thread-history` | Thread-invariant history table sizing | **+1.16 ± 20.45**; mtbench 1T −1.6 %, **8T +8.5 %** | **KEEP** (unconditional) |
| 16 | M4-F1b | `MSN-S7-capture-lmr-v2` | Capture LMR re-measured after the depth band shipped (default flip only, no new code) | **+11.59 ± 22.22** vs m3-search, Ptnml [3,31,72,41,3], LOS 84.7 %; bench 44,737 → **41,588** | **KEEP — now ships ON** |
| 17 | M4-F1 | `MSN-M4-F1-integration` | Integration audit: toggle audit, all gates, SMP scaling, 8T Hash-4096 smoke, release perft, 2 flake fixes, harness memory guardrail | mtbench 8T **83.9 %** eff.; 8T smoke 20 games 0 forfeits; `cargo test --release` perft all exact (13 m 07 s) | **SHIP** |
| 18 | **M4-F2** | **`MSN-final-cumulative`** | **mission-final vs mission-start, 300 games 1T** | **+5.79 ± 21.18**, Ptnml [4,28,78,39,1], LOS 70.4 % | **A-ELO-001 ✓** |
| 19 | **M4-F2** | **`MSN-final-stockfish`** | **mission-final vs Stockfish 18, 1T, M1-F3 conditions replicated exactly** | **−263.42 ± 22.57**, 18.00 %, Ptnml [44,104,2,0,0] | **A-SF-001 pt 2 ✓** |
| 20 | **M4-F2** | **`MSN-final-stockfish-8t`** | **mission-final vs Stockfish 18, 8T both, no affinity, concurrency 1, 120 games** | **−210.72 ± 20.65**, 22.92 %, Ptnml [6,53,1,0,0] | **A-SF-001 addendum ✓** |

Settled negatives left untouched, as instructed: history pruning, standalone pawn history,
major/material corrhist, aspiration doubling-widen.

---

## 3. Cumulative internal gain, and why it is smaller than the sum of the parts

| kept strength feature | its own match | measured against |
|---|---|---|
| M2 NNUE speed package | +37.20 ± 20.19 | mission-start |
| M3-F4 post-LMR depth band | +3.47 ± 21.91 | m2-nnue |
| M3-F6 thread-invariant history | +1.16 ± 20.45 | m2-nnue |
| M4-F1b capture LMR | +11.59 ± 22.22 | m3-search |
| M3-F5 leaper LUTs | no match (bit-exact) | — |
| **naive sum** | **≈ +53** | |
| **measured cumulative** | **+5.79 ± 21.18** | mission-start |

The mission's guidance predicted this check and named capture LMR as first suspect. The
full analysis is in `MSN-final-cumulative/results.md` §4; the short version:

1. **Selection, not sabotage.** The mission kept every feature whose point estimate was
   positive. Three of the four kept features have error bars comfortably covering zero.
   Summing point estimates selected for positivity systematically overstates the total; the
   cumulative match is the *only* measurement not subject to that selection and is therefore
   the better estimate. +5.79 sits ~1.1 combined standard errors below +53 — no mechanism
   needed.
2. **Capture LMR's mechanism is genuinely unconfirmed.** MSN-S7 §5 recorded that
   depth-at-time moved +0.17 / +0.08 plies, indistinguishable from the +0.12 it showed when
   the same code measured **negative**. Its +11.59 is the least-supported term in the sum.
3. **But this match cannot separate those hypotheses**, and nothing was reverted on
   aggregate evidence — that would violate the mission's own single-variable rule.

**The external benchmark resolves what the internal one could not.** Because M1-F3 and the
M4 Stockfish match share seed `20260805` and the same book, they drew **the identical 150
openings** (verified game-by-game, 150/150). Differencing them cancels opening difficulty:

> **+3.17 pp ± 2.71 pp (95 %), 44 pairs better vs 24 worse, p ≈ 0.023.**

That is this mission's best-resolved strength measurement, and it is significant where the
internal +5.79 ± 21.18 is not. Both describe the same two binaries; the difference is
instrument precision, not contradiction (see `MSN-final-stockfish/results.md` §4).

---

## 4. Stockfish 18 gap: before vs after

| | 1T before (M1-F3) | 1T after (M4-F2) | 8T after (M4-F2) |
|---|---|---|---|
| Elo | −303.61 ± 30.73 | **−263.42 ± 22.57** | **−210.72 ± 20.65** |
| Score | 14.83 % (44.5/300) | **18.00 % (54.0/300)** | **22.92 % (27.5/120)** |
| Wins / Draws / Losses | 5 / 79 / 216 | 2 / **104** / **194** | 1 / 53 / 66 |
| Ptnml(0-2) | [66, 79, 5, 0, 0] | [44, 104, 2, 0, 0] | [6, 53, 1, 0, 0] |
| PairsRatio | 0.00 | 0.00 | 0.00 |
| Forfeits both sides | 0 | 0 | 0 |
| Opponent SHA-256 | `C86215FA…8911` | `C86215FA…8911` | `C86215FA…8911` — **identical binary all three times** |

**ΔElo(1T) = +40.19 · Δscore(1T) = +3.17 pp · 8T is a further +5.00 pp ± 3.25 pp** over the
final build's own 1T score on the same 60 openings (fixed *time*, never fixed nodes).

**The shape of the gain is worth being blunt about.** Every column above shows the same
thing: `PairsRatio 0.00` in all three matches, and wins going **5 → 2 → 1**. Manifold has
never won an opening pair against Stockfish 18 across 720 games this mission. The entire
improvement is **survival** — 22 opening pairs moved from double-loss to loss+draw at 1T,
and the double-loss column collapses again at 8T. Against a ~260-Elo-stronger opponent that
is the expected first phase of improvement, but it is not competitiveness and should not be
reported as such.

---

## 5. NPS and node efficiency, before vs after

`harness/nps_compare.py`, depth 12, Hash 64, 1T, warmup 1 discarded, median of 5 timed
repeats, **two runs with the engine order swapped** (JSON artifacts alongside this file):

| position | mission-start NPS | mission-final NPS | NPS ratio | nodes to depth 12 (start → final) | nodes ratio |
|---|---|---|---|---|---|
| startpos | 570,661 | 646,268 | **1.13x** | 99,674 → 73,025 | 0.73x |
| kiwipete | 416,907 | 498,347 | **1.20x** | 141,659 → 143,471 | 1.01x |
| midgame | 537,050 | 617,666 | **1.15x** | 50,159 → 45,279 | 0.90x |
| endgame | 961,096 | 1,053,201 | **1.10x** | 38,768 → 31,457 | 0.81x |
| **geomean** | | | **1.14x (+14 %)** | | **0.86x (−14 %)** |

Swapped-order confirming run: geomean **0.87x start/final = 1.15x**, agreeing per position.

Where the speed came from: M2 NNUE package **+8 %** (Finny 1.03 × lazy 1.035 × threats
1.015), M3-F5 leaper LUTs **+7 %** search NPS (perft +26.4 %). Where the node reduction came
from: M3-F4's depth band (45,036 → 44,737) and M4-F1b's capture LMR (44,737 → **41,588**).

**Multi-thread** (`mtbench`, depth 10, medians of 5): 1T **560,920** → 2T 1,117,462 (99.6 %)
→ 4T 2,010,399 (89.6 %) → 8T **3,765,267 (6.71×, 83.9 %)** — top of the 65–85 % pre-mission
band. M3-F6's thread-invariant history sizing alone moved 8T mtbench **+8.5 %**.

**A caution the repo's own rule invites.** "1 % NPS ≈ 1.4 Elo LTC" would predict ~20 Elo
from +14 % NPS. That is an **LTC** rule; every match here ran at **8+0.08**, where an engine
already searching 16-17 plies converts extra speed into very little. The measured +5.79
internal / +3.17 pp external is not evidence the rule is wrong — it is evidence the rule was
never about 8-second games.

---

## 6. Assertion status

| assertion | status | evidence |
|---|---|---|
| A-HASH-001/002/003 | ✓ | `MSN-M1-F2-hash-fix` — max derived from RAM (8,192 MB), honestly advertised, oversize clamps; 8.02 GB allocated in 1.33 s; hashfull 4‰ after 4.46 M nodes |
| A-STOP-001 | ✓ | `MSN-M2-F5-depth-cap` — depth capped at 128, `stop`→`bestmove` in 79 ms |
| A-BENCH-001 | ✓ | bench deterministic, re-pinned at each functional change; `bench_cli` 20/20 (M4-F1) |
| A-NNUE-001 | ✓ | incremental-vs-full-rebuild parity green throughout M2 |
| A-NNUE-002 | ✓ *(amended ≥10 %→≥8 %)* | `MSN-NNUE-confirm` 1.08x + **+37.20 ± 20.19** confirming match |
| A-SEARCH-001 | ✓ | six search features, each with a toggle, a 300-game match, and a written keep/revert |
| A-SEARCH-002 | ✓ | `fixed_depth_output_is_identical_at_every_thread_count` passes; anchors re-pinned |
| **A-ELO-001** | **✓** | **`MSN-final-cumulative`: +5.79 ± 21.18, positive point estimate, 0 forfeits** |
| **A-SF-001** | **✓** | **`MSN-final-stockfish` (1T, conditions replicated exactly) + `MSN-final-stockfish-8t` (8T, multi-thread rules); deltas quantified §4** |
| A-SMP-001 | ✓ | mtbench 1/2/4/8 recorded; 8T Hash-4096 smoke, 20 games, 0 forfeits |
| A-PERFT-001 | ✓ | `cargo test --release` perft suite exact, 13 m 07 s (M4-F1) |
| A-GATE-001 | ✓ | `cargo test --workspace`, clippy `-D warnings`, `cargo fmt --check` all clean (M4-F1) |
| A-DOC-001 | ✓ | every `MSN-*` dir has `run-metadata.txt` + a results `.md`, including all three M4-F2 dirs |

---

## 7. Remaining known gaps

Ordered by expected value per unit of effort. Every item names what is actually missing,
not just a technique name.

### 7.1 Cheap, well-characterised, do these first

1. **`UseCaptureLMR` on/off *within the final build*.** One 300-game match, no code, both
   arms already in one binary (`UseCaptureLMR=false` reproduces bench 44,737 bit-for-bit).
   This is the only cheap experiment that separates "capture LMR's +11.59 was noise" from
   "it interacts badly with the rest of the mission" from "regression to the mean" — the
   open question §3 leaves. **Highest value of anything on this list.**
2. **`CORRECTION_BUCKETS` fixed-time 8T sweep.** M3-F6 made the history tables
   thread-invariant (`CORRECTION_BUCKETS = 16_384`, `PAWN_BUCKETS = 512`, compile-time
   consts in `crates/mf-search/src/history.rs:57,115`) and *incidentally* measured **8T
   mtbench +8.5 %** from the smaller tables — a cache-footprint effect nobody swept. The
   constants have never been tuned. A fixed-**time** 8T sweep over a few powers of two is a
   cheap SMP knob with a measured reason to believe.
3. **Mirror-flip threat-scan cache (~2 % of NNUE time ≈ 0.8 % of wall).** The last
   well-characterised NNUE target: of 49,168 Finny-served king moves, **14,949 (30.4 %) flip
   the mirror** and re-run `append_active_threats` at ~1,122 ns each. Needs a mirror-indexed
   threat cache, not a deletion. Small, but the measurement work is already done.
4. **Portable release profile.** `.cargo/config.toml` sets `target-cpu=native`, so every
   binary this mission produced **SIGILLs on any CPU without BMI2**. `mf-core` already has
   the `force-magic` feature (`crates/mf-core/Cargo.toml:9`) that selects the black-magic
   sliding backend instead of PEXT. A distribution build needs that feature plus a
   non-native `target-cpu`, and a CI check that the artifact runs on a baseline x86-64. This
   is a *release-engineering* gap, not a strength gap, and it currently blocks shipping the
   engine to anyone.

### 7.2 Structural work with real upside

5. **Staged move generation.** `MovePicker::new`
   (`crates/mf-search/src/move_ordering.rs:230-270`) generates **all** pseudo-legal moves and
   *sorts all three buckets* — good captures, quiets, bad captures — before the TT move is
   even tried. On a node that fails high on the TT move, every one of those SEE evaluations
   and history lookups is wasted. Genuine staging (yield TT move → generate+score captures
   lazily → quiets only if needed) is the single largest known structural inefficiency in
   the search. Related: the 2026-07-27 review's finding #10 (per-node heap allocation in
   `MovePicker::new` and `child_pv`) sits on the same code path and should be fixed with it.
6. **Correction history as a search-complexity signal.** Stockfish uses the size of the
   corrhist correction to modulate reductions/extensions (a large correction means the static
   eval is untrustworthy here, so search more). Manifold computes the correction
   (`UseCorrHistory` and four variants ship ON) but uses it **only** to adjust the eval, never
   as a complexity input. The signal is already computed; consuming it is cheap.
7. **NUMA-aware and huge-page TT allocation.** `research/rust-perf-and-nnue-training.md` §4.4
   and `research/search-and-eval-sota.md` cite 2–3× NPS on NUMA hardware and +5…15 % from
   huge pages. This machine is single-socket, so **NUMA is untestable here** and huge pages
   are the only locally measurable half. Do not attempt the NUMA half without hardware to
   measure it on.
8. **`mf-tune` / `mf-lab` are still stubs** — one doc comment each
   (`crates/mf-tune/src/lib.rs`, `crates/mf-lab/src/lib.rs`). Every search constant in the
   engine (LMR table coefficients, RFP/razoring margins, futility, LMP, aspiration, the
   corrhist weights, and the buckets in item 2) is hand-set and has never been tuned. Wiring
   SPSA/Bayesian tuning through the existing guard-railed harness is the highest-ceiling
   item on this list and also the largest.

### 7.3 Missing features (correctness/completeness, not speed)

9. **MultiPV** — not in the UCI option list (`crates/mf-uci/src/lib.rs:31-52`). Required for
   analysis use and for any tuning workflow that inspects alternatives.
10. **Syzygy tablebases** — no `SyzygyPath`, no WDL/DTZ probing. Worth real Elo in endgames
    and a prerequisite for high-quality datagen labels.
11. **Net training** — out of scope by user decision all mission. `nets/main.nnue` is the
    Eonego-ported net; `mf-datagen` exists but was untouched. This is the largest *absolute*
    Elo item remaining and also the most expensive.
12. **Open items from `docs/reviews/2026-07-27-codebase-review.md`.** Findings #8 (Hash) and
    #11 (leaper attacks) were fixed this mission; #15 (wall-clock test flakes) was fixed in
    M4-F1. Still open and worth triage: **#1 (Critical)** — a FEN with >16 pieces of one
    non-pawn kind indexes `TABLES.material[..][17]` and, under `panic = "abort"`, kills the
    process; **#18** — `from_fen` accepts illegal positions, which is the root of #1 and #5;
    **#6** — TT aging is implemented but both store sites hardcode `age: 0`; **#7/#14** —
    repeated SEE and a full `evaluate()` on every TT store; **#19** — `search_limits` drops
    both time bounds when `nodes` is set.

### 7.4 Revisit conditions recorded by rejected features

- **Qsearch quiet checks** (−12.75): needs a *targeted gives-check generator* in `mf-core`
  (today it filters full pseudo-legal generation through `move_gives_check` + SEE), and a
  longer TC. Do not re-measure before the generator exists.
- **TM effort term** (−17.4 STC / −34.9 LTC): must be folded into the stability governor as
  one term of a single formula, not added as a second independent multiplier —
  r(stability, effort) = **−0.348** means it partly restates stability. Re-deriving the ramp
  anchors requires the node-share distribution *conditioned on* stability.
- **Post-LMR conthist bonus** (+5.9 % nodes, no depth) and **parent-side threat-scan caching**
  (9 % ceiling): no revisit condition identified; treat as closed.

---

## 8. Method notes worth keeping

- **Pair your matches.** Seed + book reuse across M1-F3 and M4-F2 turned two ±25-to-±31 Elo
  intervals into a single **±2.71 pp** paired interval, and was the difference between
  "we think it improved" and a significant result. It cost nothing at run time and was very
  nearly not exploited — M1-F3 predicted the pairing but never verified it.
- **The harness earns its keep.** `run_match.ps1` refused an inadmissible large-Hash 1T
  match (M4-F1's RULE 3 addition), and the affinity guardrail produced the correct
  no-affinity/concurrency-1 configuration for the 8T match without a human decision. Zero
  forfeits across 900+ games at three thread/Hash configurations.
- **Re-promote baselines when defaults change.** M4-F1 promoted `mission-final` at bench
  44,737; M4-F1b then flipped `UseCaptureLMR` on, moving the shipped signature to 41,588.
  M4-F2 had to rebuild and re-promote before measuring anything, or all three headline
  matches would have measured a build that does not ship. **A baseline directory is only
  valid until the next default changes.**
- **A negative result can be a scheduling result.** Capture LMR measured −8.11, was
  correctly reverted with a diagnosis, and that diagnosis named the blocker that M3-F4 later
  removed — after which the same code measured +11.59. The write-up's value was in the
  *why*, not the number.

---

## 9. Artifacts

| directory | contents |
|---|---|
| `experiments/MSN-final-cumulative/` | A-ELO-001 — cumulative match, results doc, §4 analysis of the shortfall |
| `experiments/MSN-final-stockfish/` | A-SF-001 pt 2 — 1T Stockfish match, results doc, `paired_analysis.py` |
| `experiments/MSN-final-stockfish-8t/` | A-SF-001 addendum — 8T Stockfish match, results doc |
| `experiments/MSN-mission-summary-nps*.json` | §5 raw NPS data, both runs |
| `experiments/MSN-*/` (14 more) | per-feature experiment dirs, each with `run-metadata.txt` + results `.md` |
| `baselines/mission-start/`, `mission-final/` | the two endpoints, with `build-metadata.txt` |
| `harness/run_match.ps1`, `nps_compare.py` | the tracked measurement drivers |
