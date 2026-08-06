# M4-F1b — Capture LMR, re-measured

**Decision: KEEP. `UseCaptureLMR` now ships `true`.** One 300-game match against the M3
kept build scored **+11.59 ± 22.22 Elo**, zero forfeits. The shipped bench signature
moves **44,737 → 41,588**.

This is the one re-measurement `experiments/MSN-S2-capture-lmr/results.md` §7 authorized,
and the reason it was authorized is the whole result. The same code, unchanged, measured
-8.11 ± 20.67 six features ago. What changed in between is the thing that write-up named
as the binding constraint.

---

## 1. Purpose

M3-F2 implemented capture LMR, measured it, and shipped it OFF. That write-up did not
stop at the Elo: it diagnosed *why*, and the diagnosis made a prediction.

> A reduced move that fails high is re-searched at full depth, and **captures fail high
> far more often than quiets at the same move index**. […] Re-measuring capture LMR
> *before* [a `doDeeperSearch`/`doShallowerSearch` adjustment] exists would be re-running
> this experiment.

M3-F4 then shipped exactly that adjustment (`UsePostLMRDepth`, default on): the
verification re-search depth now responds to how far the reduced scout beat the incumbent
score instead of always paying full `child_depth`. The constraint named in the revisit
condition was removed, so the revisit condition was met.

This feature is **measurement only**. No search or evaluation logic was written. The only
code change is a default flip plus the anchors and comments that flip invalidates.

## 2. Baseline and single-variable discipline

`baselines/m3-search/manifold.exe`, bench **44,737**, SHA-256 `D080358B…B66FB5D`, built
from `c43dcdf`. That is the previous *kept* build: M3-F4 is the only M3 feature that
shipped enabled, and M4-F1 promoted the baseline and verified the whole toggle vector
against its results docs without touching engine code.

The two arms are **the same binary**. Engine A is `target/release/manifold.exe` with
`option.UseCaptureLMR=true`; engine B is the baseline at its defaults. The current tree
and the baseline are functionally identical with the toggle off — verified before the
match, not assumed: the pre-match build benched 44,737 twice consecutively, identical to
the baseline binary's own bench. So the only difference between the arms is the toggle.

## 3. The match

```powershell
.\harness\run_match.ps1 `
    -OutDir experiments\MSN-S7-capture-lmr-v2 `
    -Purpose 'M4-F1b: capture LMR re-measurement vs the M3 kept build, after UsePostLMRDepth shipped' `
    -AName capture-lmr-v2 -ACmd .\target\release\manifold.exe `
    -AOptions 'option.UseCaptureLMR=true' `
    -BName m3-search -BCmd .\baselines\m3-search\manifold.exe `
    -Rounds 150
```

8+0.08, Threads=1 both sides, Hash 64, UHO_4060_v4 book, `-use-affinity -concurrency 8`
(harness-enforced), seed 57376260, 17m00s wall. Pre-run CPU 10%, 15.5 GB free.

| | this match (M4-F1b) | M3-F2, for contrast |
|---|---|---|
| **Elo** | **+11.59 ± 22.22** | -8.11 ± 20.67 |
| nElo | +20.54 ± 39.32 | -15.44 ± 39.32 |
| Games | 300 — W87 / L77 / D136, 51.67% | 300 — W73 / L80 / D147, 48.83% |
| Ptnml(0-2) | [3, 31, 72, 41, 3] | [2, 37, 79, 30, 2] |
| LOS | 84.71% | 22.07% |
| PairsRatio | 1.29 | 0.82 |
| DrawRatio | 48.00% | 49.00% |
| Forfeits / crashes / illegal | **0 / 0 / 0** both engines | 0 / 0 / 0 |
| Adjudications | 0 | 0 |

The two matches are not directly comparable as measurements — different baselines, and
the M3-F2 arm lacked the depth band on both sides. What they share is conditions, book,
seed policy, TC, and game count, and the pentanomial distributions are the same shape
with the tails swapped: `[2,37,79,30,2]` became `[3,31,72,41,3]`. Nine decisive pairs
moved from the loss column to the win column.

### 3.1 An interrupted run was discarded

A first attempt at this match was interrupted at 58/300 games by a session pause. It was
deleted rather than resumed or reported: a truncated fixed-length match is not a shorter
match, it is a match whose stopping point correlates with nothing. The run above is a
clean start-to-finish 300 games. This is recorded because the partial console output
looked perfectly usable, which is exactly what makes it dangerous.

## 4. What the honest reading is

