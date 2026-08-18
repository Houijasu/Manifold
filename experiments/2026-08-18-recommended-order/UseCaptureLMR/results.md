# UseCaptureLMR authoritative post-fix matches

> This post-fix primary and validation set supersedes the earlier `ba403d5` match set, its pooled result, and its retain-`true` decision. The earlier `primary/` and `validation/` artifacts remain for history but are not authoritative for the final option decision.

## Frozen binary and shared setup

- Source commit: `7c226557510c77ad2cf0ef5c9baa033737df68dd`
- Binary A/B: `baselines/recommended-order-post-fix/manifold.exe`
- SHA-256 A/B: `48D65DECCD56DF1670CB727C4045F465CC3C20B691052AB97B61D337D2DFF229`
- Bench verification: `35859` nodes twice
- A: `capture-lmr-off`, `UseCaptureLMR=false`
- B: `capture-lmr-on`, default `true`
- Time control: `8+0.08`; hash: 64 MB
- 150 paired openings per run, 300 games per run, fixed length, `UHO_4060_v4.epd`
- Affinity enabled; concurrency 8; threads A=1 and B=1
- Raw harness metadata records commit `5021efdfd73f8b69a003257150867260577c63c2` because `run_match.ps1` runs `git rev-parse` in a hard-coded separate main checkout. That metadata value was left unchanged and is not the frozen binary's source commit.

## Post-fix primary

- Directory: `post-fix-primary/`
- Seed: `2026081823`

Harness invocation:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCaptureLMR\post-fix-primary `
  -Purpose 'Post-fix primary: measure UseCaptureLMR=false against true after all correctness commits' `
  -AName capture-lmr-off `
  -ACmd .\baselines\recommended-order-post-fix\manifold.exe `
  -AOptions 'option.UseCaptureLMR=false' `
  -BName capture-lmr-on `
  -BCmd .\baselines\recommended-order-post-fix\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081823
```

Resolved fastchess command:

```text
C:\Users\Samaritan\Projects\Manifold\tools\fastchess\fastchess.exe -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order-post-fix\manifold.exe name=capture-lmr-off option.Hash=64 option.Threads=1 option.UseCaptureLMR=false -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order-post-fix\manifold.exe name=capture-lmr-on option.Hash=64 option.Threads=1 -each proto=uci tc=8+0.08 -openings file=C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd format=epd order=random -repeat -games 2 -rounds 150 -concurrency 8 -srand 2026081823 -report penta=true -ratinginterval 50 -pgnout file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\post-fix-primary\games.pgn append=false -log file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\post-fix-primary\fastchess.log level=warn append=false -use-affinity
```

From A (`UseCaptureLMR=false`) perspective:

- Games: 300
- W/L/D: 79/77/144
- Pentanomial: `[2, 36, 73, 36, 3]`
- Elo: `+2.32 +/- 21.80`

An independent PGN parse reproduced 300 games and W/L/D 79/77/144. The final fastchess summary reproduced the pentanomial and Elo values. The harness and fastchess exited 0, and every recorded integrity guardrail was zero.

## Post-fix validation

- Directory: `post-fix-validation/`
- Seed: `2026081833`
- The first post-fix validation directory was invalid because it was timeout-contaminated and was deleted. The current directory is a clean rerun and is the only authoritative post-fix validation evidence.

Harness invocation:

```powershell
.\harness\run_match.ps1 `
  -OutDir experiments\2026-08-18-recommended-order\UseCaptureLMR\post-fix-validation `
  -Purpose 'Post-fix validation rerun after invalid timeout-contaminated run: validate UseCaptureLMR=false' `
  -AName capture-lmr-off `
  -ACmd .\baselines\recommended-order-post-fix\manifold.exe `
  -AOptions 'option.UseCaptureLMR=false' `
  -BName capture-lmr-on `
  -BCmd .\baselines\recommended-order-post-fix\manifold.exe `
  -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 2026081833
```

Resolved fastchess command:

```text
C:\Users\Samaritan\Projects\Manifold\tools\fastchess\fastchess.exe -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order-post-fix\manifold.exe name=capture-lmr-off option.Hash=64 option.Threads=1 option.UseCaptureLMR=false -engine cmd=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\baselines\recommended-order-post-fix\manifold.exe name=capture-lmr-on option.Hash=64 option.Threads=1 -each proto=uci tc=8+0.08 -openings file=C:\Users\Samaritan\Projects\Manifold\tools\books\UHO_4060_v4.epd format=epd order=random -repeat -games 2 -rounds 150 -concurrency 8 -srand 2026081833 -report penta=true -ratinginterval 50 -pgnout file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\post-fix-validation\games.pgn append=false -log file=C:\Users\Samaritan\AppData\Local\Temp\manifold-execution-order\experiments\2026-08-18-recommended-order\UseCaptureLMR\post-fix-validation\fastchess.log level=warn append=false -use-affinity
```

From A (`UseCaptureLMR=false`) perspective:

- Games: 300
- W/L/D: 81/77/142
- Pentanomial: `[0, 33, 80, 37, 0]`
- Elo: `+4.63 +/- 19.00`

An independent PGN parse reproduced 300 games and W/L/D 81/77/142. The final fastchess summary reproduced the pentanomial and Elo values. The harness and fastchess exited 0, and every recorded integrity guardrail was zero.

For both runs, the zero guardrails cover time forfeits, timeouts, crashes, illegal moves, illegal PV reports, `No output from` events, and adjudications.

## Pooled authoritative result

Combining the two post-fix runs from A (`UseCaptureLMR=false`) perspective:

- Games: 600
- W/L/D: 160/154/286
- Pentanomial: `[2, 69, 153, 73, 3]`
- Score: 303/600 (`50.5%`)
- Elo point estimate: `+3.47`

The pooled point estimate uses fastchess's score convention: `400 * log10(score / (1 - score))`, with `score = (wins + draws / 2) / games`. The unrounded result is `+3.474472` Elo.

## Decision

Flip the default to `UseCaptureLMR=false`. The policy conditions all pass for the alternative-off configuration: primary `+2.32 > 0`, validation `+4.63 >= 0`, and pooled `+3.47 > 0`.

## Applied default

Task 13 applied only this evidence-backed flip:

- `SearchOptions::default().use_capture_lmr`: `true` -> `false`
- UCI `UseCaptureLMR` advertised default: `true` -> `false`
- Shipped deterministic bench signature: `35,859` -> `37,420` nodes

`UseTtMoveHistory=false` and `UseCorrplexity=false` remain unchanged. The capture-LMR
heuristic and all numeric parameters remain unchanged.

## Concern

The confidence intervals for the individual runs include zero, so the point estimates are not statistically decisive in isolation. The specified decision policy is point-estimate based and is satisfied. The only provenance caveat is the separately disclosed hard-coded checkout commit in raw metadata; binary identity was independently verified by source commit record, SHA-256, and two matching bench node counts.
