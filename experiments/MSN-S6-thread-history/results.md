# MSN-S6 — Thread-invariant history table sizing (M3-F6)

**Decision: KEPT.** This is a determinism fix, not an Elo feature. It ships unconditionally
(no toggle) because a toggle would preserve the defect in one of its positions.

**Result: +1.16 ± 20.45 Elo** at 1 thread over 300 games vs `baselines/m2-nnue`, zero
forfeits, and **every one of the 34 `bench_cli.rs` anchors byte-identical to M2**. The
point is not the Elo — the point is that fixed-depth output is now identical at every
thread count, which it was not on any build this mission has produced.

## The defect

`SharedHistory::new(thread_count)` sized pawn history and all four correction tables as
`BASE_BUCKETS * nextPow2(threads)` (`crates/mf-search/src/history.rs`). The bucket
**mask** was therefore a function of `Threads`. A hash collision that happens at 512
pawn buckets and not at 4,096 — or at 16,384 corrhist buckets and not at 131,072 —
changes the residual applied to a static eval, which changes the tree.

This is **deterministic**, not a race: it reproduces identically on repeat runs, with
every helper thread parked, and it is present on the **M2 baseline binary**.

Pre-existing state, reproduced with `experiments/MSN-S1-qchecks/threads_scan.ps1`
(5 positions × depths 8/9/10, Threads=1 vs Threads=8):

| build | verdict |
|---|---|
| `baselines/m2-nnue` (`baseline-m2-scan.txt`) | **DIFFERENT** at kiwipete depth 10; 14/15 identical |
| this branch before the fix (`pre-fix-debug-scan.txt`) | **DIFFERENT** at kiwipete depth 10; 14/15 identical |
| this branch after the fix (`post-fix-debug-scan.txt`) | **IDENTICAL, 15/15** |

The exact divergence the strengthened test caught, at kiwipete depth 10:

```
1T: info depth 10 seldepth 29 ... score cp -201 nodes 107259 hashfull 53 pv e2a6 ...
8T: info depth 10 seldepth 29 ... score cp -201 nodes 107279 hashfull 53 pv e2a6 ...
```

Same score, same PV, 20 nodes apart. Twenty nodes is enough: the coupling is real and it
grows with the tree.

## Why this mattered enough to fix

`A-SEARCH-002` (`fixed_depth_output_is_identical_at_every_thread_count`) was passing only
because the shipped defaults happened to keep the startpos depth-8 anchor under the
collision threshold. Every M3 feature that enlarges the tree could trip it and look like a
regression it did not cause — M3-F1 already hit exactly that (5/15 cells DIFFERENT with
quiet checks on) and had to spend a diagnosis pass proving the defect was inherited.

A test that only passes when the tree stays small is not a guard. It is a tripwire aimed
at the next author.

## The fix

`SharedHistory::new()` now takes **no thread count**. `PAWN_BUCKETS` (512) and
`CORRECTION_BUCKETS` (16,384) are compile-time constants, and the two masks are
`const` derived from them, so the index arithmetic cannot vary with the pool width. This
also removes two per-construction `checked_mul`s and two `u64` fields from the hot
index path.

### The contention tradeoff, stated explicitly

The scaling existed to reduce cross-thread pressure. It does not, and the distinction is
what makes this fix safe:

- **The tables stay SHARED and the atomics are unchanged.** Every access is the same
  single relaxed load / relaxed store it always was. Nothing about how workers contend
  changed in kind.
- **Table size never reduced *contention*, only *collisions*.** Workers contend on the
  entries they both touch, and a bigger table does not stop two workers from updating
  the same bucket for the same position. That sharing is the entire point of the design
  — one worker consumes correction values another paid to search for.
- **What the scaling bought was collision headroom, paid for in cache.** At 8 threads the
  four corrhist tables went 256 KiB → 2 MiB and pawn history 768 KiB → 6 MiB, i.e. well
  out of L2 on this machine. That is the AGENTS.md 4.54 trap that already cost 18% NPS
  once on the pawn table.

So the fix trades headroom the cache could not hold for determinism. Measured below.

## Measurements

### SMP throughput — no regression

`manifold mtbench --threads 1,2,4,8 --depth 13`, one run each
(`mtbench-m2-nnue.txt`, `mtbench-fixed.txt`):

| Threads | m2-nnue NPS | fixed NPS |
|---|---|---|
| 1 | 567,232 | 507,882 |
| 2 | 842,254 | 750,713 |
| 4 | 1,342,636 | 1,364,336 |
| 8 | 2,390,160 | 2,093,100 |

