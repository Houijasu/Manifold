# MSN-M2-F5 — Cap iterative deepening at 128 plies

**Feature:** M2-F5-depth-cap (user-reported defect, fixed before the M2 confirm match)
**Kind:** correctness fix. No Elo measurement — any effect is absorbed by the M2-F4 confirm match.
**Verdict:** KEEP.

## The report

A user ran infinite analysis on a position with a forced mate. The engine iterated to
depth 3546 and "couldn't even be stopped." Two suspected bugs: no depth cap, and poor
`stop` responsiveness.

## Reproduction (pre-fix)

Driver: `infinite_depth_probe.ps1` (in this dir). It redirects engine stdout to a **file**
rather than a pipe, because a full OS pipe blocks the engine mid-search and is
indistinguishable from a genuine hang — the original pipe-based probe reported a 30 s
`stop` timeout for that reason alone, before the real defect was even reached.

Position `7k/6Q1/6K1/8/8/8/8/8 w - - 0 1` (mate in one), `go infinite`, 8 s of thinking:

| | pre-fix | post-fix |
|---|---:|---:|
| deepest `info depth` | **322013** (a second run: 118090) | **128** |
| engine stdout in 8 s | 186 MB | 115 KB |
| `stop` → `bestmove` | **never** (30 s timeout, engine had to be killed) | **79 ms** |

## Diagnosis

Both symptoms are one bug.

**Bug 1 — no depth cap.** `search_worker_with_history_callback_options` set
`maximum_depth = u32::MAX` for `limits.infinite`, and used `limits.depth` verbatim
otherwise. Nothing bounded the deepening loop.

Those iterations were pure waste, not merely excessive. `pvs` returns the static
evaluation once `ply >= MAX_SEARCH_PLY` (search.rs:895) rather than recursing, so a
nominal depth above 128 re-searches exactly the same tree. On a forced mate the tree is
also tiny, so each "iteration" costs microseconds and emits an info line.

The mate-score early exit at the bottom of the loop is guarded by `!limits.infinite`.
That guard is correct — UCI forbids answering before `stop` in infinite mode — but with no
cap the only remaining alternative was to iterate forever.

**Bug 2 — stop responsiveness is a consequence of bug 1, not an independent defect.**
The stop flag is checked every node (`visit_node`, search.rs:2602 — on entry, not gated by
`TIME_CHECK_INTERVAL`; only the *time* check is sampled every 512 nodes). `stop` is handled
on the UCI reader thread, which never blocks on the search: the search runs on its own
thread and `stop_and_join` sets the flag before joining.

What actually delayed `bestmove` was the info-line backlog. Each iteration sends an
`IterationInfo` down an **unbounded** `mpsc` channel; the pool's collector loop drains
that channel and writes each line to stdout while holding the writer lock. At ~40000
iterations/second the producer outran the consumer without bound, so when `stop` finally
set the flag the collector still had a multi-million-line backlog to write before it could
reach `bestmove`. 186 MB of stdout in 8 seconds is the measurement of that backlog.

Capping the loop removes the producer's ability to outrun the consumer, which is why one
change fixes both symptoms — no channel bounding or flag-check-frequency change was needed.

**Ply/seldepth overflow: checked, none present.** `MAX_SEARCH_PLY == ACCUMULATOR_STACK_CAPACITY`
is a compile-time assert (search.rs:24), and `pvs`/`quiescence` return before pushing at
that ply, so the 128-frame accumulator stack was never at risk even at nominal depth
322013. `seldepth` is `max`'d against actual ply, so it stayed ≤ 128 too. The runaway was
confined to the *nominal* iteration counter.

## The change

`crates/mf-search/src/search.rs`: new `iteration_ceiling(&SearchLimits)`, bounded by
`MAX_ITERATIVE_DEEPENING_DEPTH = MAX_SEARCH_PLY = 128` in every mode.

```rust
fn iteration_ceiling(limits: &SearchLimits) -> u32 {
    if limits.infinite {
        MAX_ITERATIVE_DEEPENING_DEPTH
    } else {
        limits.depth.unwrap_or(DEFAULT_MAX_DEPTH).clamp(1, MAX_ITERATIVE_DEEPENING_DEPTH)
    }
}
```

