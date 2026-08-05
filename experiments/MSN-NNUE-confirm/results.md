# MSN-NNUE-confirm — M2 confirmation: combined NPS gain and strength match

**Feature:** M2-F4-nnue-confirm-match (milestone M2, NNUE inference speed)
**Date:** 2026-08-06
**Decision:** **KEEP the whole M2 package; promoted to `baselines/m2-nnue/`.** The 300-game match
vs `baselines/mission-start` scored **+37.20 ± 20.19 Elo with zero forfeits**, which is a clear
non-regression and the strongest result the milestone could have asked for.
**The A-NNUE-002 ≥10% NPS target was MISSED: the measured combined 1T gain is 1.08x (+8%),
not ≥1.10x.** Per the feature description this is a report-and-return situation, not a failure —
the actual numbers and the reason are below, and the orchestrator makes the call.

## Purpose

Close M2 by answering two questions with one build:

1. What is the **combined** 1T NPS gain of the whole M2 package against the mission-start build
   (A-NNUE-002 asks for ≥10%)?
2. Does that speed cost any strength? A speed-only change should be neutral-to-positive; a
   regression would mean one of the four M2 commits is not the bit-exact change it claims to be.

## Provenance

| | |
|---|---|
| Repo commit | `c9fc4542007266b71bd9617973ef888ed138e99c` ("Stop iterative deepening from running past the board") |
| Build | `cargo build --release`, confirmed `Finished` with no `os error 5` |
| Binary A (`m2-nnue`) | `target/release/manifold.exe`, SHA-256 `BC0C445CD9A26BEC0EEA446EEF555B1EE8C756250BAD943973D1C33D776DB48C`, 112,959,488 bytes |
| Binary B (`mission-start`) | `baselines/mission-start/manifold.exe`, SHA-256 `43EB8A0DD0C81172EFDD1F914899C080DFD757CA26EACA144913E52CEAD2CB28`, 112,948,224 bytes |
| Machine | i9-13980HX (8 P + 16 E, 32 logical), 31.6 GB RAM, Windows 11 |
| Toolchain | `--release` (fat LTO, 1 CGU, `panic=abort`), `target-cpu=native` |
| Net | `nets/main.nnue`, 111,261,604 bytes, embedded (`embedded-net`) |
| Bench signature | **45,036**, identical for both binaries — the M2 package is bit-exact |

Raw output committed beside this document: `nps-depth12.json`, `nps-depth12-run2-swapped.json`,
`nps-depth12-run3.json`, `run-metadata.txt`, `console.txt`, `fastchess.log`, `launch-confirm.ps1`,
and `run1-inadmissible/` (see "The first match was thrown away, and why").

## What is in the M2 package

M2 was **reordered Finny-first** on the evidence of the M2-F1 profile (king-move rebuilds were
14.9% of NNUE time while `MAX_CHANGED` overflow rebuilds were exactly zero), so the roadmap's
lazy-updates-first order was not followed. The four commits between mission-start and this build:

| # | Feature | Commit | Nature |
|---|---|---|---|
| 1 | M2-F3 Finny tables (accumulator refresh cache) | `a6b092f` | speed, bit-exact |
| 2 | M2-F2 Lazy accumulator updates | `71f0fac` | speed, bit-exact |
| 3 | M2-F3b Threat-discovery empty/occupied split | `7f56c46` | speed, bit-exact |
| 4 | M2-F5 Iterative-deepening depth cap (128) | `c9fc454` | user-reported defect fix |

All four kept the bench signature at 45,036, which is why the match below is a pure speed
measurement rather than a behaviour comparison.

## Per-feature NPS deltas, and why they do not add to +8%

Each feature measured itself against the previous **kept** build, at depth 12 / Hash 64 / 1T,
`harness/nps_compare.py`, geometric mean over the same four positions:

| Feature | Baseline it measured against | Reported gain | Source |
|---|---|---|---|
| M2-F3 Finny tables | mission-start | **1.03x** | `experiments/MSN-NNUE-finny/results.md` |
| M2-F2 Lazy updates | post-Finny | **1.03x–1.04x** (two runs) | `experiments/MSN-NNUE-lazy/results.md` |
| M2-F3b Threat discovery | post-lazy | **1.01x–1.02x** (three clean runs) | `experiments/MSN-NNUE-threats/results.md` |
| M2-F5 Depth cap | — | not a speed change | `experiments/MSN-M2-F5-depth-cap/` |
| **Naive product** | | **1.07x–1.09x** | |
| **Measured end-to-end** | mission-start | **1.08x** | this document |

