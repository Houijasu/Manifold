# M4-F2 — Stockfish 18 benchmark, final build at 1T (A-SF-001 part 2)

**Result: `mission-final` vs Stockfish 18 = −263.42 ± 22.57 Elo, 18.00 % (54.0 / 300),
zero forfeits.** The M1-F3 anchor was −303.61 ± 30.73, 14.83 %.

**Δ = +40.19 Elo, Δscore = +3.17 pp.**

And because both matches drew **the identical 150 openings** (same seed, same book, verified
game-by-game in §4), the improvement can be quantified with a *paired* error bar rather than
two independent ones: **+3.17 pp ± 2.71 pp at 95 % confidence, p ≈ 0.023**. The gap closed,
and unlike the internal cumulative match this measurement resolves it.

---

## 1. Conditions — replicated from M1-F3 exactly

`experiments/MSN-F3-stockfish-baseline/MSN-M1-F3-results.md` froze the conditions and said
M4 must change only `-OutDir`, `-Purpose`, `-AName`, `-ACmd`. That is exactly what changed.

| Parameter | M1-F3 anchor | this match | same? |
|---|---|---|---|
| Driver | `harness/run_match.ps1` | `harness/run_match.ps1` | ✓ |
| Engine A | `baselines\mission-start\manifold.exe` | `baselines\mission-final\manifold.exe` | (the variable) |
| SHA-256 A | `43EB8A0D…CB28` | `4BC94E99A512E5DEEFE0AA057B7C839AE2D3E4A5557C03AA0256F64A813F5516` | (the variable) |
| Engine B | `C:\Users\Samaritan\bin\stockfish.exe` | same path | ✓ |
| SHA-256 B | `C86215FA1977D53B82ED854540A4C7B025BE4CD042276C85BA3DE53FB9118911` | `C86215FA1977D53B82ED854540A4C7B025BE4CD042276C85BA3DE53FB9118911` | ✓ **byte-identical opponent** |
| Time control | `8+0.08` | `8+0.08` | ✓ |
| Hash | 64 MB both | 64 MB both | ✓ |
| Threads | 1 both | 1 both | ✓ |
| Affinity / concurrency | `-use-affinity`, 8 | `-use-affinity`, 8 | ✓ |
| Book | `UHO_4060_v4.epd`, epd/random, `-repeat -games 2` | same | ✓ |
| Rounds | 150 (300 games) | 150 (300 games) | ✓ |
| **Seed** | **`20260805`** | **`20260805`** | ✓ |
| `-ForfeitsAllowedFor` | not used | not used | ✓ |

The Stockfish binary hash matching the one recorded in August is the point that makes the
delta meaningful: the opponent did not move between the two measurements.

Repo commit `cec5d4354359dfe8021d2f1dd38f5afedd5bfbdb` · Pre-run CPU 32 % (see §5) ·
Date 2026-08-06T16:28:34Z · Wall time **15 min 51 s** (anchor: 15 min 39 s).

### Exact command

```powershell
.\harness\run_match.ps1 `
    -OutDir 'experiments\MSN-final-stockfish' `
    -Purpose 'M4-F2 / A-SF-001 part 2: mission-final build vs Stockfish 18 at 1T, replicating experiments/MSN-F3-stockfish-baseline EXACTLY ...' `
    -AName 'mission-final' -ACmd '.\baselines\mission-final\manifold.exe' `
    -BName 'stockfish' -BCmd 'C:\Users\Samaritan\bin\stockfish.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260805
```

Preserved verbatim as `launch.ps1`.

### Secondary / weakened anchor: correctly not run

M1-F3's fallback was conditional — a weakened-Stockfish anchor only *if* the primary scored
0 % with no draws in the first ~100 games. That condition was not met then (28 draws in the
first 100) and **is not met now**: this match drew 35 of its first 100 games and finished
with 104 draws and 2 wins. M1-F3 states explicitly that "no secondary anchor was run … M4
therefore repeats this configuration and nothing else." **There is no secondary config to
replicate**, and adding one now would introduce a second variable with no M1 counterpart to
compare against.

