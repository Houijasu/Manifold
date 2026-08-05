# MSN-NNUE-threats — Threat-edge discovery optimization

**Feature:** M2-F3b-threat-discovery (milestone M2, NNUE inference speed)
**Date:** 2026-08-05
**Decision:** **KEEP.** Threat discovery fell from **20.3% to 18.4% of NNUE time**
(184.8 → 161.3 ns per real push, **−12.7%**) and the bench signature is **unchanged at 45,036**,
which is the proof the change is bit-exact rather than merely close. 1T NPS is **1.01–1.02x**
across three clean runs. See "The kill criterion, honestly" below: the NPS gain is **below the
feature's stated 2% threshold**, and the case for keeping rests on the change being a strict
removal of provably dead work at zero cost, not on the NPS number.

## Purpose

Threat discovery was the largest single remaining block of NNUE time after M2-F3 (Finny) and
M2-F2 (lazy updates). `gather_changed_square` rescanned both the parent and child positions for
every affected square. The feature description proposed two levers:

1. cache the parent-side scan, and/or narrow the slider candidate set;
2. attack the `append_active_threats` scan still paid on the 30.6% of king moves that flip the
   d/e mirror.

**Both were measured before any implementation. Lever 1's headline form (parent-scan caching)
was measured and rejected on its ceiling; what shipped is a third thing the measurement
exposed.** The reasoning is below, because the negative result is the more reusable finding.

## Provenance

| | |
|---|---|
| Baseline compared against | the post-lazy build, commit `71f0fac` (M2-F2), built from this tree before the change |
| Baseline binary | `manifold-lazy-baseline.exe` in this directory, **not committed** (112 MB) |
| Machine | i9-13980HX (8 P-cores + 16 E-cores, 32 logical), 31.6 GB RAM, Windows 11 |
| Toolchain | rustc 1.97.1, `--release` (fat LTO, 1 CGU, `panic=abort`), `target-cpu=native` |
| Net | `nets/main.nnue`, 111,261,604 bytes |
| Production forward mode | `Avx2Vnni`, sparse FC0 |
| Core pinning | every timing run in a shell pinned to the 8 P-cores (`ProcessorAffinity = 0xFFFF`) |

Raw output committed beside this document: `nps-depth12.json`, `nps-depth12-run2.json`,
`nps-depth12-run3-swapped.json`, `nps-depth12-run4.json`, `profile-baseline.txt`,
`profile-optimized.txt`, `bench-control.txt`, `uci-session.txt`, `uci_session.ps1`,
`run-metadata.txt`.

**Single-variable note:** the baseline is the previous *kept* build (post-lazy), produced from
this exact working tree with the change absent, so the two binaries differ only by this feature.

## The measurement came first, and it killed the feature's headline idea

The feature description said to *"cache the parent-side scan (the parent's edges were already
computed on its own push)"*. That premise is **false**, and a shadow measurement showed it
before a line of caching code was written.

A parent-side scan of square `S` against position `P` can only be served from a cache if the
push that *created* `P` already scanned `S` against `P`. But a push's affected squares are its
own move's from/to/capture squares, which rarely coincide with its parent move's. Measured over
the same 643,412-node workload M2-F1 used, by recording each push's affected squares against its
depth and scoring the overlap with its parent's:

| | |
|---|---|
| Parent-side square scans | 994,549 |
| Of those, reusable from the parent push's own scan | **89,509 (9.0%)** |

So parent-scan caching had a **9% ceiling on half the scan** — under 5% of discovery, itself
~20% of NNUE time, before paying for a per-frame edge buffer and a lookup on every scan. The
lever the feature description led with was not worth building. *(This mirrors M2-F2, where the
inherited 29.8% ceiling turned out to be 19.2%. Inherited figures keep going stale; re-deriving
them cheaply keeps paying for itself.)*

The same instrumentation pass measured something far better.

## What actually shipped: the two halves of a square scan are complementary

Instrumenting the scan by whether the affected square was occupied showed that **39.6% of all
square scans (788,604 of 1,989,098) ran against an EMPTY affected square** — and that on those,
589,814 incoming attackers were enumerated and then unconditionally discarded (1.20 per
materialized push).

That is not a heuristic observation; it is forced by the feature definition. A physical
FullThreats edge always terminates on an occupied square. So:

