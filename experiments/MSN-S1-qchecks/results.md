# M3-F1 — Quiet checks in quiescence search

**Decision: REVERT to disabled. `UseQSearchChecks` ships OFF (default `false`).**
The shipped bench signature is unchanged at **45,036**, and fixed-depth node counts stay
Threads-independent.

## Purpose

The M3 audit named this the largest search feature Manifold was missing: `qsearch`
generated only captures and promotions when not in check, so a position whose best
continuation is a *quiet* move giving check was scored by the standing pat — the search
returned a number for a position it had not actually resolved. Every Stockfish-class
engine widens the first quiescence ply with quiet checks. The TT depth-domain machinery
already reserved `checks = -1` against `captures = -2` for exactly this.

## What was built

- **`crates/mf-search/src/move_ordering.rs`** — `quiet_checks()`: filters a full
  pseudo-legal generation to non-capture, non-promotion, non-castling moves, keeps those
  passing `move_gives_check`, then gates on `static_exchange_evaluation >= 0`
  (`QUIET_CHECK_SEE_THRESHOLD`) so the ply does not expand every spite check. Ordered by
  `ordering_history` via `sort_by_cached_key`. `quiescence_moves` gained an
  `include_quiet_checks` parameter and **appends** the checks after every capture: a
  capture resolves material immediately and a quiet check does not, so a check must never
  displace a capture that could raise the standing pat and cut off first.
- **`crates/mf-search/src/search.rs`** — `quiescence` gained a `first_qsearch_ply` flag.
  The three entries from `pvs` (depth ≤ 0, razoring, ProbCut verification) pass `true`;
  every recursive qsearch call passes `false`. The widening is one ply deep because a
  quiet check costs a node without resolving material, so applying it at every qsearch
  ply grows the tree geometrically.
- **TT depth domain wired correctly.** A first-ply node that widened stores under
  `QSEARCH_CHECKS_TT_DEPTH` (`-1`), not the captures domain (`-2`). It searched strictly
  more than the captures, so its bound is legitimate evidence for a captures-only probe,
  while a captures-only entry must never satisfy it — taking that cutoff would silently
  discard the widening. (The in-check and widened-first-ply node kinds can never collide:
  being in check is a property of the position and the TT is keyed on the position.)
- **`crates/mf-uci/src/lib.rs`** — UCI option `UseQSearchChecks`, following the existing
  toggle pattern.

No mf-core change was needed. A gives-check-only generator does not exist there; the
filtered full quiet generation is the honest first cut, and its cost is recorded below.

## Verification before measuring

| Command | Result |
|---|---|
| `cargo test -p mf-search` | 92 + 21 + 11 + 19 + 1 passed, 0 failed |
| `cargo test --release -p mf-uci --test bench_cli` | 14/14 passed |
| `cargo test -p mf-uci --test uci_protocol` | 49/49 passed |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `manifold bench` × 2 | 45,036 twice (deterministic) |

Live UCI session (`uci_probe.ps1`, transcript in `uci-probe-transcript.txt`), on
`3r3k/8/8/8/8/6q1/P7/7K w - - 0 1` where White has only two legal moves and Black mates
with the quiet `Rd1#`:

```
checks ON : info depth 1 seldepth 2 score mate -1  nodes 5  pv a2a4 d8d1
checks OFF: info depth 1 seldepth 1 score cp -1007 nodes 2  pv a2a4
```

The feature works exactly as intended. The blind arm reports a merely-losing score for a
forced mate one ply away.

## Measurement

### The match (the evidence)

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S1-qchecks `
    -Purpose 'M3-F1 quiet checks in qsearch: single-variable measurement vs the M2 kept build' `
    -AName qchecks -ACmd .\target\release\manifold.exe `
    -BName m2-nnue -BCmd .\baselines\m2-nnue\manifold.exe `
    -Rounds 150 -Seed 20260806
