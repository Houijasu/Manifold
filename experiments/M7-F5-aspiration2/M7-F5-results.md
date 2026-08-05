# M7-F5 — Score-scaled aspiration windows (aspiration2)

> **Written retroactively.** This experiment ran on 2026-08-03 and was never given a
> results summary; the mission that inherited the branch found the directory holding
> raw artifacts and no verdict. This document reconstructs the outcome **from those
> artifacts only** (`run-metadata.txt`, `console.txt`, `fastchess.log`). No new match
> was run, and nothing about the experiment's state was changed. Where the artifacts do
> not settle a question, this document says so rather than guessing.

## Purpose

Second attempt at aspiration-window tuning, after M7-F4 measured its predecessor as
inconclusive. Verbatim from `run-metadata.txt`:

> Phase 4 revised: aspiration with score-scaled initial delta (8 + prev^2/16053 capped
> 512), +1/3 widening (doubling measured inconclusive in M7-F4), beta-to-midpoint on
> fail low, fail-high depth reduction up to 3 plies. Measured +0.77 plies vs phase3 at
> 1s over 30 positions. SPRT vs phase3.

The `+1/3` widening is the single deliberate change of direction from M7-F4: window
doubling had already been measured and rejected, and remains on the mission's settled-
negatives list.

## Conditions

| | |
|---|---|
| Commit | `cf331d1` (`Widen the pruning windows so the search reaches its depth`) |
| Engine A | `aspiration2` — `target/release/manifold.exe`, SHA-256 `F5A7DCC6…9C0DEB` |
| Engine B | `phase3` — `baselines/phase3/manifold.exe`, SHA-256 `54F5A8C7…F6999F` |
| TC / Hash / Threads | 8+0.08, 64 MB, 1T both sides |
| Book | `UHO_4060_v4.epd`, random order, `-repeat -games 2` (paired openings) |
| Harness | `harness/run_match.ps1`, `-use-affinity -concurrency 8` (mandatory at 1T) |
| Seed | 20260808 |
| Planned length | 1000 rounds = 2000 games, **SPRT** `elo0=0 elo1=5 alpha=0.05 beta=0.05` |
| Started | 2026-08-03T22:16:03Z |

Full command line is preserved in `run-metadata.txt`.

## Result

**The run did not terminate.** `console.txt` ends mid-tournament at "Started game 1994
of 2000" with no final report block and no SPRT termination line — neither an `H0/H1
accepted` nor a completion summary. The process ended without writing a verdict, so the
numbers below are the **last periodic report** fastchess emitted (at the 1900-game
`-ratinginterval` boundary), not a final result.

```
Results of aspiration2 vs phase3 (8+0.08, 1t, 64MB, UHO_4060_v4.epd):
Elo: 19.22 +/- 9.01, nElo: 33.41 +/- 15.62
LOS: 100.00 %, DrawRatio: 46.63 %, PairsRatio: 1.44
Games: 1900, Wins: 548, Losses: 443, Draws: 909, Points: 1002.5 (52.76 %)
Ptnml(0-2): [15, 193, 443, 270, 29], WL/DD Ratio: 0.99
LLR: 2.41 (81.8%) (-2.94, 2.94) [0.00, 5.00]
```

**+19.22 ± 9.01 Elo** over 1900 games, LLR **2.41** against an upper bound of 2.94 —
81.8% of the way to accepting H1, and still climbing at the point the run stopped.

Stability of the estimate over the last 500 games (the run was not drifting toward the
bound on a single lucky stretch):

| games | Elo | LLR |
|---:|---:|---:|
| 1400 | +19.88 ± 10.52 | — |
| 1500 | +21.80 ± 10.24 | — |
| 1602 | +22.15 ± 9.87 | — |
| 1700 | +22.10 ± 9.54 | — |
| 1800 | +19.71 ± 9.22 | — |
| 1900 | +19.22 ± 9.01 | 2.41 |

For comparison, M7-F4 (the doubling-widen variant this replaced) reached only
**+10.72 ± 11.00** at 1200 games with LLR 0.82 — well inside noise. The score-scaled
delta plus `+1/3` widening roughly doubled the point estimate and halved the distance
to significance.

### Forfeits and crashes

`fastchess.log` contains exactly one warning for the whole run:

```
[WARN ] [03:04:27] <  8> fastchess --- Engine phase3 loses on time
```

That is **the baseline**, not the build under test. `aspiration2` recorded zero
forfeits, zero crashes, and zero disconnects across ~1990 games. The single baseline
time-loss is one game in ~1990 and cannot move a ±9 Elo estimate materially.

## Assessment

The evidence supports **KEEP**, and the change was in fact kept — `cf331d1` is the tip
of `feature/nnue-optimizations` and the aspiration logic it introduced is live in the
mission-start baseline built from it.

Two honest caveats, recorded rather than smoothed over:

1. **This is not a completed SPRT.** LLR 2.41 < 2.94: H1 was never formally accepted.
   The correct description is "a strong positive trend that did not reach its stopping
   rule", not "an SPRT-confirmed gain". The +19.22 ± 9.01 point estimate is a
   fixed-length reading of a run that happened to stop at 1900 games; taken at face
   value its lower bound is ≈ +10 Elo, which is comfortably positive, and LOS is
   100.00%.
2. **Why the run stopped is not recoverable from the artifacts.** There is no error in
   the log, no crash line, and no partial final block — the console simply ends. Loss
   of the driving shell is the most likely explanation given the clean truncation, but
   the artifacts do not prove it and this document will not invent a cause.

Neither caveat is grounds to revisit the decision now: re-running it would cost a
2000-game match to re-confirm a result whose direction, magnitude, and consistency are
already clear, and the mission's match budget is better spent on untested changes.

## Provenance and lessons

The reason this file exists is that M7-F5 was allowed to end without one. That gap is
now a standing mission requirement (assertion **A-DOC-001**): every experiment
directory gets a results `.md` stating purpose, command, result, and decision — written
when the experiment ends, not reconstructed by whoever finds the directory later.

Retained artifacts, unmodified:

- `run-metadata.txt` — provenance, binaries with SHA-256, full fastchess command
- `console.txt` — 4133 lines, 19 periodic report blocks, truncated at game 1994
- `games.pgn` — 7.6 MB of game records
- `fastchess.log` — the single baseline time-loss warning
