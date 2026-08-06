# M3-F4 — Post-LMR re-search improvements

**Decision: SPLIT, then KEEP one half and REVERT the other.**

- `UsePostLMRDepth` (verification re-search depth band) ships **`true`**.
  **+3.47 ± 21.91 Elo** over 300 games vs the running baseline, zero forfeits, and it
  is the first M3 feature whose fixed-depth tree got *smaller* in the direction the
  milestone was aiming at (bench `45_036` → `44_737`, median book tree −4% to −6.5%).
- `UsePostLMRContHist` (post-LMR continuation bonus) ships **`false`**. Measured alone
  it costs **+5.9% median nodes at depth 12** and buys nothing at equal time.

**This is the first M3 feature to ship enabled, and therefore the first to move the
shipped bench signature. The running baseline for M4 onward is the M3-F4 build (bench
`44_737`)** — see §8.

---

## 1. Purpose and the specified scope

The feature description elevated this work after M3-F1 and M3-F2 both produced real
fixed-depth node savings that would not convert to depth at equal time.
`experiments/MSN-S2-capture-lmr/results.md` §5 named the always-full-depth verification
re-search as the binding constraint and named this feature as the prerequisite before
capture LMR is ever re-measured.

Two sub-mechanisms were specified, to be measured **as one package (single toggle)
"unless the worker finds cause to split"**:

1. **post-LMR continuation-history update** — bonus continuation history once an LMR
   re-search resolves;
2. **doDeeperSearch / doShallowerSearch** — let the verification depth respond to how
   far the reduced scout's score exceeded the incumbent.

There was cause to split. §4 is that evidence, and it is the most important section
here.

## 2. Baseline (single-variable rule)

`baselines/m2-nnue/manifold.exe`, bench **45,036**.

M3-F1, M3-F2 and M3-F3 all shipped their features OFF, so the M3-F3 build is
functionally identical to M2 and no new baseline was promoted for any of them.
Confirmed against `library/m3-search-notes.md`.

## 3. What was implemented

`crates/mf-search/src/search.rs`:

- `post_lmr_verification_depth(child_depth, reduced_depth, best_score, scout_score)` —
  the verification depth. `+1` when the scout cleared the incumbent best score by more
  than `POST_LMR_DEEPER_MARGIN` (53) **and the scout was actually reduced**, `-1` when
  it cleared it by less than `POST_LMR_SHALLOWER_MARGIN` (8), unchanged in between,
  floored at 1. When the adjusted depth lands at or below `reduced_depth` the
  verification is skipped entirely and the scout score stands — the scout already
  searched at least that deep.
- `POST_LMR_CONTINUATION_BONUS` (1,334) applied through the existing
  `update_continuation_histories` fan-out at the fail-high. The moving piece is
  resolved **before** `make_move`, because `mv.from()` is empty by the time the update
  site runs.
- Both sites sit inside the existing `score > alpha && reduced_depth < child_depth`
  branch, so both are structurally unreachable when nothing was reduced. That is
  asserted rather than assumed (`post_lmr_handling_cannot_reach_the_tree_without_lmr`,
  `disabling_lmr_disables_both_post_lmr_mechanisms`): the continuation bonus writes to a
  table that move ordering, the LMR `statScore`, and pruning history all read, so a leak
  would stop `UseLMR=false` being the clean control every other selectivity anchor is
  read against.

`crates/mf-uci/src/lib.rs`: `UsePostLMRDepth` (default true) and `UsePostLMRContHist`
(default false).

**Margin units.** The reference's 53 and 8 sit in its own internal eval scale. They were
kept unconverted because this engine's other search margins are denominated in the same
rough scale (`RFP_MARGIN_PER_DEPTH = 105`, `FUTILITY_BASE_MARGIN = 124`,
`PROBCUT_BASE_MARGIN = 241` are all direct ports), and because re-tuning them is a
second variable this measurement is not equipped to separate.

## 4. The split: why one toggle would have measured the wrong thing

The package as specified was built first and measured as one toggle. It looked bad:

| depth | package nodes vs off |
|---|---|
| 10 | +0.9% |
| 12 | **+25.9%** |
| 14 | **+30.0%** |

(`depth-nodes.txt`, six tactical positions, `depth_nodes.ps1`.)

At that point the honest options were "revert the package" or "find out which half".
Temporary environment-gated instrumentation (`MF_DIAG_NO_DEPTH`, `MF_DIAG_NO_HIST`,
since deleted) split it into four arms, and the six-position sweep
(`split-probe.txt`) came back **incoherent**: `depth-only` read −6.8% at depth 10,
+43.4% at depth 12, −8.7% at depth 14. Signs flipping with depth on the same six
positions is not a measurement.

