# Plan: UCI completeness batch

Five gaps in `crates/mf-uci/src/lib.rs` (plus one small mf-search touch): `go searchmoves`,
`go mate N`, `Clear Hash` button, `Move Overhead` option, `d` + `eval` debug commands.
No Elo claims; bench signature must be unchanged (none of these touch default search
behavior).

## 1. `go searchmoves` (the one mf-search touch)

Root-move restriction machinery already exists: `SearchContext.root_move_filter:
Option<Vec<Move>>` (search.rs:3735), consumed in `root_search` (search.rs:1332-1337),
currently set only by the Syzygy DTZ probe (search.rs:1066-1078).

- Thread an optional root-move list from the public entry points (`search_with_callback`,
  `search_with_shared_history`, the four `SearchPool` methods) → `WorkerParameters` →
  initial `root_move_filter`, exactly the pattern used for `tablebases`. When both
  searchmoves and the TB root filter apply, intersect (TB probe replaces the filter today;
  make it intersect with a pre-existing one).
- mf-uci: replace the consume-and-drop loop (lib.rs:1337-1346, delete its apology comment)
  with parsing into `GoParameters.searchmoves: Vec<String>`; convert via the existing
  UCI-move parsing used by `position ... moves` (respect `chess960`); illegal/unknown
  moves are skipped; an empty valid set = no restriction (search normally, matching the
  current lenient behavior).

## 2. `go mate N`

Map to a stop condition in mf-uci — mf-search untouched. Parse `mate: Option<u32>`
(replacing the ignore path at lib.rs:1347-1356; remove it from `ignored`). Run as an
otherwise-unbounded search (no soft/hard time unless clock tokens also present) whose
`on_iteration` callback flips the stop flag once
`score_to_uci_mate(iteration.score).is_some_and(|m| m > 0 && m <= n)`. The search
already self-terminates on found mate for non-infinite limits (search.rs:1184); the
callback is belt-and-braces and makes bare `go mate N` terminate instead of running
infinite. Bestmove printing is unchanged (search thread tail).

## 3. `Clear Hash` button

- Advertise `option name Clear Hash type button`.
- `handle_setoption` currently early-returns without a `value` token (lib.rs:~805-812) —
  special-case button options (name-only setoption).
- Action: `SearchPool::clear(table)` (thread_pool.rs:110 — the parallel path
  `ucinewgame` already uses), not reallocation. No-op errors reported as `info string`.

## 4. `Move Overhead` option

- `option name Move Overhead type spin default 10 min 0 max 2000` (Reckless parity).
- Replace the sole use of `TIME_OVERHEAD_MILLIS` (lib.rs:92, used at lib.rs:1468) with a
  value stored on `EngineState`, threaded into `search_limits()`/`clock_limits()` (which
  currently take only `&Position` — add a parameter). `movetime` path stays
  overhead-free (current behavior). Keep the const as the default value.
- The two-word option name: `handle_setoption` name parsing must join tokens between
  `name` and `value` (verify it already handles multi-word names; `Clear Hash` needs the
  same).

## 5. `d` and `eval` commands

- `d`: print an ASCII board diagram (ranks 8→1, `.` empties, piece letters), then
  `Fen: <fen>` and `Key: <zobrist hex>`. mf-core has no `to_fen` — add
  `Position::to_fen(chess960: bool) -> String` to `crates/mf-core/src/fen.rs` (X-FEN
  rook-file castling rights when chess960, standard letters otherwise; round-trip test
  `from_fen(to_fen(p)) == p` over a FEN corpus incl. Chess960 and ep positions).
- `eval`: `state.network.evaluate_production(&state.position)` (eval.rs:45 — the mode
  the engine searches with), printed as centipawns from the side to move's perspective
  with a note of the perspective (keep it one line). Stale-position guard same as `go`.
- Both are debug commands: no interaction with an active search beyond the existing
  `stop_active_search` convention other commands use.

## Tests

- Parser units (lib.rs `mod tests` pattern): searchmoves parsing (valid/illegal/empty,
  chess960 notation), mate N parsing, Move Overhead spin bounds + clock_limits math
  (update the pinned test at lib.rs:2294), button setoption without value.
- Protocol tests (uci_protocol.rs patterns — extend `expected_response` for new blocking
  commands or use `InteractiveUci`): `go searchmoves e2e4` from startpos answers
  `bestmove e2e4`; `go mate 1` on a mate-in-1 FEN (e.g.
  `k7/8/KQ6/8/8/8/8/8 w - - 0 1`) returns promptly with a mate score; `Clear Hash`
  accepted and engine alive; `d` output contains the round-tripping FEN; `eval` prints a
  number consistent between two invocations.
- Bench signature unchanged (capture before/after).

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench   # must equal pre-change
```

## Non-goals

MultiPV, pondering (separate plan), `go mate` proof-tree search (score-based detection
is the accepted approximation), `flip`/`compiler`/`speedtest` extras.