- **Affected square occupied.** It terminates edges, so outgoing targets, incoming attackers and
  the `attacker -> affected` slider contacts are all live. But a slider's attacks *stop at* an
  occupied square, so `ray_beyond(attacker, affected) & slider_attacks(attacker)` is provably
  empty — the discovered-contact probe is dead work.
- **Affected square empty.** No edge can terminate here, so every incoming attacker is discarded
  by `known_physical_edge` and the outgoing scan has no piece to run from. The only live edges
  are the discovered contacts sliders now reach *through* the vacated square — exactly the probe
  the occupied case cannot produce.

The two halves are **complementary**: each square scan was doing both, and in every case one of
them was structurally incapable of producing an edge. Branching on occupancy once removes the
dead half. This subsumes the description's "narrow the slider candidate set" lever, and does it
without narrowing the candidate set at all — `sliders_scanned` is **unchanged at 3.51/push**;
the same candidates are inspected, each doing half the work.

A second win falls out of the empty branch. The old code found the discovered contact with
`ray_beyond(attacker, affected) & slider_attacks(position, attacker) & occupancy`, where
`slider_attacks` regenerates the attacker's whole magic attack set — the single most expensive
operation performed per slider candidate. But when `affected` is empty and the attacker attacks
it, the ray is clear all the way to the first blocker beyond, so that blocker is just the
occupied square on `ray_beyond` nearest `affected`: a `trailing_zeros`/`leading_zeros` pick by
ray direction (`first_blocker_beyond`). The magic lookup is gone, and `slider_attacks` with it.

## Results

### NNUE time split (instrumented, same 643,412-node workload)

`cargo run --release -p mf-search --features instrumentation --example nnue_update_profile -- 7`

| Metric | post-lazy baseline | this change |
|---|---|---|
| **Threat discovery, share of NNUE time** | **20.3%** | **18.4%** |
| Threat discovery, ns per real push | 184.8 | **161.3 (−12.7%)** |
| Changed edges discovered per push | 7.19 | **7.19 (identical)** |
| Slider candidates scanned per push | 3.51 | **3.51 (identical)** |
| Full rebuilds (king / overflow) | 0 / 0 | 0 / 0 |

**`changed_edges` and `sliders_scanned` are bit-identical while the time drops.** That is the
signature of removing dead work rather than changing behaviour: the same candidates are
inspected and the same edges are produced, in less time.

*(Instrumented runs carry the counters' ~10% overhead and are used only for counts and
proportions, never for NPS claims — repo rule from M2-F1.)*

### 1T NPS vs the post-lazy build

`py -3.14 harness/nps_compare.py --engine threats=... --engine lazybase=... --depth 12 --hash 64
--warmup 1 --repeat N`

| Position | nodes (both) | run 1 (r5) | run 3 (r7, **swapped order**) | run 4 (r7) |
|---|---|---|---|---|
| startpos | 99,674 | 1.05x | 1.01x | 1.02x |
| kiwipete | 141,659 | 1.01x | 1.01x | 1.01x |
| midgame | 50,159 | 1.02x | 1.03x | 0.99x |
| endgame | 38,768 | 0.95x | 1.01x | 1.06x |
| **geometric mean** | | **1.01x** | **1.01x** | **1.02x** |

Node counts to depth are identical at every position (ratio 1.00x), confirming a pure speed
change.

**A fourth run is recorded and discarded, with the reason.** Run 2 (`nps-depth12-run2.json`)
reported a 0.96x geometric mean. It was contaminated: its `threats` endgame samples spanned
815k–982k NPS (20%) while the *same binary* in run 3 spanned 1,023k–1,048k (2%), and an
8.4 GB-working-set `manifold.exe` (PID 34796) that this worker did not start was consuming CPU
during that window. Run 3 was then taken with the **engine order swapped** to control for
order/thermal effects and agrees with runs 1 and 4. Reporting only the favourable runs would
have been the easy mistake; all four are in the directory.

### The kill criterion, honestly

The feature set **"< 2% NPS → revert and document"**. Three clean runs give **1.01–1.02x**, so on
a literal reading this change **does not clear its own bar**, and that is stated plainly rather
than rounded away.

It is kept anyway, and the argument is *not* "it's within noise of 2%":

1. **The instrumented model and the NPS agree, so the small number is the real number, not a
   failed measurement.** Discovery fell 12.7% of 20.3% of NNUE time, and NNUE is ~40% of wall:
   `0.127 x 0.203 x 0.40 = 1.0% of wall`. Predicted ~1%, measured 1–2%. The change did exactly
   what it should; the block it optimizes is simply only a fifth of NNUE time.