---

## 2. Result

```
Results of mission-final vs stockfish (8+0.08, 1t, 64MB, UHO_4060_v4.epd):
Elo: -263.42 +/- 22.57, nElo: -658.19 +/- 39.32
LOS: 0.00 %, DrawRatio: 1.33 %, PairsRatio: 0.00
Games: 300, Wins: 2, Losses: 194, Draws: 104, Points: 54.0 (18.00 %)
Ptnml(0-2): [44, 104, 2, 0, 0], WL/DD Ratio: inf
```

### Side by side with the M1 anchor

| | M1-F3 (mission-start) | **M4-F2 (mission-final)** | delta |
|---|---|---|---|
| **Elo** | −303.61 ± 30.73 | **−263.42 ± 22.57** | **+40.19** |
| **Score** | 14.83 % (44.5 / 300) | **18.00 % (54.0 / 300)** | **+3.17 pp** (+9.5 points) |
| nElo | −622.74 ± 39.32 | −658.19 ± 39.32 | −35.45 |
| Wins | 5 | 2 | −3 |
| Draws | 79 | **104** | **+25** |
| Losses | 216 | **194** | **−22** |
| Ptnml(0-2) | [66, 79, 5, 0, 0] | **[44, 104, 2, 0, 0]** | 22 double-losses → half-point pairs |
| DrawRatio | 3.33 % | 1.33 % | −2.00 pp |
| Forfeits / crashes / illegal | 0 / 0 / 0 both | **0 / 0 / 0 both** | — |

### Rating-interval progression

| after games | score | Elo |
|---|---|---|
| 100 | 17.50 % | −269.37 ± 38.57 |
| 204 | 17.65 % | −267.60 ± 26.54 |
| 300 | 18.00 % | **−263.42 ± 22.57** |

### The pentanomial is the story, and it is not a flattering one

`[66, 79, 5, 0, 0]` → `[44, 104, 2, 0, 0]`. **Twenty-two opening pairs moved out of the
double-loss column into the loss+draw column**, and the count of pairs reaching a full point
went *down*, 5 → 2. Total wins fell 5 → 2 while draws rose 79 → 104.

So the entire gain is **survival, not competitiveness**. The final build loses less often
outright but wins even less often than the mission-start build did — with a 260 Elo gap that
is the expected shape of improvement (you first stop losing before you start winning), but
it should not be dressed up as anything else. `PairsRatio 0.00` and no pair above 1 point in
either match: Manifold has never been the better side of an opening pair against Stockfish 18
at this TC, before or after.

### Admissibility

```
affinity: enabled   concurrency: 8   threads: A=1 B=1
  mission-final time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  stockfish     time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  adjudications: 0
```

`run_match.ps1` exit **0**, fastchess exit **0**, `fastchess.log` clean. **Zero manifold
forfeits.** `-ForfeitsAllowedFor` was not needed for Stockfish, matching M1-F3. Admissible.

---

## 3. A-SF-001 part 2 status

> Two Stockfish 18 benchmark matches exist at identical conditions (same TC, book, 1T,
> ~300 games): one against the mission-start build (M1), one against the final build (M4).
> A summary doc quantifies the score/Elo-gap delta.

Both matches exist at verified-identical conditions ✓ · 300 games each ✓ · deltas quantified
here and in `experiments/MSN-mission-summary.md` ✓ · the 8T addendum is
`experiments/MSN-final-stockfish-8t/` ✓. **Satisfied.**

---

## 4. The two matches are paired at the opening level — verified, not assumed

M1-F3 predicted this ("same seed + same book = identical opening sequence") but never
checked it. `paired_analysis.py` in this directory reads both PGNs and confirms it:

```
mission-start (M1-F3): 150 opening pairs, total 44.5 points
mission-final (M4-F2): 150 opening pairs, total 54.0 points
common rounds: 150
rounds whose opening FEN is IDENTICAL in both runs: 150/150
```

