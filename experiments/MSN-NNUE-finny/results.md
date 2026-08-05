# MSN-NNUE-finny — Finny tables (accumulator refresh cache)

**Feature:** M2-F3-finny-tables (milestone M2, NNUE inference speed)
**Date:** 2026-08-05
**Decision:** **KEEP.** King-move full rebuilds went from 91.43 per 1000 nodes to **zero**;
1T NPS is **1.03x** `baselines/mission-start` (geometric mean, depth 12), with the largest gain
(1.05x) exactly where the M2-F1 profile said rebuilds hurt most. Bench signature unchanged.

## Purpose

M2-F1 measured king-move rebuilds at **91.4 per 1000 searched nodes at ~1461 ns each** — 14.9%
of NNUE time and 6.6% of wall time, and the single largest addressable block of waste in the
NNUE path (`experiments/MSN-NNUE-baseline/results.md`). This feature caches, per
`(perspective, king square)`, the HalfKA accumulator and the piece bitboards that produced it,
so a king move applies a small diff instead of rebuilding a perspective from scratch.

Per the same profile, **the `MAX_CHANGED` overflow path was not touched**: it executed exactly
zero times in 606,591 real pushes, so it remains the plain full-rebuild fallback.

## Provenance

| | |
|---|---|
| Repo commit at measurement | branch `feature/nnue-optimizations`, this feature's tree |
| Machine | i9-13980HX (8 P-cores + 16 E-cores, 32 logical), 31.6 GB RAM, Windows 11 |
| Toolchain | `--release` (fat LTO, 1 CGU, `panic=abort`), `target-cpu=native` |
| Net | `nets/main.nnue`, 111,261,604 bytes |
| Production forward mode | `Avx2Vnni`, sparse FC0 |
| Core pinning | Every timing run in a shell pinned to the 8 P-cores (`ProcessorAffinity = 0xFFFF`) |
| Baseline compared against | `baselines/mission-start/manifold.exe` (bench 45,036) |

Raw output committed beside this document: `nps-depth12.json`, `update-profile.txt`,
`bench-control.txt`, `uci_session.ps1`.

## The design decision the feature asked for, with numbers

The feature description asked whether the cache should cover HalfKA rows only or threats too.
**That question was measured, not argued.** Temporary instrumentation split a full rebuild into
its three phases over the same 643,412-node workload as M2-F1:

| Rebuild phase | ns per rebuild | Share of rebuild |
|---|---|---|
| HalfKA piece rows | 727.2 | **49.4%** |
| Threat scan (`append_active_threats`) | 221.9 | 15.1% |
| Threat rows | 468.4 | 31.8% |
| unattributed | 53.8 | 3.7% |

The decisive structural fact is in `threats::make_index`: it consults the king square **only**
through `ORIENT_TABLE`, which is a pure function of the king's *file* (0 for files a–d, 7 for
e–h). So a king move changes every FullThreats index **only when it crosses the d/e mirror
line**. Measured over the same workload, **69.4% of king moves keep the mirror.**

That yields a two-tier design, and both tiers use the cache:

- **Mirror held (69.4% of king moves):** the parent's threat contribution is still exactly
  correct. The cached HalfKA accumulator for the parent's king square is subtracted and the
  child's added, and the move-local threat deltas are netted in during the same pass. **No
  threat work at all** — neither scan nor rows.
- **Mirror flipped (30.6%):** every threat index changed, so the threat half is genuinely
  recomputed. But the piece half — the **49.4%** majority of the rebuild — still comes from the
  cache. This tier was added after a first implementation measured only +2%: caching just the
  mirror-held case left endgames at 1.00x, because an active king crosses the mirror often.

**A cache keyed on threat rows was therefore never needed**: in the 69.4% case the threat rows
are already correct and cost nothing, and in the 30.6% case no cached threat state could be
reused at all, since every index changed.

Keying by **king square** rather than by HalfKA bucket is required, not conservative: two king
squares produce identical HalfKA indices only if they share both the bucket offset and the
mirror orientation, and `WHITE_KING_BUCKETS` is mirror-symmetric with 32 distinct values, so
that pairing is unique per square.

