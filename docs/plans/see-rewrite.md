# Plan: exact SEE rewrite — instrument, de-clone, borrow, threshold

Approved spec: `C:\Users\Samaritan\.factory\specs\2026-08-09-exact-see-rewrite.md` (2026-08-10).

## Goal

Reduce pin-exact `static_exchange_evaluation` (`crates/mf-core/src/see.rs`) from the
audit-measured ~73 ns warm toward 20–25 ns **without changing any returned value**, any move
ordering, or the deterministic `bench` signature. Steps 0–5 only. The Step-6 riders from the
deep-dive (`Piece` packing, qsearch partial sort, `tt_cutoff_is_safe`) are explicitly OUT of
scope — each sits at a separate seam with its own risk/performance gate.

## Global invariants (every step)

- `cargo test -p mf-core see` — the 40,000+-position greedy-vs-exhaustive differential
  (see.rs:431) and both pinned-exchange regression FENs stay byte-for-byte untouched and green.
  Any disagreement = correctness bug in the new code; tighten or disable the optimization,
  never edit the oracle or re-pin a test.
- `cargo run --release -p mf-uci --bin manifold -- bench` — signature identical after every
  step. SEE feeds move ordering, so value drift moves the node count; drift = bug.
- No new dependencies, no heap allocation in hot paths, no public pin module, no unrelated
  cleanup, no touching the dirty workspace's other edits.

## Step 0 — measure first

The 8–15%-of-node-time figure is derived, not measured. Add counters before touching the
algorithm:

- New default-off `instrumentation` feature on **mf-core** (`instrumentation = []`), forwarded
  from mf-search's existing feature: `instrumentation = ["mf-nnue/instrumentation",
  "mf-core/instrumentation"]`.
- New `crates/mf-core/src/instrumentation.rs` mirroring mf-nnue's pattern: thread-local
  `Cell<SeeCounters>`, `rdtsc` cycles. `SeeCounters { calls: u64, cycles: u64 }` with
  `reset_see_counters` / `see_counters` re-exported (feature-gated) from mf-core's lib.rs;
  `record_see` / `cycles` stay `pub(crate)`.
- Time at the single deep seam: the body of `static_exchange_evaluation` in see.rs (and
  `static_exchange_evaluation_ge` once Step 5 lands), cfg-gated so production builds keep zero
  counter state.
- New `crates/mf-search/examples/see_profile.rs` (`required-features = ["instrumentation"]`),
  shaped like `nnue_update_profile.rs`: six bench positions + deep cases, reporting nodes,
  NPS, SEE calls/node, ns/call (via the example's measured TSC rate), and SEE share of wall
  time.

Baseline gate: `cargo run --release -p mf-search --example see_profile --features instrumentation`.

**Decision rule:** measured share ≤ ~4% → stop after Step 2 and report; Steps 3–5 need explicit
approval. Share ≥ ~8% → full run justified.

## Step 1 — kill clone-per-candidate (see.rs:305)

Replace `legal_recapture_from(&self) -> Option<LegalRecapture>` (clone per candidate) with a
mutable trial: apply → test → undo on the one live `SeeState`.

- `Recapture { from: Square, placed_kind: PieceKind, promotion_gain: i32 }` — a decision, no
  state.
- `RecaptureUndo { from: Square, attacker: Piece, victim: Piece }` — enough to invert:
  `remove(target)` (the placed piece), `place(from, attacker)`, `place(target, victim)`.
  `place` already maintains `kings`, so king recaptures restore exactly.
- `least_valuable_legal_recapture` and `best_greedy_recapture` become `&mut self` and return
  `Option<Recapture>`; `greedy_lva_recapture_gain` applies/undoes each step and restores the
  state on exit via a fixed `[Option<RecaptureUndo>; 32]` stack.
- The chosen recapture is applied once to the live state; `LegalRecapture { state }` is
  deleted.
- Preserve exactly: king-first candidate order in `least_valuable_legal_recapture`,
  `PieceKind::ALL` × bit-forward iteration, strict-`>` first-max tie-breaking in
  `best_greedy_recapture`, and both gains-array parity schemes (main loop +/−, continuation
  −/+).

## Step 2 — stop copying the Position (see.rs:131)

Replace the owned mailbox/bitboard snapshot with a borrowed projection:

```rust
#[derive(Clone)]
struct SeeState<'p> {
    position: &'p Position,
    target: Square,
    occupied: Bitboard,          // live occupancy; starts as position.occupancy()
    target_piece: Option<Piece>, // piece standing on target (None only transiently)
    kings: [Square; 2],
}
```

- `piece_at(sq)`: `target_piece` at target; elsewhere `position.piece_at(sq)` iff the
  `occupied` bit is set. `remove(sq)`/`place(sq, p)` keep their exact signatures so
  `prepare_exchange` and the `#[cfg(test)]` exhaustive oracle stay verbatim; `place` asserts
  the square is the target (SEE only ever places there).
