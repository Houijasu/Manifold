# Checked-node static-eval removal: design

Date: 2026-08-19
Status: proposed, awaiting approval
Depends on: `execution/reliability-foundation` merged (instrumentation, harness
provenance, CI); evidence lives in `experiments/2026-08-18-reliability-foundation/`.

## Question

Should the interior search (`pvs`) spend an NNUE forward evaluation on nodes where the
side to move is in check, when no TT entry supplies a static evaluation?

Today it does. `quiescence` already skips static evaluation in check (it uses the
`UNEVALUATED_STATIC_EVAL` sentinel path), but `pvs` computes `raw_static_eval`
unconditionally: TT static-eval reuse when available, otherwise a fresh
`context.static_eval(position)` NNUE forward — even though every pruning consumer of
that value is gated on `!in_check`.

## Evidence ceiling (reliability-foundation telemetry, depth 7, bench suite)

- Checked interior nodes: 1,553 of 29,594 interior nodes (5.248%).
- Interior static evaluations: 7,041; all NNUE forwards: 21,755.
- The 1,553 checked nodes are an **upper bound** on avoidable forwards: the counters do
  not separate checked nodes that performed a fresh forward from checked nodes that
  reused a TT static evaluation.
- Upper-bound ceiling: 22.057% of interior forwards, 7.139% of all forwards.
- NNUE forward cycles this run: 12,699,077 (machine-local, not a portable claim).

A 7% upper bound on all forwards is worth one measured experiment. It is not evidence
for a default change: in-check evals may carry tactical weight (they feed `improving`
and `corrplexity`, which LMR and reduction margins read), and removing them changes
the tree, not just its cost.

## Current behavior (code audit)

In `crates/mf-search/src/search.rs::pvs`:

1. `in_check` is computed first.
2. `raw_static_eval` = TT entry's static eval when present and not the sentinel, else a
   fresh `context.static_eval(position)` — **unconditionally, including in check**.
3. `correction` and `corrplexity` are computed unconditionally; `corrplexity =
   correction.abs()` when `use_corrplexity`.
4. `context.static_evals[ply]` is recorded only when `!in_check`.
5. `improving = is_improving(static_eval, static_evals[ply - 2])` is computed
   unconditionally.
6. Razoring, RFP, NMP, ProbCut, and the move-loop pruning (LMP/futility/history/SEE)
   are all gated on `!in_check` already.
7. The final pvs TT store writes `static_eval: raw_static_eval` unconditionally, so a
   checked node's entry today carries a real static eval that later probes reuse. The
   Syzygy and M2 stores already write the `UNEVALUATED_STATIC_EVAL` sentinel, and
   qsearch stores the sentinel for checked nodes.
8. `improving` flows into LMR reduction depth and ProbCut margins; `corrplexity` flows
   into LMR, RFP, and the singular double margin.

So the only NNUE cost under experiment is step 2; the behavioral consumers to re-pin
are steps 3 and 5.

## Proposal

Add a UCI toggle following the existing `UseXxx` pattern:

- `option name UseCheckedNodeEval type check default true` in mf-uci.
- `use_checked_node_eval: bool` on `SearchOptions` in mf-search, default `true`
  (current behavior, bit-identical trees).

When the toggle is **off** and the node is in check:

- Do not call `context.static_eval(position)` and do not read a TT static eval.
- `raw_static_eval` is the sentinel; the pvs TT store writes
  `UNEVALUATED_STATIC_EVAL` for this node so later probes cannot reuse a value that was
  never computed (matching the existing qsearch in-check behavior).
- `context.static_evals[ply]` stays `None` (unchanged).
- `improving := false` (primary semantics; matches "no information, no improving
  evidence"). Record `improving := is_improving(..., None)` (which returns `true` for
  `None`) as the ablation arm if the primary regresses.
- `corrplexity := 0`; `correction` is not applied (there is no eval to correct).
- All pruning paths remain gated on `!in_check`, so none of them can read the absent
  eval; the audit must confirm every `static_eval`/`improving`/`corrplexity` consumer
  in the move loop and extension logic respects this.

With the toggle **on**, the code path must be bit-identical to today: same tree, same
TT contents, same 37,420-node bench signature.

## Instrumentation

Reuse the default-off `instrumentation` feature. Add counters:

- `checked_node_static_evals` — fresh forwards performed at checked nodes (toggle on).
- `checked_node_evals_skipped` — checked nodes that skipped evaluation (toggle off).

Both must sum with the existing counters to keep `checked_interior_nodes` consistent.
No instrumentation ships in default builds.

## Tests (all deterministic, no wall-clock assertions)

1. Toggle on: bench signature stays 37,420; TT contents after a fixed search are
   unchanged.
2. Toggle off: a fixed-depth fixed-position search produces a stable node count
   (recorded as the experiment's own signature); checked nodes never call
   `static_eval` (assert via instrumentation counters: `checked_node_static_evals ==
   0` when off).
3. Toggle off: `static_evals[ply]` history is never populated in check; `improving`
   and `corrplexity` take their pinned values (unit test at the pvs boundary).
4. Regression: mate-in-N puzzles in check still resolve correctly with the toggle off
   (uses the existing mate test corpus); no illegal moves over a fixed random-game
   suite.

## Measurement

Harness: `harness/run_match.ps1` with its enforced affinity/forfeit guardrails.
Same binary, toggle as the only variable, fixed **time** (never fixed nodes across
different work profiles).

1. NPS: `harness/nps_compare.py`, depth 12, Hash 64, Threads 1, warmup 1, repeat 3,
   default (on) vs off. Expect a small positive NPS ratio for off; record, do not gate.
2. STC SPRT: 8+0.08, Threads=1, `-use-affinity`, concurrency 8, book
   UHO_4060_v4.epd, `elo0=0 elo1=2 alpha=0.05 beta=0.05`.
3. LTC SPRT if STC passes: 40+0.4, Threads=1, same guardrails, `elo0=0 elo1=2`.

Decision rule:

- STC fails → keep default `true`, record results, stop.
- STC passes, LTC fails → default stays `true`, record, stop.
- Both pass → PR flips the default to `false` with both SPRT logs, the NPS comparison,
  and both bench signatures (on: 37,420; off: recorded) in the evidence directory.

## Risks and mitigations

- **Tactical blindness in check**: mitigated by the mate corpus test and the SPRT
  gates; in-check nodes are still searched with check extensions, only the eval is
  skipped.
- **`improving` semantics**: pinned to `false` as primary; the `None`-based `true`
  arm is a documented ablation, not a silent fallback.
- **TT interaction**: skipping the static-eval write changes what future probes can
  reuse at checked nodes; the experiment measures the net effect, and the determinism
  tests pin both toggle states.
- **Scope creep**: this experiment touches `pvs` only. Qsearch already skips checked
  evals; threshold SEE is a separate deferred question and is out of scope here.

## Out of scope

- Changing any default in the experiment PR without both SPRT passes.
- Threshold SEE, checked-node *qsearch* behavior, or any other pruning change.
- Training-data implications (datagen uses the same pvs; a default flip would be a
  separate decision after the SPRT evidence).