**The error bar covers zero.** +11.59 ± 22.22 does not establish that capture LMR gains
Elo, and this doc does not claim it does. LOS 84.7% means roughly a one-in-six chance the
true effect is negative.

The keep decision rests on the standing rule stated in the feature description — ship if
the point estimate is positive — and that rule is what M3-F2 was reverted under, with the
sign flipped. Applying the same criterion in both directions is what makes the pair of
measurements mean anything.

What **is** well supported is the ordering. Two 300-game matches under identical
conditions, on the same technique, separated by one change that a prior write-up had
named in advance as the blocker, moved the point estimate by ~20 Elo in the predicted
direction. That is a stronger claim than either match makes alone, and it is weaker than
"capture LMR is worth 11 Elo".

## 5. Where the gain does *not* come from: depth at equal time

M3-F2's sharpest finding was that a 25–33% fixed-depth node saving bought only +0.12
plies. The obvious hypothesis for this match is that the depth band lets the saving
convert. **It does not, or at least not visibly.**

`harness/depth_at_time.py`, 24 book positions, `movetime 1000`, Hash 64, run twice
(`depth-at-time.txt`, `depth-at-time-run2.txt`):

| run | capture-lmr-v2 | m3-search | delta | deeper / equal |
|---|---|---|---|---|
| 1 | mean 17.04, median 17.0 | mean 16.88, median 16.5 | **+0.17 plies** | 9/24, 7 equal |
| 2 | mean 16.92, median 17.0 | mean 16.83, median 16.5 | **+0.08 plies** | 8/24, 8 equal |

+0.17 and +0.08 plies, against +0.12 before. **The depth-at-time picture is unchanged.**

This was measured *because* the mechanism story predicted it would improve, and it is
reported *because* it did not. The saving is still not converting into depth in any way
this instrument can see, so whatever moved the match result is not "searches deeper".

The remaining candidate is the one the median hints at without proving: the median depth
is 17.0 vs 16.5 in both runs while the means are nearly equal, i.e. the enabled arm's
depth distribution is slightly tighter rather than uniformly higher. A reduction that
changes *which* lines get the last ply, rather than how many plies there are, would
produce that and would not show up in a mean. Establishing it would take a different
instrument than depth-at-time, and this feature's budget was one match.

**So the honest summary of §5 is: the mechanism by which this feature's number improved
is not established.** The re-measurement was justified by a mechanism argument, the
argument's most testable prediction was checked, and the prediction did not confirm.

## 6. Node measurements

Bench, depth 7 (`anchors.txt`, `anchors2.txt`; deterministic, two consecutive identical
runs before and after the flip):

| arm | signature |
|---|---:|
| **shipped (both features on)** | **41,588** |
| `UseCaptureLMR=false` | 44,737 — the M3 shipped signature, bit-for-bit |
| `UsePostLMRDepth=false` | 42,409 — the M3-F2 build, bit-for-bit |
| both off | 45,036 — the M2/M7 build, bit-for-bit |

-7.0% against the M3 shipped signature.

That table is now an attribution **square** rather than a chain, and all four corners are
pinned as tests in `crates/mf-uci/tests/bench_cli.rs`
(`capture_lmr_ships_enabled_and_reproduces_the_m3_signature_when_disabled`,
`post_lmr_depth_ships_enabled_and_reproduces_the_m3_f2_build`, and the new
`the_shipped_search_decomposes_to_its_two_predecessor_signatures`). Two interacting
features on the same code path can otherwise each be blamed for the other's contribution;
with four corners pinned, any drift identifies which one moved.

The `UsePostLMRDepth=false` corner is worth naming: **42,409 is the exact number M3-F2
recorded for its enabled build.** That arm reconstructs the rejected engine bit-for-bit,
which is the strongest available evidence that nothing else changed underneath this
technique between the two matches.

### 6.1 Every other anchor moved, and that is expected

Capture LMR ships ON, so unlike M3-F1/F2/F3 it is *supposed* to move the whole vector.
The full re-collected set is in `anchors.txt` / `anchors2.txt`; re-pinned values include
the history isolation vector (`38,858 / 37,593 / 41,436 / 37,032`), the LMR coupling
anchors, and the correction variants.

Two of those re-pins are worth reading rather than just diffing:

- **The material corrhist variant is tempting again.** It now benches `40,161` against
  the shipped `41,588` — a 3.4% saving. It read *cheaper* pre-M7, *dearer* under M7
  (`47,522` vs `45,036`), and cheaper again now. The thing that condemned it was a
  depth-14 probe showing a 3.3x node regression and a +25 cp score drift, plus its
  removal from the reference engine. None of that changed. The comment on that test now
  records the flip-flop explicitly, because an anchor that has swung twice is exactly
  the kind of number someone re-opens a settled decision with.