## Results

### Full rebuilds eliminated

`cargo run --release -p mf-search --features instrumentation --example nnue_update_profile -- 7`

| Metric | M2-F1 baseline | This feature |
|---|---|---|
| King-move rebuilds per 1000 nodes | **91.43** | **0.00** |
| `MAX_CHANGED` overflow rebuilds per 1000 nodes | 0.00 | 0.00 |
| Cost per king move | 1461.0 ns (rebuild) | **797.8 ns** (cache) |
| Rebuild share of NNUE time (`rbld%`) | 15.5% | **0.0%** |
| Accumulator update, per real push | 577.0 ns | **534.2 ns** |
| HalfKA rows applied per cache refresh | (32 pieces rebuilt) | **6.07** |

Every king move is now served by the cache: `king_moves=58826`, of which 18,009 (30.6%) also
recomputed the threat half after a mirror flip. A refresh touches **6.07 rows** on average
instead of one row per piece on the board.

**Note on instrumented NPS:** these runs carry the counters' ~10% overhead and are used only for
*proportions and counts*. All NPS claims below come from uninstrumented builds.

### 1T NPS vs `baselines/mission-start`

`py -3.14 harness/nps_compare.py --engine finny=.\target\release\manifold.exe --engine mission-start=.\baselines\mission-start\manifold.exe --depth 12 --hash 64 --warmup 1 --repeat 5`

| Position | nodes (both) | finny NPS | mission-start NPS | ratio |
|---|---|---|---|---|
| startpos | 99,674 | 615,079 | 602,270 | 1.02x |
| kiwipete | 141,659 | 469,658 | 449,915 | 1.04x |
| midgame | 50,159 | 585,539 | 570,803 | 1.03x |
| endgame | 38,768 | 1,025,554 | 981,344 | **1.05x** |
| **geometric mean** | | | | **1.03x** |

Node counts to depth are identical at every position (ratio 1.00x), confirming the change is
bit-exact and purely a speed change.

The endgame result is the profile cashing out: M2-F1 measured bench3 (an endgame) at **308.9
rebuilds per 1000 nodes**, the worst of any position, and it is the position that gains most.

**The intermediate measurement is worth recording**, because it is what justified the second
tier. With only the mirror-held case cached, the same command gave startpos 1.02x, kiwipete
1.03x, midgame 1.00x, endgame **1.00x** — geometric mean **1.02x**, below the feature's 2%
decision threshold and flat exactly where rebuilds were worst. Extending the cache to serve the
piece half of mirror-flip moves moved endgame from 1.00x to 1.05x.

### Determinism and correctness

- **Bench signature: 45,036**, identical across consecutive runs, **unchanged** from the pinned
  anchor. `crates/mf-uci/tests/bench_cli.rs` needed no re-pinning, and `cargo test -p mf-uci`
  passes 13/13. This is the strongest evidence the change is bit-exact: any deviation in a
  single accumulator lane would move the node count.
- `cargo test --workspace` green.
- `cargo clippy --workspace --all-targets -- -D warnings` green, and also green with
  `--features instrumentation` on `mf-nnue`/`mf-search`.
- `cargo fmt --all -- --check` green.

### Manual UCI verification

`experiments/MSN-NNUE-finny/uci_session.ps1` (Process-based driver; piped here-strings abort
`go movetime`):

- `uci` → `uciok`, `isready` → `readyok`.
- Endgame FEN `8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1`, `go movetime 3000` → depth 25,
  legal `bestmove b4f4`, well-formed info lines.
- Chess960 `1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1` with `UCI_Chess960 true`, `go movetime 2000`
  → `bestmove e1e8` (king-takes-rook castling notation), `score mate 8` — this drives the
  mirror-flip path through the real protocol.
- Working set 133.6 MiB (dominated by the 106 MiB embedded net); process exits cleanly.

## Memory

