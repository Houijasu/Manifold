# M5 Lazy SMP Validation Results — CORRECTED

> **This document supersedes, in place, a prior verdict of `BLOCKED` that was based on
> two separate broken measurements.** The raw artifacts of the superseded run are
> retained unchanged alongside this file (`fixed-node-console.txt`,
> `fixed-node-games.pgn`, `fixed-node.log`, `run-metadata.txt`). Nothing was deleted;
> only the *conclusion* changed. Corrected by M6-F1 on 2026-07-30.

## Status

**PASS.** Lazy SMP works. Commit `d866f76` (validated) and current HEAD `9fd3035` pass
correctness, deterministic bench, throughput scaling, the `Threads`-independence
invariant, and an equal-**time** 8T-vs-1T strength measurement.

The original `BLOCKED` verdict rested on an 8T-vs-1T match at an equal **aggregate**
node budget that returned **−308.24 ± 116.63 Elo**. That gate is invalid by
construction, and the equal-time replacement that was run to replace it was *also*
invalid, for a completely different reason. Both are documented below with their
single-variable controls. Neither gate may be re-run in its broken form.

---

## Correction 1 — the equal-AGGREGATE-node gate measures budget division, not strength

Lazy SMP threads search *overlapping* trees. Under an aggregate node budget, N threads
each receive roughly 1/N of the useful tree, so the search is shallower by construction.
A large negative result at equal aggregate nodes is the **expected** behaviour of a
*working* implementation.

The decisive control is Stockfish. Position `startpos moves e2e4 e7e5 g1f3`,
`go nodes 100000`, `Hash=64`:

| engine | 1 thread | 8 threads | plies lost |
|---|---:|---:|---:|
| Manifold | depth 9, 67,210 nodes | depth 6, 26,463 nodes | **3** |
| **Stockfish 18** | **depth 15, 100,010 nodes** | **depth 10, 100,415 nodes** | **5** |

**Stockfish loses more plies on this gate than Manifold does.** If the gate condemned
Manifold it would condemn Stockfish harder. Manifold's 8T-at-100k result (depth 6) is
exactly its own 1T-at-12,500 result — i.e. 100,000/8. The gate measured division of the
node budget by the thread count and nothing else.

The valid comparison is **equal time**. At `go movetime 3000`, `Hash=64`:

| position | 1T depth | 8T depth |
|---|---:|---:|
| `startpos moves e2e4 e7e5 g1f3` | 17 | 17 |
| `r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq -` | 15 | **16** |
| `2rq1rk1/pb2bppp/np2pn2/3p4/3P4/1P2PN2/PB1NBPPP/R2Q1RK1 w - -` | 15 | **17** |
| `8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - -` (Lasker–Reichhelm) | 19 | **23** |

8 threads is equal or deeper on every position tried.

**Rule: never compare different thread counts at a fixed node budget.** Fixed-node
comparisons stay valid only between builds at the *same* thread count — that is what
the deterministic `bench` signature is for.

## Correction 2 — `-use-affinity` invalidates any match where an engine has `Threads>1`

The obvious replacement gate, an equal-**time** 8T-vs-1T match at `tc=10+0.1`,
`Hash=128`, 140 paired games, `-concurrency 1 -use-affinity`, seed `20260801`, returned
**−301.33 ± 78.79 Elo** with **69 of 140 games decided by "loses on time"** plus 14
`Warning; No output from MF-8T`. Per-player forfeit attribution from
`experiments/M6-smp-diag/equal-time-console.txt`:

| player | forfeits as White | forfeits as Black | total |
|---|---:|---:|---:|
| MF-8T | 35 | 34 | **69** |
| MF-1T | 0 | 0 | **0** |

Every single forfeit charged to the 8-thread side. That looks exactly like a hung
multi-thread time manager. **It is not. It is the harness.**

Single-variable control — identical seed `20260802`, identical 20 games, identical
everything except one flag:

| run | flag | Elo | forfeits |
|---|---|---:|---:|
| `experiments/M6-smp-diag/noaff-console.txt` | *(none)* | **+214.85 ± 167.75** | **0** |
| `experiments/M6-smp-diag/aff-control-console.txt` | `-use-affinity` | **−381.70** | 0 |

A roughly 600 Elo swing from one flag. `-use-affinity` pins each engine *process* into a
CPU subset; an engine asked for `Threads=8` inside a subset that cannot hold 8 threads
oversubscribes, its clock-owning worker 0 is descheduled, and it blows the time budget.

Isolated timing never revealed this and cannot be used to chase it: single
`go wtime 10000` calls returned in ~530 ms at both 1 and 8 threads, 25 sequential
searches in one live session showed a worst move time of 534 ms at 8 threads versus
538 ms at 1 thread, and low-clock probes down to `wtime 50` returned in 89–91 ms at both
thread counts. The forfeits exist only under `-use-affinity`.

**The mission rule "`-use-affinity` mandatory, `-concurrency 8`" is still correct — for
single-threaded matches, which is every M1–M5 SPRT.** It does not generalize:

- **Both engines `Threads=1`** → `-use-affinity -concurrency 8`. Mandatory; unpinned is invalid.
- **Any engine `Threads>1`** → **no `-use-affinity`**, and `-concurrency 1`. Pinning is invalid.

---

## The definitive equal-TIME measurement (M6-F1)

Run without `-use-affinity` and with `-concurrency 1`, per the rule above.

Command (`experiments/M6-F1-smp/run-equal-time.ps1`):

```
fastchess.exe
  -engine cmd=target\release\manifold.exe name=MF-8T option.Hash=128 option.Threads=8
  -engine cmd=target\release\manifold.exe name=MF-1T option.Hash=128 option.Threads=1
  -each proto=uci tc=10+0.1
  -openings file=tools\books\UHO_Lichess_4852_v1.epd format=epd order=random
  -games 2 -rounds 150 -repeat
  -concurrency 1
  -srand 20260803
  -report penta=true -ratinginterval 20
```

Result, from the 8-thread engine's perspective (`experiments/M6-F1-smp/equal-time-noaff-console.txt`):

| Metric | Result |
|---|---:|
| Games | 300 (150 paired rounds) |
| Wins / losses / draws | 213 / 18 / 69 |
| Points | 247.5 / 300 (82.50%) |
| **Elo** | **+269.37 ± 41.19** |
| nElo | +377.26 ± 39.32 |
| LOS | 100.00% |
| Pentanomial `Ptnml(0-2)` | `[2, 3, 17, 54, 74]` |
| Pairs ratio | 25.60 |
| Wall time | 02:28:26 |
| **Time forfeits — MF-8T** | **0** |
| **Time forfeits — MF-1T** | **0** |
| Crashes / disconnects / illegal moves / `No output` warnings | 0 / 0 / 0 / 0 |
| fastchess exit code | 0 |

Game terminations: 116 "White mates", 115 "Black mates", 36 threefold, 20 insufficient
material, 11 fifty-move, 2 stalemate. Every game ended in a normal chess result; the
`equal-time-noaff.log` warn-level log is empty apart from its header.

**8 threads measures clearly positive at equal time, with zero forfeits.** For scale,
Stockfish measures roughly +178.6 ± 14.0 Elo at 8 threads; a young engine gains more
from depth because its search is shallower, so a larger number here is expected rather
than suspicious. The interval excludes parity by more than six sigma.

The three independent measurements of the same engine now line up as follows, and the
only thing that separates them is the measurement method:

| gate | flags | Elo for 8T | forfeits (8T) | verdict |
|---|---|---:|---:|---|
| equal aggregate nodes, 100 games | `-use-affinity`, `-concurrency 1` | −308.24 ± 116.63 | 0 | **invalid gate** (Correction 1) |
| equal time, 140 games | `-use-affinity`, `-concurrency 1` | −301.33 ± 78.79 | 69 | **invalid harness** (Correction 2) |
| equal time, 20 games | none, `-concurrency 1` | +214.85 ± 167.75 | 0 | valid pilot |
| **equal time, 300 games** | **none, `-concurrency 1`** | **+269.37 ± 41.19** | **0** | **definitive** |

