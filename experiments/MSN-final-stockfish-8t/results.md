# M4-F2 — Stockfish 18 benchmark, final build at Threads=8 (A-SF-001 addendum)

**Result: `mission-final` vs Stockfish 18, both at Threads=8 = −210.72 ± 20.65 Elo,
22.92 % (27.5 / 120), zero forfeits, zero crashes.**

Against the 1T match on the **same 60 openings** (17.92 %), running 8 threads is worth
**+5.00 pp ± 3.25 pp (95 %)** of score against a Stockfish 18 that also went from 1 to 8
threads. Manifold's Lazy SMP therefore scales at least as well as Stockfish's over this
1→8 step at 8+0.08 — a mild but statistically resolvable result, and the first time this
mission has measured SMP against an external opponent rather than against itself.

---

## 1. Conditions

| Parameter | Value | why |
|---|---|---|
| Driver | `harness/run_match.ps1` (exit 0) | mandatory |
| Engine A | `mission-final` → `baselines\mission-final\manifold.exe`, bench 41,588 | the shipped build |
| SHA-256 A | `4BC94E99A512E5DEEFE0AA057B7C839AE2D3E4A5557C03AA0256F64A813F5516` | |
| Engine B | `stockfish` → `C:\Users\Samaritan\bin\stockfish.exe` (Stockfish 18) | |
| SHA-256 B | `C86215FA1977D53B82ED854540A4C7B025BE4CD042276C85BA3DE53FB9118911` | byte-identical to the M1-F3 and 1T opponent |
| **Threads** | **8 on BOTH engines** | the variable |
| **Affinity** | **disabled** | mandatory at Threads>1 — see §2 |
| **Concurrency** | **1** | mandatory at Threads>1 |
| Time control | `8+0.08` | identical to the 1T anchor |
| Hash | 64 MB both | identical to the 1T anchor |
| Book | `UHO_4060_v4.epd`, epd/random, `-repeat -games 2` | identical |
| Seed | `20260805` | identical — see §4, this pairs the openings |
| Rounds | **60 (120 games)** | see §3 |
| Repo commit | `cec5d4354359dfe8021d2f1dd38f5afedd5bfbdb` | |
| Pre-run CPU | 26 % (max of 5 samples; harness self-load, see the 1T doc §5) | |
| Date (UTC) | 2026-08-06T16:46:11Z | |
| **Wall time** | **51 min 31 s** for 120 games | §3 |

### Exact command

```powershell
.\harness\run_match.ps1 `
    -OutDir 'experiments\MSN-final-stockfish-8t' `
    -Purpose 'M4-F2 / A-SF-001 8T addendum ...' `
    -AName 'mission-final' -ACmd '.\baselines\mission-final\manifold.exe' -AThreads 8 `
    -BName 'stockfish' -BCmd 'C:\Users\Samaritan\bin\stockfish.exe' -BThreads 8 `
    -TC '8+0.08' -Hash 64 -Rounds 60 -Seed 20260805
```

Preserved verbatim as `launch.ps1`.

---

## 2. The multi-thread harness rules, confirmed by the harness itself

The driver derives affinity and concurrency from the thread counts and refuses to be
overridden. Its own metadata records what it chose and why:

```
Affinity:    disabled   Concurrency: 1   Threads: A=8 B=8   Hash: 64
Guardrail:   an engine runs Threads>1 (A=8 B=8); AGENTS.md 4.451 forbids -use-affinity here
```

This is the rule that cost a ~600 Elo artifact and 69 forfeits in 140 games when it was
violated: `-use-affinity` pins each engine process into a CPU subset too small for 8 threads,
worker 0 gets descheduled, and the engine forfeits on time. **This run has zero forfeits on
both sides**, which is the positive control that the configuration is right.

The second rule — never compare thread counts at a fixed *node* budget — is also respected:
§4's 1T-vs-8T comparison is at **fixed time** (both matches ran 8+0.08), never fixed nodes.

---

## 3. Why 120 games and not 300

At Threads=8 the harness must run `-concurrency 1`, so exactly one game is in flight and it
owns all 8 P-cores. Throughput collapses relative to a 1T match, which plays 8 games at once:

| match | games | wall time | games/hour |
|---|---|---|---|
| 1T vs Stockfish (`MSN-final-stockfish`) | 300 | 15 min 51 s | **1,136** |
| **8T vs Stockfish (this)** | **120** | **51 min 31 s** | **140** |

A 300-game 8T match would have taken **~2 h 09 m** of exclusive machine time. The feature
specified 100–150 games with a sensible wall-time cap and instructed that the count be
documented; 60 rounds = 120 games sits mid-range and cost 51 minutes. It is also an even
number of *rounds*, which keeps the pentanomial pairing intact — a 125-game run would not.

The cost is resolution: ±20.65 Elo here versus ±22.57 over 300 games at 1T. That the error
bars are comparable despite 2.5× fewer games is not luck — it is the draw rate. This match
drew 53 of 60 pairs, and pentanomial error scales with pair *variance*, which a wall of
loss+draw pairs makes small.

---

## 4. Result

```
Results of mission-final vs stockfish (8+0.08, 8t, 64MB, UHO_4060_v4.epd):
Elo: -210.72 +/- 20.65, nElo: -803.48 +/- 62.16
LOS: 0.00 %, DrawRatio: 1.67 %, PairsRatio: 0.00
Games: 120, Wins: 1, Losses: 66, Draws: 53, Points: 27.5 (22.92 %)
Ptnml(0-2): [6, 53, 1, 0, 0], WL/DD Ratio: inf
```

Rating-interval progression: after 100 games −205.04 ± 20.84 (23.50 %); after 120,
**−210.72 ± 20.65 (22.92 %)**.

### Admissibility

```
affinity: disabled   concurrency: 1   threads: A=8 B=8
  mission-final time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  stockfish     time forfeits (PGN): 0  console Timeouts: 0  Crashed: 0  illegal MOVES played: 0  illegal PV reports: 0  'No output from': 0
  adjudications: 0
