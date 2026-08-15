# Plan 001: Stage the quiescence move loop

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- crates/mf-search/src/move_ordering.rs crates/mf-search/src/search.rs`
> This plan was written against commit `b9d15bf` **plus its uncommitted working tree** on `feature/nnue-optimizations`. If the excerpts below no longer match the live code, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED (moves the bench node signature — a strength change that must be re-pinned and justified, never hidden)
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

## Why this matters

Quiescence search is the majority of all nodes. Today every non-check qsearch node eagerly generates all captures, runs an **exact** static exchange evaluation (SEE) on every one, scores them, and selection-sorts the whole list — before the first child search runs. A qsearch node that cuts off on its TT capture or the first good capture (the common case) pays full SEE for every capture on the board. The interior search already solved exactly this with the staged `MovePicker` (TT move yielded with zero generation; captures generated and scored only if the TT move did not cut off). This plan applies the same staging to qsearch and deletes the eager `quiescence_moves` path.

## Current state

- `crates/mf-search/src/move_ordering.rs` — move generation, ordering, and the staged `MovePicker`. `quiescence_moves` (lines ~601-659) is the eager path this plan replaces:

```rust
pub(crate) fn quiescence_moves(
    position: &Position,
    tt_move: Option<Move>,
    see_threshold: i32,
    include_quiet_checks: bool,
    ordering: OrderingContext<'_>,
) -> MoveList {
    let captures = generate_pseudo_legal_captures(position);
    let tt_move = tt_move.filter(|mv| {
        (mv.flag().is_capture() || mv.flag().promotion().is_some()) && captures.contains(mv)
    });
    let mut kept = MoveList::new();
    let mut scores = uninit_scores();
    for &mv in &captures {
        if Some(mv) == tt_move {
            continue;
        }
        let see = static_exchange_evaluation(position, mv);
        if mv.flag().promotion().is_none() && see < see_threshold {
            continue;
        }
        scores[kept.len()].write(capture_score_with_see(position, mv, see, ordering));
        kept.push(mv);
    }
    let mut moves = MoveList::new();
    if let Some(tt_move) = tt_move {
        moves.push(tt_move);
    }
    for &mv in &sorted_by_score_descending(&kept, &scores) {
        moves.push(mv);
    }
    if include_quiet_checks {
        for &mv in &quiet_checks(position, ordering) {
            if Some(mv) != tt_move {
                moves.push(mv);
            }
        }
    }
    moves
}
```

- `crates/mf-search/src/search.rs` lines ~3014-3030 — the qsearch call site. The in-check branch already uses the staged picker (drained into a stack list); the non-check branch calls `quiescence_moves`:

```rust
    let moves: MoveList = if in_check {
        let mut evasions = MoveList::new();
        let mut picker = MovePicker::new(tt_move, [None, None], ordering);
        while let Some(mv) = picker.next(position) {
            evasions.push(mv);
        }
        evasions
    } else {
        quiescence_moves(
            position,
            tt_move,
            qsearch_see_threshold(context.options.use_see_pruning),
            searches_quiet_checks,
            ordering,
        )
    };
```

- `crates/mf-search/src/move_ordering.rs` lines ~310-321 — the existing captures-only picker variant to reuse:

```rust
    /// Captures-and-promotions iteration for ProbCut: the quiet stages are skipped
    /// entirely, so quiets are never generated or scored. Bad captures ARE yielded,
    /// after the good ones, preserving the eager picker's captures-only sequence.
    pub(crate) fn captures_only(tt_move: Option<Move>, ordering: OrderingContext<'a>) -> Self {
        Self::staged(tt_move, [None, None], ordering, true)
    }
```

- `crates/mf-search/src/search.rs` lines ~2506-2510 — the interior loop's pattern for reusing the picker's already-computed SEE (replicate this in qsearch):

```rust
            if within_window
                && picker
                    .current_capture_see()
                    .unwrap_or_else(|| static_exchange_evaluation(position, mv))
                    < threshold
