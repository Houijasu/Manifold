# M1-F3 — Stockfish 18 baseline benchmark (mission anchor)

**Purpose.** Quantify the strength gap between the mission-start Manifold build and
Stockfish 18 under fixed, exactly reproducible conditions. This is the *first* of the two
matches required by **A-SF-001**; the M4 counterpart must be run with the conditions
recorded below **unchanged** so the two numbers are directly comparable and their delta is
the mission's headline strength result.

This match measures the starting gap only. A heavy loss was expected and is not a defect.

---

## Conditions (M4 must replicate these exactly)

| Parameter | Value |
|---|---|
| Driver | `harness/run_match.ps1` (guardrails enforced) |
| fastchess | `tools/fastchess/fastchess.exe`, alpha 1.8.1 (20260405-1525c4b) |
| Engine A | `manifold-mission-start` → `baselines\mission-start\manifold.exe` |
| SHA-256 A | `43EB8A0DD0C81172EFDD1F914899C080DFD757CA26EACA144913E52CEAD2CB28` |
| Engine B | `stockfish` → `C:\Users\Samaritan\bin\stockfish.exe` (Stockfish 18) |
| SHA-256 B | `C86215FA1977D53B82ED854540A4C7B025BE4CD042276C85BA3DE53FB9118911` |
| Time control | `8+0.08` |
| Hash | 64 MB (both engines) |
| Threads | 1 (both engines) |
| Affinity / concurrency | `-use-affinity`, concurrency 8 (mandatory at 1T-vs-1T) |
| Book | `tools\books\UHO_4060_v4.epd`, `format=epd order=random`, `-repeat -games 2` |
| Rounds | 150 (300 games, paired openings) |
| **Seed** | **`20260805`** |
| Repo commit | `164a3d23432bde43e9d7b9333b39ab54b206bab2` |
| Pre-run CPU load | 5 % (max of 5 samples); machine otherwise idle |
| Date (UTC) | 2026-08-05T12:08:13Z |
| Wall time | 15 min 39 s |

### Exact command

```powershell
.\harness\run_match.ps1 `
    -OutDir 'experiments\MSN-F3-stockfish-baseline' `
    -Purpose 'M1 anchor benchmark: mission-start build vs Stockfish 18 at 1T, quantifying the starting strength gap (A-SF-001). M4 must replicate these conditions exactly.' `
    -AName 'manifold-mission-start' -ACmd '.\baselines\mission-start\manifold.exe' `
    -BName 'stockfish' -BCmd 'C:\Users\Samaritan\bin\stockfish.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260805
```

(Preserved verbatim as `launch-primary.ps1` in this directory. For M4, change only
`-OutDir`, `-Purpose`, `-AName`, and `-ACmd`; **leave `-TC`, `-Hash`, `-Rounds`, `-Seed`,
the book default, and the thread counts untouched.**)

The fully expanded fastchess command line is in `run-metadata.txt`.

---

## Result

```
Results of manifold-mission-start vs stockfish (8+0.08, 1t, 64MB, UHO_4060_v4.epd):
Elo: -303.61 +/- 30.73, nElo: -622.74 +/- 39.32
LOS: 0.00 %, DrawRatio: 3.33 %, PairsRatio: 0.00
Games: 300, Wins: 5, Losses: 216, Draws: 79, Points: 44.5 (14.83 %)
Ptnml(0-2): [66, 79, 5, 0, 0], WL/DD Ratio: inf
```

**Headline: Manifold (mission-start) is −303.6 ± 30.7 Elo vs Stockfish 18, scoring 14.83 %
(44.5 / 300) at 8+0.08, 1T, Hash 64.**

Pentanomial `[66, 79, 5, 0, 0]`: of 150 opening pairs, 66 were double losses, 79 were
1 loss + 1 draw or equivalent, 5 reached 1 point, and **no pair scored above 1 point**.

### Rating-interval progression (stability check)

| after games | score | Elo |
|---|---|---|
| 100 | 14.00 % | −315.35 ± 50.52 |
| 200 | 15.00 % | −301.33 ± 35.63 |
| 300 | 14.83 % | −303.61 ± 30.73 |

The estimate is stable from 100 games onward; the final ±30.7 error bar is small relative
to the gap, so this anchor has ample resolution for detecting a mission-scale improvement.

---

## Admissibility

Harness self-check (both engines):

```
affinity: enabled   concurrency: 8   threads: A=1 B=1
  manifold-mission-start  time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  stockfish               time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  adjudications: 0
```

- `run_match.ps1` exit code **0**; fastchess exit code **0**.
- **Zero Manifold forfeits, crashes, and illegal moves.** Zero for Stockfish as well.
- `-ForfeitsAllowedFor` was **not** used and was not needed — Stockfish behaved correctly
  throughout, confirming the plumbing verified in `experiments/readiness-smoke/`.
- `fastchess.log` (level=warn) contains only INFO banner lines; no warnings.
- Every one of the 300 games terminated by a normal chess result (mate, 3-fold repetition,
  insufficient material, 50-move); none by time forfeit or adjudication.

The result is admissible evidence.

---

## Secondary (weaker) anchor — not required

The feature specified a fallback: if the score were 0 % across the first ~100 games with
**no draws at all**, stop early and add a weakened-Stockfish anchor (node-limited or
`UCI_LimitStrength`) for resolution.

**That condition was not met and no secondary anchor was run.** The first 100 games already
produced 28 draws (14.00 %), and the full match produced 5 wins and 79 draws. A 14.83 %
score with a ±30.7 Elo error bar has more than enough resolution to register the M1–M4
improvement, so a weaker anchor would only add an unnecessary second variable to compare
against in M4. M4 therefore repeats this configuration and nothing else.

---

## Interpretation and what M4 must do

- The mission's starting point is roughly **300 Elo behind Stockfish 18** at fast 1T time
  control. The loss profile — 72 % outright losses, only 3.3 % draws, no pair above
  1 point — is a search-depth/eval-quality gap, not an instability or time-management gap
  (zero forfeits, zero crashes).
- **A-SF-001 part 1 is satisfied** by this directory.
- For part 2, M4 runs the identical match with the final build as engine A. The reportable
  deliverable is the delta, e.g. `ΔElo = Elo_final − (−303.61)` and
  `Δscore = score_final − 14.83 %`, computed in the M4 summary doc.
- Because both matches use seed `20260805`, the same book, and `order=random`, the opening
  sequence is identical between the two runs — the comparison is paired at the opening
  level, not merely at the aggregate level.

## Files

| file | contents |
|---|---|
| `run-metadata.txt` | AGENTS.md §4.7 provenance: commit, both SHA-256 hashes, TC, seed, book, affinity/concurrency/threads/hash, CPU load, full command line, self-check block |
| `games.pgn` | all 300 games (1.03 MB) |
| `console.txt` | full fastchess output incl. rating intervals and self-check |
| `fastchess.log` | fastchess log at `level=warn` (clean) |
| `launch-primary.ps1` | the exact launcher used, preserved for M4 replication |
| `MSN-M1-F3-results.md` | this document |
