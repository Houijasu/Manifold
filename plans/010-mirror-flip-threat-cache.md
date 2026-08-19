# Plan 010: Derive mirror-flip FullThreats from the parent's active edge set

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat f3651cd..HEAD -- crates/mf-nnue/src/accumulator.rs crates/mf-nnue/src/threats.rs crates/mf-nnue/src/finny.rs crates/mf-nnue/src/instrumentation.rs crates/mf-search/examples/nnue_update_profile.rs`
> This plan was written against commit `f3651cd` (2026-08-20). If the excerpts below no
> longer match, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M-L
- **Risk**: MED — touches the hottest data structure in the engine, but the change is
  value-neutral **by construction** (the derived index set must equal the scanned one
  bit-for-bit; wrapping i16/i32 addition is commutative, so even a different summation
  order cannot move a value), so the bench signature — not a match — is the gate.
- **Depends on**: nothing open (plan 005's prefetches are DONE; this is the structural
  fix the README investigate list has named since 2026-08-19)
- **Category**: perf
- **Planned at**: commit `f3651cd`, 2026-08-20

## Why this matters

NNUE is **45.7 % of wall** over the profile mix (nnue_update_profile, 2026-08-20,
671,669 nodes, TSC 2.419 GHz). Inside it, Finny-served king moves cost **77.674 per
1000 nodes at 841.3 ns each ≈ 4.0 % of wall**, and **28.3 % of them are mirror flips**
(the king crosses the d/e file line, 21.99 flips/kn).

A flip invalidates every FullThreats index for the mover's perspective —
`ORIENT_TABLE` keys the orientation off the king's file — so `update_from`'s flip
branch calls `rebuild_threats_onto`, which does two things:

1. **A full board scan** (`append_active_threats`): re-enumerates every pawn contact,
   knight contact, and slider ray of the child position to rediscover an edge set that
   the parent already knew — the king was the only piece that moved.
2. **A full row re-stream**: every active threat row (1 KiB each from the ~62 MB
   `ThreatWeights` table) is re-added onto the Finny base, because every index changed.

The AVX2 `rebase_accumulator` kernel (2026-08-19) proved the piece half is already
near its memory-bound ceiling (~2 % NPS from vectorizing a 1024-iteration loop), which
is what promoted this item to a plan: the flip path is the remaining structural waste.
**This plan eliminates (1), the scan. (2) is not removable** — the child sums genuinely
different rows — and the plan must not claim otherwise. The honest ceiling is therefore
the scan's share of the flip path times 2.2 % of nodes; step 0 measures that share and
gates the whole plan on it.

## The invariant the design rests on

- **Physical FullThreats edges are perspective- and orientation-independent.**
  `append_active_threats` loops `for color in [perspective, !perspective]` — both
  perspectives enumerate the same physical edges; only the *encoding* (`swap`) and the
  *index* (`orientation` from the king square) differ. `DirtyThreat` is documented as
  "perspective-independent" and packs exactly the physical bits:
  `attacker | attacked << 4 | from << 8 | to << 14` (20 bits, sign in bit 20).
- **Per-perspective index exclusion is applied at index time, not enumeration time**:
  `emit` and `append_changed_threat_indices` both drop `index >= DIMENSIONS`. A cached
  *physical* edge set therefore serves both perspectives: index it per perspective with
  the child's king square and re-apply the same exclusion.
- **The per-move physical deltas are already discovered.** Every materialized real
  frame runs `discover_changed_threats`; the mirror-held king-move path already trusts
  these deltas to update the threat contribution. The same relation holds on a flip —
  physical edges don't care about orientation — so
  `child_edges = parent_edges ⊕ changed_edges`, always.

The flip branch ignores all of this today and rescans from scratch.

## Why the cache must be LAZY (measured constraint)

An eager per-push netting of a per-frame edge set would cost O(changed_edges ×
set_size) per push — at the measured 7.44 changed edges/push over a ~30-40 edge set,
roughly a hundred nanoseconds on **every** push (~951 pushes/kn), which is an order of
magnitude MORE than the ~22 flips/kn this plan optimizes. Net loss. So:

- **No per-push set maintenance.** Materialization instead *stores the frame's delta
  prefix* (the `ChangedThreatBuffer` entries, ~30 B average — copy the used prefix
  only) into a per-depth side array.
- **A flip derives the parent's edge set on demand**: walk from the flip's parent
  frame down to the nearest ancestor with a valid edge slot, applying the stored
  deltas forward, populating each slot it passes through (so consecutive flips — king
  walks — hit a warm ancestor immediately), then apply the flip move's own deltas from
  scratch. The root slot is seeded once at `AccumulatorStack::build`.
- **O(1) invalidation on pop** (clear the popped depth's slot): slots are per-depth
  arrays, and a sibling push at the same depth must never read the previous line's set.

Fallbacks keep every slot honest: a `MAX_ACTIVE` cap hit invalidates the slot (the
scan's drop-past-cap semantics and the derived order would otherwise drop different
edges — a real divergence, not a theoretical one); a frame whose stored delta is the
overflow marker (its state was fully rebuilt) becomes a walk barrier — scan that
frame's *position* (frames store their position eagerly) to repopulate and continue.

## Steps

### Step 0: size the ceiling with instrumentation (gate for everything after) — DONE (2026-08-20)

Add to `UpdateCounters` (feature-gated, thread-local, same pattern as `finny_cycles`):

- `finny_threat_rebuild_cycles` — cycles inside the flip branch (the `else` arm of
  `mirror_held` in `update_from`, around `rebuild_threats_onto`).
- `threat_scan_cycles` — cycles inside `rebuild_threats_onto`'s
  `append_active_threats` call only.
- `threat_scan_edges` — sum of active-threat counts returned by those scans.

Extend `nnue_update_profile`'s totals to print per-flip nanoseconds, the scan's share
of the flip path, and the projected ceiling:
`flips/kn × scan_ns_per_flip / node_wall_ns`, as % of wall.

**Verify**: `cargo test -p mf-nnue --features instrumentation` green;
`cargo run --release -p mf-search --features instrumentation --example nnue_update_profile`
records, over the profile mix: flip count/kn, per-flip ns, scan share, projected
ceiling %. Default build untouched (counters are feature-gated): bench 37557.

**GATE — STOP here if the projected ceiling is below 0.5 % of wall.** Record the
measurement in `plans/README.md`'s investigate item as REJECTED-by-measurement and
retire it: nobody should re-audit a named ceiling that measurement has retired. (For
orientation: the AVX2 rebase kernel landed ~2 %; flips are 2.2 % of nodes, so the scan
share must be ≥ ~25 % of a ~1.5 µs flip for the plan to clear the bar — plausible but
unproven until this step runs.)

**Execution record (2026-08-20)**: measured over the profile mix (671,669 nodes,
TSC 2.419 GHz, two runs agreeing):

- flips = 14,747 (**22.0 / 1000 nodes**), whole flip path **974–998 ns per flip**
- board scan = **256–258 ns per flip = 25.6–26.5 % of the flip path**, 24.43 edges/scan
- rows + prefetch = **716–742 ns per flip** — the dominant cost, and unremovable
- **projected ceiling: scan = 0.32 % of wall — BELOW the 0.50 % gate, in both runs**

**Verdict: REJECTED-BY-MEASUREMENT.** Steps 1–4 are not executed. The scan the cache
would remove is worth 0.32 % of wall at best, before subtracting the walk + delta-apply
replacement cost and ~200 KB per worker; the flip path's real cost is streaming
re-indexed threat rows, which no edge cache can avoid because every FullThreats index
changes on a flip. The step-0 instrumentation (three counters plus the profile's
flip-path and ceiling lines) is kept so this stays cheap to re-check on future hardware
or nets. Default build untouched: bench 37557 exact.

Side find: `cargo test -p mf-nnue --features instrumentation` had two pre-existing
failures at `f3651cd` — `a_king_move_that_{keeps,flips}_the_mirror_*` read Finny
counters immediately after `push_real`, but lazy updates defer that work to
materialization. Nobody had run the instrumentation suite since the lazy-update
change landed (it is not part of the default workspace run). Both tests now
materialize via `current()` before asserting; 68/68 instrumentation tests green.

### Step 1: `append_active_threat_edges` — physical-edge enumeration (value-neutral)

Split the enumeration from the indexing in `threats.rs`:

- `pub(crate) fn append_active_threat_edges(position: &Position, buffer: &mut [u32; MAX_ACTIVE]) -> usize`
  walks exactly `append_active_threats`' walk (same loops, same contacts) but stores
  `DirtyThreat::physical_bits()`-shaped words — attacker/attacked/from/to, no sign —
  with the same `count < MAX_ACTIVE` cap.
- `append_active_threats` becomes enumerate-then-index: walk once, then per
  perspective call a new `index_active_edges` helper that loops the edges through
  `make_index` and applies the `index < DIMENSIONS` filter — the exact loop shape of
  `append_changed_threat_indices`, minus the sign split.

`emit` currently looks the victim up via `position.piece_at(to)`; the edge-packing
walk keeps doing that (the position is in hand during enumeration), and the *re-index*
path needs neither the position nor a victim lookup — everything is in the packed word.

**Verify**: `cargo test -p mf-nnue` green (the king-walk/castling/Chess960 parity
tests and `eonego_parity` pin behavior bit-for-bit); `cargo test -p mf-nnue --features
force-magic` is irrelevant here (no sliding-attack change) but cheap; **bench 37557
exact**; fmt/clippy clean. Commit alone.

### Step 2: lazy edge slots + flip-path rewiring (the feature)

In `accumulator.rs`:

- `AccumulatorStack` gains parallel side arrays (allocated in `build`, keeping
  `accumulator_stack_allocation.rs`'s no-alloc-after-creation contract green):
  - `edge_slots: Box<[[u32; MAX_ACTIVE]]>` + `edge_lengths: Box<[u16]>` with
    `u16::MAX` as the invalid sentinel (~132 KB),
  - `frame_deltas: Box<[[u32; MAX_CHANGED]]>` + `frame_delta_lengths: Box<[u8]>` +
    an overflow marker bit (~66 KB).
- `build` seeds `edge_slots[0]` via `append_active_threat_edges(root)`.
- `materialize`'s Null arm copies the parent's slot length... no — null frames change
  no physical edge, so a null frame stores an **empty delta**; the walk steps through
  it for free. No slot copy.
- `apply_real`, after discovery and before `update_from`: store the delta prefix into
  `frame_deltas[depth]` (overflowed buffer → marker); if this move flips the mirror
  for the moving perspective (`!mirrors_alike(parent king, child king)`, same check
  `update_from` makes), derive the **child's** edge set by the backward walk, apply
  the move's own deltas, and hand the set to `update_from` through `UpdateContext`
  (new field `child_edges: Option<(&[u32], usize)>`).
- `update_from`'s flip branch: when `child_edges` is `Some`, index them per
  perspective (`index_active_edges` with the child king square), prefetch
  (`prefetch_threat_rows`), and stream rows onto the Finny base — replacing the
  `append_active_threats` call inside `rebuild_threats_onto`. `None` (barrier
  fallback) keeps today's scan. The derived index order differs from the scan order —
  that is fine and must stay fine: wrapping addition is commutative, so the
  accumulator and PSQT sums are bit-identical.
- `pop` invalidates the popped depth's slot (`edge_lengths[old] = u16::MAX`) — O(1).
- A walk that cannot find any valid ancestor before hitting an overflow-barrier frame
  scans that frame's position to repopulate its slot, then continues forward. (The
  root is always valid, so the walk always terminates.)
- Instrumentation: flips served from the cache increment a `finny_threat_cache_hits`
  counter; `threat_scan_cycles` then collapses toward root/overflow scans only —
  a built-in self-check that the fast path actually engaged.

**Verify (all must pass before committing)**:

- `cargo test --workspace` → all pass, including the new tests below.
- **Bench 37557 exact, three runs.** This plan is refactor-class: a moved signature
  means the derived set ≠ the scanned set — STOP, do not re-pin.
- New `mf-nnue` tests:
  - `mirror_flip_edge_derivation_matches_a_fresh_scan` — random reachable positions
    (the eonego_parity generator pattern), random legal walks incl. king moves;
    after each push compare the derived edge set against
    `append_active_threat_edges(child)` as a set.
  - `flip_after_overflow_falls_back_exactly` — `push_real_with_threat_capacity::<0>`
    then a mirror-flip king move; parity vs `from_position_production` (pattern at
    accumulator.rs ~1584).
  - `flip_derived_across_a_null_frame_and_deep_ancestors` — null push, several
    quiets, then a flip; parity vs fresh build.
  - `sibling_repush_does_not_read_the_previous_line's_slot` — flip on a branch, pop,
    push a different move, flip again; parity (pop-invalidation proof).
  - Existing king-walk / castling / Chess960 parity suites stay green untouched.
