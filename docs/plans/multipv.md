# Plan: MultiPV

`MultiPV` UCI option (spin, default 1, max 256 = MAX_LEGAL_MOVES) with Stockfish-style
per-index root re-search. Analysis feature; prerequisite for EXP-E. Known cost when
enabled (SF measurements: −97 Elo at MultiPV=2) — irrelevant at default 1.

**Prime directive: bit-identical search at MultiPV=1.** Bench (40705) and datagen seeded
outputs must be unchanged. The K>1 path must be additional gated code, not a
restructuring of the K=1 path.

## Design — worker-0-only, exclusion via root filter

Stockfish wraps aspiration in `for pv_idx in 0..multiPV` over a persistent sorted
root-move list. Manifold's minimal equivalent, reusing the existing include-list
`root_move_filter` (search.rs:3849, consumed at 1445-1451):

- `SearchOptions.multi_pv: u32` (default 1; `SearchOptions` is `Copy`, a u32 is fine).
- In `search_worker_with_callback_options`, when `multi_pv > 1` **and this is worker 0**:
  per depth, run K passes. Pass 0 = the normal attempt (unchanged code path). Passes
  1..K: re-run the attempt with the root filter narrowed to
  `base_allowed − {best moves of passes 0..i}` (base_allowed = searchmoves ∩ TB filter,
  or all legal root moves). Collect K `(score, pv)` lines per depth, sorted by score.
  Helper workers (SMP) keep searching normally (single line) — they exist to fill the
  TT; voting stays single-move.
  K is capped at the number of allowed root moves.
- Aspiration: passes 1..K seed their window from that move's previous-depth score when
  available, else full-width. Simplest correct: full-width for secondary lines at first;
  note as a `ponytail:` ceiling (per-line aspiration windows are the upgrade).
- `IterationInfo` gains `multipv_index: u32` (1-based; always 1 on the K=1 path) —
  worker 0 emits K Progress events per depth in score order. `SearchResult`/voting
  unchanged (bestmove = line 1; vote.rs untouched).
- Mate-found early break (search.rs:1184): only when line 1 has the mate AND multi_pv
  lines are all decided — simplest: disable the early break when `multi_pv > 1`.
- Soft-time iteration decisions: unchanged (worker-0 clock; K passes just consume the
  budget — analysis mode is typically infinite anyway).

## mf-uci

- `option name MultiPV type spin default 1 min 1 max 256` in `UCI_RESPONSE`
  (lib.rs:35-78); `handle_setoption` branch → `state.search_options.multi_pv`.
- `write_iteration_info` (lib.rs:1829-1860): replace the hardcoded `multipv 1`
  (lib.rs:1850) with `iteration.multipv_index`; the other two hardcoded sites
  (lib.rs:1699, 1751) are single-result paths — they print the selected line, index 1,
  and stay literal.
- `go searchmoves` + MultiPV compose naturally (base_allowed already includes it).

## Tests

- MultiPV=1 bit-identity: bench signature unchanged (capture before/after); datagen
  seeded byte-identity tests already enforce the library path.
- Protocol: startpos `go depth 6` with MultiPV=3 emits `multipv 1|2|3` lines per depth,
  strictly non-increasing scores per depth, distinct first moves, and exactly one
  bestmove (line 1's move). MultiPV=2 with `searchmoves e2e4 d2d4` restricts lines to
  those moves. MultiPV greater than legal-move count clamps (e.g. a position with 2
  legal moves + MultiPV=5 → 2 lines, no crash).
- Unit: exclusion filter construction (base minus found moves, interaction with TB/
  searchmoves intersection).

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench   # must equal pre-change exactly
```

Manual smoke: release binary, MultiPV=3 on startpos and on a sharp tactical FEN;
verify GUI-style output ordering.

## Non-goals

EXP-E bandit allocation (this only unlocks it), per-line aspiration windows (ponytail
ceiling), helper-thread multi-line search, MultiPV-aware time management, `go mate`
interaction beyond line 1.
