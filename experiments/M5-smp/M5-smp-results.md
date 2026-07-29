# M5 Lazy SMP Validation Results

## Status

**BLOCKED.** Commit `d866f768fe8c3f02ce98d96626378779566f8dcb` passed
correctness, deterministic bench, and throughput-scaling checks, but failed the
fixed-node 8-thread-versus-1-thread equivalence gate by a large margin.

The approved LTC match was not run and no `baselines/M5` binary was archived.
Running an hours-long time-control measurement or publishing a release baseline
after this gate failure would treat a known selectivity defect as validated.

## Correctness and Build Checks

| Check | Result |
|---|---:|
| `cargo fmt --all -- --check` | Pass |
| Workspace Clippy, all targets, warnings denied | Pass |
| Optimized `cargo test --workspace` | Pass |
| Test profile retained `debug-assertions=on` | Confirmed |
| `mf-core` tests with `force-magic` | Pass |
| Release build | Pass |
| Release bench run 1 | `175944` nodes |
| Release bench run 2 | `175944` nodes |

The complete workspace suite used `CARGO_PROFILE_TEST_OPT_LEVEL=3` to avoid the
two known debug-duration failures. Cargo reported an optimized test profile, and
a verbose proof build showed `-C opt-level=3 -C debug-assertions=on`.

## Throughput Scaling

Command:

```powershell
& 'C:\Users\Samaritan\Projects\Manifold\target\release\manifold.exe' mtbench --threads 1,2,4,8 --depth 10
```

| Threads | Depth | Nodes | Time (ms) | NPS | Speedup | Efficiency |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 10 | 1,989,904 | 2,400 | 829,039 | 1.0000x | 100.00% |
| 2 | 10 | 3,800,223 | 2,440 | 1,557,302 | 1.8784x | 93.92% |
| 4 | 10 | 7,363,237 | 2,754 | 2,673,428 | 3.2247x | 80.62% |
| 8 | 10 | 8,638,797 | 1,676 | 5,153,062 | 6.2157x | 77.70% |

The 8-thread efficiency was **77.70%**, comfortably above the 60% stop
threshold. Aspiration jitter was therefore not tuned.

## Fixed-Node Equivalence

The bounded gate used 100 paired-book games, equal **100,000 aggregate nodes per
move**, `Hash=64`, one-game concurrency, affinity, and seed `20260729`.

Result from the 8-thread engine's perspective:

| Metric | Result |
|---|---:|
| Games | 100 |
| Wins / losses / draws | 10 / 81 / 9 |
| Score | 14.5% |
| Elo | **-308.24 ± 116.63** |
| Approximate 95% Elo interval | **[-424.87, -191.61]** |
| nElo | -317.98 ± 68.10 |
| LOS | 0.00% |
| Pentanomial | `[35, 8, 3, 1, 3]` |
| Crashes / timeouts / disconnects / illegal moves | 0 / 0 / 0 / 0 |

This is not sampling noise around equivalence: even the upper end of the reported
95% interval is roughly -192 Elo. It is a blocking multi-thread
selectivity/correctness defect despite the strong raw NPS scaling.

Fastchess completed all 100 games and emitted its final report. Its process exit
code was `1` after the completed non-SPRT match; the PGN contains all 100 games,
and neither the log nor PGN contains a crash, timeout, disconnect, illegal-move,
or incomplete-game indication.

## LTC Measurement

**Not run.** The fixed-node gate is the explicit blocking defect permitted by the
approved validation instructions. The exact approved post-fix LTC command is
preserved in `run-metadata.txt`.

## Release Decision

Do not publish this binary as the M5 baseline. Diagnose and fix the equal-node
8-thread regression, rerun the 100-game fixed-node gate, then run the approved
200-game `60+0.6` LTC measurement before creating `baselines/M5`.

## Artifacts

- `run-metadata.txt` — exact commands, paths, hashes, host/toolchain, and results.
- `fixed-node-console.txt` — complete fastchess console report.
- `fixed-node-games.pgn` — all 100 fixed-node games.
- `fixed-node.log` — fastchess run log.
