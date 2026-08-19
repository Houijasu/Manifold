# Plan 009: A threshold-early-exit SEE predicate and lazy SEE in the qsearch picker

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 9ad3f87..HEAD -- crates/mf-core/src/see.rs crates/mf-core/src/instrumentation.rs crates/mf-search/src crates/mf-search/examples/see_profile.rs`
> This plan was written against commit `9ad3f87` (2026-08-19). If the excerpts below no
> longer match, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW for steps 0–2 (value-neutral by construction); MED for step 3 (capture ordering changes → node counts move, bench re-pin and a match required)
- **Depends on**: plans/001-stage-qsearch-move-loop.md (DONE — the staged picker this plan modifies)
- **Supersedes**: the un-landed steps 3–5 of plans/002 (002 is BLOCKED at its step 2; this plan drops that step entirely and keeps the full legality trial)
- **Category**: perf
- **Planned at**: commit `9ad3f87`, 2026-08-19

## Why this matters

SEE is **8.7 % of bench-mix wall time** (10.1 % over the see_profile mix) at
**1374 calls per 1000 nodes, ~118 ns/call** (measured 2026-08-19, TSC 2.419 GHz). The calls
fall into two kinds:

1. **Predicate calls** — "is this exchange worth at least X?" — where the full swap-off walk
   (up to 32 plies of apply/undo with x-ray reveals, then a minimax fold) is wasted work: a
   threshold walk can stop as soon as the answer cannot cross X, typically after 2–3
   recaptures.
2. **Ordering calls** in `load_captures`, where exact SEE is computed for *every* generated
   capture solely so the first yielded one is best-ordered. Most captures are never yielded
   (cutoffs land after 1–3 moves), so their SEE was paid for nothing.

Step 1–2 attack kind 1 value-neutrally. Step 3 attacks kind 2 in the qsearch picker, where
profile evidence shows SEE is consumed *only* as a predicate (see Current state), so the
per-capture exact walk can be deferred to yield time and downgraded to a threshold walk —
at the cost of changing capture ordering from SEE-weighted to MVV-LVA, which is a
strength-sensitive change gated by a match.

## Current state

- `crates/mf-core/src/see.rs` — the whole implementation. The public entry is a
  pass-through around `static_exchange_evaluation_greedy` (the full swap-off loop, `gains`
  fold at the end). The exhaustive minimax oracle
  (`static_exchange_evaluation_exhaustive`, test-only) and the differential battery in
  `see.rs` tests compare greedy against the oracle over 40k+ random capture positions.
  Plan 002's step-1 invariant hoists have landed; the step-2 king-only legality trial was
  BLOCKED (diverges from the oracle on off-target-ray pins, repro
  `2B2br1/.../1N6 w` `d5e6`) — **this plan keeps the full legality trial**.

- The SEE call sites, with their consumption kind:

  | Site | Location | Kind |
  |---|---|---|
  | `load_captures` | `move_ordering.rs` ~474 | ordering + predicate (shared) |
  | `validate_tt_move` | `move_ordering.rs` ~527 | value (feeds `current_capture_see` readback) |
  | interior SEE-pruning, quiets fallback | `search.rs` ~2724 | **pure predicate** |
  | `quiet_checks` gate | `move_ordering.rs` ~630 | **pure predicate** (feature default OFF) |
  | ProbCut threshold | `search.rs` ~2458 | reads memoized value — no fresh call |
  | interior SEE-pruning, captures | `search.rs` ~2723 | reads memoized value — no fresh call |

- `load_captures` computes **one** SEE per capture, shared by the ordering score, the
  good/bad split, the qsearch gate, and the `current_capture_see` readback. The ordering
  score (`capture_score_with_see`, `move_ordering.rs` ~711) is
  `see * 32 + victim * 16 - attacker + promotion + capture_history` — SEE dominates it.

- **The qsearch move loop never reads `current_capture_see()`** (`search.rs` ~3287–3361:
  delta pruning, make/unmake, recursion — no SEE consumer). In the qsearch variant, SEE is
  consumed *only* through the picker's load-time gate and good/bad split.

- `QSEARCH_SEE_THRESHOLD = 0` (`search.rs` ~3844). With `UseSEEPruning=true` (default)
  the qsearch gate drops exactly the `SEE < 0` non-promotions, which coincide with the
  good/bad split; with `UseSEEPruning=false` the threshold is `i32::MIN` and the gate must
  admit every capture. Promotions are exempt from the gate either way.

- Measured baseline (2026-08-19, this machine, release + `instrumentation`):
  see_profile BENCH row **1374.45 calls/kn, 117.3 ns/call, 8.7 % of wall**; TOTAL row
  1664.45/kn, 119.8 ns, 10.1 %. Bench signature **37420**.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| SEE oracle tests | `cargo test -p mf-core see` | all pass |
| Both sliding backends | `cargo test -p mf-core && cargo test -p mf-core --features force-magic` | all pass |
| Workspace gates | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` | exit 0 |
| SEE profile | `cargo run --release -p mf-search --example see_profile --features instrumentation` | per-site shares recorded |
| NPS compare | `py -3.14 harness/nps_compare.py --engine old=<exe> --engine new=<exe> --depth 12 --hash 64 --warmup 1 --repeat 5` | nodes ratio + NPS ratio |
| Bench | `cargo run --release -p mf-uci --bin manifold -- bench` | 37420 through step 2; re-pin after step 3 |
| Strength match | `.\harness\run_match.ps1 -OutDir experiments\<run-name> -Purpose '...' ...` | per AGENTS.md: 1T both engines → `-use-affinity -concurrency 8` |