The distribution is the reason. A verification-depth change has a long-tailed
per-position node effect, and on six positions one outlier owns the aggregate — at
depth 12 `sicilian` alone went 55,728 → 251,377 and carried the entire `depth-only`
column. So the corpus was widened to 24 positions from the same UHO book the matches
use, with `ucinewgame` between positions so no arm inherits another's tables, and the
**median ratio** reported alongside the sum (`book_nodes.ps1`):

| arm | d12 total | **d12 median** | d14 total | **d14 median** |
|---|---:|---:|---:|---:|
| neither (= both toggles off) | — | 1.000 | — | 1.000 |
| depth band only | +0.57% | **0.935** | −0.98% | **0.960** |
| conthist bonus only | +5.92% | **1.068** | +1.30% | 0.986 |
| both | +9.47% | 1.053 | +9.87% | 1.061 |

(`book-nodes.txt`, `book-nodes-d14.txt`.)

**The two sub-mechanisms move the tree in opposite directions, and together they are
worse than either alone.** A single toggle would have reported their *difference* and
called it "the package" — which is exactly the +26%/+30% number that nearly got the
whole feature reverted. That is the cause to split the description allowed for.

Mechanistically this is unsurprising in hindsight. The depth band is a **search**
change: it spends a ply where the evidence is strong and saves one where it is weak,
and the saving dominates because the shallower band fires far more often than the
deeper one. The continuation bonus is an **ordering** change, and it adds a fourth
writer to a continuation table that three tuned consumers already read, with a bonus
magnitude imported from an engine whose other history sites all use different ones.
That is the composition failure M3-F3 measured at ~20 Elo, in a different table.

## 5. Depth at equal time — and a noise floor worth recording

`harness/depth_at_time.py`, 40 book positions, `movetime 1000`, Hash 64, each arm
against `baselines/m2-nnue`:

| arm | mean depth | vs m2-nnue |
|---|---:|---:|
| **neither** (bit-identical to m2-nnue) | 15.10 | **+0.25 plies** |
| depth band only | 15.20 | +0.35 plies |
| conthist bonus only | 14.97 | +0.12 plies |
| both | 15.05 | +0.18 plies |

The first row is the finding. The `neither` arm produces the **identical bench
signature 45,036** and is functionally the same search as the baseline, and it still
reads **+0.25 plies**. So on this corpus at this movetime, ±0.25 plies is noise —
which retroactively means M3-F1's −0.12 and M3-F2's +0.12 were both inside the noise
floor of that instrument, and the *only* reason they were readable as mechanisms is
that they came with node deltas of 12–33% pointing the same way.

**Reusable rule: `depth_at_time.py` needs a bit-identical control arm before any
sub-ply difference in it is quoted.** It cost ~4 minutes here and it is what stopped
"+0.35 vs +0.25" being written up as a depth gain.

## 6. Node measurements for the shipped build

Bench, depth 7: **45,036 → 44,737 (−0.66%)**, deterministic across two consecutive
runs. `UsePostLMRDepth=false` reproduces **45,036 bit-for-bit**, which is the
attribution proof that this feature moved the signature and nothing else did.

Every `bench_cli.rs` anchor was re-collected against the release binary
(`collect_anchors.ps1`, `anchors.txt`, ~90 s) and re-pinned. The anchors that moved,
moved because the shipped tree moved; the two all-off ablation anchors (`3_473_717`
and `2_848_247`) and the `UseLMR=false` anchors (`124_323`, `151_903`) are **unchanged
bit-for-bit**, which is the control proving the search core outside the LMR
verification path was not touched.

Live UCI session (`uci-probe-transcript.txt`, `uci_probe.ps1`): `uci` → `uciok` with
both options advertised at their real defaults, `isready` → `readyok`, `go movetime
2000` on kiwipete → well-formed info lines with depth/seldepth/nps/hashfull and a legal
`bestmove`. At `go depth 14` the three arms are 281,041 / 467,043 / 660,324 nodes, all
playing `e2a6`.

## 7. Match

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S4-postlmr `
    -Purpose 'M3-F4 post-LMR verification-depth band single-variable measurement vs the M2 kept build' `
    -AName postlmr -ACmd .\target\release\manifold.exe `
    -BName m2-nnue -BCmd .\baselines\m2-nnue\manifold.exe `
    -Rounds 150