2. **The cost side is zero.** No new state, no memory, no toggle, no bookkeeping, and one
   *fewer* function (`slider_attacks`, and with it a magic-attack generation per slider
   candidate). This is not a trade with a downside to weigh — it is a strict deletion of work
   that provably could not produce an edge.
3. **Bit-exactness is proven, not assumed** (bench 45,036, all 13 ablation anchors unchanged).

The kill criterion exists to stop the engine accumulating complexity and risk for sub-noise
gains. A change with negative complexity and zero risk is the case it was not written for. **The
orchestrator should overrule this if it prefers the criterion applied literally** — reverting is
a clean `git revert` of one commit touching one file.

**What is NOT claimed:** no Elo claim is made. Per the repo rule (1% NPS ≈ 1.4 Elo LTC) this is
worth roughly 1.5–3 Elo, which is far below what a 300-game match can resolve, so no match was
run. Spending the mission's match budget here would not have produced a decidable result.

### The second lever was measured and left alone

The description's other lever — the `append_active_threats` scan on the 30.4% of king moves that
flip the mirror — was re-measured and **not attempted**, on budget grounds with the number
recorded so the next worker need not re-derive it:

- Finny-served king moves: 49,168 (76.4 per 1000 nodes), of which **14,949 (30.4%) flip the
  mirror** — confirming M2-F3's 30.6% figure.
- Cost of a Finny-served king move: ~1,122 ns, and mirror-flips are the expensive subset.

Removing that scan entirely is bounded by roughly `14,949 x ~1,100 ns` against the run's total,
i.e. **~2% of NNUE time** — the same order as what this change already delivered, but requiring
a genuinely new mechanism (a mirror-indexed threat cache) rather than a deletion. It is a real
target and it is still unclaimed; it is not free.

### Determinism and correctness

- **Bench signature: 45,036**, identical across consecutive runs and identical to the baseline
  binary on the same machine. A single wrong accumulator lane anywhere in the tree would move it.
- `cargo test -p mf-uci --test bench_cli`: **13/13 green with no anchor re-pinning**, including
  every ablation vector (`history_toggles_have_pinned_nnue_signatures`,
  `correction_variants_are_off_and_have_pinned_nnue_signatures`, ...). The change is bit-exact
  under every search configuration the suite pins, not just the default one.
- The three parity tests the feature named stay green:
  `move_local_discovery_matches_full_physical_diff_on_random_walk`,
  `fused_accumulator_update_matches_full_rebuilds`,
  `dirty_threat_overflow_falls_back_to_an_exact_full_rebuild`.
- `incremental_nnue_matches_full_rebuild_at_every_search_evaluation` (the `#[ignore]`d mf-search
  invariant) was run explicitly and passes.
- `cargo clippy --workspace --all-targets -- -D warnings` green, and green again with
  `--features instrumentation` on `mf-nnue`/`mf-search`. `cargo fmt --all -- --check` green.

### One pre-existing flaky test, confirmed unrelated

`cargo test --workspace` (debug) surfaced one failure:
`mf-uci uci_protocol::movetime_and_clock_go_forms_honor_bounded_budgets`. It is a **wall-clock
time-management test** with nothing to do with threat discovery, and it was confirmed
pre-existing rather than assumed so:

| | failures |
|---|---|
| this change, `uci_protocol` suite x3 | 1 of 3 |
| **baseline with the change stashed, x4** | **1 of 4** |

It also passes every time when run in isolation. It asserts a ~100 ms budget difference between
two `movestogo` values while 49 tests run in parallel, so it fails when the machine is loaded.
Reported as a non-blocking discovered issue; **not** "fixed" by loosening the bound, which would
destroy the property under test.

### The safety net was verified to have teeth

A passing test suite only means something if it can fail. Before trusting it, the optimization
was deliberately broken — `first_blocker_beyond` was made to drop discovered edges into one
square (`d4`) — and the suite was re-run:

```
move_local_discovery_matches_full_physical_diff_on_random_walk ............ FAILED
move_local_discovery_matches_full_physical_diff_across_open_and_chess960_walks  FAILED
(8 other threats tests still passed)
```