The table is **per search thread**: 128 entries (2 perspectives x 64 king squares) of
`[i16; 1024]` values + `[i32; 8]` PSQT + 12 piece bitboards, 64-byte aligned = 2,176 bytes per
entry, **272 KiB per thread**. At 8 threads that is 2.1 MiB; at 64 threads, 17 MiB. A test
(`the_table_stays_small_enough_to_hold_one_per_search_thread`) pins these numbers.

## Tests added

Parity is asserted against an independent full rebuild, which is the invariant that matters:

- `finny.rs::a_cold_refresh_reproduces_the_halfka_only_accumulator` — a cold entry (empty board,
  i.e. biases only) refreshes to exactly the HalfKA-only oracle.
- `finny.rs::a_warm_refresh_applies_only_the_difference_and_still_matches_the_oracle` — replays
  a real opening line, checking every ply against the oracle so drift cannot accumulate.
- `finny.rs::refreshing_the_same_key_from_an_unrelated_position_stays_exact` — the same key hit
  by wildly different positions (32 pieces → 2 pieces → 31 pieces), which is the stale-entry
  case that a diff-based cache gets wrong if the bitboard bookkeeping is off.
- `accumulator.rs::king_walks_keep_incremental_state_equal_to_a_full_rebuild` — a 30-ply king
  walk crossing the mirror line repeatedly in both colours, alternating the two tiers.
- `accumulator.rs::castling_keeps_incremental_state_equal_to_a_full_rebuild` — kingside and
  queenside for both colours.
- `accumulator.rs::chess960_castling_keeps_incremental_state_equal_to_a_full_rebuild` —
  king-takes-rook notation, including a castle where the king does not change square.
- `accumulator.rs::a_king_move_that_keeps_the_mirror_uses_the_cache_instead_of_rebuilding` and
  `..._that_flips_the_mirror_reuses_the_cached_piece_rows` — assert which tier ran, so a future
  change that silently falls back to rebuilding is caught by a test rather than by a benchmark.

The three `finny.rs` parity tests were mutation-checked: seeding a bug that skips cached-piece
removals fails two of them, so they genuinely bite rather than passing vacuously.

The pre-existing `mf-search` invariant (incremental == full rebuild at *every* eval) passes
unchanged and covers this feature across whole searches.

## Changes made

- `crates/mf-nnue/src/finny.rs` (new) — the cache: 128 entries, diff-against-cached-bitboards
  refresh, `MAX_DELTA = 32`.
- `crates/mf-nnue/src/accumulator.rs` — the king-move branch now routes through the cache in
  both tiers (`rebase_halfka`, `rebuild_threats_onto`); `UpdateContext` carries the parent
  position and the table; `AccumulatorStack` owns one table.
- `crates/mf-nnue/src/threats.rs` — `mirrors_alike`, documenting the orientation rule the whole
  design rests on.
- `crates/mf-nnue/src/simd.rs` — `subtract_i16_row` / `subtract_i8_row` / `subtract_psqt_row`
  and their AVX2 kernels were `#[cfg(test)]`; they are now production code.
- `crates/mf-nnue/src/test_support.rs` (new) — shared `local_network` helper, previously
  duplicated inside the accumulator test module.
- `crates/mf-nnue/src/instrumentation.rs` + `crates/mf-search/examples/nnue_update_profile.rs` —
  `finny_king_updates`, `finny_threat_rebuilds`, `finny_cycles`, `finny_refreshes`,
  `finny_delta_rows` replace the now-meaningless rebuild reporting (still default-off).

## Follow-up for the orchestrator

**This changes the input to M2-F2 (lazy updates), as M2-F1 predicted it would.** Reaching the
accumulator is still the dominant cost (threat discovery 20.7% + accumulator update 60.9% of
NNUE time), and the unread-push ceiling is unchanged at 29.8%. But the per-push cost that lazy
updates would defer is now lower (534.2 ns vs 577.0 ns), so the ≤11%-of-wall ceiling M2-F1
computed should be recomputed against this build, not against mission-start.

The threat path is now the clear next target: `threat_discovery` is 20.7% of NNUE time and the
mirror-flip tier still pays a full threat scan on 30.6% of king moves. That is M2-F3b.