```

8+0.08, Threads=1 both sides, Hash 64, UHO_4060_v4 book, `-use-affinity -concurrency 8`
(harness-enforced), seed 79205006, 17m22s wall, pre-run CPU 6%.

| | |
|---|---|
| **Elo** | **+3.47 ± 21.91** |
| nElo | +6.24 ± 39.32 |
| Games | 300 — W81 / L78 / D141, 50.50% |
| Ptnml(0-2) | [2, 38, 66, 43, 1] |
| LOS | 62.22% |
| PairsRatio | 1.10 |
| Forfeits / crashes / illegal moves | **0 / 0 / 0**, both engines |
| Adjudications | 0 |

The binary under test is the shipped configuration: depth band ON, continuation bonus
OFF. The continuation bonus was **not** matched — it lost on fixed-depth nodes at both
depths with no equal-time depth to show for it, and spending 20 minutes of match time
to put a ±22 Elo error bar around a mechanism already measured as a node regression
would not have changed the decision.

## 8. Decision

**KEEP the depth band (`UsePostLMRDepth=true`), REVERT the continuation bonus
(`UsePostLMRContHist=false`).**

The Elo is inside noise, so the feature description's tiebreak applies: *"if the
measurement is null, default ON only if fixed-depth node counts improved, else OFF."*
For the depth band they improved — median 0.935 at depth 12 and 0.960 at depth 14
against a bit-identical control, plus −0.66% on bench — so it defaults ON. For the
continuation bonus they got worse, so it defaults OFF. Both halves stay implemented,
tested, and toggleable.

Consequences, all verified:

- `BENCH_NODE_COUNT` moves **45,036 → 44,737**. This is the first M3 feature to move it,
  because it is the first one to ship enabled. `UsePostLMRDepth=false` restores 45,036
  exactly and that equality is pinned as a test.
- **The running baseline for M4 onward is the M3-F4 build (bench 44,737), not
  `baselines/m2-nnue`.** The three-feature run of "the shipped build is functionally
  identical to M2" ends here. A new baseline directory should be promoted from this
  build before the next feature is measured.

### Flagged to the orchestrator: capture LMR is now worth one re-measurement

`experiments/MSN-S2-capture-lmr/results.md` §7 made re-measuring capture LMR
conditional on the re-search being addressed first. Half of that condition is now met:
the verification depth responds to the scout margin. Capture LMR (`UseCaptureLMR`,
enabled signature now **41,588**, down from 42,409 under the new band) is therefore
worth **one** re-measurement — against the new M3-F4 baseline, not against m2-nnue.

**That re-measurement was deliberately NOT run in this feature** (single-variable rule,
and the 300-game budget was spent on the mechanism this feature owns). Note that the
other half of the M3-F2 condition — the post-LMR continuation update — is now measured
and rejected, so a re-measurement tests capture LMR against the depth band alone.

## 9. Artifacts

| file | what |
|---|---|
| `run-metadata.txt` | harness provenance + self-check (AGENTS.md 4.7) |
| `console.txt`, `fastchess.log` | match output |
| `games.pgn` | 300 games (untracked by repo convention; seed + command reproduce it) |
| `anchors.txt` / `collect_anchors.ps1` | every `bench_cli.rs` anchor vector in ~90 s |
| `anchors-new.txt` | the same vector for the ONE-TOGGLE package build (§4), kept as the record of what the un-split feature measured |
| `book-nodes.txt`, `book-nodes-d14.txt` / `book_nodes.ps1` | the four-arm 24-position sweep that justified the split — §4 |
| `split-probe.txt` / `split_probe.ps1` | the six-position sweep that did NOT resolve it, kept because "too few positions" is the lesson |
| `depth-nodes.txt` / `depth_nodes.ps1` | the one-toggle package's fixed-depth cost |
| `depth-at-time.txt` | depth at equal time, plus the control-arm noise floor — §5 |
| `uci-probe-transcript.txt` / `uci_probe.ps1` | live UCI session, all three arms |

### Note on the deleted instrumentation

The four-arm attribution in §4 was first produced by two temporary env-gated predicates
in `search.rs` (`MF_DIAG_NO_DEPTH`, `MF_DIAG_NO_HIST`), because the split toggles did
not exist yet — they are the *conclusion* of that measurement. The predicates are
deleted from the committed tree, and `book_nodes.ps1` now selects the same four arms
through the shipped UCI toggles instead. Both sweeps were re-run against the committed
build and reproduce **every figure in the §4 table bit-for-bit** (d12 medians 1.000 /
0.935 / 1.068 / 1.053; d14 medians 1.000 / 0.960 / 0.986 / 1.061), so nothing in this
document depends on instrumentation that no longer exists.

`split_probe.ps1` is left as-is, still referencing the env vars: it is kept as the
record of the six-position sweep that FAILED to resolve the split, and rewriting it
would misrepresent what was actually run. Its lesson — too few positions for a
long-tailed distribution — does not need re-running.
