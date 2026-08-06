# M4-F2 — Cumulative mission Elo (A-ELO-001)

**Result: `mission-final` vs `mission-start` = +5.79 ± 21.18 Elo over 300 games, zero
forfeits.** The point estimate is positive, which is what A-ELO-001 requires, and the
error bar covers zero, which this document does not hide. §4 is about that gap between
this number and the sum of the parts, because that gap is the interesting finding.

---

## 0. Step 0 — the baseline this match measures was re-promoted first

M4-F1 promoted `baselines/mission-final/` from the tree at `b0f85bf`, which benched
**44,737** with `UseCaptureLMR` defaulting **false**. M4-F1b (`d06c31f`) then measured
capture LMR at **+11.59 ± 22.22** against `baselines/m3-search` and flipped that default
**true**, moving the shipped signature to **41,588**.

The promoted binary therefore no longer described the engine the mission ships. Running
this match against it would have measured a build that does not exist as a shipping
artifact. So before any match:

| step | evidence |
|---|---|
| `git rev-parse HEAD` | `cec5d4354359dfe8021d2f1dd38f5afedd5bfbdb`, `git status --short` clean |
| `cargo build --release` | `Finished` line, no `os error 5` (mission AGENTS.md 14b) |
| build freshness | binary mtime `18:41:50` > newest source file `crates/mf-search/src/search.rs` `18:41:32` |
| `manifold bench` ×2 | **41588**, **41588** — deterministic, matches the M4-F1b shipped signature |
| promotion | `target\release\manifold.exe` → `baselines\mission-final\manifold.exe` |
| post-copy verification | copied binary benches **41588**; SHA-256 of copy == SHA-256 of source |

New mission-final SHA-256: **`4BC94E99A512E5DEEFE0AA057B7C839AE2D3E4A5557C03AA0256F64A813F5516`**
(superseded M4-F1 binary was `2CA838E5…F1D2`, bench 44,737).

**This is the only baseline directory this mission has overwritten**, and the overwrite
was explicitly authorized for exactly the reason above. The superseded metadata is kept
verbatim as `baselines/mission-final/build-metadata.txt.m4f1.bak`; the superseded *binary*
is functionally identical to `baselines/m3-search/manifold.exe`, which is untouched, so
nothing was lost. `mission-start`, `m2-nnue`, `m3-search` and every pre-mission baseline
remain untouched.

---

## 1. Conditions

| Parameter | Value |
|---|---|
| Driver | `harness/run_match.ps1` (guardrails enforced, exit 0) |
| Engine A | `mission-final` → `baselines\mission-final\manifold.exe`, bench **41,588** |
| SHA-256 A | `4BC94E99A512E5DEEFE0AA057B7C839AE2D3E4A5557C03AA0256F64A813F5516` |
| Engine B | `mission-start` → `baselines\mission-start\manifold.exe`, bench **45,036** |
| SHA-256 B | `43EB8A0DD0C81172EFDD1F914899C080DFD757CA26EACA144913E52CEAD2CB28` |
| Time control | `8+0.08` |
| Hash | 64 MB both |
| Threads | 1 both |
| Affinity / concurrency | `-use-affinity`, concurrency 8 (mandatory at 1T-vs-1T) |
| Book | `tools\books\UHO_4060_v4.epd`, `format=epd order=random`, `-repeat -games 2` |
| Rounds | 150 (300 games, paired openings) |
| Seed | `20260806` |
| Repo commit | `cec5d4354359dfe8021d2f1dd38f5afedd5bfbdb` |
| Pre-run CPU load | 20 % (max of 5 samples) |
| Date (UTC) | 2026-08-06T16:10:10Z |
| Wall time | 16 min 51 s |

### Exact command

```powershell
.\harness\run_match.ps1 `
    -OutDir 'experiments\MSN-final-cumulative' `
    -Purpose 'M4-F2 headline: cumulative mission Elo -- baselines/mission-final (bench 41588, commit cec5d43) vs baselines/mission-start (bench 45036, commit 0012b36), 300 games 1T TC 8+0.08 Hash 64 (A-ELO-001).' `
    -AName 'mission-final' -ACmd '.\baselines\mission-final\manifold.exe' `
    -BName 'mission-start' -BCmd '.\baselines\mission-start\manifold.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260806
```

