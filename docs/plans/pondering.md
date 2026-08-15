# Plan: Pondering

`Ponder` UCI option, `go ponder`, `ponderhit`, correct `stop` semantics. Depends on the
UCI completeness batch landing first (same files; searchmoves parsing rework touches the
same `GoParameters` loop).

## Semantics (UCI spec)

- GUI sends `position ... moves <predicted>` then `go ponder <clock tokens>`. Engine
  searches that position but must NOT print bestmove until `ponderhit` (prediction
  played — convert to a normal timed search) or `stop` (ponder miss — print bestmove
  immediately; GUI discards it and sends a fresh `position`/`go`).
- `bestmove ... ponder <move>` output: when the selected PV has ≥2 moves, append the
  second as the ponder suggestion. Emit only when the `Ponder` option is true (GUIs
  that don't ponder don't need it; harmless either way — emit always is also legal.
  Choose: always emit when PV length ≥ 2; simplest and spec-legal).
- `Ponder` option (`type check default false`) is advisory (tells the engine time
  management may assume pondering); advertise it so GUIs enable the feature.

## Design — clock-starts-at-ponderhit (option c from exploration, no stop/restart)

Existing machinery reused: `ActiveSearch` persists across commands in `run()`'s loop
(lib.rs:242); the search thread owns bestmove printing and already has the
`wait_for_stop` spin (lib.rs:496) for "don't answer until told"; `stop_active_search`
joins and triggers the print — exactly ponder-miss behavior.

mf-search change (the one real piece): time limits are immutable after spawn
(`SearchLimits` is `Copy`, `started: Instant` fixed at spawn, worker 0 owns the clock).
Add a shared ponder latch:

- New `Arc<PonderState>` (or `Arc<AtomicBool>` `pondering` + `Mutex<Instant>` clock
  start — worker's choice, minimal) passed to the pool search alongside `stop`.
- While `pondering` is true: worker 0 skips soft/hard time checks entirely (search runs
  as if infinite) and `should_stop_after_iteration` ignores soft time; the
  mate-found early break (search.rs:1184) must also be suppressed while pondering
  (engine must keep searching until ponderhit/stop).
- On `ponderhit`: mf-uci flips `pondering = false` and re-bases the clock — worker 0
  reads the re-based start `Instant` for all subsequent elapsed computations. Time
  budget (soft/hard) was computed at `go ponder` from the clock tokens and is already
  in the limits; only the start instant moves.

mf-uci changes:

- `GoParameters.ponder: bool` parsed (replace the no-op at lib.rs:1329-1336, delete the
  apology comment). `go ponder` starts the search with real `search_limits()` from the
  clock tokens plus the ponder latch armed and `wait_for_stop`-style deferral REPLACED
  by the latch (bestmove prints when the search returns; the search doesn't return
  until stop/ponderhit flips something — on stop, last-iteration result prints
  immediately; on ponderhit the search continues to normal time-based completion).
- `ponderhit` command branch in `run()`: if an active ponder search exists, flip the
  latch + re-base clock; otherwise ignore silently. Must NOT call
  `stop_active_search`.
- `stop` during ponder: existing path already correct (join → thread prints bestmove).
- New `go`/`position`/`setoption` while pondering: existing `stop_active_search` call
  gives correct ponder-miss-then-new-search behavior. No exemption needed beyond
  `ponderhit` itself.
- Append `ponder <move>` to bestmove lines when the winning PV has ≥2 moves
  (`write_bestmove`, lib.rs:1591 — PV is available in the pool result tail).
- Advertise `option name Ponder type check default false`; store on EngineState
  (advisory; no TM adjustment in this iteration).

## Edge cases

- `go ponder` with no clock tokens: legal; limits end up infinite — after ponderhit the
  search continues as infinite until `stop` (correct).
- `ponderhit` racing search completion: the latch flip after the search thread already
  finished is harmless (thread already printed? No — during ponder the search cannot
  time out; it can only complete via depth ceiling `MAX_ITERATIVE_DEEPENING_DEPTH`. If
  it does complete, the thread must still defer printing until ponderhit/stop: reuse the
  `wait_for_stop` spin gated on `pondering || !stop`).
- `quit` during ponder: `stop_active_search` already runs on quit path — verify.
- Chess960: ponder move formatting goes through `format_uci_move` with the chess960
  flag (existing bestmove path already does).

## Tests

- Unit: `go ponder` parsing (flag + clock tokens coexist); bestmove line gains
  `ponder <mv>` when PV ≥ 2.
- Protocol (`InteractiveUci`, timing-sensitive — use `spawn_exclusive`):
  1. `go ponder wtime 60000 btime 60000` → no output for ~300 ms (assert no bestmove),
     `stop` → bestmove arrives (ponder miss).
  2. Same, then `ponderhit` → bestmove arrives within the normal time budget without
     any `stop` (ponder hit converts to timed search).
  3. `ponderhit` with no active search → no crash, no output, engine answers `isready`.
  4. Ponder → new `position` + `go` without stop (GUI shortcut path) → old search
     joined, bestmove for the new search only... (verify single bestmove per go).
- Bench signature unchanged.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench   # must equal pre-change
```

Manual smoke: Nibbler/CuteChess-style command sequence by hand through the release
binary (ponder → ponderhit and ponder → stop).

## Non-goals

Ponder-aware time management (spending more when Ponder=true), pondering on the TB root
filter path specialization, SMP-specific ponder tuning.