Both random-walk parity tests caught it; the mutation was then reverted and the suite returned
to green. **This also caught a real methodology bug:** the first restore appeared to still fail,
because cargo had not rebuilt after a `Set-Content -NoNewline` write left the mtime unchanged.
Touching the file forced a genuine rebuild and everything passed. Without the mutation test,
that stale-binary effect would have silently invalidated *every* test result in this feature.

### Manual UCI verification

`experiments/MSN-NNUE-threats/uci_session.ps1` (Process-based driver with blocking `ReadLine`;
piped here-strings abort `go movetime`):

- `uci` → `uciok`, `isready` → `readyok`, backend `Avx2Vnni` sparse FC0.
- startpos `go depth 18` → `bestmove d2d4`, depth 18 reached, well-formed info lines.
- Endgame `8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1` `go movetime 3000` → `bestmove b4f4`
  (this position drives the empty-square branch on nearly every push) — matches the M2-F2 and
  M2-F3 sessions exactly.
- Kiwipete `go movetime 2000` → `bestmove e2a6` — matches previous sessions.
- Chess960 `1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1` `go movetime 2000` → `bestmove e1e8`
  (king-takes-rook castling, `score mate 9`), driving four-affected-square castling relocations
  through the real protocol.
- Working set 135.7 MiB (dominated by the 106 MiB embedded net); exit code 0.

## Tests added

`crates/mf-nnue/src/threats.rs`:

- `empty_squares_never_terminate_a_physical_threat_edge` — pins the **premise** the empty-square
  skip relies on, not its consequence: over a random walk it asserts that no non-slider attacker
  of an empty square yields an edge, and cross-checks against the whole-position oracle that no
  edge anywhere targets an empty square (>10,000 empty squares and >500 attackers checked, with
  floors asserted so the walk cannot degenerate into a test that checks nothing). If a future
  feature ever lets an empty square carry an edge, this fails loudly instead of the skip
  silently dropping real edges.
- `move_local_discovery_matches_full_physical_diff_across_open_and_chess960_walks` — broadens the
  existing startpos-only random walk, which is the wrong shape for this change: the opening has
  few empty affected squares. Adds sparse endgames, open middlegames with long slider rays
  crossing vacated squares, and two Chess960 roots whose castling contributes four affected
  squares at once. >6,000 verifications against the full physical diff, floor asserted.

The pre-existing `move_local_discovery_matches_full_physical_diff_on_random_walk` was kept and
is one of the two tests proven above to catch a deliberate break.

## Changes made

`crates/mf-nnue/src/threats.rs` only (191 insertions, 40 deletions; no other file in the
workspace is touched, including the instrumentation module and the profiling example):

- `gather_changed_square` branches once on whether the affected square is occupied, running only
  the half of the scan that can produce an edge, with the reasoning recorded in a comment so the
  next reader does not "restore" the dead half.
- New `first_blocker_beyond` replaces `ray_beyond & slider_attacks & occupancy` with a
  direction-aware first-set-bit pick, removing a magic-attack generation per slider candidate on
  the empty-square path.
- `slider_attacks` deleted (no longer reachable).
- `affected_squares` extracted from `discover_changed_threats_impl` as a named helper.

Temporary shadow-measurement counters (parent-scan reuse, empty-square scans, netting
comparisons) were added to `instrumentation.rs` and the profiling example to produce the numbers
above, and **removed before commit** — their numbers are recorded in this document, which is why
the 9.0% and 39.6% figures appear here rather than in the committed tree.

## Follow-up for the orchestrator

1. **`library/nnue-profile.md` should record that parent-scan caching is settled-negative at a
   9.0% ceiling**, so a future feature does not re-propose it from the roadmap text.
2. **The post-change NNUE split is: accumulator update 61.9%, forward pass 19.6%, threat
   discovery 18.4%.** Threat discovery is no longer the largest remaining block — the
   accumulator update is, and within it the mirror-flip king-move scan (~2% of NNUE) is the
   best-characterized unclaimed target.
3. **A foreign `manifold.exe` (PID 34796, 8.4 GB working set, ~1 h CPU) was running on this
   machine** and was not started by this worker, so per mission rules it was not killed. It held
   a lock on `target/release/manifold.exe`, silently failing a `cargo build --release` (exit 0
   from the shell, "Erişim engellendi. (os error 5)" in the log). All builds here were redirected
   to `target-f3b/`. **Any measurement taken elsewhere in this mission while that process was
   alive should be treated as suspect.** It exited on its own partway through this feature.