```

`run_match.ps1` exit **0**, fastchess exit **0**. **Zero manifold forfeits** — the feature's
hard requirement, and specifically meaningful here because forfeits are the failure mode of
a misconfigured multi-thread match. Admissible.

### The openings are paired with both 1T matches — verified

Seed `20260805` and the same book were used for M1-F3, the M4 1T match, and this match. A
game-by-game FEN comparison confirms the 60 rounds played here are the **first 60 rounds of
the 150 played at 1T, opening-for-opening**:

```
8T rounds: 60   common with the 1T final match: 60   identical FEN: 60/60
                common with M1-F3 (mission-start): 60   identical FEN: 60/60
```

So the thread-count comparison can be made *paired*, at fixed time, on identical openings
against an identical opponent binary:

| comparison (same 60 openings, TC 8+0.08) | score | paired difference |
|---|---|---|
| mission-final **1T** | 21.5 / 120 = **17.92 %** | — |
| mission-final **8T** | 27.5 / 120 = **22.92 %** | **+5.00 pp ± 3.25 pp (95 %)** |
| pairs where 8T scored more / less / equal | 15 / 3 / 42 | sign test p ≈ 0.008 |
| mission-**start** 1T (M1-F3) on the same 60 | 12.5 / 120 = 10.42 % | 8T-final vs start-1T: **+9.17 pp ± 3.68 pp** |

**What this does and does not mean.** Both engines gained 8 threads, so this is *relative*
SMP quality, not absolute scaling: +5.00 pp says Manifold's Lazy SMP converts the extra
7 cores into playing strength **slightly better than Stockfish 18 converts its own** over
this particular 1→8 step, at this TC, on these 60 openings. It does not say Manifold's SMP
is better in general — a 60-pair sample at one time control and one thread count is thin
evidence for so broad a claim, and at 8 seconds Stockfish is already deep enough that its
returns from threads are diminishing faster than Manifold's.

The absolute scaling number remains the M4-F1 `mtbench` measurement: 1T 560,920 → 8T
3,765,267 NPS = **6.71× speedup, 83.9 % efficiency**.

### Pentanomial

`[6, 53, 1, 0, 0]`. Six double-losses, fifty-three loss+draw pairs, one pair reaching a
point, none above. Compare the 1T final match's `[44, 104, 2, 0, 0]` scaled to 60 pairs
(≈ `[17.6, 41.6, 0.8]`): at 8T the double-loss column collapses from ~18 to **6**, and the
loss+draw column swells to 53. `PairsRatio 0.00` again — **Manifold has still never won an
opening pair against Stockfish 18** in 720 games this mission, at either thread count.

The 8T gain is once more entirely *survival*: 53 draws in 120 games (44.2 %, versus 34.7 %
at 1T), and wins going 2 → 1. Extra depth is buying Manifold the ability to hold positions
it used to lose, not the ability to win them.

---

## 5. A-SF-001 addendum status

> (Final additionally includes an 8T-vs-8T Stockfish match under multi-thread rules: no
> affinity, concurrency 1.)

8T on both engines ✓ · no affinity, concurrency 1, enforced and recorded by the harness ✓ ·
same TC/Hash/book as the 1T anchor ✓ · fixed time, never fixed nodes ✓ · zero forfeits ✓ ·
game count documented with its wall-time rationale ✓. **Satisfied.**

---

## 6. Files

| file | contents |
|---|---|
| `run-metadata.txt` | harness provenance + self-check, including the guardrail's own statement of why affinity is disabled |
| `console.txt` | full fastchess output incl. rating intervals and the self-check |
| `fastchess.log` | fastchess log at `level=warn` (clean) |
| `games.pgn` | all 120 games (untracked by repo convention; seed + command reproduce it) |
| `launch.ps1` | the exact launcher used |
| `driver-stdout.txt` / `driver-stderr.txt` | driver stdout/stderr (stderr empty) |
| `results.md` | this document |

The paired-openings analysis in §4 is produced by
`experiments/MSN-final-stockfish/paired_analysis.py` (generalised over the three PGNs).
