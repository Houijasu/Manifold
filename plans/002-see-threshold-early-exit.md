# Plan 002: Add a threshold-early-exit SEE and hoist loop invariants out of the exchange walk

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- crates/mf-core/src/see.rs crates/mf-search/src crates/mf-search/examples/see_profile.rs`
> This plan was written against commit `b9d15bf` **plus its uncommitted working tree**. If the excerpts below no longer match, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW for the predicate; MED overall (node counts move if any gate verdict changes — bench re-pin required)
- **Depends on**: plans/001-stage-qsearch-move-loop.md (the qsearch gate call site moves there first)
- **Category**: perf
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

## Why this matters

Every SEE consumer in the search asks a *threshold* question — "is this capture worth at least X?" — but the engine only has the exact `static_exchange_evaluation`, which walks the entire exchange sequence (up to 32 plies of apply/undo with attacker-set rebuilds) and then minimax-folds it, every call. A threshold walk can stop as soon as the running result cannot cross the threshold — typically after 2-3 recaptures. SEE sits on the per-node path at 8+ call sites (qsearch gate, capture staging, TT-capture SEE, quiet-check gate, SEE-pruning fallback). Additionally, three loop invariants inside the walk are recomputed per step and can be hoisted for free.

## Current state

- `crates/mf-core/src/see.rs` — the whole SEE implementation. The public entry (lines ~14-24) is a pass-through:

```rust
pub fn static_exchange_evaluation(position: &Position, mv: Move) -> i32 {
    #[cfg(feature = "instrumentation")]
    let started = crate::instrumentation::cycles();
    let value = static_exchange_evaluation_greedy(position, mv);
    #[cfg(feature = "instrumentation")]
    crate::instrumentation::record_see(crate::instrumentation::cycles().wrapping_sub(started));
    value
}
```

  The greedy walk (`static_exchange_evaluation_greedy`, lines ~26-68) runs the full swap-off loop, folds `gains` from the end, and returns the exact value.

- Three hoistable invariants inside `SeeState`:
  1. `reveal_xray` (lines ~329-336) recomputes `bishop_attacks(target, Bitboard::EMPTY)` and `rook_attacks(target, Bitboard::EMPTY)` on **every call**, though both are constant per target:

```rust
    fn reveal_xray(&self, from: Square, attackers: &mut [Bitboard; 2]) {
        let target = self.target;
        let diagonal = bishop_attacks(target, Bitboard::EMPTY);
        let orthogonal = rook_attacks(target, Bitboard::EMPTY);
```

  2. `greedy_lva_recapture_gain` (lines ~307-311) rebuilds both colors' attacker sets from scratch on entry, though it is called per candidate from the multi-attacker branch of `best_greedy_recapture`.
  3. `legal_recapture_from` (lines ~395-419) trials **every** candidate recapture with `apply_recapture` → `is_attacked(king)` → `undo_recapture`, where a classical SEE treats recaptures as legal and only king recaptures can actually be illegal in a way that changes the swap result.

- Call sites that only need a predicate (after plan 001 lands): the qsearch SEE gate, the interior SEE-pruning gate (`crates/mf-search/src/search.rs` ~2506, already reusing `picker.current_capture_see()` where available), the quiet-check gate (`move_ordering.rs` `quiet_checks`), and capture good/bad classification at capture-stage load (`move_ordering.rs` `load_captures`).
- The SEE oracle: `static_exchange_evaluation_exhaustive` (test-only, lines ~70+) and the differential test suite in `see.rs` tests (~543-680) compare greedy against the exhaustive minimax oracle over 40k+ random capture positions. Any change must keep that differential green (exact-SEE contract unchanged).
- `crates/mf-search/examples/see_profile.rs` exists to measure SEE cost — use it for before/after numbers.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| SEE oracle tests | `cargo test -p mf-core see` | all pass |
| Full core tests (both backends) | `cargo test -p mf-core && cargo test -p mf-core --features force-magic` | all pass |
| Workspace gates | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` | exit 0, all pass |
| SEE profile | `cargo run --release -p mf-search --example see_profile` | records per-call cost; capture before/after |
| Bench | `cargo run --release -p mf-uci --bin manifold -- bench` | node signature (re-pin if moved) |

## Scope

**In scope**:
- `crates/mf-core/src/see.rs`
- `crates/mf-search/src/move_ordering.rs` and `crates/mf-search/src/search.rs` (gate call sites only)
- `crates/mf-core/tests/` (new differential coverage for the predicate)

**Out of scope**:
- The exact-SEE contract: `static_exchange_evaluation`'s values and its oracle tests must not change.
- `crates/mf-search/src/move_ordering.rs` staging logic beyond swapping gate calls.
- The `instrumentation` feature's counter plumbing (keep it working for both functions).

## Git workflow

- Commit style: `Add a threshold-early-exit SEE predicate` then `Hoist the SEE walk's loop invariants`, or one commit if small.

## Steps

### Step 1: Hoist the invariants

1. Compute the two empty-board ray sets once in `SeeState` construction (find it via `rg -n "fn from_position" crates/mf-core/src/see.rs`) and store them; `reveal_xray` reads the stored sets.
2. Thread the caller's attacker sets into `greedy_lva_recapture_gain` instead of rebuilding (its doc comment says "this path is only entered from the rare multi-attacker branch" — verify that still holds before changing the signature).

**Verify**: `cargo test -p mf-core see` → all pass (values unchanged, oracle still green)

### Step 2: Restrict the legality trial to king recaptures

In `legal_recapture_from`, skip the apply/undo/`is_attacked` trial when the recapturing piece is not the king: only a king recapture can be illegal in a way that changes the exchange outcome for this algorithm (a pinned piece's recapture is handled by the x-ray reveal of the pinning slider). Keep the trial for kings.

**Verify**: `cargo test -p mf-core see` → all pass. If the differential fails, the pin assumption is wrong — STOP and report; do not widen the trial back without diagnosing which position class broke.

### Step 3: Add `see_ge(position, mv, threshold) -> bool`

Implement the classic swap-list loop with an early exit: walk the exchange maintaining the best/worst achievable result for the side to move, and return as soon as the answer cannot cross `threshold`. Model it on `static_exchange_evaluation_greedy` (same `gains` structure, `apply_recapture`/`reveal_xray` cadence) but with the cutoff. Correctness contract: `see_ge(p, m, t)` must equal `static_exchange_evaluation(p, m) >= t` for all positions and moves — the exhaustive oracle makes this cheap to verify exhaustively.

**Verify**: add `see_ge` to the existing differential test battery (same random-position generator, comparing predicate vs exact for thresholds {-200, -100, -50, 0, 50, 100, 200}); `cargo test -p mf-core see` → all pass.

### Step 4: Route the gate call sites through the predicate

At each *threshold* consumer — qsearch gate (post-plan-001), interior SEE pruning, quiet-check gate, capture good/bad split — call `see_ge` instead of comparing exact SEE against the threshold. Leave exact SEE wherever the *value* is consumed: capture ordering scores (`capture_score_with_see`) and `picker.current_capture_see()`.

**Verify**: `cargo test --workspace` → all pass; `bench` → signature will likely move (fewer nodes walked to the same decisions); re-pin and record old → new.

### Step 5: Measure

Run `see_profile` before (on the pre-change commit) and after; record per-call cost and the share change. Also record 1-thread mtbench NPS before/after. If the predicate shows <1% end-to-end NPS after step 4, keep it anyway (it also unblocks cheaper gates later) but note the measurement in the commit message.

**Verify**: numbers recorded in commit message or `experiments/` notes.

## Test plan

- Differential: predicate ≡ exact-SEE comparison across the existing 40k-position battery and the threshold list above (step 3).
- Regression: full `cargo test -p mf-core --features force-magic` (SEE is backend-independent, but the repo rule is to test both).
- Node-count: re-pinned bench signature with the delta documented.

## Done criteria

- [ ] All workspace gates exit 0
- [ ] `cargo test -p mf-core see` green including the new predicate differential
- [ ] Exact-SEE oracle tests unchanged and passing
- [ ] Bench signature unchanged or re-pinned with old → new recorded
- [ ] see_profile before/after numbers recorded

## STOP conditions

- Step 2's differential fails after restricting the legality trial (pin assumption wrong — report the position class).
- `see_ge` cannot be made exactly equivalent to the exact-SEE comparison on the battery (do not ship an approximate predicate).
- The excerpts above do not match live code.

## Maintenance notes

- Any new search feature that gates on SEE should default to `see_ge`; exact SEE is for ordering scores only.
- If SEE ever gains true pin-awareness (beyond the king trial), the equivalence contract in step 3 is the safety net — keep the differential test alive.