No change was made to `stop` handling, the flag-check interval, or the UCI layer. The
post-cap idle behaviour is what the existing code already did: after the loop ends,
`start_search` spins on `wait_for_stop && !stop` before writing the tail, so an infinite
search that saturates at 128 **idles awaiting `stop`/`quit` and does not emit a premature
`bestmove`** — confirmed by the flat stdout byte count between the 8 s and 15 s samples.

## Post-fix verification

Full transcripts in `uci-session.txt`.

| scenario | deepest depth | `stop` → `bestmove` | premature bestmove? |
|---|---:|---:|---|
| forced mate, Threads=1, 15 s think | 128 | 79 ms | no |
| forced mate, Threads=8, 10 s think | 128 | 69 ms | no |
| kiwipete middlegame, Threads=1, 10 s | 20 | 57 ms | no |

`go depth 200` on the forced mate answers immediately, clamped to the ceiling.

### Bench signature — UNCHANGED

`bench-control.txt`: two consecutive runs, **45036 nodes** both times, identical to the
anchor pinned in `crates/mf-uci/tests/bench_cli.rs`. Expected: bench runs depth 7, far
below the cap, so no anchor re-pin was needed. `cargo test --release -p mf-uci --test
bench_cli` passes 13/13 unchanged.

### Test suites

| command | result |
|---|---|
| `cargo test --release -p mf-search` | 88 + 1 + 19 + 11 + 19 passed, 0 failed |
| `cargo test --release -p mf-uci` | 37 + 6 + 13 + 4 + 1 + 6 + 3 + 2 + 3 + 5 + 3 + 49 passed, 0 failed |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |

## Tests added

- `crates/mf-search/src/search.rs::iterative_deepening_is_bounded_by_the_ply_ceiling` —
  `iteration_ceiling` unit test: infinite → 128; infinite carrying a depth → still 128;
  depth 0 → 1; depth 12 → 12; depth 200 and `u32::MAX` → 128; no depth → `DEFAULT_MAX_DEPTH`.
- `crates/mf-search/tests/search_invariants.rs::an_infinite_search_stops_iterating_at_the_ply_ceiling`
  and `::a_requested_depth_above_the_ceiling_is_clamped_to_it` — library-level, driven from
  the user's forced-mate FEN.
- `crates/mf-uci/tests/analysis_stop.rs` — three protocol-level tests:
  `an_infinite_search_on_a_forced_mate_saturates_at_the_depth_ceiling` (depth ≤ 128, no
  bestmove before `stop`, `stop` < 500 ms from the saturated idle state),
  `stop_answers_within_half_a_second_during_deep_analysis`, and
  `a_go_depth_above_the_ceiling_clamps_to_it`.

The shared session helper now also asserts, in every existing `go infinite` test, that no
`bestmove` was emitted before `stop` — the UCI rule GUIs enforce by discarding such a move.

Red-phase evidence: before the fix the two ceiling tests failed with
`infinite analysis reported depth 322013, past the 128-ply ceiling` and
`go depth 200 never produced a bestmove`.

## Note on test-position choice

`8/8/3k4/3p4/3P4/3K4/8/8` (a blocked pawn wall) was the obvious "cheap iterations"
position, but a *clamped* `go depth 200` on it takes well over 90 s to reach 128 — it has
enough tree to be slow at high depth while still being cheap per node. The user's
forced-mate FEN saturates in ~100 ms and is the position actually reported, so all the
depth-ceiling tests use it. This is why the suite stays fast.

## Files

- `infinite_depth_probe.ps1` — re-runnable driver (file-redirected stdout; `-Fen`,
  `-ThinkMs`, `-Threads`, `-Go`).
- `uci-session.txt` — post-fix transcripts for the three scenarios above.
- `bench-control.txt` — two identical bench runs at 45036.
- `run-metadata.txt` — commit, branch, machine, binary provenance.