- `cargo run --release -p mf-search --features instrumentation --example nnue_update_profile`:
  `finny_threat_cache_hits` ≈ `finny_threat_rebuilds`, `threat_scan_cycles` collapses.
- fmt/clippy clean. Commit alone.

### Step 3: measure

- `nnue_update_profile` before/after (step 0's baseline): per-flip ns, scan share.
- `nps_compare` depth 12, old (f3651cd release binary stashed aside) vs new,
  swapped engine order, 5-9 repeats — the established honest-measurement recipe.
- 1-thread `mtbench` before/after.
- Record NPS ratio, flip-path improvement, and the delta-prefix overhead (visible in
  `accumulator_update_cycles` if it moves at all).

**Verify**: numbers recorded below in this file's execution records. Expect low
single-digit % NPS at best; anything negative beyond noise is a finding to report,
not to hide — the fallback is to keep the plan unmerged at step 2's commit and revert.

### Step 4: bookkeeping

Update this plan's step headers with execution records, flip the README row to DONE
(with the measured numbers), annotate the investigate item as resolved-by-plan-010,
commit.

## Test plan

- Parity (exact, bit-for-bit): king walks crossing the mirror repeatedly; castling
  (standard + Chess960 king-takes-rook, incl. the b1/b8 queenside mirror crossing);
  flip-after-overflow; flip across null frames; sibling repush. All against
  `AccumulatorState::from_position_production` / fresh scans.
- Set-equality fuzz: derived edge set ≡ `append_active_threat_edges` over random
  reachable walks.
- Value-neutrality: bench 37557 exact at every step; 73 workspace targets green.
- Instrumentation self-check: cache-hit counter ≈ flip counter; scan cycles collapse.

## Done criteria

- [x] Step 0 ceiling measured: **0.32 % of wall < 0.50 % → REJECTED-by-measurement** (the
      criterion's else-branch; steps 1–4 not executed)
- [x] All workspace gates exit 0 (68/68 instrumentation tests; default bench 37557 exact)
- [x] Bench signature 37557 exact through every executed step (only step 0 ran)
- [x] Flip-path cost and ceiling recorded (step 0 execution record above)
- [x] README row and investigate item updated (REJECTED, retired)

## STOP conditions

- Step 0's projected ceiling < 0.5 % of wall — record and retire the item.
- Bench signature moves at any step — the derivation is wrong; diagnose, never re-pin.
- Any parity failure that fixing the cache cannot resolve — report the divergence
  with the repro FEN and move list.
- The excerpts above do not match live code.

## Maintenance notes

- Memory: ~200 KB per worker stack (edge slots + delta prefixes) beside the existing
  Finny table (~300 KB) and 129 full accumulator frames; all allocated at
  construction (the allocation test pins this).
- The per-frame delta prefix is a ~30 B average copy per materialization — the lazy
  design exists precisely because the eager alternative (~100+ ns on every push)
  costs more than the flips it would serve.
- If `MAX_CHANGED` or `MAX_ACTIVE` ever change, the side arrays' shapes change with
  them; they are deliberately sized by the same constants.
- The AVX-512 investigate item is dead on this machine (Raptor Lake i9-13980HX has no
  AVX-512); do not plan around it without new hardware.
