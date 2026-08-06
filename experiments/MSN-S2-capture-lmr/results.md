# M3-F2 — Capture LMR

**Decision: REVERT (ships `UseCaptureLMR=false`).** Measured **-8.11 ± 20.67 Elo** over
300 games against the running baseline, zero forfeits. The error bar covers zero, so the
honest reading is "not shown to help", not "shown to hurt" — and the feature's stated
criterion was a positive point estimate.

The interesting part is not the Elo. It is that this feature **saves a fifth to a third
of the search tree at fixed depth, plays the same moves, and converts all of that into
+0.12 plies at equal time.** That is a much sharper result than M3-F1's, and Section 5
is the part worth reading.

---

## 1. Purpose

Extend LMR to late captures. Before this change `reduction` was computed only for quiet
non-check moves and was hard-zero for everything else, so a capture sitting 25th in the
move list was searched at full depth on the strength of being a capture alone.

Scope, per the feature description: reduce late captures using the existing log-log
table with capture history in place of the quiet `statScore`; never reduce the TT move,
checking captures, or queen promotions; keep the existing verification re-search on fail
high; add a UCI toggle; re-pin bench anchors; measure with one 300-game match.

## 2. Baseline (single-variable rule)

`baselines/m2-nnue/manifold.exe`, bench **45,036**.

M3-F1 (quiet checks in qsearch) shipped its feature OFF, so the M3-F1 build is
functionally identical to M2 and no new baseline was promoted for it. m2-nnue is
therefore still the previous *kept* build. Confirmed against
`library/m3-search-notes.md` and `experiments/MSN-S1-qchecks/results.md`.

## 3. What was implemented

`crates/mf-search/src/search.rs`:

- `capture_stat_score(captured_material, capture_history)` — the `statScore` a capture
  presents to the reduction formula, `873 * material / 128 + capture_history`.
- `capture_late_move_reduction(...)` — **literally `late_move_reduction` fed that
  statScore**. Same log-log table, same `+982` base, same improving / cut-node / ttPv
  adjustments, same `-statScore * 439 / 4096` divisor. One reduction shape, two kinds of
  evidence. This identity is pinned by a unit test, because it is what keeps the change
  single-variable.
- `capture_reduction_allowed(mv, tt_move, gives_check)` — the three exemptions. TT move
  (reducing the engine's own best guess), checking captures (forced replies make the
  reduced subtree unrepresentative, mirroring the quiet `gives_check` exemption), and
  queen promotions (a nine-point material swing is not a "late move" in any sense the
  table models). Under-promotions are deliberately NOT exempt.
- The reduction application is nested inside `use_lmr`: `UseCaptureLMR` can only do
  anything while `UseLMR` is on. That keeps the `UseLMR=false` ablation arm meaning "no
  late-move reduction of any kind", which is the control every other selectivity delta
  in `bench_cli.rs` is read against.

`crates/mf-uci/src/lib.rs`: `UseCaptureLMR` option, default false (see §7).

The verification re-search was untouched, as specified.

### 3.1 The design that was measured and rejected first

The feature description proposed *"reduction from the same table minus a constant"*. That
was implemented first, as a flat one-ply base discount, and it is **worse than not
reducing captures at all**:

| depth | flat one-ply discount | proportional (shipped impl) |
|---|---|---|
| 10 | **+5.7%** nodes | **-24.7%** |
| 12 | **+51.6%** nodes | **-33.1%** |
| 14 | -8.8% nodes | **-21.6%** |

A flat discount protects a late pawn grab exactly as much as it protects taking a hanging
queen. The pawn grabs get reduced anyway once the log-log scale grows, the queen captures
get reduced too early, they fail high, and the full-depth re-searches cost more than the
reductions saved. Making the protection **proportional to captured material** fixed the
node counts completely — and, as §5 shows, moved the Elo not at all.

This is recorded here because the inherited constant looked entirely reasonable, and one
cheap fixed-depth sweep is what disqualified it. Both numbers come from
`depth_nodes.ps1`, which is committed.

## 4. Node measurements (feature ON vs OFF, same binary)

Bench, depth 7: **45,036 → 42,409 (-5.8%)**. Deterministic across two consecutive runs.

Fixed depth over six tactical positions (`depth-nodes.txt`, regenerate with
`depth_nodes.ps1`):

| depth | ON | OFF | delta | bestmove disagreements |
|---|---:|---:|---:|---:|
| 10 | 234,277 | 311,122 | **-24.7%** | 1 / 6 |
| 12 | 619,731 | 926,469 | **-33.1%** | 1 / 6 |
| 14 | 2,295,283 | 2,927,053 | **-21.6%** | 2 / 6 |

The disagreements were checked rather than assumed (`promo-probe.txt`). On `promo-race`
the enabled arm plays a rook under-promotion instead of a queen and scores it **higher**
(556 cp vs 540 cp at depth 14, both winning) — a different tree finding a better line,
not a mis-scored promotion. On `startpos` at depth 14 both arms are within 10 cp.

## 5. Why it fails: the saving cannot be spent

`harness/depth_at_time.py`, 24 book positions, `movetime 1000`, Hash 64
(`depth-at-time.txt`):