## Scope

**In scope**:
- `crates/mf-core/src/see.rs` (the `see_ge` predicate)
- `crates/mf-core/src/instrumentation.rs` + `crates/mf-search/examples/see_profile.rs`
  (per-site counters)
- `crates/mf-search/src/move_ordering.rs` (qsearch-variant capture staging, quiet-checks
  gate)
- `crates/mf-search/src/search.rs` (the quiets-fallback gate swap only)

**Out of scope**:
- The exact-SEE contract: `static_exchange_evaluation`'s values and its oracle tests must
  not change.
- The full-search picker's capture ordering (interior nodes keep eager exact SEE; a
  follow-up plan can revisit after step 3's match verdict).
- The ProbCut path and `validate_tt_move` (both consume memoized/exact values).
- Restricting the legality trial (002's blocked step 2 — do not retry it here).

## Git workflow

- One commit per step or logical pair: `Add a threshold-early-exit SEE predicate`,
  `Route SEE threshold gates through see_ge`, `Score qsearch captures without eager SEE`
  (+ the re-pin commit recording old → new signature and the match evidence).

## Steps

### Step 0: per-site SEE counters — DONE (08fec3b)

Extend `SeeCounters` with one counter per call site (`load_captures`, `tt_validation`,
`interior_quiets_fallback`, `quiet_checks`; record from those four sites exactly). Report
the per-site shares in `see_profile`'s totals. This sizes step 3's ceiling before it is
built: the fraction of `load_captures` calls whose capture is never yielded.

**Verify**: instrumentation build green; default build's bench still 37420 (counters are
feature-gated); per-site shares recorded here (2026-08-19, profile mix, 668,558 nodes):

