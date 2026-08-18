# UseTtMoveHistory authoritative post-fix primary match

> This post-fix match supersedes the earlier `ba403d5` match set and its recorded metrics. The earlier `primary/` artifacts remain for history but are not authoritative for the final option decision.

## Frozen binary

- Source commit: `7c226557510c77ad2cf0ef5c9baa033737df68dd`
- Binary A/B: `baselines/recommended-order-post-fix/manifold.exe`
- SHA-256 A/B: `48D65DECCD56DF1670CB727C4045F465CC3C20B691052AB97B61D337D2DFF229`
- Bench verification: `35859` nodes twice
- Raw harness metadata records commit `5021efdfd73f8b69a003257150867260577c63c2` because `run_match.ps1` runs `git rev-parse` in a hard-coded separate main checkout. That metadata value was left unchanged and is not the frozen binary's source commit.

## Setup

- A: `tt-move-history-on`, `UseTtMoveHistory=true`
- B: `tt-move-history-off`, default `false`
- Seed: `2026081821`
- Time control: `8+0.08`; hash: 64 MB
- 150 paired openings, 300 games, fixed length, `UHO_4060_v4.epd`
- Affinity enabled; concurrency 8; threads A=1 and B=1

Harness invocation:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseTtMoveHistory\post-fix-primary `
  -Purpose 'Post-fix primary: measure UseTtMoveHistory=true against false after all correctness commits' `
  -AName tt-move-history-on `
  -ACmd .\baselines\recommended-order-post-fix\manifold.exe `
  -AOptions 'option.UseTtMoveHistory=true' `
  -BName tt-move-history-off `
  -BCmd .\baselines\recommended-order-post-fix\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081821
```

Resolved fastchess command:

```text
C:\Users\Samaritan\Projects\Manifold\tools\fastchess\fastchess.exe -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order-post-fix\manifold.exe name=tt-move-history-on option.Hash=64 option.Threads=1 option.UseTtMoveHistory=true -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order-post-fix\manifold.exe name=tt-move-history-off option.Hash=64 option.Threads=1 -each proto=uci tc=8+0.08 -openings file=C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd format=epd order=random -repeat -games 2 -rounds 150 -concurrency 8 -srand 2026081821 -report penta=true -ratinginterval 50 -pgnout file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseTtMoveHistory\post-fix-primary\games.pgn append=false -log file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseTtMoveHistory\post-fix-primary\fastchess.log level=warn append=false -use-affinity
```

## Authoritative result

From A (`UseTtMoveHistory=true`) perspective:

- Games: 300
- W/L/D: 73/85/142
- Pentanomial: `[4, 42, 68, 34, 2]`
- Elo: `-13.90 +/- 22.66`

An independent PGN parse reproduced 300 games and W/L/D 73/85/142. The final fastchess summary reproduced the pentanomial and Elo values. The harness and fastchess exited 0. Time forfeits, timeouts, crashes, illegal moves, illegal PV reports, `No output from` events, and adjudications were all zero.

## Decision

Retain `UseTtMoveHistory=false`. The authoritative primary point estimate for the `true` alternative is `-13.90`, which is `<= 0`; no validation match is required by policy.
