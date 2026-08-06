# M3-F5 — leaper attack lookup tables

**Date:** 2026-08-06
**Feature:** `M3-F5-leaper-luts` (audit finding 11)
**Change:** `crates/mf-core/src/attacks.rs` — knight/king/pawn attacks served from
`const` lookup tables instead of a per-call delta loop.
**Baseline:** `baselines/m2-nnue/manifold.exe` (bench 45,036, commit `c9fc454`) — still the
running baseline, since M3-F1/F2/F3 all shipped their features OFF.
**Decision: KEEP.** Bit-exact work removal, +7% search NPS and +26% perft NPS, zero
behavioural change anywhere in the anchor vector.

## What changed

`knight_attacks`, `king_attacks` and `pawn_attacks` each walked an offset table on every
call — bounds-checking a file/rank pair per delta, eight deltas for knight and king. All
three now index a table built at compile time:

```rust
const fn leaper_table<const N: usize>(deltas: &[(i8, i8); N]) -> [Bitboard; 64]

const KNIGHT_ATTACKS: [Bitboard; 64] = leaper_table(&KNIGHT_DELTAS);
const KING_ATTACKS:   [Bitboard; 64] = leaper_table(&KING_DELTAS);
const PAWN_ATTACKS: [[Bitboard; 64]; 2] =
    [leaper_table(&[(-1, 1), (1, 1)]), leaper_table(&[(-1, -1), (1, -1)])];
```

The tables are `const`-evaluated in `attacks.rs` rather than generated in `build.rs`. The
sliding tables need `build.rs` because black-magic generation is a randomized search; a
64-entry leaper walk is trivially const-evaluable, so adding it to the build script would
have bought nothing and made a compile-time constant harder to read.

`is_square_attacked_with_occupancy` and every other call site were left untouched — all
leaper use in the workspace (`mf-nnue/threats.rs`, `mf-search/repetition.rs`,
`mf-search/search.rs`, `mf-core/see.rs`, `movegen.rs`, `position.rs`) already funnels
through the three public functions, so swapping the bodies swapped every consumer.

`offset()` stays: `movegen.rs` still uses it for pawn pushes and castling paths.

## TDD

Two tests were written and watched fail (`cannot find value KNIGHT_ATTACKS in this scope`,
3 compile errors) **before** the function bodies were changed:

- `leaper_tables_match_the_delta_loop_on_every_square` — the delta loop is kept verbatim
  in the test module as the oracle; asserts the table entry equals it for all 64 squares,
  both colors for pawns.
- `leaper_lookups_serve_the_same_bits_the_delta_loop_produced` — the same assertion
  through the public API, so a future indexing bug in the accessor is caught even if the
  table itself is right.

## Correctness evidence

| check | result |
|---|---|
| `cargo test -p mf-core` (PEXT backend, debug) | 22 test binaries, all ok, 0 failed |
| `cargo test -p mf-core --features force-magic` | 22 test binaries, all ok, 0 failed |
| debug perft anchors (in both runs above) | unchanged |
| `manifold bench` × 2 consecutive | **45,036 / 45,036** — identical to baseline |
| full `bench_cli.rs` anchor vector (34 values) | **byte-identical** to `MSN-S6-thread-history/anchors-m2-nnue.txt` (`Compare-Object` → no differences) |
| `cargo test --workspace` (debug, incl. ~11 min `bench_cli`) | all green |
| `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -D warnings` | clean |
| perft 5 / perft 6 node counts | 4,865,609 / 119,060,324 — unchanged |
| depth-12 node counts, 4 positions (`nps_compare.py`) | ratio **1.00x** at every position |

Anchor vectors: `anchors-leaper-luts.txt` (collected with
`experiments/MSN-S1-qchecks/collect_anchors.ps1`).

## Measurements

### Perft NPS (pure movegen, the path this change dominates)

5 alternating repeats per build, median reported.

| build | perft 5 median NPS | perft 6 median NPS |
|---|---|---|
| `baselines/m2-nnue` | 41,104,773 | 43,416,913 |
| leaper-luts | **52,758,770** | **54,861,662** |
| delta | **+28.4%** | **+26.4%** |