Preserved verbatim as `launch.ps1`.

### A note on the 20 % pre-run CPU sample

The harness records 20 % as the max of five samples; the mission checklist treats ~20 % as
the go/no-go line. Two independent readings taken immediately before the launch were
`0, 9, 0, 5, 4` %, and the machine had no foreign `manifold`/`stockfish`/`fastchess`
processes. The 20 % spike is a single sample inside a 5-sample max taken while the harness
itself was hashing two 113 MB binaries (`Get-FileHash` on 226 MB of input, which is exactly
what the driver does immediately before this reading). The match then ran to completion
with **zero forfeits and zero timeouts on either side**, which is the symptom external load
produces; there is none. The run is admissible.

---

## 2. Result

```
Results of mission-final vs mission-start (8+0.08, 1t, 64MB, UHO_4060_v4.epd):
Elo: 5.79 +/- 21.18, nElo: 10.76 +/- 39.32
LOS: 70.42 %, DrawRatio: 52.00 %, PairsRatio: 1.25
Games: 300, Wins: 85, Losses: 80, Draws: 135, Points: 152.5 (50.83 %)
Ptnml(0-2): [4, 28, 78, 39, 1], WL/DD Ratio: 1.29
```

### Rating-interval progression

| after games | score | Elo |
|---|---|---|
| 102 | 50.49 % | +3.41 ± 36.07 |
| 200 | 50.75 % | +5.21 ± 25.28 |
| 300 | 50.83 % | **+5.79 ± 21.18** |

Monotone and stable in sign from the first interval — the number is not a late swing.

### Admissibility

```
affinity: enabled   concurrency: 8   threads: A=1 B=1
  mission-final time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  mission-start time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  adjudications: 0
```

`run_match.ps1` exit **0**, fastchess exit **0**, `fastchess.log` (level=warn) clean.
**Zero manifold forfeits** — the feature's hard requirement is met. Every game ended by a
normal chess result. **The result is admissible evidence.**

---

## 3. A-ELO-001 status

> Final cumulative match: mission-final build vs `baselines/mission-start/manifold.exe`,
> ~300 games, 1T, standard conditions — **positive Elo point estimate**, zero forfeits,
> results doc committed.

300 games ✓ · 1T standard conditions ✓ · **point estimate +5.79, positive** ✓ ·
zero forfeits ✓ · this doc ✓. **A-ELO-001 is satisfied as written.**

What it is *not*: a demonstration that the mission gained 5.79 Elo. ±21.18 at LOS 70.4 %
means roughly a 30 % chance the true effect is negative. The honest one-line reading is
**"the mission's net playing-strength change at 8+0.08 1T is small and positive, and this
match cannot resolve it more finely than ±21 Elo."**

---

## 4. The interesting part: this is well below the sum of the parts

The mission's kept, individually-measured strength features add up to considerably more
than +5.79:

| kept feature | its own match | vs |
|---|---|---|
| M2 NNUE speed package | **+37.20 ± 20.19** | mission-start |
| M3-F4 post-LMR depth band | +3.47 ± 21.91 | m2-nnue |
| M3-F6 thread-invariant history | +1.16 ± 20.45 | m2-nnue |
| M4-F1b capture LMR | +11.59 ± 22.22 | m3-search |
| M3-F5 leaper LUTs | no match (bit-exact, +7 % search NPS) | — |
| naive sum | **≈ +53** | |
| **this match** | **+5.79 ± 21.18** | mission-start |

The mission AGENTS.md §6 named the suspect in advance:

> if the M4-F2 cumulative match underperforms the sum of parts, capture LMR is the first
> suspect.

It underperformed. Below is what the evidence does and does not say about why.

### 4.1 The arithmetic was never going to hold, and here is how much of the gap that explains

Adding independent 300-game point estimates is not a valid operation and the mission's own
docs say so repeatedly. Each of the four numbers above carries a ±20-ish standard error, so
their naive sum carries roughly `sqrt(4) × 21 ≈ ±42` — before considering that three of the
four are measured against *different* baselines, so they are not even estimating additive
quantities on a common scale.