A single mtbench run is scheduling jitter (M3-F3's lesson). Repeated 5× alternating:

**1 thread, depth 13** — nodes are 1,975,225 in every single run on both builds, so this
is a pure speed comparison:

| run | m2-nnue | fixed |
|---|---|---|
| 1 | 530,636 | 525,958 |
| 2 | 512,028 | 508,734 |
| 3 | 513,637 | 519,091 |
| 4 | 540,653 | 521,941 |
| 5 | 542,127 | 530,602 |
| **median** | **530,636** | **521,941** |

-1.6% at the median, inside the ±3% run-to-run spread visible in either column alone.

**8 threads, depth 13** (nondeterministic node counts, as expected for lazy SMP):

| run | m2-nnue NPS | fixed NPS |
|---|---|---|
| 1 | 3,315,486 | 3,453,437 |
| 2 | 3,784,410 | 3,180,782 |
| 3 | 3,285,671 | 3,758,879 |
| 4 | 3,818,663 | 4,061,169 |
| 5 | 3,465,161 | 4,074,266 |
| **median** | **3,465,161** | **3,758,879** |

+8.5% at the median, i.e. the smaller tables are if anything *helping* at 8 threads,
consistent with the cache argument above. Either way there is no throughput regression to
trade against the determinism, which is what this measurement had to rule out.

### Bench signature — unmoved

Two consecutive release `bench` runs: **45,036** both times (deterministic), which is the
**unchanged M2 signature**. The 1T table sizing is preserved exactly, so no anchor needed
re-pinning.

Full 34-anchor vector collected against both binaries with
`experiments/MSN-S1-qchecks/collect_anchors.ps1` (`anchors-m2-nnue.txt`,
`anchors-fixed.txt`). `Compare-Object` over the two files returns **nothing** — every
ablation, history-toggle, correction-variant, pawn-history, LMR-coupling and qsearch-check
anchor is byte-identical to `baselines/m2-nnue`.

That is the strongest available statement that this change is behaviour-neutral at one
thread: it does not merely leave the default bench alone, it leaves all 34 measured
sub-configurations alone.

### Elo — 1 thread, 300 games

```
.\harness\run_match.ps1 -OutDir experiments\MSN-S6-thread-history `
  -Purpose 'M3-F6 thread-invariant history table sizing: 1T single-variable measurement vs previous kept build' `
  -AName thread-invariant-history -ACmd .\target\release\manifold.exe `
  -BName m2-nnue -BCmd .\baselines\m2-nnue\manifold.exe -Rounds 150
```

```
Elo: 1.16 +/- 20.45, nElo: 2.23 +/- 39.32
LOS: 54.42 %, DrawRatio: 52.00 %, PairsRatio: 1.06
Games: 300, Wins: 87, Losses: 86, Draws: 127, Points: 150.5 (50.17 %)
Ptnml(0-2): [2, 33, 78, 36, 1], WL/DD Ratio: 1.69
```

Harness self-check: affinity enabled, concurrency 8, Threads=1 both sides (AGENTS.md
4.451). **Zero** time forfeits, crashes, illegal moves, and adjudications on both engines.
Wall time 16:57.

87 wins against 86 losses is as close to the null result as 300 games can produce, which
is the expected outcome: at one thread the tables are sized exactly as before, so the two
binaries search identical trees. The match exists to confirm the refactor did not
accidentally change 1T behaviour, and it did not.

## Test changes

- `crates/mf-uci/tests/uci_protocol.rs::fixed_depth_output_is_identical_at_every_thread_count`
  now runs **two** cases — the original startpos depth 8, plus **kiwipete depth 10**, the
  cell that reproduces the defect. Verified RED before the fix (107,259 vs 107,279 nodes)
  and GREEN after.
- `crates/mf-search/src/history.rs::every_hash_keyed_table_indexes_a_key_the_same_way_regardless_of_pool_width`
  replaces the two `*_scales_with_the_next_power_of_two_thread_count` tests, which asserted
  the defect as intended behaviour. It pins both bucket counts, pins both masks as derived
  constants, and asserts behaviourally that a key aliases identically in two independently
  constructed tables.
- `correction_history_is_sized_by_its_bucket_count_and_stays_inside_l2` keeps the
  AGENTS.md 4.54 guard (sized by bucket count, never by the gravity bound; 64 KiB per
  table) that the deleted test also carried.
- `shared_history_requires_a_nonzero_thread_count` deleted: `new()` takes no thread count,
  so there is no zero to reject.

## Verification

| command | result |
|---|---|
| `cargo test --workspace` | green (`workspace-tests.txt`); `bench_cli` 16 passed in 846 s |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| release `bench` ×2 | 45,036 / 45,036 |
| `threads_scan.ps1` on the fixed build | 15/15 IDENTICAL |
| `Get-Process manifold,fastchess,stockfish` after the match | empty |

## Note for later features

The running baseline for M4 onward is **still `baselines/m2-nnue` (bench 45,036)**. This
change is behaviour-neutral at one thread — all 34 anchors match — so it does not promote
a new baseline. It does mean that from here on, a `fixed_depth_output_is_identical_at_every_thread_count`
failure is a **real regression in your change**, not the inherited coupling
`library/m3-search-notes.md` warned about. That warning is now obsolete.
