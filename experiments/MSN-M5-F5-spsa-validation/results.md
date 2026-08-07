# M5-F5 validation match — tuned SPSA spins vs shipped defaults

**Purpose.** The single measurement match that decides M5-F5. The full session write-up,
the theta trajectory and the keep decision live in
`experiments/MSN-M5-F5-spsa/results.md`; this directory holds the match itself.

## What was compared

**One binary, two option sets.** `run-metadata.txt` records the same SHA-256
(`FA81C511…63D1`) for engine A and engine B, so the only difference between the arms is
the eight spins passed via `-AOptions`. Nothing about the build, the network or the
harness varies across the comparison.

| Parameter | A (`spsa-tuned`) | B (`defaults`) |
|---|---:|---:|
| LmrCoefficient | 2,754 | 2,872 |
| LmrBase | 996 | 982 |
| LmrTtPvReduction | 1,028 | 1,024 |
| LmrHistoryNumerator | 459 | 439 |
| RfpMarginPerDepth | 95 | 105 |
| RfpTtPvMargin | 22 | 21 |
| FutilityBaseMargin | 125 | 124 |
| FutilityMarginPerDepth | 106 | 109 |

The A values are the final theta of the 345-iteration / 5,520-game session, rounded to
spins.

## Command

```powershell
.\harness\run_match.ps1 -OutDir experiments\MSN-M5-F5-spsa-validation `
    -Purpose 'M5-F5 SPSA validation: tuned 8-param spins (345 iters / 5520 games) vs shipped defaults' `
    -AName spsa-tuned -ACmd .\target\release\manifold.exe `
    -AOptions 'option.LmrCoefficient=2754','option.LmrBase=996','option.LmrTtPvReduction=1028',`
              'option.LmrHistoryNumerator=459','option.RfpMarginPerDepth=95','option.RfpTtPvMargin=22',`
              'option.FutilityBaseMargin=125','option.FutilityMarginPerDepth=106' `
    -BName defaults -BCmd .\target\release\manifold.exe `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260807
```

Fixed length, 300 games, no SPRT — the mission's measurement-match rules.

## Result

| | |
|---|---|
| **Elo** | **+4.63 ± 23.40** |
| nElo | +7.80 ± 39.32 |
| LOS | 65.13% |
| Games | 300 — 88 W / 84 L / 128 D, 152.0 points (50.67%) |
| Ptnml(0–2) | [5, 33, 68, 41, 3] |
| PairsRatio | 1.16 |
| DrawRatio | 45.33% |
| WL/DD Ratio | 1.52 |
| Wall clock | 17 min 10 s |

## Guardrails

Both engines `Threads=1`, so `-use-affinity -concurrency 8` was mandatory and the driver
applied it (AGENTS.md 4.451). Memory pre-flight: 16 processes × 64 MiB = 1,024 MiB
against 19,277 MiB free. Pre-run CPU 7%, max of 5 samples.

```
spsa-tuned   time forfeits 0   Timeouts 0   Crashed 0   illegal moves 0
defaults     time forfeits 0   Timeouts 0   Crashed 0   illegal moves 0
adjudications: 0
```

Zero forfeits and zero crashes for both engines: the run is admissible.

## Decision

**KEEP** — the criterion was a positive point estimate and +4.63 is one. The pentanomial
agrees with the sign (41 pairs won against 33 lost).

The error bar is four times the point estimate. What 300 games rule out is a regression
beyond roughly 20 Elo; they cannot separate +5 from 0. The tuned values ship as the
compiled defaults with that caveat recorded next to them, in the same form as the
mission's other small-positive keeps.
