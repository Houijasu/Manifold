# MSN-S3 — Time-management best-move effort term (M3-F3)

**Decision: REVERT. `UseTimeEffort` ships `default false`.**

Third consecutive M3 feature to ship off. The running baseline for the remaining M3
features is **still `baselines/m2-nnue/manifold.exe` (bench 45,036)** — do not promote a
new baseline.

## Purpose

Manifold's time manager splits soft/hard limits in `mf-uci` and governs the soft limit
in `mf-search` with a stability factor: 110% for a root move that just changed, stepping
down 5 points per stable iteration to 80% at six, plus +20% per 50cp of falling score,
capped at 180% and never past the hard limit. That package is worth **+51 Elo**
(M7-F2-v2).

`research/search-and-eval-sota.md` §7.2 lists five multiplicative factors in the
reference engine's between-iterations decision. Manifold has two of them (falling score,
stability). This feature added the third: **`highBestMoveEffort`**, the best root move's
share of the tree, described there as *"the single strongest verified adaptive
time-control signal"* and credited to Koivisto.

## What was implemented

`crates/mf-search/src/search.rs`:

- Per-root-move node accounting in `root_search`: `context.nodes` is sampled before and
  after each root move's subtree and credited to that move (`record_root_effort`).
  Accounting is **reset at the start of every root search** (`begin_root_effort`), so an
  aspiration re-search replaces the previous distribution rather than adding to it — the
  last root search is the one whose result the iteration reports, and a failed window's
  truncated subtrees describe a tree that was thrown away.
- `time_effort_percent(best_move_nodes, total_nodes)`: a linear ramp in **per-mille**,
  110% at or below 500‰, 90% at or above 900‰, interpolated between. Integer arithmetic
  throughout — a float here would make the time manager, and therefore the games,
  platform-dependent.
- `scaled_time_percent(stability, effort)`: multiplicative composition, clamped by the
  same `TIME_SCALE_MAX_PERCENT` = 180 ceiling stability alone obeys, so a second factor
  cannot raise the maximum a soft limit can be stretched to. The hard limit is untouched.
- UCI toggle `UseTimeEffort`.

**Node accounting reads worker 0's own counts, never the aggregated pool total.** Only
worker 0 owns the clock, and only worker 0's root loop fills `root_effort`; helpers
search their own trees with their own root move orders, so summing the pool would divide
one worker's subtree by every worker's nodes and drive the fraction toward zero as
threads are added. Reading worker 0 alone makes the factor thread-count invariant by
construction.

**Ramp anchors were placed against a measured distribution, not transplanted.** Before
either match, a temporary `MF_EFFORT_TRACE` instrumentation measured the actual node
share over 301 iterations at depth ≥ 4 across 24 UHO book positions at `movetime 1000`:

| statistic | per-mille |
|---|---|
| min / p10 / p25 | 101 / 307 / 494 |
| **median** | **757** |
| p75 / p90 / max | 925 / 974 / 998 |
| mean | 696 |

With the shipped 500/900 anchors that puts **25.3%** of iterations at the low anchor
(extending), **45.0%** on the ramp, and **29.7%** at the high anchor (shrinking), for a
mean applied factor of **98.9%**. The term redistributes time rather than systematically
spending more or less of the clock, which is what makes it a single-variable change
against a fixed TC.

## Bench signature: UNCHANGED, as required

`45_036`, deterministic across two consecutive runs, and identical with the toggle ON,
OFF, and at the shipped default. The full `bench_cli.rs` anchor vector was re-collected
against the release binary and is **byte-identical to M2** in all 34 values.

That is the expected result and is asserted as a test
(`the_time_effort_term_cannot_move_the_fixed_depth_bench_signature`): bench is a
fixed-depth search with no soft limit, so the term has nothing to act on. Note that the
per-root-move accounting **does** run on every search including bench — only its consumer
is time-gated — so this equality is a real guard against the accounting acquiring a side
effect on the tree, not a tautology.

## Measurements