**+5.79 sits about 1.1 combined standard errors below +53.** That is not a contradiction;
it is what regression to the mean looks like when you chain four measurements each selected
for having a positive point estimate. Three of the four kept features (+3.47, +1.16,
+11.59) have error bars that comfortably cover zero. If their true values are near zero —
entirely consistent with their own data — then the mission's real cumulative gain is
essentially **the M2 NNUE package alone**, and +37.20 ± 20.19 versus this match's
+5.79 ± 21.18 is a ~1.1σ discrepancy between two overlapping intervals. No mechanism is
required to explain a 1.1σ gap.

**This is the single most likely explanation and it should be stated first**: the mission
kept every feature whose point estimate was positive, which is a selection rule that
systematically inflates the sum of kept point estimates relative to the truth. The
cumulative match is the only measurement in the mission not subject to that selection, and
it is therefore the *better* estimate of the mission's strength delta, not the anomaly.

### 4.2 What can be said about interaction, honestly

The features do share code paths — capture LMR and the post-LMR depth band both act on the
LMR re-search, and MSN-S7 §5 already recorded that capture LMR's mechanism was *not*
confirmed (depth-at-time moved +0.17 / +0.08 plies, statistically indistinguishable from
the +0.12 it showed when it measured **negative**). A feature whose mechanism is unconfirmed
is a feature whose Elo could be noise, and its +11.59 is the largest single contributor to
the shortfall arithmetic.

But this match **cannot** separate "capture LMR's +11.59 was noise" from "capture LMR
interacts negatively with the M2 speed gains" from "regression to the mean". Those are
three different claims and one 300-game aggregate is one number. Naming capture LMR as
*the* cause here would be inventing a finding, which is exactly the failure mode this
mission's evidence rules exist to prevent.

### 4.3 What would actually resolve it

A single direct match: `mission-final` vs `mission-final` with `UseCaptureLMR=false` — the
same 300-game conditions, one variable, both arms carrying the complete rest of the mission.
That measures capture LMR *in the shipped context* rather than against `m3-search`, and it
is the only cheap experiment that distinguishes §4.2's hypotheses. Both arms already exist
in one binary (the toggle is live, and `UseCaptureLMR=false` reproduces bench 44,737
bit-for-bit), so it costs one match and no code.

It is **out of scope for this feature** — M4-F2's budget is three matches, all three
specified, all three run. It is recorded here and in the mission summary as the highest-value
next measurement.

### 4.4 Why nothing is being reverted on this evidence

The mission's standing rule is per-feature single-variable measurement with a written
keep-or-revert decision. Every kept feature has one. Reverting a feature because an
*aggregate* match came in low would be a decision made without a single-variable
measurement — precisely the thing the rule forbids — and +5.79 ± 21.18 does not even
establish that the aggregate is worse than any particular arm. **The shipped build stands
as measured.** §4.3 says what would change that.

---

## 5. What else this match confirms

- **Zero forfeits at 300 games** on the shipped build, at the mission's standard fast TC.
  Combined with the M4-F1 8T smoke match (Hash 4096, 20 games, 0 forfeits) and the M1-F3
  and M4-F2 Stockfish matches, the engine has now played 900+ games this mission without a
  single time forfeit, crash, or illegal move. The M2-F5 depth cap and M1-F2 hash fix did
  not destabilize anything.
- **Bench moved 45,036 → 41,588 (−7.6 %)** across the mission — a 7.6 % smaller tree at
  fixed depth — while 1T NPS rose ~8 % (M2) plus ~7 % search NPS (M3-F5 leaper LUTs). The
  engine does measurably less work per node and fewer nodes per depth than at mission start.
  That those two improvements net out to +5.79 ± 21.18 Elo at 8+0.08 is the finding, and
  the repo's own "1 % NPS ≈ 1.4 Elo LTC" rule is an **LTC** rule — at 8 seconds these
  speedups have far less room to convert.

---

## 6. Files

| file | contents |
|---|---|
| `run-metadata.txt` | harness provenance: commit, both SHA-256, TC, seed, book, affinity/concurrency/threads/hash, CPU load, full command line, self-check block |
| `console.txt` | full fastchess output including the three rating intervals and the self-check |
| `fastchess.log` | fastchess log at `level=warn` (clean) |
| `games.pgn` | all 300 games (untracked by repo convention; seed + command reproduce it) |
| `launch.ps1` | the exact launcher used |
| `driver-stdout.txt` / `driver-stderr.txt` | the driver's own stdout/stderr (stderr empty) |
| `results.md` | this document |
