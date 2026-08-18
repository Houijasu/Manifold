# UseTtMoveHistory primary match

## Setup

- Frozen-binary source commit:
  `ba403d5e6c43793c636e73fa373761f3d1e49e51`
- Raw harness metadata records commit
  `5021efdfd73f8b69a003257150867260577c63c2` because
  `run_match.ps1` uses a hard-coded separate main checkout for
  `git rev-parse`; that value is not the frozen binary's source commit.
- Binary A/B: `baselines/recommended-order/manifold.exe`
- SHA-256 A/B: `D33F99DAC8BC9F538652BC092F62BD8C06EE6005231BFB6ADE2AF121EA324A7E`
- A: `tt-move-history-on`, `UseTtMoveHistory=true`
- B: `tt-move-history-off`, shipped `false` default
- Time control: `8+0.08`; hash: 64 MB; seed: `2026081801`
- 150 paired openings (300 games), fixed length, UHO_4060_v4.epd
- Affinity: enabled; concurrency: 8; threads: A=1, B=1

Harness invocation:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseTtMoveHistory\primary `
  -Purpose 'Measure UseTtMoveHistory=true against the shipped false default in the recommended-order binary' `
  -AName tt-move-history-on `
  -ACmd .\baselines\recommended-order\manifold.exe `
  -AOptions 'option.UseTtMoveHistory=true' `
  -BName tt-move-history-off `
  -BCmd .\baselines\recommended-order\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081801
```

Resolved fastchess command:

```text
C:\Users\Samaritan\Projects\Manifold\tools\fastchess\fastchess.exe -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order\manifold.exe name=tt-move-history-on option.Hash=64 option.Threads=1 option.UseTtMoveHistory=true -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order\manifold.exe name=tt-move-history-off option.Hash=64 option.Threads=1 -each proto=uci tc=8+0.08 -openings file=C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd format=epd order=random -repeat -games 2 -rounds 150 -concurrency 8 -srand 2026081801 -report penta=true -ratinginterval 50 -pgnout file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseTtMoveHistory\primary\games.pgn append=false -log file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseTtMoveHistory\primary\fastchess.log level=warn append=false -use-affinity
```

## Result

From A (`UseTtMoveHistory=true`) perspective:

- Games: 300
- W/L/D: 75/89/136
- Pentanomial: `[3, 41, 75, 29, 2]`
- Elo: `-16.23 +/- 21.45`

The harness exited 0. Both engines recorded zero time forfeits, timeouts,
crashes, illegal moves, illegal PV reports, and `No output from` events.
Adjudications were zero.

## Decision

Retain the shipped `UseTtMoveHistory=false` default because the primary point
estimate is `<= 0`. Task 12 validation is not requested.
