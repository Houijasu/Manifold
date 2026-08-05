# MSN-NNUE-lazy — Lazy accumulator updates

**Feature:** M2-F2-lazy-updates (milestone M2, NNUE inference speed)
**Date:** 2026-08-05
**Decision:** **KEEP.** 1T NPS is **1.03x–1.04x** the post-Finny build across two independent
runs (geometric mean, depth 12), clearing the feature's 3% kill criterion. Bench signature is
**unchanged at 45,036**, which is the proof the change is bit-exact rather than merely close.

## Purpose

Defer each push's accumulator work until an evaluation actually reads it, so pushes into
subtrees that are pruned before any evaluation (NMP, futility, LMP, SEE) never pay for their
accumulator at all.

## Provenance

| | |
|---|---|
| Baseline compared against | the post-Finny build, commit `a6b092f` (M2-F3), built from this tree with the feature stashed |
| Baseline binary | built into this directory during measurement, **not committed** (110 MB); reproduce with `git stash push -u` at this commit's parent tree, `cargo build --release` |
| Machine | i9-13980HX (8 P-cores + 16 E-cores, 32 logical), 31.6 GB RAM, Windows 11 |
| Toolchain | `--release` (fat LTO, 1 CGU, `panic=abort`), `target-cpu=native` |
| Net | `nets/main.nnue`, 111,261,604 bytes |
| Production forward mode | `Avx2Vnni`, sparse FC0 |
| Core pinning | every timing run in a shell pinned to the 8 P-cores (`ProcessorAffinity = 0xFFFF`) |

Raw output committed beside this document: `nps-depth12.json`, `nps-depth12-run2.json`,
`update-profile.txt`, `bench-control.txt`, `uci-session.txt`, `uci_session.ps1`.

**Single-variable note:** the baseline is the *previous kept build* (post-Finny), not
mission-start, and it was produced from this exact working tree with the feature stashed, so the
two binaries differ only by this feature.

## The ceiling was wrong, and measuring it was the first thing this feature did

The feature description carried forward M2-F1's figure — *29.8% of pushes go unevaluated* — as
the bound on what lazy updates could skip. **That number overstates the ceiling, and the error
is structural, not statistical.**

A push that is never evaluated *at its own ply* is still not free. The moment any descendant is
evaluated, the frame must be materialized anyway, because the descendant's incremental update
reads its parent's accumulator. Only a frame whose **entire subtree** is popped without a single
evaluation is genuinely skippable.

Before writing the implementation, a shadow model of exactly this rule (walk back to the nearest
materialized frame, materialize forward) was run under the existing default-off instrumentation
feature over the same 643,412-node workload M2-F1 used:

| | M2-F1's figure | measured truth |
|---|---|---|
| Pushes never evaluated at their own ply | 188,236 of 631,900 (**29.8%**) | same |
| Pushes whose whole subtree went unevaluated | — | 116,232 of 606,591 (**19.2%**) |

So the reachable ceiling was **19.2%**, not 29.8% — about two thirds of the assumed headroom.
Recomputed against the post-Finny build's own proportions (accumulator update 60.8% + threat
discovery 20.7% = 81.5% of NNUE time, NNUE 43.1% of wall):

```
0.192 x 0.815 x 0.431 = 6.7% of wall time, as a hard upper bound
```

That is above the 3% kill criterion but well under half the ~11% the feature description
carried, and it is *before* bookkeeping cost. The feature was implemented on that basis, and the
delivered 3–4% against a 6.7% ceiling is the expected fraction once bookkeeping is paid.

**The shadow model predicted the outcome exactly.** It said 116,232 pushes were skippable; the
finished implementation skipped **116,232**. The design was validated before it was written.

## Design

`AccumulatorFrame` gains `pending: Option<PendingUpdate>`. A push stores the move and its `Undo`
and returns; an evaluation walks back to the nearest materialized frame and applies the chain
forward.

Two decisions are worth recording:

**Deferring the move, not the computed deltas.** A pending real frame stores `(mv, undo)` — not
the HalfKA deltas and changed threat edges a push would have computed. This is what lets a
skipped push avoid **changed-threat discovery** as well as the accumulator update. Discovery is
20.7% of NNUE time in the post-Finny profile, so storing precomputed deltas would have forfeited
a quarter of the available win. The measured effect is visible in the profile below: edges
discovered per push fell from 8.91 to 6.16 on the bench workload, because discovery now runs
only for frames that are actually materialized.