**The chain multiplies out to exactly what was measured end-to-end.** That is the useful finding
here: no feature's number was inflated, and no gain was lost to interaction between them. The
milestone's shortfall against the 10% target is not a measurement error in any individual
feature — it is that the sum of three honest small gains is 8%.

## Result 1 — combined 1T NPS vs mission-start

`py -3.14 harness/nps_compare.py --engine m2=.\target\release\manifold.exe --engine mission-start=.\baselines\mission-start\manifold.exe --depth 12 --hash 64 --warmup 1 --repeat N`
(shell pinned to the 8 P-cores, `ProcessorAffinity = 0xFFFF`)

| Position | nodes (both) | run 1 (r7) m2 NPS | run 1 mission-start NPS | run 1 ratio | run 3 (r9) ratio |
|---|---|---|---|---|---|
| startpos | 99,674 | 573,043 | 539,623 | 1.06x | 1.05x |
| kiwipete | 141,659 | 437,375 | 395,734 | 1.11x | 1.10x |
| midgame | 50,159 | 534,871 | 500,560 | 1.07x | 1.09x |
| endgame | 38,768 | 979,148 | 894,271 | 1.09x | 1.08x |
| **geometric mean** | | | | **1.08x** | **1.08x** |

**Two independent clean runs agree at 1.08x per position and in the mean.** Node counts to depth
are identical at every position (ratio 1.00x), confirming the package is a pure speed change.

**A third run is recorded and discarded, with the reason.**
`nps-depth12-run2-swapped.json` was taken with the engine order swapped (to control for order and
thermal effects) and reported a nonsensical mission-start/m2 = 1.04x. It was contaminated: its
`m2` kiwipete samples spanned **180,743–400,301 NPS (2.2x)** and startpos **332,619–562,129**,
against 2–3% spreads for the same binaries in runs 1 and 3. An external application
(Hearthstone, plus browser processes) was consuming CPU during that window. It is kept in the
directory rather than deleted, because reporting only the runs that agree is the easy mistake.

### Against A-NNUE-002's ≥10%: missed, at 8%

The target is not met and is not rounded away. **The M2-F1 profile bounded this before the work
started**, which is why the feature description flagged this outcome in advance:

- NNUE was **44.6% of wall time**, and within NNUE the forward pass was only 17.4%.
- Amdahl on the *whole* NNUE block: even making all NNUE work free buys ~44.6% of wall. Making
  only the addressable parts faster buys far less.
- The three features' own ceilings, each re-derived by the feature that implemented it:
  Finny targeted 14.9% of NNUE; lazy updates' true ceiling was **6.7% of wall** (not the
  roadmap's ~46%, and not the 11% M2-F1 estimated — see `MSN-NNUE-lazy/results.md`);
  threat discovery's shipped change was predicted at **~1% of wall** and measured 1–2%.

Summing those honest ceilings lands near 10–13% *before* bookkeeping costs, which is exactly the
range the feature description named as the realistic ceiling. **8% is inside that band, at the
low end.** The gap to 10% is not recoverable by re-measuring; it would need a further NNUE
feature, and the best-characterized unclaimed target left (the mirror-flip `append_active_threats`
scan) is bounded at **~2% of NNUE time ≈ 0.8% of wall** — not enough to close it.

## Result 2 — 300-game match vs mission-start

```powershell
.\harness\run_match.ps1 -OutDir experiments\MSN-NNUE-confirm `
    -Purpose 'M2 confirmation: the combined NNUE speed package ... vs the mission-start build ...' `
    -AName 'm2-nnue' -ACmd '.\target\release\manifold.exe' `
    -BName 'mission-start' -BCmd '.\baselines\mission-start\manifold.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260806
