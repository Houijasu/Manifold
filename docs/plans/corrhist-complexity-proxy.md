# Plan: Corrhist magnitude as complexity proxy (Stage 3 item 23)

Consume `|correction_value|` (the pre-division blend, already computed per node and
documented as "the engine's complexity proxy", search.rs:3412-3415) at three sites.
This is ranked item #1 in `experiments/perf-audit/search.md:21-46`: "the
computed-but-unused proxy", size S, +5–15 Elo at LTC expectation.

**STC trap (documented, do not forget at measurement time):** the reference singular
patch measured **−4.40 ± 1.4 Elo at STC** and passed two VVLTC SPRTs. An STC-only
pipeline deletes this feature. Any future SPRT must include an LTC (30+0.3) arm before
a revert decision. Measurement is deferred entirely for now.

## Signal

`let correction = correction_value(position, context, ply);` is already a live local in
`pvs` at all consumption sites (bound at search.rs:1611). The proxy is
`correction.abs()`. No new computation; qsearch is untouched (it does not bind the
value and the reference consumes nothing there).

Note on divisor transfer: Manifold's blend deliberately omits the reference's `64049`
first-move substitute and uses a hand-tuned cp eval scale, so Stockfish's exact divisors
(`26310`, `198435`, `198368`) are starting points, not gospel — hence tunables.

## Consumption sites (each ~one line + one plumbed parameter)

Reference formulas from `research/_src/search.cpp` (§3.7, novel-techniques §4.6):

1. **LMR** (`late_move_reduction`, search.rs:2805-2830, called ~1871-1892):
   `reduction -= correction.abs() / parameters.lmr_corrplexity_divisor` — applied in the
   shared helper so both quiet and capture LMR paths get it (reference applies to `r`
   generally). New parameter `corrplexity: i32` on `late_move_reduction` and
   `capture_late_move_reduction`.
2. **RFP margin** (`reverse_futility_margin`, search.rs:2762-2772, call 1635-1650):
   `margin += correction.abs() / parameters.rfp_corrplexity_divisor` (bigger margin =
   harder to prune when eval is unreliable — matches reference sign:
   `futilityMargin ... + |cv|/198435`). The reference consumes at RFP (Step 8), NOT the
   move-loop frontier futility — leave `frontier_futility_margin` alone.
3. **Singular double margin** (`singular_extension`, search.rs:3045-3069):
   `corr_val_adj = correction.abs() / parameters.singular_corrplexity_divisor;`
   `double_margin -= corr_val_adj` (alongside the existing ttMoveHistory adjustment).
   No triple margin exists in Manifold; do not add one.

## Tunables and toggle

- Three new `search_parameters!` spins (per the perf-audit prescription):
  `LmrCorrplexityDivisor = 26310`, `RfpCorrplexityDivisor = 198435`,
  `SingularCorrplexityDivisor = 198368`, ranges wide (e.g. 4096..=1_000_000; worker
  picks sane bounds consistent with neighboring specs). Constraint: search_invariants.rs
  asserts `SEARCH_PARAMETERS.len()` in `20..=40` — verify the count still fits; if it
  would exceed 40, fall back to plain consts and note it.
- One toggle `UseCorrplexity` (`SearchOptions.use_corrplexity`, **default true**), wired
  exactly like `UsePawnHistory` (option string mf-uci lib.rs:41-68, `parse_check_option`
  branch ~lib.rs:909). Gates all three reads (signal maintenance is already
  unconditional — corrhist updates are untouched). When `use_correction_history` is off,
  `correction_value` already returns 0, so the proxy naturally vanishes — no extra
  gating needed for that interaction.

## Tests

- Toggle equality: with `UseCorrplexity=false`, bench signature equals the pre-change
  baseline exactly (capture baseline BEFORE any edit; current defaults-on signature is
  39051 after the history features).
- Divisor wiring: changing each new spin via setoption changes fixed-depth node counts
  (pattern of the existing LMR-coefficient wiring test — beware its depth-granularity
  caveat; pick a depth where the aggregate moves).
- Sign sanity unit test: larger `|correction|` never *increases* the LMR reduction and
  never *decreases* the RFP margin (call the helpers directly).
- Existing bench_cli.rs defaults-on anchors will move — re-pin from live measurements,
  same as the previous feature did.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p mf-uci --bin manifold -- bench   # baseline + defaults-on + toggle-off
```

Elo measurement deferred; when run, per perf-audit: SPRT at 8+0.08
(`-use-affinity -concurrency 8`) AND a 30+0.3 arm before any revert decision.

## Non-goals

- No NMP consumer (added and reverted upstream, PR #5272/#5375).
- No triple margin, no qsearch consumer, no Viridithas-style `|static_eval - tt_value|`
  alternative signal, no frontier-futility consumer.
