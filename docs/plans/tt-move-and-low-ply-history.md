# Plan: ttMoveHistory + low-ply history (Stage 3 items 21–22)

Two remaining history-zoo features from `research/search-and-eval-sota.md` §3.5 / Stage 3.
Reference semantics confirmed against `research/_src/` (read-only Stockfish source).

## Feature A — ttMoveHistory

A single per-worker `i16` scalar (gravity bound D=8192), answering "is my TT move usually
best right now?". Not a table, not shared.

- **Updates** (gravity formula, same `apply` semantics as history.rs):
  1. Node conclusion in `pvs`, non-PV nodes only, when a TT move existed:
     `<< 918` if `best_move == tt_move` else `<< -747` (site: ~search.rs:2240 where
     `bound` is computed; `best_move`/`tt_move`/`pv_node` all in scope).
  2. On singular multicut early-return: `<< (-421 - 110*depth)` (site:
     `singular_multicut_value` path, search.rs:1968–2027 block).
- **Consumption — one site only:** the singular double-extension margin in
  `singular_extension` (search.rs:2983–2997):
  `double_margin -= 1175 * tt_move_history / 114178` on top of Manifold's existing
  tunable trio. Does NOT feed move ordering or LMR.
- **Reset:** zeroed per search start (per-worker state, like `KillerTable`).
- Constants 918 / 747 / 421 / 110 / 1175 / 114178 / 8192: plain `const`s, not
  `search_parameters!` spins (SPSA later if the feature sticks).

## Feature B — low-ply history

Per-worker table `[[i16; 65536]; 5]` indexed `[ply][move.raw16()]` (the raw u16 of `Move`),
gravity bound D=7183. ~640 KiB per worker — acceptable; per-worker (thread-local in the
reference), NOT in `SharedHistory`.

- **Update:** inside `update_histories` (search.rs:3269, ply already a parameter):
  when `ply < 5`, apply the same bonus/malus stream as butterfly scaled by `712/1024`
  (bonus for the cutoff quiet, malus for `searched_quiets`).
- **Ordering read:** in quiet scoring (`quiet_score` → `ordering_history`,
  move_ordering.rs:696–718 / 110–123): add `8 * lph[ply][mv.raw()] / (1 + ply)` when
  `ply < 5`. `OrderingContext` (move_ordering.rs:94–102) gains a `ply` field plus access
  to the per-worker table (reference or raw slice — worker's choice; built per-node at
  search.rs:3706–3716 where ply and worker state are in scope). Ordering read only — do
  NOT add it to `pruning_history` or LMR's `quiet_history` stat-score.
- **Refill between root iterations:** `fill(102)` (tuned nonzero prior, per reference) at
  the top of each iterative-deepening iteration in
  `search_worker_with_callback_options`.
- **Killers stay.** Stockfish absorbed killers into low-ply history; Manifold keeps its
  `KillerTable` untouched (minimal diff). Removing killers is a separate future SPRT.

## Repo invariants that bind this change

- **Maintenance unconditional, reads gated** (search.rs:2204–2207, move_ordering.rs:68–71):
  both features get `Use*` toggles that gate ONLY the read/consumption:
  - `UseTtMoveHistory` → gates the `double_margin` adjustment.
  - `UseLowPlyHistory` → gates the ordering term.
  Both `SearchOptions` bools, **default true**, wired exactly like `UsePawnHistory`
  (SearchOptions field → option string in mf-uci lib.rs:41–68 → `parse_check_option`
  branch ~lib.rs:907). Updates always run regardless of the flag.
- **No allocation in hot paths**: the low-ply table is allocated once per worker at
  construction, refilled in place.
- **Bench signature WILL change** (defaults on). Capture before/after
  `cargo run --release -p mf-uci --bin manifold -- bench`; both numbers go in the report.
  With both toggles off, the signature must equal the before-capture (proves the
  maintenance/read separation is clean).

## Tests (behavioral names, alongside existing history tests)

- ttMoveHistory scalar moves toward +max when TT move keeps winning, toward −max on
  multicut penalties; clamped within ±8192.
- Low-ply ordering: after rewarding a quiet at ply 0, that move is yielded by the picker
  ahead of an otherwise-equal quiet at low ply; no effect at ply ≥ 5.
- Refill: after an iteration boundary the table reads 102, not 0 and not stale.
- Toggle test in the bench_cli.rs pattern (lib.rs:1090–1151): `UseTtMoveHistory=false` and
  `UseLowPlyHistory=false` reproduce the pre-change bench signature.
- Existing invariant search_invariants.rs:210 (`SEARCH_PARAMETERS.len()` in 20..=40) must
  still hold — no new spins are added.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench   # before + after + toggles-off
```

Elo measurement deferred to `harness/run_match.ps1` SPRT (STC first), per
`measurement_harness`. Each feature should be SPRT-able independently via its toggle.

## Non-goals

- No killer removal, no corrhist-complexity proxy (item 23 — separate plan), no SPSA of
  the new constants yet, no sharing of either stat across threads.