```

- Design constraints to honor (quoted from the code you are changing):
  - The TT move is searched first and is **exempt from the SEE gate** ("the entry that named it was produced by a search, which is strictly better evidence than a static exchange estimate").
  - Promotions are **exempt from the SEE gate** ("dropping one because the pawn is recaptured loses the tactic the qsearch exists to find").
  - Quiet checks are appended **after every capture**, never interleaved (comment at the end of `quiescence_moves`).
  - The picker "reads the history tables WARM" by design; do not freeze scores at construction (doc comment on `MovePicker`, lines ~248-260).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Format | `cargo fmt --all -- --check` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Bench | `cargo run --release -p mf-uci --bin manifold -- bench` | prints node signature |
| NPS spot-check | `cargo run --release -p mf-uci --bin manifold -- mtbench --threads 1` | NPS comparable or better than pre-change (record both) |

## Scope

**In scope** (the only files you should modify):
- `crates/mf-search/src/move_ordering.rs`
- `crates/mf-search/src/search.rs`
- `crates/mf-search/tests/` (new/updated tests only if a behavior test needs adjusting)

**Out of scope** (do NOT touch):
- `crates/mf-core/src/see.rs` — SEE algorithm changes are plan 002.
- The in-check qsearch branch, ProbCut's `captures_only` call site, and the interior `pvs` loop — only the non-check qsearch path changes.
- Any tuning parameter or toggle default.

## Git workflow

- Branch off the current `feature/nnue-optimizations` working state.
- Commit style: short imperative sentence-case subject, e.g. `Stage the quiescence move loop like the interior search`. One commit for the code change, one for the bench re-pin if the signature moves.

## Steps

### Step 1: Extend the picker with a qsearch stage sequence

Add a picker variant (e.g. `MovePicker::qsearch(tt_move, see_threshold, ordering)`) whose stage sequence is: `Tt` (only if the TT move is a capture or promotion, matching the current `tt_move.filter`) → capture stages from `captures_only` → an optional final quiet-checks stage. Behavior requirements, in order of authority:

1. The TT move is yielded first, exempt from the SEE gate.
2. Captures below `see_threshold` are **dropped, not deferred**, except promotions which are always kept (both exemptions are documented in `Current state`).
3. Quiet checks, when enabled, yield after all captures.

The picker already computes each capture's exact SEE while loading its capture stage and exposes the last yielded capture's SEE via `current_capture_see()` — use that value for the gate rather than recomputing.

**Verify**: `cargo clippy --workspace --all-targets -- -D warnings` → exit 0

### Step 2: Switch the qsearch call site

Replace the `quiescence_moves(...)` call in `search.rs` with the new staged picker, iterating `picker.next(position)` inside the existing child-search loop exactly as the interior loop does (this is sound mid-loop because `make_move`/`unmake_move` restore the position bit-for-bit — the `MovePicker` doc comment pins this). Keep delta pruning and the existing `check_info` lazy construction untouched.

**Verify**: `cargo test -p mf-search` → all pass

### Step 3: Delete the eager path

Remove `quiescence_moves` and, if now unused, `sorted_by_score_descending` (check `quiet_checks` and other callers first — `quiet_checks` itself is still needed for the new stage). Remove any imports your deletion orphaned.

**Verify**: `rg -n "quiescence_moves" crates/` → no matches; `cargo clippy --workspace --all-targets -- -D warnings` → exit 0

### Step 4: Gate, re-pin, and record

1. `cargo test --workspace` → all pass (the `bench_cli` node-signature test will fail if the signature moved — that is expected and informative, not a bug).
2. If the signature moved: update the pinned signature in `crates/mf-uci/tests/bench_cli.rs` and state the old → new numbers in the commit message. Repo policy (README): "Any change that moves the signature is a strength change — deliberate or bug — and must be justified and re-pinned explicitly, never silently."
3. Record NPS before/after with the mtbench command above on 1 thread.

**Verify**: `cargo test --workspace` → all pass, including re-pinned `bench_cli`

### Step 5 (optional but recommended): confirming match

Run a 300-game confirming match old-vs-new at TC 8+0.08, Hash 64, 1T through `harness/run_match.ps1`. **Mandatory harness rules** (AGENTS.md): with both engines at `Threads=1` use `-use-affinity -concurrency 8`; never compare different thread counts at fixed nodes. Record Elo ± error in the commit message or `experiments/`.

**Verify**: match completes with 0 forfeits; result recorded.

## Test plan

- Adapt the existing qsearch behavior tests (find them via `rg -n "quiescence" crates/mf-search/tests/`) to the staged sequence; the assertions that matter: TT capture searched first; below-threshold captures absent from the search; promotions present regardless of SEE; quiet checks last.
- The bench signature test is the regression net for the whole change.

## Done criteria

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all exit 0
- [ ] `rg -n "quiescence_moves" crates/` returns no matches
- [ ] Bench signature either unchanged or re-pinned with old → new recorded
- [ ] `git status` shows no modified files outside the in-scope list

## STOP conditions

- The excerpts above do not match the live code (drift).
- The staged rewrite cannot preserve the two SEE-gate exemptions (TT move, promotions) without touching out-of-scope files.
- The bench signature moves AND `cargo test -p mf-search` reveals an ordering-dependent test failure you cannot resolve by updating the test's expectation (do not weaken a test to pass it — report).
- NPS regresses >2% on the 1-thread mtbench check after two inspection attempts.

## Maintenance notes

- Plan 002 (`see_ge`) builds on this: the SEE gate added here becomes its first consumer.
- Future qsearch features (checks extensions in qsearch, delta-pruning changes) will interact with the stage sequence; review them against the exemption contracts above.