- load_captures: **1201.24 calls/kn — 72.2 % of SEE calls** (the step-3 ceiling)
- tt_validation: 48.43 calls/kn (2.9 %)
- interior_quiets_fallback: 414.78 calls/kn (24.9 % — step 2's target)
- quiet_checks: 0.00 calls/kn (as expected at default options — `UseQSearchChecks=false`)

Cross-check: the four sites sum to exactly the mf-core `see_calls` total (1,112,784), so
they are exhaustive.

### Step 1: `see_ge(position, mv, threshold) -> bool` — re-plan of 002 step 3 — DONE (48e5b49)

Implement the classic swap-list loop with an early exit: walk the exchange maintaining the
best/worst achievable result for the side to move, and return as soon as the answer cannot
cross `threshold`. Model it on `static_exchange_evaluation_greedy` (same `gains`
structure, `apply_recapture`/`reveal_xray` cadence, **full legality trial**) but with the
cutoff. Correctness contract: `see_ge(p, m, t)` ≡ `static_exchange_evaluation(p, m) >= t`
for all positions, moves, and thresholds.

**Verify**: add `see_ge` to the existing differential battery (same random-position
generator, thresholds `{-200, -100, -50, 0, 50, 100, 200}`);
`cargo test -p mf-core see && cargo test -p mf-core --features force-magic` → all pass.

### Step 2: route the pure-predicate sites through `see_ge` (value-neutral) — DONE (65dfc06)

**Execution record (2026-08-19)**: bench 37420 exact across three runs; all 73 workspace
test targets green; depth-12 node counts identical on all four `nps_compare` positions.
Default-build NPS A/B vs the previous commit (swapped engine order, 5–9 repeats): geomean
0.98x–1.01x — neutral to ~2 % positive; the midgame position's apparent dip was scheduling
luck (reversed at 9 repeats). The instrumented `see_profile` per-call figure rises
(120 → 133 ns, stable across three runs) — an instrumentation/code-layout artifact, since
the predicate walk is provably not longer than the exact walk and the default build shows
no end-to-end regression.

- Interior SEE-pruning quiets fallback (`search.rs` ~2724):
  `unwrap_or_else(|| static_exchange_evaluation(position, mv)) < threshold` becomes
  `current_capture_see().is_none_and(|_| !see_ge(position, mv, threshold))`-shaped — keep
  the memoized path for captures, replace only the fresh walk.
- `quiet_checks` gate (`move_ordering.rs` ~630):
  `static_exchange_evaluation(...) < QUIET_CHECK_SEE_THRESHOLD` →
  `!see_ge(..., QUIET_CHECK_SEE_THRESHOLD)`.
- Leave exact SEE wherever the value is consumed: `load_captures` (score), 
  `validate_tt_move` (readback), and the memoized reads.

**Verify**: `cargo test --workspace` → all pass; **bench must still be 37420**. A moved
signature means the equivalence is broken — STOP, do not re-pin your way past it.

### Step 3: lazy SEE in the qsearch picker (strength-sensitive)

Restructure the **qsearch variant only** (`MovePicker::qsearch`, `qsearch_see_threshold`
set):

- Score captures WITHOUT SEE at load time: `victim * 16 - attacker + promotion +
  capture_history` (drop the `see * 32` term for this variant; the full-search variant
  keeps its eager exact-SEE score).
- Gate each candidate at yield time with `see_ge(mv, threshold)`, skipping failures exactly
  as the load-time gate does today — promotions exempt, `UseSEEPruning=false` admitting
  everything.
- Resolve the good/bad staging against the yielded set: with `UseSEEPruning=true` the
  threshold is 0, so today's load-time gate already drops every `SEE < 0` non-promotion
  and the yielded SET is unchanged by deferral; with `UseSEEPruning=false` bad captures
  must still be yielded after good ones. If reading the staging shows the yielded set
  cannot be preserved, STOP and report the conflict.

The yielded ORDER changes (SEE-weighted → MVV-LVA) — that is the point, and why the bench
signature moves.

**Verify**: `cargo test --workspace` → all pass (picker tests that pin exact ordering for
the qsearch variant must be re-pinned — record old → new expectations); bench signature
re-pinned old → new; see_profile's `load_captures` share collapses toward
"yielded captures only".

### Step 4: measure

`nps_compare` depth 12 (old vs new binaries) and 1-thread `mtbench` before/after. Record
the NPS ratio and the see_profile share change. Expected ceiling: the step-0-measured
never-yielded fraction of the 8.7 % SEE share, plus the cheaper predicate on yielded
captures.

### Step 5: strength gate

Run the match through `harness/run_match.ps1` into `experiments/<run-name>/` per the
AGENTS.md harness rules (both engines `Threads=1` → `-use-affinity -concurrency 8`;
never call fastchess directly). Keep step 3 if Elo is within noise or positive; revert
step 3 (steps 0–2 stand on their own) if clearly negative. Record the verdict in the
README row.

## Test plan

- Differential: `see_ge` ≡ exact-SEE comparison across the 40k-position battery and the
  threshold list (step 1).
- Regression: both sliding backends (`force-magic`) — SEE is backend-independent, but the
  repo rule is to test both.
- Node-count: bench unchanged (37420) through step 2; re-pinned with old → new after
  step 3.
- Strength: match evidence for step 3 recorded under `experiments/`.

## Done criteria

- [x] All workspace gates exit 0 (through step 2; re-run after step 3)
- [x] `cargo test -p mf-core see` green including the new predicate differential (default and force-magic backends)
- [x] Exact-SEE oracle tests unchanged and passing (through step 2)
- [ ] Bench signature 37420 unchanged through step 2; re-pinned with old → new after step 3
- [x] Per-site SEE shares (step 0) and see_profile before/after recorded
- [x] `nps_compare` ratio recorded (steps 0–2: 0.98x–1.01x vs pre-009; step 3 pending)
- [ ] Match verdict for step 3 recorded

## STOP conditions

- `see_ge` cannot be made exactly equivalent to the exact-SEE comparison on the battery
  (do not ship an approximate predicate).
- The bench signature moves after step 2 (equivalence broken — diagnose before proceeding).
- Step 3 cannot preserve the qsearch yielded set (gate semantics) — report the staging
  conflict instead of changing which moves qsearch searches.
- Step 5's match shows a clear loss beyond noise → revert step 3 only, keep steps 0–2.
- The excerpts above do not match live code.

## Maintenance notes

- Any new search feature that gates on SEE should default to `see_ge`; exact SEE is for
  ordering scores and value readbacks only.
- Interior-node lazy SEE is the follow-up, contingent on step 3's match verdict: interior
  ordering is SEE-informed to serve deeper cutoffs, and ProbCut plus the interior
  SEE-pruning site depend on the picker's memoized `current_capture_see` — a lazy interior
  variant must keep memoizing for those readers or route them through predicates too.
- If SEE ever gains true pin-awareness (beyond the king trial), the step-1 equivalence
  contract is the safety net — keep the differential test alive.