**Positions stay eager.** `AccumulatorFrame::position` is still written on every push, because
the *next* ply's changed-threat discovery reads it as the parent position whether or not this
frame is ever evaluated. Reconstructing it lazily would cost more than storing it.

`current()`, `evaluate()`, `evaluate_internal()` and `dump_features()` now take `&mut self` and
materialize first. That is deliberate: handing out a shared reference to a possibly-stale frame
is precisely the bug lazy updates invite, so the borrow checker forbids it rather than a comment
asking callers not to.

`pop()` clears the pending update. Besides being the point of the feature, this is what stops a
discarded branch from leaking into the sibling that reuses the slot — covered by a test.

Per the feature description and the M2-F1 evidence (zero occurrences in 606k pushes), the
`MAX_CHANGED` overflow path was **not touched**.

## Results

### 1T NPS vs the post-Finny build

`py -3.14 harness/nps_compare.py --engine lazy=.\target\release\manifold.exe --engine postfinny=.\experiments\MSN-NNUE-lazy\manifold-postfinny.exe --depth 12 --hash 64 --warmup 1 --repeat N`

| Position | nodes (both) | run 1 (5 repeats) | run 2 (7 repeats) |
|---|---|---|---|
| startpos | 99,674 | 1.03x | 1.02x |
| kiwipete | 141,659 | 0.97x | 1.05x |
| midgame | 50,159 | 1.03x | 1.02x |
| endgame | 38,768 | **1.08x** | **1.08x** |
| **geometric mean** | | **1.03x** | **1.04x** |

Node counts to depth are identical at every position (ratio 1.00x), confirming a pure speed
change.

**Two runs were taken deliberately.** Run 1's geometric mean landed exactly on the 3% kill
threshold with kiwipete apparently *regressing* (0.97x), which is a keep-or-revert decision too
close to make on one sample. Run 2 with more repeats put kiwipete at 1.05x — the 0.97x was
noise, and kiwipete is the noisiest position in the set (its 5 raw samples in run 1 spanned
431k–466k NPS, a 7.5% spread). Reporting only the favourable run would have been the easy
mistake here; both are recorded.

The endgame gains most (1.08x, consistent across both runs). That is the expected shape: endgame
searches are the most prune-heavy per node, so they push the most frames that never get read.

### What the implementation actually skipped

`cargo run --release -p mf-search --features instrumentation --example nnue_update_profile -- 7`

| Metric | post-Finny | lazy |
|---|---|---|
| Deferred pushes skipped entirely | — | **116,232 of 606,591 (19.2%)** |
| Accumulator update, per real push | 533.9 ns | **451.2 ns** |
| Threat discovery, per real push | 182.2 ns | **164.5 ns** |
| Changed edges discovered per push (bench) | 8.40 | **6.16** |
| Finny-served king moves | 58,826 | **49,168** |
| Full rebuilds (king / overflow) | 0 / 0 | **0 / 0** |

King moves served by the Finny table dropped 16%, and edges discovered per push dropped 27% —
both are deferred work that was never needed, which is the feature working as designed.

*(Instrumented runs carry the counters' ~10% overhead and are used only for counts and
proportions. Every NPS claim above comes from uninstrumented builds.)*

### Determinism and correctness

- **Bench signature: 45,036**, identical across consecutive runs, and identical to the
  post-Finny baseline binary benched on the same machine. This is the strongest evidence the
  change is bit-exact: a single wrong accumulator lane anywhere in the tree would move the node
  count.
- `cargo test --workspace` green, including `bench_cli` 13/13 at the unchanged anchor — no
  re-pinning was needed.
- `cargo clippy --workspace --all-targets -- -D warnings` green, and also green with
  `--features instrumentation` on `mf-nnue`/`mf-search`.
- `cargo fmt --all -- --check` green.
- The pre-existing `mf-search` invariant (incremental == full rebuild at *every* eval,
  `incremental_nnue_matches_full_rebuild_at_every_search_evaluation`) passes unchanged, over
  whole searches including Chess960.

### Manual UCI verification

`experiments/MSN-NNUE-lazy/uci_session.ps1` (Process-based driver with blocking `ReadLine`;
piped here-strings abort `go movetime`, and `add_OutputDataReceived` fails inside a script file):

