# Plan 006: Replace the TT-cutoff's full legal-move generation with a targeted legality test

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If
> anything in the "STOP conditions" section occurs, stop and report. When
> done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- crates/mf-core/src crates/mf-search/src/search.rs`
> Written against commit `b9d15bf` **plus its uncommitted working tree**. Excerpt mismatch = STOP.

## Status

- **Priority**: P2
- **Effort**: S-M
- **Risk**: MED (a wrong legality predicate silently changes search results; the differential net below is mandatory, not optional)
- **Depends on**: none (but land after plans 001/002 to avoid bench re-pin collisions)
- **Category**: perf
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

## Why this matters

`tt_cutoff_is_safe` decides whether the search can trust a transposition-table cutoff at depth ≥ 7. Its verification step asks "is the TT move legal here?" by running a **full legal-move generation** (`generate_legal_moves(position).contains(&tt_move)`) and searching the list. The Stockfish reference in this very repo (`research/_src/search.cpp`) answers the same question with a cheap targeted check (`pos.pseudo_legal(ttData.move) && pos.legal(ttData.move)` — a single-move test, no list). A full generation at every deep non-decisive TT cutoff is the last broad "generate everything to ask one question" pattern in the search.

## Current state

- `crates/mf-search/src/search.rs` lines ~3288-3337 — `tt_cutoff_is_safe`. The clock gating (≥96, and the depth ≤ 8 / ≥ 80 capture-or-pawn branch) matches the reference and stays untouched. The expensive tail:

```rust
    if depth < 7 || is_decisive_score(score) {
        return true;
    }
    let Some(tt_move) = entry.best_move else {
        return true;
    };
    if !generate_legal_moves(position).contains(&tt_move) {
        return true;
    }

    let mut child = position.clone();
    child.make_move(tt_move);
    let Some(child_entry) = transposition_table.probe(tt_key(&child, depth - 1)) else {
        return true;
    };
```

  (Semantics: an illegal TT move means "not safe" → `true` lets the caller cut off anyway — read the call site at `search.rs` ~2042-2052 before touching anything to confirm the polarity you must preserve.)

- Reference behavior (`research/_src/search.cpp`): `if (depth >= 7 && ttData.move && pos.pseudo_legal(ttData.move) && pos.legal(ttData.move) && !is_decisive(...))` then do_move/probe/undo — the same verification, targeted legality instead of full generation.
- `crates/mf-core/src/movegen.rs` already has `is_pseudo_legal` with an exhaustive equivalence test pinned over the entire 16-bit move space (`movegen.rs` ~700-730 — find the exact test via `rg -n "is_pseudo_legal" crates/mf-core`). There is **no** `is_legal` single-move predicate today; `generate_legal_moves` filters pseudo-legal moves through make/unmake or pin logic (check `rg -n "fn generate_legal_moves" crates/mf-core/src/movegen.rs` for which).
- mf-core conventions: flat re-export from `src/lib.rs` (`pub use`), documented public API, `#[inline]` on hot small helpers, Chess960 castling is first-class (any legality predicate must use the existing castling legality helpers, not assume standard geometry).
- The perft differential suite (`crates/mf-core/tests/perft_differential.rs`, cozy-chess oracle) and the movegen equivalence battery are the correctness nets.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -p mf-core` | all pass |
| Both sliding backends | `cargo test -p mf-core --features force-magic` | all pass |
| Differential (cozy-chess oracle) | `cargo test -p mf-core --test perft_differential` | all pass |
| Search tests | `cargo test -p mf-search` | all pass |
| Gates | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` | exit 0 |
| Bench | `cargo run --release -p mf-uci --bin manifold -- bench` | node signature: **must be bit-identical** (same predicate, cheaper) |

## Scope

**In scope**:
- `crates/mf-core/src/movegen.rs` (new `is_legal` predicate + tests)
- `crates/mf-core/src/lib.rs` (re-export only)
- `crates/mf-search/src/search.rs` (the one call site)

**Out of scope**:
- `tt_cutoff_is_safe`'s gating thresholds and clock logic.
- Any other `generate_legal_moves` caller (the root loop legitimately needs the full list).
- Search behavior changes of any kind — this is a pure cost swap.

## Git workflow

- One commit: `Test TT-move legality with a targeted predicate`.

## Steps

### Step 1: Add `Position::is_legal(&self, mv: Move) -> bool` in mf-core

Requirements: `mv` must be pseudo-legal (reuse `is_pseudo_legal`) and, after making it, own king not attacked — implement with the cheapest correct shape: if the mover's king is not currently in check and the piece is not on a line with the king and not a king move and not en passant, it is legal (the classical fast path); otherwise fall back to make/`is_attacked`/unmake using the existing `Undo` machinery. Use the existing castling legality helper for castling moves (Chess960-correct). `false` for anything not pseudo-legal.

**Verify**: exhaustive equivalence test — for every position in the existing movegen test battery (`rg -n "positions|fen" crates/mf-core/src/movegen.rs | head` to find the battery) assert `is_legal(mv) == generate_legal_moves(position).contains(&mv)` for every pseudo-legal move, plus a sample of illegal encodings. `cargo test -p mf-core` → pass.

### Step 2: Fuzz the predicate against the list

Add to `crates/mf-core/tests/` a differential test modeled on `perft_differential.rs`'s random-position generator: for N random reachable positions (include Chess960 starts), compare `is_legal` against list membership for **all** generated pseudo-legal moves and a handful of corrupted encodings. This is the safety net that justifies the MED risk rating.

**Verify**: `cargo test -p mf-core --test perft_differential` and the new test → all pass.

### Step 3: Swap the call site

In `tt_cutoff_is_safe`, replace `!generate_legal_moves(position).contains(&tt_move)` with `!position.is_legal(tt_move)` (keep a `is_pseudo_legal` short-circuit only if `is_legal` does not already include it — it does per step 1, so one call).

**Verify**: bench signature **identical** to pre-change. If it differs, the predicate disagrees with list membership somewhere — STOP, do not re-pin.

### Step 4: Full gates

**Verify**: `cargo test --workspace`, clippy, fmt → all clean; `cargo test -p mf-core --features force-magic` → pass.

## Test plan

- Step 1's battery equivalence + step 2's fuzz differential (new file, modeled on `perft_differential.rs`).
- Bench bit-identity as the end-to-end proof the swap changed no decisions.

## Done criteria

- [ ] `Position::is_legal` exists, re-exported from mf-core, documented
- [ ] Equivalence + fuzz differential tests pass (new)
- [ ] `tt_cutoff_is_safe` uses it; no other behavior changed
- [ ] Bench node signature **bit-identical**; `rg -n "generate_legal_moves\(position\)\.contains" crates/mf-search/src/search.rs` → no matches
- [ ] All gates exit 0

## STOP conditions

- Bench signature moves after step 3 (predicate ≠ list membership; diagnose, do not re-pin).
- Chess960 positions break equivalence in step 2 — the castling fast path is wrong; report the position.
- A correct implementation appears to require touching files outside scope.

## Maintenance notes

- Future consumers that need one-move legality (contempt lines, root `searchmoves` validation, ponder-move checks) should use this predicate rather than generating lists — that was the point.
- If `generate_legal_moves` ever gains a bulk-fast-path change, re-run the fuzz differential against it.