- **History pruning still benches favourably** (`40,046`, -3.7%). That makes three
  different searches under which it has looked good on bench and two matches it has lost
  (-103.68 and -45.63 Elo). Its test comment was updated to say three rather than two.

- **`BENCH_NODE_COUNT_WITHOUT_LMR` (124,323) and the `UseLMR=false` coupling anchors did
  NOT move**, which is a positive check rather than an omission: capture LMR is nested
  inside `use_lmr`, so the LMR-off control must be untouched by this flip. It is, exactly.
  The `post_lmr_handling_cannot_reach_the_tree_without_lmr` anchor (`80,425` × 3) is
  likewise unchanged.

## 7. Live UCI verification

`uci_probe.ps1` (Process-driven; redirected stdin aborts timed searches),
`uci-probe-transcript.txt`. Five repeats per arm per position, because one timed
observation is scheduling jitter:

```
option name UseCaptureLMR type check default true
option name UsePostLMRDepth type check default true

startpos  ON  depths [19,19,19,16,16] median 19    bestmove e2e4
          OFF depths [16,16,16,16,16] median 16    bestmove e2e4
kiwipete  ON  depths [15,13,15,14,14] median 14    bestmove e2a6
          OFF depths [12,12,13,13,14] median 13    bestmove e2a6
sicilian  ON  depths [15,15,15,15,16] median 15    bestmove c3d5
          OFF depths [16,16,16,16,16] median 16    bestmove c3d5
```

The shipped binary advertises the new default, both arms return the same legal bestmove
on all three positions, and info lines are well-formed (score, nodes, nps, hashfull, pv).

The repeats earn their cost here. On startpos the enabled arm splits 19/19/19/16/16 across
five identical runs — a single sample would have reported either "+3 plies" or "equal"
depending on which one it caught. And **sicilian is a full ply worse in the enabled arm,
consistently, 5/5**. That is not noise and it is not hidden here: a reduction that saves
a third of the tree will sometimes reach less depth on positions where the captures it
reduces were the ones worth searching. It is the same story §5 tells, on one position
instead of 24.

## 8. Decision and consequences

**KEEP. `UseCaptureLMR` ships `true`.**

- `crates/mf-search/src/search.rs`: `use_capture_lmr: false` → `true`; the comment now
  carries both measurements and the reason the second one was authorized.
- `crates/mf-uci/src/lib.rs`: advertised default `false` → `true`.
- `crates/mf-uci/tests/bench_cli.rs`: `BENCH_NODE_COUNT` 44,737 → **41,588**; every
  dependent anchor re-pinned from a freshly collected vector; the capture-LMR test
  inverted from "ships disabled" to "ships enabled and reproduces the M3 signature when
  disabled"; new `the_shipped_search_decomposes_to_its_two_predecessor_signatures`.
- `crates/mf-search/tests/search_invariants.rs`: the default-vector assertion updated;
  `capture_lmr_saves_nodes_on_tactical_middlegames_without_changing_the_move` has its
  arms swapped (the default is now the reduced one) with the assertion **unchanged** —
  it was written as a property, not as pinned numbers, and survived its feature's default
  flipping intact.

**The running baseline for M4-F2 is this build, bench 41,588.** M4-F1 promoted
`baselines/mission-final/` from a tree that benches 44,737, so that binary no longer
matches the shipped defaults and M4-F2 must re-promote it.

### Mission bookkeeping

Mission AGENTS.md 4.6 lists capture LMR as getting exactly ONE re-measurement, "if that
fails it becomes settled". It did not fail. It is no longer a mission-measured negative
and should be struck from that list.

## 9. Artifacts

| file | what |
|---|---|
| `run-metadata.txt` | harness provenance + self-check (AGENTS.md 4.7) |
| `console.txt`, `fastchess.log` | match output |
| `games.pgn` | 300 games (untracked by repo convention; seed + command reproduce it) |
| `anchors.txt` / `collect_anchors.ps1` | the MSN-S1 anchor sweep, retargeted at the new defaults |
| `anchors2.txt` / `collect_anchors2.ps1` | the anchor sessions the sweep above does not cover |
| `depth-at-time.txt`, `depth-at-time-run2.txt` | §5, both runs |
| `uci-probe-transcript.txt` / `uci_probe.ps1` | §7, 5 repeats per arm |