## Correctness and build checks

| Check | Result |
|---|---:|
| `cargo fmt --all -- --check` | Pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass |
| `cargo test --workspace --release` | Pass |
| `mf-core` tests with `force-magic` | Pass (M5 validator) |
| Release build | Pass |
| Release bench, 3 consecutive runs | `175944` / `175944` / `175944` |

`175944` matches the `BENCH_NODE_COUNT` anchor at `crates/mf-uci/tests/bench_cli.rs:6`
and the per-commit table in the mission `AGENTS.md` for commit `a97c994`.

## The `Threads`-independence invariant

The design's core invariant is that public UCI `go depth N` dispatches worker 0 only, so
fixed-depth output is unperturbed by the `Threads` setting. Measured on
`position startpos moves e2e4 e7e5 g1f3`, `go depth 12`, `Hash=64`:

| Threads | depth-12 nodes | seldepth | score | bestmove |
|---:|---:|---:|---:|---|
| 1 | 314,248 | 29 | cp -15 | `b8c6` |
| 2 | 314,248 | 29 | cp -15 | `b8c6` |
| 8 | 314,248 | 29 | cp -15 | `b8c6` |

Every `info` line at every depth is byte-identical across the three runs apart from the
wall-clock `time` and `nps` fields. This is now locked down by the regression test
`fixed_depth_output_is_identical_at_every_thread_count` in
`crates/mf-uci/tests/uci_protocol.rs`, which compares canonicalized `info` lines and
`bestmove` at Threads 1, 2, and 8.

## Throughput scaling (mtbench, re-run at M6-F1)

```
target\release\manifold.exe mtbench --threads 1,2,4,8 --depth 10
target\release\manifold.exe mtbench --threads 1,2,4,8 --depth 11
```

`speedup(T) = NPS(T)/NPS(1)`, `efficiency(T) = speedup(T)/T`.

| Depth | Run | 1T NPS | 2T eff. | 4T eff. | 8T eff. |
|---:|---:|---:|---:|---:|---:|
| 10 | M5 validator | 829,039 | 93.92% | 80.62% | **77.70%** |
| 10 | M6-F1 | 810,695 | 93.90% | 76.53% | **64.94%** |
| 11 | M6-F1 run 1 | 794,577 | 94.68% | 89.90% | **85.45%** |
| 11 | M6-F1 run 2 | 778,968 | 90.34% | 83.20% | **82.15%** |

8-thread efficiency stayed above the 60% floor on every run. The depth-10 M6-F1 figure
is the weakest of the four; depth 10 completes in about 2 seconds at 8 threads, so
ramp-up dominates and background machine load is visible. The depth-11 runs, which give
the pool more time to amortize, land at 82–85%.

## Release decision

Lazy SMP is validated. The previous instruction to "diagnose and fix the equal-node
8-thread regression" is **withdrawn**: there is no such regression, only a broken gate.

## Artifacts

Superseded run (retained unchanged, conclusions void, raw data valid):

- `run-metadata.txt` — exact commands, paths, hashes, host/toolchain of the original run.
- `fixed-node-console.txt`, `fixed-node-games.pgn`, `fixed-node.log` — the equal-aggregate-node match.

Diagnostic controls:

- `../M6-smp-diag/equal-time-console.txt`, `equal-time-games.pgn`, `equal-time.log` — the invalid pinned equal-time match.
- `../M6-smp-diag/noaff-console.txt`, `noaff-games.pgn` — unpinned 20-game control, +214.85 Elo, zero forfeits.
- `../M6-smp-diag/aff-control-console.txt` — pinned 20-game control, −381.70 Elo, same seed.
- `../M6-smp-diag/uci_probe.ps1` — interactive UCI probe helper that holds stdin open.

Definitive measurement:

- `../M6-F1-smp/run-equal-time.ps1`, `equal-time-noaff-console.txt`, `equal-time-noaff-games.pgn`, `equal-time-noaff.log`, `run-metadata.txt`.