- `uci` → `uciok`, `isready` → `readyok`, backend `Avx2Vnni` sparse FC0.
- startpos `go depth 18` → `bestmove d2d4`, well-formed info lines.
- Endgame `8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1`, `go movetime 3000` → `bestmove b4f4`,
  matching the M2-F3 session exactly.
- Kiwipete `go movetime 2000` → `bestmove e2a6`.
- Chess960 `1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1`, `go movetime 2000` → `bestmove e1e8`
  (king-takes-rook castling, `score mate 8`) — this drives a deferred king move through the
  mirror-flip tier via the real protocol.
- Working set 135.6 MiB (dominated by the 106 MiB embedded net); exit code 0.

## Memory

`AccumulatorFrame` grew from 4,544 to **4,608 bytes** — one cache line, holding the deferred
`Move`, `Undo` and enum tag under the frame's 64-byte alignment. Across the 129-frame stack that
is **8 KiB more per search thread** (586,176 → 594,432 bytes). At 8 threads, 64 KiB. The size is
pinned by `stack_frames_keep_compact_metadata_and_cache_line_alignment` so a future change that
inflates the frame is caught by a test rather than by a benchmark.

## Tests added

`crates/mf-nnue/tests/lazy_updates.rs` — six tests, each aimed at a deferral pattern a real
search produces. All assert the same invariant the eager path had: what an evaluation reads
equals a scalar full rebuild, through `current()`, `dump_features`, `evaluate_internal` and
`evaluate`.

- `a_chain_of_unevaluated_pushes_materializes_correctly_at_the_first_evaluation` — ten plies
  pushed unread, then one evaluation collapsing the whole chain. This is the pruned-subtree
  shape.
- `evaluating_at_every_ply_of_a_deferred_chain_matches_a_full_rebuild` — evaluates on the way
  back *up*, so materializing a child must not corrupt the ancestors it walked through.
- `a_deferred_king_move_materializes_through_both_mirror_tiers` — king walks that hold and flip
  the d/e mirror, deferred. The mirror-held tier reads the Finny entry for the *parent's* king
  square, so a deferred king move must materialize its parent before that read.
- `a_sibling_searched_after_an_unread_pop_still_matches_a_full_rebuild` — the pattern that
  breaks a naive dirty-flag scheme: push, never evaluate, pop, push a different child from the
  same parent.
- `null_moves_interleaved_with_deferred_real_moves_keep_parity` — a null frame inside a pending
  chain, which materialization has to reach *through* to the last real materialized frame.
- `a_randomized_deferred_walk_matches_full_rebuilds_in_standard_and_chess960` — the broad net:
  evaluations at pseudorandom plies across five roots including two Chess960, so pending chains
  of every length occur. Asserts a floor on the number of verifications performed, so the walk
  cannot silently degenerate into a test that checks nothing.

The two initially-failing tests in this file failed on illegal move lists (test-authoring bugs),
not on engine behaviour; they were fixed and the suite is green.

## Changes made

- `crates/mf-nnue/src/accumulator.rs` — `PendingUpdate` enum and `AccumulatorFrame::pending`;
  `push_real`/`push_null` defer; new `materialize()` walk-back and `apply_real()`; `pop()` drops
  pending work; the four read paths take `&mut self` and materialize first.
- `crates/mf-nnue/src/instrumentation.rs` — `deferred_pushes_skipped` counter.
- `crates/mf-search/src/search.rs` — `SearchEvaluator::evaluate` and `SearchContext::static_eval`
  take `&mut self`, following the read paths.
- `crates/mf-search/examples/nnue_update_profile.rs` — reports both the old (overstated) and the
  true skip figures, so the distinction this feature discovered is not lost again.
- `crates/mf-nnue/tests/lazy_updates.rs` (new), plus mechanical `&mut` propagation in
  `accumulator_stack.rs` and `eonego_parity.rs`.

## Follow-up for the orchestrator

**The M2-F1 "29.8% unread pushes" figure is superseded by 19.2%** and `library/nnue-profile.md`
should carry the corrected number, because it is the input to any future lazy-related decision.

The remaining NNUE distribution after this feature: accumulator update 58.1%, threat discovery
21.2%, forward pass 20.7% of NNUE time, with NNUE at 39.8% of wall (down from 43.1%). **Threat
discovery is now the clearest unclaimed target** — it is M2-F3b, and this feature reduced the
number of pushes that reach it without making any push cheaper.
