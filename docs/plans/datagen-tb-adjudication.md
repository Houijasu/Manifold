# Plan: Syzygy TB adjudication in mf-datagen

Wire `mf-tb` into self-play generation: adjudicate games the instant they enter the
tablebases instead of waiting for the score rule (|score| ≥ 1250 for 5 plies) or a long
grind to mate/draw. Better labels (exact WDL truth), shorter games, faster generation.

## Design

- **Handle passing (Network pattern):** the CLI constructs `Tablebases::new(paths)` and
  passes `Option<&Tablebases>` into `generate(config, network, tablebases, sink)`.
  `GenerateConfig` stays `Copy` — the handle is a separate parameter, exactly like
  `&Network`. mf-datagen gains a direct `mf-tb` dependency (already in the build graph
  via mf-search).
- **CLI:** new `--syzygy-path <dirs>` arg in `parse_arguments`
  (datagen_cli.rs:113-160, `set_once` dedup), added to `first_generate_only_flag`
  (datagen_cli.rs:280-291). Load failure = hard CLI error (datagen is a batch tool;
  silent degradation would corrupt an expected-adjudicated run).
- **Adjudication site:** in `play_game`'s loop (generate.rs:247), after the move is
  applied — gate: `tablebases.is_some() && position.occupancy().count() <= max_pieces()
  && halfmove_clock() == 0 && no castling rights` (same idiom as the search root gate,
  search.rs:1174). On a hit, `probe_wdl` maps to the white-relative outcome:
  Win(stm)/Loss(stm) → converted via side to move; CursedWin/BlessedLoss/Draw → Draw
  (50-move-rule truth). Set outcome, break — same shape as the existing score
  adjudication (generate.rs:350-372). Probe only when `halfmove_clock() == 0` (the ply
  the position enters TB range via a capture/pawn move) — cheap and correct; positions
  already in TB range at nonzero clock were adjudicated when they entered, and the
  random opening can't start inside TB range (32 men).
- **Search TB stays off in datagen:** keep passing `None` as `search_with_callback`'s
  tablebases argument. Adjudication-only. (Search-time probing changes played moves and
  thus seeded outputs; adjudication only truncates games at the moment truth is known.)

## Determinism

- With `--syzygy-path` absent: byte-identical outputs to today (no RNG draws added, no
  search behavior change; the gate is `tablebases.is_some()`). Existing seeded tests
  (`a_fixed_seed_reproduces_byte_identical_output`, thread-count independence,
  datagen_cli.rs:804 CLI test) must pass unchanged.
- With TB present: outputs differ (games truncate earlier) but remain deterministic for
  a fixed seed + fixed table set. New test: fixed seed + `MF_SYZYGY_PATH` (skip
  silently when unset, repo pattern) produces byte-identical output across two runs and
  across thread counts.

## Tests

- Unit: WDL→white-relative-outcome mapping (Win as black to move → white-relative Loss;
  CursedWin → Draw, etc.).
- Integration (skip without `MF_SYZYGY_PATH`): a seeded run with TB reaches at least one
  TB adjudication (assert via a counter or by comparing game-length distributions
  TB-on vs TB-off); determinism tests above.
- CLI: `--syzygy-path` parse + dedup + rejected for `--validate`/`--from-jsonl`; bad
  path errors out with a clear message.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
$env:MF_SYZYGY_PATH='C:\Syzygy\Syzygy345WDL;C:\Syzygy\Syzygy345DTZ;C:\Syzygy\Syzygy6WDL;C:\Syzygy\Syzygy6DTZ'
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench   # unchanged (datagen-only feature)
```

## Non-goals

DTZ-based move forcing in datagen, search-time TB probing during generation, TB-rescoring
of existing JSONL conversions, adjudication statistics reporting beyond a simple count.