```

8+0.08, Threads=1, Hash 64, UHO_4060_v4.epd, `-use-affinity -concurrency 8` (harness
enforced), 17 min 25 s wall.

| | |
|---|---|
| **Elo** | **-12.75 ± 23.01** (nElo -21.83 ± 39.32) |
| Games | 300 — W 77 / L 88 / D 135, 48.17% |
| Ptnml(0-2) | [5, 38, 74, 29, 4], PairsRatio 0.77 |
| LOS | 13.83% |
| Forfeits / crashes / illegal moves | **0 / 0 / 0, both engines** |

Provenance in `run-metadata.txt` (A SHA-256 `3750F1A8…`, B `BC0C445C…`, seed 20260806).

### Diagnosis: where the Elo went

One focused pass, not a match-burning iteration.

**Depth at equal time** (`harness/depth_at_time.py`, 24 book positions,
`movetime 1000`, Hash 64; raw output in `depth-at-time.txt`):

| build | mean depth | median | min | max |
|---|---|---|---|---|
| qchecks | 15.96 | 16.0 | 14 | 19 |
| m2-nnue | 16.08 | 16.0 | 14 | 19 |

**-0.12 plies**, deeper in only 7 of 24 positions. And the widening costs **+12.3% bench
nodes** (45,036 → 50,569).

That is the whole story, and the two numbers agree with each other. Every quiet check is
a node that resolves no material, so the quiescence grows without the standing pat
converging any faster; the extra time comes straight out of the iterative deepening that
actually finds moves. The tactics the widening genuinely buys are real — the quiet mate
above is pinned as a regression test — but at 8+0.08 they are rarer than the tenth of a
ply they cost.

## Decision

**Revert to disabled**, per the feature's stated criterion (keep only if the point
estimate is positive) and validation contract **A-SEARCH-001** ("only non-regressing
features remain enabled in the final build").

The honest reading of -12.75 ± 23.01 is **"not shown to help"**, not "shown to hurt" —
the error bar covers zero and LOS is 13.8%. The feature ships off because a technique
with no demonstrated gain has no claim on being the default, not because the measurement
proved it harmful.

The code is kept, maintained, and toggleable rather than deleted, matching how this repo
already treats pawn history and history pruning. Consequences of shipping it off:

- The shipped bench signature is **unchanged at 45,036** — a disabled feature must leave
  it bit-for-bit identical, and `BENCH_NODE_COUNT` not moving is now itself an assertion
  in `bench_cli.rs`. No anchor in the file moved.
- Fixed-depth node counts remain **Threads-independent** (A-SEARCH-002); see the
  pre-existing issue below for why that mattered.
- `BENCH_NODE_COUNT_WITH_QSEARCH_CHECKS = 50_569` is pinned so the disabled technique
  stays measurable without a rebuild.

### Conditions for revisiting

Two things would change the arithmetic, and neither is speculative:

1. **A targeted gives-check generator in mf-core.** The current implementation filters a
   full pseudo-legal generation through `move_gives_check` *and* SEE. That is the honest
   first cut, not the cheap one. Generating only check-giving moves directly (from the
   enemy king's attack rays plus the discovered-check candidates) removes most of the
   throughput cost, which is a large share of the 0.12-ply loss.
2. **A longer time control.** A lost tenth of a ply buys back less as depth grows, while
   the tactics the widening finds do not become rarer.

## Pre-existing issue found (NOT caused by this feature)

While verifying, `uci_protocol::fixed_depth_output_is_identical_at_every_thread_count`
failed. It was diagnosed to root cause rather than worked around, because a
Threads-dependence regression would be a genuine correctness defect.

**It is pre-existing and lives in correction history, not in this feature.** Evidence,
all reproducible with the committed probes:

| build | setup | Threads 1 vs 8 |
|---|---|---|
| `baselines/m2-nnue` (no qsearch-check code at all) | default | **DIFFERENT** at depth 10 on kiwipete |
| `baselines/m2-nnue` | `UseCorrHistory=false` | IDENTICAL, all 15 position×depth cells |
| new build, checks ON | default | DIFFERENT (5 of 15 cells) |
| new build, checks ON | `UseCorrHistory=false` | IDENTICAL, all 15 cells |
| new build, checks OFF (shipped) | default | IDENTICAL |

Mechanism: `SharedHistory::new(thread_count)` sizes the correction and pawn tables as
`BASE_BUCKETS * nextPow2(threads)` (`history.rs`), so the bucket **mask** differs between
Threads=1 and Threads=8 even when every helper stays parked. A hash collision that occurs
at 512 buckets and not at 4096 changes the residual applied to a static eval, which
changes the tree. It is fully deterministic (three identical repeats), so it is
table sizing, not a race.

The widening did not introduce this; it enlarged the qsearch and so raised the collision
rate enough to make an already-coupled test position cross the threshold. With the
feature shipping off, the test passes. Reproduce with
`threads_probe.ps1` / `threads_scan.ps1` in this directory. Reported as a non-blocking
discovered issue; the fix is to size corrhist independently of the thread count, which is
out of scope here and must be measured on its own.

## Artifacts in this directory

| file | what |
|---|---|
| `results.md` | this document |
| `run-metadata.txt` | harness provenance + self-check (A-DOC-001) |
| `console.txt` | full fastchess output |
| `fastchess.log` | fastchess log |
| `games.pgn` | 300 games (untracked per repo convention) |
| `depth-at-time.txt` | depth-at-equal-time measurement |
| `uci-probe-transcript.txt` | live UCI session, both arms |
| `collect_anchors.ps1` | one-pass bench anchor sweep against the release binary |
| `uci_probe.ps1` | Process-driven UCI probe |
| `threads_probe.ps1`, `threads_scan.ps1` | Threads-dependence diagnosis |