```
capture-lmr  mean depth 15.88  median 15.0  min 14  max 19
m2-nnue      mean depth 15.75  median 15.0  min 13  max 19
+0.12 plies mean; deeper in 6/24, equal in 15
```

**A 25–33% node saving buys 0.12 plies.** Those two numbers cannot both be describing the
same search unless the saving is being handed straight back somewhere, and the place is
the verification re-search: a reduced move that fails high is re-searched at full depth,
and **captures fail high far more often than quiets at the same move index**. That is
exactly the asymmetry the material term *prices* — it is why the proportional design
fixed the node counts — but pricing it only decides *which* captures get reduced. It
cannot remove the re-search cost for the ones that still do.

Contrast with M3-F1, which is the other shape of failure: quiet checks reached **0.12
plies LESS** depth while costing +12.3% nodes. That feature spent more to get less. This
one genuinely gets more for less and cannot convert it. Two different mechanisms, and
only a depth-at-time run distinguishes them — the node counts alone would have called
this feature a clear win.

Note also what is *not* wrong with it: the moves agree, the scores agree, and the
reduction is sound. This is not history pruning (which pruned away the best move while
showing a favourable bench delta). It is a correct optimisation with nowhere to put its
winnings.

## 6. Match

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S2-capture-lmr `
    -Purpose 'M3-F2 capture LMR single-variable measurement vs the M2 kept build' `
    -AName capture-lmr -ACmd .\target\release\manifold.exe `
    -BName m2-nnue    -BCmd .\baselines\m2-nnue\manifold.exe `
    -Rounds 150
```

8+0.08, Threads=1 both sides, Hash 64, UHO_4060_v4 book, `-use-affinity -concurrency 8`
(harness-enforced), seed 43008549, 17m03s wall.

| | |
|---|---|
| **Elo** | **-8.11 ± 20.67** |
| nElo | -15.44 ± 39.32 |
| Games | 300 — W73 / L80 / D147, 48.83% |
| Ptnml(0-2) | [2, 37, 79, 30, 2] |
| LOS | 22.07% |
| PairsRatio | 0.82 |
| Forfeits / crashes / illegal moves | **0 / 0 / 0**, both engines |
| Adjudications | 0 |

Pre-run CPU read 23%. Per `library/m3-search-notes.md` that reading is treated together
with the forfeit count and wall time rather than as an automatic abort: zero forfeits on
both sides and a 17m03s wall time matching M2's ~17 min and M3-F1's 17m25s all indicate
an otherwise-idle machine.

## 7. Decision

**REVERT — the toggle ships `false`.** The point estimate is negative and the criterion
was a positive one. The implementation stays in the tree, maintained and toggleable, for
the same reason M3-F1's does: the technique is now *measured* rather than *missing*, and
the numbers above are the evidence for anyone who proposes it again.

Consequences, all verified:

- `BENCH_NODE_COUNT` stays **45,036**, bit-for-bit. A feature that ships disabled must
  not move the shipped signature, and every other anchor in `bench_cli.rs` is likewise
  byte-identical to M3-F1. The enabled signature 42,409 is pinned separately so the
  disabled technique stays measurable without a rebuild.
- **The running baseline for M3-F3 onward is still `baselines/m2-nnue`** (bench 45,036).
  Two M3 features in a row have now shipped off, so the shipped build remains
  functionally identical to M2. Do not promote a new baseline for this feature.

### Conditions for revisiting

Both target the re-search rather than the reduction, because §5 identifies the re-search
as the binding constraint:

1. **Post-LMR continuation-history update** — the reference updates continuation history
   when a reduced search fails high, which improves the ordering that decides *whether*
   the next capture gets reduced at all.
2. **`doDeeperSearch` / `doShallowerSearch`** — let the re-search depth respond to how
   badly the reduced scout missed, instead of always paying full `newDepth`.

Re-measuring capture LMR *before* either of those exists would be re-running this
experiment.

## 8. Artifacts

| file | what |
|---|---|
| `run-metadata.txt` | harness provenance + self-check (AGENTS.md 4.7) |
| `console.txt`, `fastchess.log` | match output |
| `games.pgn` | 300 games (untracked by repo convention; seed + command reproduce it) |
| `depth-nodes.txt` / `depth_nodes.ps1` | fixed-depth node counts, both designs |
| `depth-at-time.txt` | the +0.12 plies measurement — §5 |
| `promo-probe.txt` / `promo_probe.ps1` | the under-promotion diagnosis |
| `uci-probe-transcript.txt` / `uci_probe.ps1` | live UCI session, both toggle states |
| `anchors.txt` / `collect_anchors.ps1` | every `bench_cli.rs` anchor vector in ~90 s |

### Driver note (cost ~15 minutes)

The first version of `depth_nodes.ps1` redirected engine stdin from a file, and `go
depth` is **asynchronous** — `quit` arrived first and truncated every search. The symptom
was plausible-looking output: 90 nodes at depth 14, node counts that did not grow with
depth. `library/user-testing.md` documents this for `go movetime`; it applies to `go
depth` identically. Anything timed or iterative needs a real `System.Diagnostics.Process`
with a live stdin writer and blocking `ReadLine()`. File redirection remains fine for
synchronous `bench`, which is why `collect_anchors.ps1` still uses it.