**All 150 openings match**, so the difference can be analysed as a paired sample:

| statistic | value |
|---|---|
| per-pair score difference (final − start), out of 2 points | mean **+0.0633**, sd 0.3392, se **0.0277** (n = 150) |
| as a score percentage | **+3.17 pp ± 2.71 pp (95 %)** |
| pairs where mission-final scored more / less / equal | **44 / 24 / 82** |

This matters. The two independent Elo error bars (±30.73 and ±22.57) would combine in
quadrature to ±38 — an interval that swallows the +40.19 delta and would leave the mission's
headline result unresolvable. The paired analysis exploits the fact that both builds faced
the *same* 150 openings against the *same* opponent binary, cancelling opening difficulty
entirely. The result is a mean difference **2.29 standard errors from zero (p ≈ 0.023,
two-sided)**, and a sign test on 44 vs 24 discordant pairs gives p ≈ 0.021.

**The improvement against Stockfish 18 is statistically significant. The improvement against
mission-start in the internal head-to-head (+5.79 ± 21.18) is not.**

### Why the paired external match resolves what the internal match could not

Both measure the same two binaries, so this is not a contradiction — it is a difference in
instrument precision:

- **The internal match is unpaired at the pair level.** mission-final vs mission-start plays
  150 openings *once*, and the pair outcome is a single noisy draw from a 52 %-draw
  distribution. There is no second observation of the same opening to difference against.
- **The Stockfish matches are two observations of the same 150 openings** against a fixed
  third party. Differencing removes opening variance, which is the dominant variance
  component at a 52 % draw rate.
- A common yardstick also compresses: against a ~260-Elo-stronger opponent, small strength
  changes move the *score* on a steeper part of the logistic curve than they do in a
  near-50 % self-match, where they mostly convert draws into other draws.

The honest summary: **+3.17 pp ± 2.71 pp vs a fixed strong opponent on identical openings is
this mission's best-resolved strength measurement**, and it is consistent with — and tighter
than — the +5.79 ± 21.18 internal number.

### What it does not say

It does not attribute the gain to any particular feature. It is a whole-mission A/B on two
binaries. `experiments/MSN-final-cumulative/results.md` §4 discusses why the per-feature
point estimates sum to far more than either measurement shows, and names the single
next experiment (`UseCaptureLMR` on/off within the final build) that would resolve it.

---

## 5. On the 32 % pre-run CPU sample

The harness records the max of five `Win32_Processor.LoadPercentage` samples taken while it
computes SHA-256 over both engine binaries — 113 MB of Manifold plus Stockfish — so that
maximum is substantially self-inflicted. It was checked rather than assumed: a 10-second
delta-CPU census of every non-match process (`zen`, `droid`, terminal, audio, security)
totalled **12.69 CPU-seconds out of 320 available across 32 logical cores = 3.96 %**, and no
foreign `manifold`/`stockfish`/`fastchess` process existed at any point.

The decisive evidence is in the result itself: external load contaminating a 1T match shows
up as time forfeits (mission AGENTS.md records 5 forfeits from one foreground app) and as a
distorted wall time. This run had **zero forfeits, zero timeouts** and finished in
**15 min 51 s** against the anchor's **15 min 39 s** — a 1.3 % difference over 300 games.
The run is admissible.

---

## 6. Files

| file | contents |
|---|---|
| `run-metadata.txt` | harness provenance: commit, both SHA-256, TC, seed, book, affinity/concurrency/threads/hash, CPU load, full command line, self-check block |
| `console.txt` | full fastchess output incl. rating intervals and the self-check |
| `fastchess.log` | fastchess log at `level=warn` (clean) |
| `games.pgn` | all 300 games (untracked by repo convention; seed + command reproduce it) |
| `launch.ps1` | the exact launcher used |
| `paired_analysis.py` | §4 — reads both PGNs, verifies the opening pairing, computes the paired difference |
| `driver-stdout.txt` / `driver-stderr.txt` | driver stdout/stderr (stderr empty) |
| `results.md` | this document |