Both matches: Threads=1, Hash 64, `UHO_4060_v4.epd`, `-use-affinity -concurrency 8`
(enforced by `harness/run_match.ps1`), **zero forfeits, zero crashes, zero illegal moves
on both sides**.

### 1. Standard TC — 300 games at 8+0.08

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S3-tm-effort `
    -Purpose 'M3-F3 time-management best-move effort term, single-variable vs the M2 kept build' `
    -AName tm-effort -ACmd .\target\release\manifold.exe `
    -BName m2-nnue  -BCmd .\baselines\m2-nnue\manifold.exe `
    -Rounds 150 -Seed 20260806
```

| | |
|---|---|
| **Elo** | **-17.39 ± 18.99** |
| nElo | -36.09 ± 39.32 |
| LOS | 3.60% |
| Score | 142.5 / 300 (47.50%) — 70W 85L 145D |
| Ptnml(0-2) | [1, 40, 82, 27, 0] |
| PairsRatio | 0.66 |
| Wall | 17m06s |

### 2. Longer TC sanity — 60 games at 30+0.3

Authorized by the feature description: 8+0.08 is short for a time-management change, and
the reference engine's own gain for this term is an LTC result. Run because the STC error
bar covered zero.

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S3-tm-effort-ltc `
    -Purpose 'M3-F3 effort term sanity at a longer TC ...' `
    -AName tm-effort -ACmd .\target\release\manifold.exe `
    -BName m2-nnue  -BCmd .\baselines\m2-nnue\manifold.exe `
    -TC '30+0.3' -Rounds 30 -Seed 20260807
```

| | |
|---|---|
| **Elo** | **-34.86 ± 44.35** |
| nElo | -69.96 ± 87.91 |
| LOS | 5.94% |
| Score | 27.0 / 60 (45.00%) — 10W 16L 34D |
| Ptnml(0-2) | [0, 11, 14, 5, 0] |
| PairsRatio | 0.45 |
| Wall | 13m41s |

Neither result is individually decisive — both error bars touch zero. What decides it is
that they **agree in direction and magnitude at two time controls**, at the TC where this
class of change is supposed to look BEST, with LOS 3.6% and 5.9%. Two independent samples
both landing near -20 Elo is not the shape of a null result.

## Why it lost — the mechanism, measured

The natural next question is whether the ramp was simply mistuned. It is not: the signal
has real range (25% / 45% / 30% across the three regimes). The problem is the
**composition**, and a second temporary instrumentation run measured it directly. Over
299 iterations at depth ≥ 4 on 24 book positions:

| stability | n | mean stability factor | mean effort factor | composed | vs stability alone |
|---:|---:|---:|---:|---:|---:|
| 0 | 44 | 110.0% | 103.2% | 113.5% | +3.2% |
| 1 | 33 | 105.0% | 101.5% | 106.5% | +1.5% |
| 2 | 27 | 100.0% | 99.9% | 99.9% | -0.1% |
| 3 | 31 | 95.0% | 103.1% | 97.7% | +2.8% |
| 4 | 27 | 90.0% | 101.1% | 91.1% | +1.2% |
| 5 | 23 | 85.0% | 100.0% | 85.0% | 0.0% |
| **6** | **114** | **80.4%** | **95.1%** | **76.4%** | **-4.9%** |

**Pearson r(stability, effort factor) = -0.348.** A high node share and a settled root
move are largely the SAME iterations. The effort term does not add a signal; it
**re-applies one the stability governor already carries** — and that governor is *tuned*
(+51 Elo), so multiplying a second, independently-chosen discount onto it overshoots.

The damage concentrates exactly where it hurts. Stability 6 is **38% of all iterations**,
and there the two factors compound to 76.4% of the nominal budget where the tuned
governor asked for 80.4%. Those are the settled positions whose saved time was supposed
to be **banked for the moves that need it** — the entire mechanism M7-F2 was worth +51
Elo for. The effort term spends that banked time twice.

