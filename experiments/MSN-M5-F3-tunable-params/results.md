# M5-F3 — Search hyperparameters exposed as UCI spin options

**Purpose.** Make the search's compile-time constants settable per game over UCI, so the
M5-F4 SPSA tuner has something to tune. The tuner discovers a parameter's name and range
from the `uci` handshake and writes values back with `setoption`, so this feature is
judged on one property above all others: **at defaults the engine must be bit-identical
to the build that shipped M4**. Bench stays `41,588`.

No match was run, and none is needed. The shipped search is bit-identical at defaults by
construction, and that was verified rather than asserted (section 3). A match would have
measured only noise.

---

## 1. What was built

`SearchParameters` in `crates/mf-search/src/search.rs` — 36 `i32` fields, carried inside
`SearchOptions` so a worker can never search with a toggle its parameters were not
sampled for. The struct field, its `Default`, and the advertised UCI spin line are all
generated from a single `search_parameters!` declaration, which is the design decision
worth recording:

```rust
search_parameters! {
    rfp_margin_per_depth: "RfpMarginPerDepth" = RFP_MARGIN_PER_DEPTH, 20 ..= 300;
    ...
}
```

A hand-maintained handshake list would eventually advertise a default that no longer
matched the constant behind it, and the failure would be silent and severe: the GUI sets
the value it was just offered, and the engine changes strength on the handshake. Deriving
all three from one line makes that class of drift unrepresentable rather than merely
tested for. `mf_search::SEARCH_PARAMETERS` is the same declaration reflected as data, and
both the UCI layer and every test below iterate it rather than repeating names.

Writes CLAMP to the advertised range rather than being rejected. A tuner steps a
parameter without knowing its bounds; a silently ignored `setoption` would leave it
tuning a value the engine never adopted, which is worse than a bounded one.

Two structural changes the parameterisation forced:

- **The LMR table is built per search, not per process.** It was a `OnceLock` static
  computed from a literal `2872.0 / 128.0`. The coefficient is now tunable, so the table
  is built in `SearchContext::new` and passed down. 128 logarithms at the start of a `go`
  is nothing against the tree that follows.
- **The LMP move-count table is computed, not tabulated.** It was a `const fn`-built
  `[[usize; 9]; 2]`. With `LmpBase` tunable the two-line formula `(base + d²) / (2 -
  improving)` is evaluated at the call site instead.

## 2. The parameter table

36 spins, all `type spin`, all defaulting to the constant the M4 build shipped with. The
`min bench` / `max bench` columns are the deterministic `bench` signature with that one
parameter driven to each end of its range, everything else at defaults — the shipped
signature is `41,588`, so a column differing from it is that parameter demonstrably
reaching the tree.

