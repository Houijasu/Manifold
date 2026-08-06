# MSN-S3-ltc — Longer-TC sanity match for the TM effort term (M3-F3)

**This directory holds the SECOND of two matches for M3-F3. The decision, the mechanism,
and the full write-up live in `../MSN-S3-tm-effort/results.md` — read that one.**

## Purpose

`UseTimeEffort` measured **-17.39 ± 18.99 Elo** over 300 games at the standard 8+0.08
(`../MSN-S3-tm-effort/`). The error bar covered zero, and 8+0.08 is short for a
time-management change: the reference engine's own gain for this term is an LTC result,
so a null or marginal STC number is exactly what a genuinely-positive TM feature could
also produce. The M3-F3 feature description authorized **one** 60-game 30+0.3 sanity
match in that situation before deciding. This is that match.

## Command

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S3-tm-effort-ltc `
    -Purpose 'M3-F3 effort term sanity at a longer TC: 8+0.08 is short for a time-management change (authorized by the feature description after a null STC result)' `
    -AName tm-effort -ACmd .\target\release\manifold.exe `
    -BName m2-nnue  -BCmd .\baselines\m2-nnue\manifold.exe `
    -TC '30+0.3' -Rounds 30 -Seed 20260807
```

Threads=1 both sides, Hash 64, `UHO_4060_v4.epd`, `-use-affinity -concurrency 8`
(enforced by the harness), same two binaries as the STC match — see `run-metadata.txt`
for the SHA-256 of each.

## Result

| | |
|---|---|
| **Elo** | **-34.86 ± 44.35** |
| nElo | -69.96 ± 87.91 |
| LOS | 5.94% |
| Score | 27.0 / 60 (45.00%) — 10W 16L 34D |
| Ptnml(0-2) | [0, 11, 14, 5, 0] |
| PairsRatio | 0.45 |
| DrawRatio | 46.67% |
| Forfeits / crashes | **0 / 0** both engines |
| Wall | 13m41s |

## Reading

60 games cannot decide anything alone — the error bar is ±44 Elo and covers zero. Its
job was to answer one question: *does the picture change at a longer control?* It does
not. The point estimate is negative, larger in magnitude than the STC one, and LOS is
5.94% against the STC's 3.60%.

Two independent samples at two time controls, both near -20 to -35 Elo, both with LOS
under 6%, is the evidence that turned a marginal STC result into a revert. **The feature
ships OFF.**

Do not read the -34.86 as a better estimate of the effect size than the STC's -17.39;
it is a fifth of the games. Read it only as agreement in direction.
