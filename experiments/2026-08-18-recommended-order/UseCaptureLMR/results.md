# UseCaptureLMR primary match

## Setup

- Source commit: `5021efdfd73f8b69a003257150867260577c63c2`
- Binary A/B: `baselines/recommended-order/manifold.exe`
- SHA-256 A/B: `D33F99DAC8BC9F538652BC092F62BD8C06EE6005231BFB6ADE2AF121EA324A7E`
- A: `capture-lmr-off`, `UseCaptureLMR=false`
- B: `capture-lmr-on`, shipped `true` default
- Purpose: measure `UseCaptureLMR=false` against the shipped `true` default in
  the recommended-order binary
- Time control: `8+0.08`; hash: 64 MB; seed: `2026081803`
- 150 paired openings (300 games), fixed length, `UHO_4060_v4.epd`
- Affinity: enabled; concurrency: 8; threads: A=1, B=1
- Pre-run CPU: 11% (maximum of five samples)
- Date: `2026-08-18T04:58:15Z`; driver: `harness/run_match.ps1`

Harness invocation:

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

## Result

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

## Decision

The positive primary point estimate requires independent validation before
changing the shipped `UseCaptureLMR=true` default. This primary result alone
does not justify turning capture LMR off.