perft-6 wall time: 2.72–2.80 s → 2.13–2.20 s, non-overlapping across all 5 repeats.

### Search NPS (`harness/nps_compare.py`, depth 12, Hash 64, Threads 1)

Two independent runs. Node counts are identical at every position in both runs, so every
ratio below is pure speed.

| position | run 1 (`--repeat 5`) | run 2 (`--repeat 7`) |
|---|---|---|
| startpos | 1.07x | 1.04x |
| kiwipete | 1.08x | 1.09x |
| midgame  | 1.77x † | 1.05x |
| endgame  | 0.88x † | 1.10x |
| **geometric mean** | 1.16x | **1.07x** |

† Run 1's midgame and endgame medians are noise, not signal: the baseline's endgame
samples spanned 594,665–968,193 NPS and its midgame samples 326,418–423,756 within a
single position. Run 2 (7 repeats) is the honest number. **Take +7% as the search NPS
result**, with the two positions that sampled cleanly in both runs (startpos, kiwipete)
bracketing it at +4% to +9%.

Raw JSON: `nps_run1.json`, `nps_run2.json`.

### UCI probe (Process-driven, `go movetime 1000`, kiwipete)

Repeated 3×, both builds alternating (`uci_probe.ps1`, transcript in
`uci-probe-transcript.txt`) — per the M3-F3 lesson that a single timed probe is jitter:

| rep | leaper-luts | m2-nnue |
|---|---|---|
| 1 | depth 14, 467,043 nodes, 855 ms | depth 14, 467,043 nodes, 910 ms |
| 2 | depth 14, 467,043 nodes, 833 ms | depth 14, 467,043 nodes, 884 ms |
| 3 | depth 14, 467,043 nodes, 832 ms | depth 14, 467,043 nodes, 903 ms |

Identical nodes, score (`cp -177`), seldepth and full PV in all six sessions; the ranges
(832–855 vs 884–910 ms) do not overlap. Same tree, ~6% less time.

## Why the perft gain is 4x the search gain

Expected and not a discrepancy. Perft is almost entirely movegen plus legality, which is
where the leaper calls live. In search, the M2-F1 profile puts NNUE at 44.6% of wall and
the remainder is spread over TT probes, move ordering, history and the search bookkeeping
— leaper attack generation is a small slice of a small slice. A ~26% win on the movegen
component landing as ~7% overall is the arithmetic working out, not a measurement problem.

## Decision

**KEEP, shipping unconditionally — no toggle.** The feature description's criterion was
"keep unless negative". It is not negative on any measurement, the change removes work and
adds no state (1.25 KiB of read-only tables), and the bit-exactness is proven at the level
of all 34 bench anchors rather than the default bench alone.

**No match was run.** The change cannot alter a single node of the tree, so a match could
only measure the NPS gain through Elo noise 3x wider than the effect. Per the repo's 1%
NPS ≈ 1.4 Elo rule the expected value is ~+10 Elo LTC, which will be absorbed by the M4
milestone match.

**The running baseline does NOT change.** `baselines/m2-nnue` remains the reference for
M3-F4: this build is functionally identical to it, so promoting a new baseline would only
mean measuring later features against a differently-named copy of the same behaviour.

## Reproduce

```powershell
cargo test -p mf-core
cargo test -p mf-core --features force-magic
cargo build --release
.\target\release\manifold.exe bench            # must print 45036, twice
.\experiments\MSN-S1-qchecks\collect_anchors.ps1   # must match anchors-leaper-luts.txt
python harness\nps_compare.py --engine leaper-luts=.\target\release\manifold.exe `
    --engine m2-nnue=.\baselines\m2-nnue\manifold.exe --depth 12 --hash 64 --warmup 1 --repeat 7
.\experiments\MSN-S5-leaper-luts\uci_probe.ps1 -Engine .\target\release\manifold.exe
```

Machine idle at both measurement windows (CPU 8–9%, no foreign `manifold`/`stockfish`
processes), per the mandatory pre-measurement checklist.