| UCI name | Default | Min | Max | Source | min bench | max bench |
|---|---:|---:|---:|---|---:|---:|
| RfpMarginPerDepth | 105 | 20 | 300 | `RFP_MARGIN_PER_DEPTH` | 22,273 | 47,528 |
| RfpTtPvMargin | 21 | 0 | 150 | `RFP_TT_PV_MARGIN` | 41,879 | 37,450 |
| RazorBaseMargin | 224 | 50 | 600 | `RAZOR_BASE_MARGIN` | 42,388 | 42,451 |
| RazorMarginPerDepth | 202 | 50 | 600 | `RAZOR_MARGIN_PER_DEPTH` | 39,812 | 42,480 |
| FutilityBaseMargin | 124 | 20 | 400 | `FUTILITY_BASE_MARGIN` | 30,073 | 50,705 |
| FutilityMarginPerDepth | 109 | 20 | 400 | `FUTILITY_MARGIN_PER_DEPTH` | 36,892 | 42,043 |
| LmpBase | 9 | 1 | 40 | `LMP_BASE` | 49,045 | 44,296 |
| LmrCoefficient | 2872 | 1000 | 6000 | `lmr_table` coefficient (over 128) | 55,503 | 24,575 |
| LmrBase | 982 | -1024 | 3072 | `late_move_reduction` base, 1024ths | 64,878 | 24,217 |
| LmrNonImprovingNumerator | 197 | 0 | 1024 | non-improving term, over 512 | 42,586 | 38,140 |
| LmrCutNodeBonus | 1024 | 0 | 3072 | cut-node term, 1024ths | 40,146 | 38,694 |
| LmrTtPvReduction | 1024 | 0 | 3072 | TT-PV refund, 1024ths | 36,742 | 58,508 |
| LmrHistoryNumerator | 439 | 50 | 1500 | history term, over 4096 | 36,957 | 40,630 |
| CaptureStatMaterialWeight | 873 | 0 | 3000 | `CAPTURE_STAT_MATERIAL_WEIGHT` | 40,894 | 41,791 |
| NmpMarginPerDepth | 13 | 0 | 60 | NMP eval precondition slope | 43,968 | 37,316 |
| NmpMarginBase | 100 | 0 | 400 | NMP eval precondition base | 37,216 | 45,504 |
| NmpReductionBase | 5 | 1 | 10 | `null_move_reduction` base | 49,755 | 41,588 |
| NmpReductionDepthDivisor | 3 | 1 | 10 | `null_move_reduction` depth divisor | 41,588 | 42,491 |
| NmpEvalReductionDivisor | 200 | 50 | 800 | `null_move_reduction` eval divisor | 41,588 | 41,588 |
| NmpEvalReductionMax | 3 | 0 | 8 | `null_move_reduction` eval cap | 41,588 | 41,588 |
| QuietSeeMarginPerDepth | 26 | 1 | 150 | `QUIET_SEE_MARGIN_PER_DEPTH` | 40,504 | 43,608 |
| CaptureSeeMarginPerDepth | 99 | 1 | 400 | `CAPTURE_SEE_MARGIN_PER_DEPTH` | 43,326 | 42,716 |
| CaptureSeeHistoryNumerator | 34 | 0 | 256 | capture SEE history relief, over 1024 | 41,579 | 41,682 |
| AspirationInitialDelta | 8 | 1 | 60 | `ASPIRATION_INITIAL_DELTA` | 38,404 | 40,286 |
| AspirationScoreDivisor | 16053 | 1000 | 60000 | `ASPIRATION_SCORE_DIVISOR` | 43,801 | 39,780 |
| AspirationMaxDelta | 512 | 16 | 2048 | `ASPIRATION_MAX_DELTA` | 41,329 | 41,588 |
| SingularBetaBase | 59 | 10 | 150 | `singular_beta` base, over 63 | 42,029 | 37,537 |
| SingularBetaTtPvBonus | 66 | 0 | 200 | `singular_beta` ttPv term, over 63 | 41,588 | 41,588 |
| SingularDoubleMargin | 16 | 0 | 100 | double-extension base | 44,216 | 40,013 |
| SingularDoubleMarginPvBonus | 16 | 0 | 100 | double-extension PV term | 40,531 | 39,750 |
| SingularDoubleMarginQuietBonus | 8 | 0 | 100 | double-extension quiet-TT term | 40,524 | 39,960 |
| PostLmrDeeperMargin | 53 | 0 | 300 | `POST_LMR_DEEPER_MARGIN` | 45,162 | 41,154 |
| PostLmrShallowerMargin | 8 | 0 | 150 | `POST_LMR_SHALLOWER_MARGIN` | 42,039 | 38,720 |
| PostLmrContinuationBonus | 1334 | 0 | 4096 | `POST_LMR_CONTINUATION_BONUS` | 41,588 | 41,588 |
| ProbCutBaseMargin | 241 | 50 | 600 | `PROBCUT_BASE_MARGIN` | 27,927 | 42,236 |
| ProbCutImprovingMargin | 64 | 0 | 300 | `PROBCUT_IMPROVING_MARGIN` | 42,468 | 30,527 |

Collected by `sweep_params.ps1` (raw output in `sweep.csv`), which harvests the names and
ranges **from the handshake** rather than from a second hard-coded list, so the sweep can
never drift from what the engine actually advertises.