**Note what the aggregate hides.** The overall mean scale moves from 92.0% to 91.5%, a
change of **-0.5%**. Any sanity check of the form "does the engine still spend about the
right amount of time?" would have passed. The regression lives in the *conditional*
distribution, not the mean — which is why the redundancy table above, and not a
time-spent average, is the evidence.

## What this adds to the M3 record

Three M3 features, three reverts, three *different* failure modes — and this one is the
first that is not about the tree at all:

- **M3-F1** (qsearch checks): spent MORE nodes to get LESS depth. A bad trade.
- **M3-F2** (capture LMR): got MORE for LESS and **could not spend it** (+0.12 plies from
  a 25% node saving); the verification re-search ate the saving.
- **M3-F3** (this): the tree is **bit-identical**. The technique is correct, the signal is
  real, and it still loses — because it was **stacked onto an already-tuned governor
  instead of being tuned jointly with it**.

The reference engine's §7.2 is five factors fit *together*. Porting the fourth-strongest
of them into a two-factor engine and multiplying is not the same operation, and this is
the measurement that shows the difference is worth ~20 Elo.

## Conditions for revisiting

Both aimed at the composition, not the signal:

1. **Fold the node share into the stability governor as one term of a single formula**
   rather than a second independent multiplier — i.e. re-derive `time_scale_percent`
   with stability, falling score, and effort as joint inputs.
2. **Re-derive the ramp anchors against the stability count they multiply.** The
   distribution above is unconditional; what the composition needs is the node-share
   distribution *conditioned on* stability, and anchors chosen so the product matches the
   tuned governor's intent at stability 6.

Do not simply widen or narrow the ±10% ramp: r = -0.348 means the term is partly a
restatement of stability at any gain.

## Artifacts

- `run-metadata.txt`, `games.pgn`, `console.txt`, `fastchess.log` (this dir, 8+0.08).
- `../MSN-S3-tm-effort-ltc/` — the same set for 30+0.3.
- `tm_probe.ps1` and `uci-probe-transcript.txt` — live UCI verification over four
  positions × {`go wtime 8000 winc 80`, `go depth 12`} × {toggle on, off}. **Every
  `go depth 12` arm is bit-identical** (e.g. kiwipete 141,659 nodes, endgame 38,768,
  tactical 55,728, startpos 99,674) — this is the observable form of "the term cannot
  reach an untimed search".

  A note on what the CLOCKED arms do and do not show. The transcript's clocked rows
  mostly agree too, and that is expected rather than disappointing: a ±10% change to a
  ~300 ms soft limit usually lands on the same completed iteration, and the last
  iteration is the one whose result is reported. One earlier run of the same script did
  show kiwipete stopping at depth 11 / 249 ms enabled against depth 12 / 300 ms
  disabled — but that did **not** reproduce across five subsequent runs (all
  depth 12, 295-301 ms both arms), so it was a soft-limit boundary being crossed by
  scheduling jitter, not a stable per-position effect. It is recorded here rather than
  quoted as evidence, because a probe result that survives one run is not a
  measurement. **The evidence that the term reaches the clock is the two matches and
  the per-stability composition table above, which are aggregates over hundreds of
  games and iterations respectively.**
- The two `MF_EFFORT_TRACE` instrumentation drivers were **deleted after their numbers
  were recorded above** (both tables), per the mission rule that instrumentation may not
  outlive the decision it justified. They are reproducible from the tables' method
  descriptions: an `eprintln!` of `depth / stability / stability factor / effort factor`
  at the `set_time_scale` call site, driven over 24 book positions at `movetime 1000`.

## Code state after the revert

The implementation is **kept, maintained and toggleable**, following the shape M3-F1 and
M3-F2 established: `use_time_effort: false` in `SearchOptions::default`, UCI option
advertised as `default false`, and the measured reason recorded in the doc comment. The
per-root-move accounting stays in the root loop unconditionally — it is three lines, it
costs nothing measurable (bench signature and node counts are unchanged), and removing it
would make the toggle un-flippable without a revert.