```

Guardrails as enforced by the driver: both engines `Threads=1` → `-use-affinity`,
`-concurrency 8`. Book `UHO_4060_v4.epd`, paired openings (`-repeat -games 2`), fixed length
(no SPRT). Pre-run CPU 17% (max of 5 samples). Wall time 17:01.

```
Results of m2-nnue vs mission-start (8+0.08, 1t, 64MB, UHO_4060_v4.epd):
Elo: 37.20 +/- 20.19, nElo: 73.07 +/- 39.32
LOS: 99.99 %, DrawRatio: 50.00 %, PairsRatio: 2.41
Games: 300, Wins: 85, Losses: 53, Draws: 162, Points: 166.0 (55.33 %)
Ptnml(0-2): [1, 21, 75, 51, 2], WL/DD Ratio: 0.67
```

**Forfeit accounting (the admissibility signal):**

| engine | time forfeits (PGN) | console Timeouts | Crashed | illegal moves played | illegal PV reports |
|---|---|---|---|---|---|
| m2-nnue | **0** | 0 | 0 | 0 | 0 |
| mission-start | **0** | 0 | 0 | 0 | 0 |

Adjudications: 0. Harness self-check: `[ok] zero forfeits, zero crashes, zero illegal moves`.
Script exit 0.

### Reading the number

The assertion required only **no regression** (point estimate ≥ 0 within error bars). The result
clears that by a wide margin: **+37.20 ± 20.19**, LOS 99.99%, and the pentanomial shape is
one-sided in the right direction (51 pairs won vs 21 lost; PairsRatio 2.41).

**Relative to the repo's 1% NPS ≈ 1.4 Elo rule, +8% NPS predicts ≈ +11 Elo, and the match
measured +37.** The measured value is above the prediction but the two are **not in conflict** at
this sample size: the ±20.19 error bar puts the interval at roughly [+17, +57], and the repo rule
is calibrated at long time control while this match is 8+0.08. Two further contributions are
plausible and are *not* separable by this match:

- **M2-F5 (the depth cap) is in this build and is not a speed change.** It fixed unbounded
  iterative deepening, so it can plausibly carry real Elo of its own in games where the old build
  wasted time iterating past useful depth. This match cannot attribute how much.
- Short TC amplifies speed gains relative to the LTC-calibrated rule.

**No claim is made that the M2 speed work alone is worth 37 Elo.** What the match establishes is
what the assertion asked for: the package is **not a regression**, and it is very likely a real
gain.

### The first match was thrown away, and why

The first 300-game run of this exact configuration (same seed 20260806, same binaries) came back
**inadmissible: exit 3, 5 time forfeits** (m2-nnue 3, mission-start 2). Its full artifacts are
preserved under `run1-inadmissible/` rather than deleted, and its Elo (+3.47 ± 20.19) is **not
quoted as evidence anywhere**.

The diagnosis, before rerunning:

- **The forfeits are symmetric across engines** (3 vs 2). A genuine time-manager defect in the M2
  build cannot make *mission-start* forfeit; both binaries have the same time manager, and
  mission-start ran a 300-game match earlier in this mission with **zero** forfeits.
- **They cluster in two tight wall-clock windows**, not across the run: 01:28:27, 01:28:28 (two
  games one second apart in different concurrency slots), then 01:33:36, 01:34:36, 01:34:43.
  A per-game engine defect scatters; an external CPU spike hits whichever games are in flight.
- **Pre-run CPU was 29%**, versus 5% for the earlier zero-forfeit matches in this mission. An
  external game (Hearthstone, ~4 s CPU per 8 s sample) and browser processes were active — the
  same load that contaminated NPS run 2 above.

Hearthstone exited on its own; the rerun started at 17% pre-run CPU and produced **zero
forfeits** with the same seed and binaries. Per the skill's admissibility rule this was the one
permitted diagnosed rerun, and it succeeded.

**Methodology note worth carrying forward:** at concurrency 8 on this machine, an interactive
foreground application is enough to manufacture time forfeits in a 1T match. The pre-run CPU
figure the harness records is the check that catches it — **anything above ~20% should be treated
as a reason to wait, not a number to note afterwards.**

## Keep / revert recommendation per component

| Component | NPS contribution | Recommendation | Basis |
|---|---|---|---|
| M2-F3 Finny tables (`a6b092f`) | 1.03x | **KEEP** | Largest single M2 gain; eliminated king-move rebuilds entirely (91.4/1000 nodes → 0). Bit-exact (bench 45,036). Costs 272 KiB/thread. |
| M2-F2 Lazy updates (`71f0fac`) | 1.03–1.04x | **KEEP** | Cleared its own 3% kill criterion against a re-derived 6.7%-of-wall ceiling; skipped exactly the 116,232 pushes its shadow model predicted. Bit-exact. Costs 8 KiB/thread. |
| M2-F3b Threat discovery (`7f56c46`) | 1.01–1.02x | **KEEP** (with the caveat below) | Below its own stated 2% kill criterion. Kept because the cost side is *negative*: it deletes provably dead work and one function, adds no state, no toggle, no memory. M2-F3b's own doc flags that the orchestrator may overrule and revert (`git revert 7f56c46`, one file). Nothing in this confirmation changes that judgement — the 1.08x combined figure is consistent with it contributing 1–2%. |
| M2-F5 Depth cap (`c9fc454`) | n/a | **KEEP** | User-reported defect fix (depth 3546 observed, unstoppable search). Not a speed change; included so the promoted baseline carries it. |

**No component is recommended for revert.** The milestone's aggregate NPS shortfall against the
10% target is a scoping question for the orchestrator, not a reason to remove any of the three
speed changes — each is individually justified above its own bar or, in M2-F3b's case, at zero
cost.

## Baseline promotion

Per the feature's criterion (promote unless the match shows a meaningful regression — it shows
+37.20), the confirmed build is promoted:

```
baselines/m2-nnue/manifold.exe        SHA-256 BC0C445C...D776DB48C   112,959,488 bytes
baselines/m2-nnue/build-metadata.txt
```

Verified after copying: `manifold.exe bench` → **45,036 nodes**, matching the pinned anchor.
No existing baseline directory was touched. This is the running baseline every M3 search feature
measures against.

## Assertion status

| Assertion | Status |
|---|---|
| A-NNUE-002, part 1 (≥10% 1T NPS vs mission-start, in a committed `experiments/` doc) | **NOT MET — 1.08x (+8%).** Two clean runs, recorded above. Returned to the orchestrator with actuals per the feature description. |
| A-NNUE-002, part 2 (~300-game match vs mission-start, no strength regression, zero forfeits) | **MET.** +37.20 ± 20.19 Elo, 300 games, 0 forfeits both sides. |
| A-DOC-001 (`run-metadata.txt` + results `.md` in the experiment dir) | **MET.** Both present, including preserved artifacts for the inadmissible run. |

## For the orchestrator

1. **The 10% NPS target is not reachable with further NNUE micro-optimization.** The remaining
   characterized target is worth ~0.8% of wall. If ≥10% matters as a number, it needs either a
   non-NNUE source of speed or an amendment to A-NNUE-002 recording that the M2-F1 profile
   (measured after the assertion was written) bounded the achievable gain below the target.
2. **The strength outcome is better than the speed outcome suggests**, likely because M2-F5's
   depth cap carries Elo the NPS number cannot see. If that attribution matters, it needs its own
   single-variable match (M2-F5 build vs the M2-F3b build) — not run here, since the milestone's
   assertion only required non-regression.
3. **Machine hygiene is now a live risk to every remaining match in this mission.** One
   interactive application produced 5 forfeits in 300 games and one 2.2x-spread NPS run. Check
   the harness's pre-run CPU line before quoting any result.

---

## Addendum (2026-08-06) — A-NNUE-002 amended to ≥8%, assertion now satisfied

The orchestrator took recommendation 1 above and **amended A-NNUE-002 in
`validation-contract.md` from ≥10% to ≥8%**, with the rationale recorded inline in the assertion
text. The amendment changes no measurement in this document; it changes the bar the measurements
are judged against, on evidence that post-dates the assertion:

- The M2-F1 profile (measured *after* A-NNUE-002 was written) put NNUE at **44.6% of wall time**,
  which bounds what any NNUE-only milestone can buy.
- The per-feature chain **Finny 1.03x × lazy 1.035x × threats 1.015x = 1.08x** reproduces the
  end-to-end measurement exactly, so the shortfall is not a lost or mis-measured gain.
- The only remaining characterized NNUE target (the mirror-flip `append_active_threats` scan) is
  worth **~0.8% of wall** — not enough to reach 10%, so the original target was unreachable within
  the milestone's scope rather than merely unmet.

**Revised assertion status (supersedes the A-NNUE-002 rows in the "Assertion status" table above):**

| Assertion | Status |
|---|---|
| A-NNUE-002, part 1 (**≥8%** 1T NPS vs mission-start, committed `experiments/` doc) | **MET.** 1.08x (+8%), two clean per-position-agreeing runs. |
| A-NNUE-002, part 2 (~300-game match, no strength regression, zero forfeits) | **MET.** +37.20 ± 20.19 Elo, 300 games, 0 forfeits both sides. |
| **A-NNUE-002 overall** | **SATISFIED** at 1.08x + non-regressing confirmation match. |

No measurement was rerun for this addendum. Re-verified only that the promoted build is intact:
`baselines/m2-nnue/manifold.exe`, SHA-256
`BC0C445CD9A26BEC0EEA446EEF555B1EE8C756250BAD943973D1C33D776DB48C`, `bench` → **45,036 nodes**,
matching the pinned anchor and Binary A in the provenance table above.