### The five rows that do not move bench, and why

Four parameters show `41,588` at both ends, and two more at one end. Bench is a single
fixed depth over six positions, so a correctly-wired parameter can still be unreachable
there — and "unreachable at depth 7" and "dead wiring" look identical in that column. All
five were therefore re-probed at **fixed depth 12** on the Kiwipete FEN
(`probe_inert.ps1`, baseline 143,471 nodes):

| Parameter | Probe | Depth-12 nodes | Delta |
|---|---|---:|---:|
| NmpEvalReductionDivisor | 200 → 50 | 174,055 | +30,584 |
| NmpEvalReductionMax | 3 → 0 | 135,275 | -8,196 |
| SingularBetaTtPvBonus | 66 → 200 | 108,969 | -34,502 |
| PostLmrContinuationBonus | 1334 → 0, with `UsePostLMRContHist=true` | 143,471 vs 110,249 | +33,222 |

`NmpReductionDepthDivisor` at min and `NmpReductionBase` at max and `AspirationMaxDelta`
at max coincide with the shipped value for an arithmetic reason rather than a wiring one
(`depth/1` at bench depths, the reduction saturating, and the cap never being reached at
`|score| < 2867`); each moves the signature at its other end, which is proof enough.

`PostLmrContinuationBonus` needed its owning toggle turned on to be reachable at all,
which is correct: `UsePostLMRContHist` ships OFF (M3-F4 measured +5.9% median nodes for
no depth), so the site that reads the bonus is unreachable in the shipped build by
construction. Its spin is still advertised because the tuner may tune it in an arm where
that toggle is on.

## 3. The invariant: bit-identical at defaults

Three independent checks, all green.

**(a) `bench` unchanged and deterministic.** Two consecutive release runs:

```
Nodes searched: 41588   Time (ms): 64   NPS: 644526
Nodes searched: 41588   Time (ms): 64   NPS: 642698
```

**(b) The full `bench_cli` anchor vector is byte-identical** to the pre-change values
(`collect_anchors.ps1` against the release binary):

```
all-on (BENCH_NODE_COUNT): 41588
ablation (14): 3473717, 1146669, 518857, 1352320, 384733, 966716, 628953,
               1730756, 3183422, 896268, 579280, 953716, 2168721, 2848247
history toggles (4): 38858, 37593, 41436, 37032
contHist off: 37032          corrHist off: 38858
correction variants (3): 41588, 45188, 40161
history pruning (2): 41588, 40046      pawn history (2): 41588, 43806
LMR coupling (4): 38858, 124323, 58272, 151903
qsearch checks (2): 41588, 41588
```

Every one of these matches the constant pinned in `crates/mf-uci/tests/bench_cli.rs`. No
anchor was re-pinned by this feature, which is the correct outcome: a change that only
adds a way to SET a constant must not move any signature at the constant's default.

**(c) Writing every advertised default back is a no-op.** The stronger form of the same
claim, and the one that actually catches a wrong default: a session that issues
`setoption name <X> value <its advertised default>` for all 36 parameters and then
benches returns `41,588`. This is
`bench_cli::setting_every_tunable_parameter_to_its_default_reproduces_the_shipped_signature`.

## 4. Determinism and Threads-independence

- `cargo test --release -p mf-search` — 162 tests green, including the SMP target's
  `nnue_fixed_depth_is_identical_across_pool_sizes` (fixed-depth node counts remain
  Threads-independent) and `bench_reports_deterministic_nodes_time_and_nps`.
- `cargo test --release -p mf-uci --test bench_cli` — 24 tests green (21 pre-existing +
  3 new), including the 4-thread `uci_bench_matches_cli_and_clears_all_search_state`.

Parameters are per-`SearchOptions` and are copied into every worker's `SearchContext`
along with the toggles, so a helper thread cannot search with a different parameter set
than worker 0 — which is what would have broken Threads-independence.

## 5. Live UCI session

`uci_probe.ps1`, Process-driven per `library/user-testing.md`:

- 38 spin lines in the handshake (36 tunables + the pre-existing `Threads` and `Hash`),
  each with the default/min/max above.