- Piece sets: `pieces_of(color, kind) = position.pieces(color, kind) & occupied`, with the
  target bit cleared unless `target_piece` matches that color/kind (then set). This override
  is mandatory — `Position` still describes the pre-exchange target occupant, and promotions
  change the kind.
- `attackers_to` masks every class with `pieces_of` and uses live `occupied` for slider
  lookups.
- Undo on the projection is two stores (`target_piece`, one occupancy bit) plus king restore.

Verification after Steps 1+2: focused SEE tests, workspace tests, identical bench signature,
`see_profile` + swapped-order `harness/nps_compare.py`. **Checkpoint per the Step-0 decision
rule.**

## Step 3 — incremental attackers with x-ray reveal (see.rs:385)

`attackers_to` recomputes five classes (two PEXT slider lookups) per swap step. In the greedy
swap loop maintain per-side attacker sets instead:

- Compute both sides' target attackers once after `prepare_exchange`.
- After vacating `from`: reveal only along the `from ↔ target` line — diagonal source → one
  `bishop_attacks(target, occupied) & (bishops|queens)` intersection; rank/file source →
  rook analogue; knight/pawn/king sources reveal nothing.
- Always intersect maintained sets with live `occupied` before selecting (the just-vacated
  square, and any square ever vacated, is masked out).
- Own-king safety checks (`is_attacked(kings[side], !side)`) attack a different square and
  stay full lookups per candidate — pin-exactness is the point of this rewrite.

## Step 4 — conservative fast/slow legality split

The multi-attacker quadratic branch (`best_greedy_recapture` scoring each candidate with a
bounded LVA continuation) exists because pins and x-rays change legality mid-exchange. An
initial pinned-only mask is **unsafe**: earlier recaptures can create later pins. Gate on a
conservative potential-attacker analysis computed once per SEE call:

- Potential attackers = union over both sides of pawn/knight/king attack masks plus
  `bishop_attacks(target, EMPTY)`/`rook_attacks(target, EMPTY)` (no blockers) ∩ sliders —
  i.e. anything that can attack the target under some later occupancy.
- A non-king potential attacker of color c is *sensitive* if the full line between its square
  and its king (`bishop_attacks(king, EMPTY)` or `rook_attacks(king, EMPTY)` ∩ line to
  attacker, non-empty ⇒ aligned) contains no other potential attacker of either color.
  Kings are always sensitive.
- Fast path (overwhelming majority): zero sensitive potential attackers ⇒ no recapture can
  ever expose its own king ⇒ plain LVA swap with no legality tests and no continuation
  branching.
- Slow path (rare): the existing exact machinery from Steps 1–3, unchanged.

The fast path may be conservative and activate less often; it may never be optimistic. One
differential disagreement = tighten or disable the condition, not the tests.

## Step 5 — exact threshold interface

```rust
pub fn static_exchange_evaluation_ge(position: &Position, mv: Move, threshold: i32) -> bool
```

Pin-exact and value-equivalent to `static_exchange_evaluation(position, mv) >= threshold`,
sharing the same swap implementation, exiting as soon as the running minimax comparison is
decided (with `gains[0] = initial_gain - threshold`, the backward pass proves the bound the
moment it leaves `[0, 1)`). Re-exported from mf-core's lib.rs.

Production migrations only:

- `move_ordering.rs:566` — `quiet_checks` vs `QUIET_CHECK_SEE_THRESHOLD` (0).
- `search.rs:2127` — the quiet arm of SEE pruning, where `current_capture_see()` is `None`.

Keep exact SEE at `load_captures` (:419), TT-move validation (:466), and `quiescence_moves`
(:629) — those consume the value (`see * 32`, `current_capture_see`). The bad-capture splits
already reuse cached values; :889/:942 are test code. No recomputation is introduced anywhere.

Test: same four-seed random-walk corpus as the differential; for every capture/promotion and
a swept threshold range assert `ge(t) == (static_exchange_evaluation(p, m) >= t)`.

## Verification ladder (after every semantic step)

```powershell
cargo test -p mf-core see                                        # semantic referee
cargo run --release -p mf-uci --bin manifold -- bench            # signature identical
cargo run --release -p mf-search --example see_profile --features instrumentation
py -3.14 harness\nps_compare.py --engine A=<base.exe> --engine B=<cand.exe> --depth 12 --hash 64 --warmup 1 --repeat 3
# NPS comparison repeated with engine order swapped
```

Final gate: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`, `cargo test --release -p mf-core`.

Expected shape: Step 1 cuts the tail, Step 2 the setup, Step 3 the mean, Step 4 the max,
Step 5 boolean-call work. Combined target ~20–25 ns/call; +5–10% NPS only if Step 0 confirms
the call-share estimate.

## Follow-ups (separate changes, not this plan)

Pin-agnostic `see_ge` (approximate, SPRT-gated), capture-ordering simplification (MVV-LVA +
capture history with `ge(0)` split), qsearch partial sort, `Piece` u8 packing,
`tt_cutoff_is_safe` via `is_pseudo_legal`, wider SEE/futility windows.
