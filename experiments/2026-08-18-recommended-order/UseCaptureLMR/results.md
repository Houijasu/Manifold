# UseCaptureLMR primary and validation matches

## Shared setup

- Frozen-binary source commit:
  `ba403d5e6c43793c636e73fa373761f3d1e49e51`
- Raw harness metadata records commit
  `5021efdfd73f8b69a003257150867260577c63c2` because
  `run_match.ps1` uses a hard-coded separate main checkout for
  `git rev-parse`; that value is not the frozen binary's source commit.
- Binary A/B: `baselines/recommended-order/manifold.exe`
- SHA-256 A/B: `D33F99DAC8BC9F538652BC092F62BD8C06EE6005231BFB6ADE2AF121EA324A7E`
- A: `capture-lmr-off`, `UseCaptureLMR=false`
- B: `capture-lmr-on`, shipped `true` default
- Time control: `8+0.08`; hash: 64 MB
- 150 paired openings (300 games), fixed length, `UHO_4060_v4.epd`
- Affinity: enabled; concurrency: 8; threads: A=1, B=1
- Driver: `harness/run_match.ps1`

## Primary

- Purpose: measure `UseCaptureLMR=false` against the shipped `true` default in
  the recommended-order binary
- Seed: `2026081803`
- Pre-run CPU: 11% (maximum of five samples)
- Date: `2026-08-18T04:58:15Z`

Invocation:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCaptureLMR\primary `
  -Purpose 'Measure UseCaptureLMR=false against the shipped true default in the recommended-order binary' `
  -AName capture-lmr-off `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseCaptureLMR=false' `
  -BName capture-lmr-on `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081803
```

Resolved fastchess command:

```text
C:\Users\Samaritan\Projects\Manifold\tools\fastchess\fastchess.exe -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order\manifold.exe name=capture-lmr-off option.Hash=64 option.Threads=1 option.UseCaptureLMR=false -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order\manifold.exe name=capture-lmr-on option.Hash=64 option.Threads=1 -each proto=uci tc=8+0.08 -openings file=C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd format=epd order=random -repeat -games 2 -rounds 150 -concurrency 8 -srand 2026081803 -report penta=true -ratinginterval 50 -pgnout file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\primary\games.pgn append=false -log file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\primary\fastchess.log level=warn append=false -use-affinity
```

From A (`UseCaptureLMR=false`) perspective:

- Games: 300
- W/L/D: 82/80/138
- Pentanomial: `[0, 38, 74, 36, 2]`
- Elo: `+2.32 +/- 20.58`

An independent PGN parse found 300 games, 150 complete two-game pairs,
W/L/D 82/80/138, pentanomial `[0, 38, 74, 36, 2]`, and 300 normal
terminations. The score-derived Elo point estimate rounds to `+2.32`, and the
fastchess report gives the `+/- 20.58` interval. A fresh SHA-256 calculation
matched the frozen hash above.

The fastchess and harness exit code was 0. Both engines recorded zero time
forfeits, timeouts, crashes, illegal moves, illegal PV reports, and
`No output from` events. Adjudications were zero.

## Validation

- Purpose: validate the positive `UseCaptureLMR=false` primary point estimate
  with an independent seed
- Seed: `2026081813`
- Pre-run CPU: 97% (maximum of five samples)
- Date: `2026-08-18T05:24:42Z`

Invocation:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCaptureLMR\validation `
  -Purpose 'Validate the positive UseCaptureLMR=false primary point estimate with an independent seed' `
  -AName capture-lmr-off `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseCaptureLMR=false' `
  -BName capture-lmr-on `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081813
```

Resolved fastchess command:

```text
C:\Users\Samaritan\Projects\Manifold\tools\fastchess\fastchess.exe -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order\manifold.exe name=capture-lmr-off option.Hash=64 option.Threads=1 option.UseCaptureLMR=false -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order\manifold.exe name=capture-lmr-on option.Hash=64 option.Threads=1 -each proto=uci tc=8+0.08 -openings file=C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd format=epd order=random -repeat -games 2 -rounds 150 -concurrency 8 -srand 2026081813 -report penta=true -ratinginterval 50 -pgnout file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\validation\games.pgn append=false -log file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\validation\fastchess.log level=warn append=false -use-affinity
```

From A (`UseCaptureLMR=false`) perspective:

- Games: 300
- W/L/D: 73/83/144
- Pentanomial: `[1, 45, 67, 37, 0]`
- Elo: `-11.59 +/- 21.02`

An independent PGN parse found 300 games, 150 complete two-game pairs,
W/L/D 73/83/144, pentanomial `[1, 45, 67, 37, 0]`, and 300 normal
terminations. The score-derived Elo point estimate rounds to `-11.59`, and
the fastchess report gives the `+/- 21.02` interval. A fresh SHA-256
calculation matched the frozen hash above.

The fastchess and harness exit code was 0. Both engines recorded zero time
forfeits, timeouts, crashes, illegal moves, illegal PV reports, and
`No output from` events. Adjudications were zero.

## Pooled result

Combining the primary and validation evidence from A
(`UseCaptureLMR=false`) perspective:

- Games: 600
- W/L/D: 155/163/282
- Pentanomial: `[1, 83, 141, 73, 2]`
- Score: 296/600 points (`49.3333%`)
- Elo point estimate: `-4.63`

The pooled point estimate uses fastchess's score convention:
`400 * log10(score / (1 - score))`, with
`score = (wins + draws / 2) / games`.

## Decision

Retain the shipped `UseCaptureLMR=true` default. The exact alternative-off
point estimates were primary `+2.32`, validation `-11.59`, and pooled
`-4.63`. The policy requires the primary alternative point estimate to be
positive, the validation alternative point estimate to be non-negative, and
the pooled alternative point estimate to be positive. The validation result
is negative, so the alternative fails policy regardless of the pooled result.

## Concern

The validation metadata recorded 97% pre-run CPU (the maximum of five
samples), so host load was high at launch. The run nevertheless completed all
300 games with the required affinity/concurrency/thread settings, no
adjudications, and every recorded integrity guardrail at zero. Its negative
point estimate independently blocks changing the shipped default.