- `go movetime 1500` from startpos at defaults: ~800k nodes, ~530k NPS, `bestmove e2e4`;
  same `bestmove` with `LmrCoefficient 2000`.
- **Depth at equal time, five runs per arm** (`depth_repeat.ps1`, transcript in
  `depth-repeat.txt`) — a single timed observation is scheduling jitter, so the claim is
  the repeated one:

  | LmrCoefficient | depths reached at `movetime 1500` | median |
  |---|---|---:|
  | 2872 (shipped) | 18, 18, 18, 18, 18 | 18 |
  | 2000 | 16, 16, 16, 16, 16 | 16 |

  A 30% smaller coefficient reduces less and reaches **two plies less** at equal time,
  with zero spread in either arm. That is the end-to-end confirmation that a spin
  reaches the tree of a TIME-MANAGED search and not merely of `bench`.
- `setoption name RfpMarginPerDepth value 99999` is accepted and clamped; `value banana`
  prints `info string invalid RfpMarginPerDepth value 'banana'` rather than being
  swallowed.
- No orphaned `manifold`/`fastchess`/`stockfish` processes afterwards.

## 6. Tests added

| File | Test | What it pins |
|---|---|---|
| `crates/mf-search/tests/search_invariants.rs` | `every_search_parameter_advertises_its_shipped_value_inside_a_usable_range` | Each spec's default equals the field's `Default`; bounds bracket it; no duplicate names; lookup by name round-trips |
| | `writing_a_search_parameter_updates_only_that_field_and_clamps_to_its_range` | A write lands on the named field only, and clamps at both ends |
| | `changing_the_lmr_coefficient_changes_fixed_depth_node_counts` | Read-side wiring: a 20% smaller coefficient searches strictly more nodes at fixed depth |
| | `selectivity_options_default_to_enabled` (updated) | `SearchOptions::default().parameters == SearchParameters::default()` |
| `crates/mf-uci/tests/uci_protocol.rs` | `every_tunable_search_parameter_is_advertised_with_its_default_and_range` | The handshake line for every parameter matches its compiled spec exactly, once |
| | `a_tunable_spin_write_is_accepted_and_an_unparseable_one_is_reported` | Valid writes are silent, unparseable ones produce one `info string` |
| `crates/mf-uci/tests/bench_cli.rs` | `setting_every_tunable_parameter_to_its_default_reproduces_the_shipped_signature` | **The make-or-break invariant**: all 36 defaults written back → `41,588` |
| | `changing_a_tunable_parameter_changes_the_bench_signature` | Write-side wiring both ways, and restoring the defaults restores `41,588` exactly |
| | `an_out_of_range_tunable_write_clamps_to_the_advertised_bound` | `value 999999` and `value <max>` produce the same tree |

## 7. Gates

| Command | Result |
|---|---|
| `cargo test --workspace` (debug) | green |
| `cargo test --release -p mf-search` | 162 passed |
| `cargo test --release -p mf-uci --test bench_cli` | 24 passed |
| `cargo test --release -p mf-uci --test uci_protocol` | 51 passed |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |

## 8. Decision

**KEEP.** The feature adds a tuning surface and, by the three checks in section 3,
changes nothing about the shipped engine. The bench signature stays `41,588` and every
pre-existing anchor is byte-identical, so no re-pinning was required and no match was
warranted.

**Handover note for M5-F4/F5.** The tuner should read `SEARCH_PARAMETERS` (or the
handshake) rather than a copied list. The section-2 sweep is also a rough sensitivity
ranking: the parameters that move the bench tree hardest — `LmrBase`, `LmrCoefficient`,
`LmrTtPvReduction`, `RfpMarginPerDepth`, `ProbCutBaseMargin`, `FutilityBaseMargin` — are
where an SPSA budget goes furthest, and the LMR cluster the feature description
recommended is exactly the top of that list. The five parameters that are inert at bench
depth 7 are NOT inert in play (section 2), but a fixed-depth-7 objective would waste
samples on them.
