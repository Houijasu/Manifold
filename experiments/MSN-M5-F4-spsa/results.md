# M5-F4 — SPSA tuner (`mf-tune`)

**Purpose.** Build the tuner that M5-F5 will run: a resumable, fishtest-style SPSA loop
that drives paired fastchess batches between two perturbed copies of the engine and steps
the search hyperparameters M5-F3 exposed as UCI spins.

**No engine code was touched.** `bench` is `41,588` on two consecutive release runs, the
same signature M5-F3 shipped, and no `bench_cli` anchor was re-pinned. **No match was run
and none was warranted** — this feature adds a tool, not a change to the engine, so a
match would have measured only noise. The full tuning session and its validation match are
M5-F5.

---

## 1. What was built

`crates/mf-tune`, previously a one-line stub, is now a library plus a **standalone
`mf-tune` binary**.

### Why a separate binary and not `manifold tune`

The engine binary is what fastchess launches **sixteen at a time** during a 1T tuning
batch, each instance loading the embedded 106 MiB network. Anything added to `manifold`
is paid for on every one of those spawns and on every `go` in every real game. The tuner
needs to spawn processes, write checkpoints and parse PGNs — none of which has any
business being reachable from a UCI session — so it lives in its own binary and the engine
keeps its dependency set and startup cost exactly as they were.

```
mf-tune init --params <Name,Name,...> [--out <file>]
mf-tune run  --config <file> --out <directory> [--iterations N]
```

### Module layering

| Module | Responsibility | Why it is separate |
|---|---|---|
| `run` | the loop: perturb → play → update → checkpoint | generic over an `Arena` trait, so the resume path is tested without games |
| `batch` | fastchess invocation, affinity guardrail, PGN scoring | the guardrail is testable without playing anything |
| `spsa` | the update itself | no I/O, no chess; converges on a synthetic objective in milliseconds |
| `config` / `checkpoint` | what a run reads, and what it must survive being killed with | both on `document`'s shared TOML subset |
| `document` | a small TOML subset | one parser for both file kinds, not two |
| `interrupt` | Ctrl+C → a flag | the loop takes the signal as a parameter so it is testable |

### The algorithm

Fishtest's schedule, unmodified, because it is the only SPSA schedule with a decade of
evidence on this exact objective:

```
c_k   = c / k^gamma              c     = c_end * N^gamma
a_k   = a / (A + k)^alpha        a     = a_end * (A + N)^alpha,   a_end = r_end * c_end^2
theta = theta + (a_k / c_k) * result * flip
```

with `alpha = 0.602`, `gamma = 0.101`, `A = 0.1 * N`. Per iteration `k`:

1. Draw `flip ∈ {+1, -1}` independently per parameter.
2. Form `theta ± c_k * flip`, **rounded to integer spins and clamped to the advertised
   range** — the tuner never emits a value the engine would clamp behind its back.
3. Play `games_per_iteration` paired games (`-repeat`, so each opening is played twice
   with colours reversed) between the two arms.
4. `result` = the plus arm's **wins minus losses** over the batch.
5. Step theta, clamp to range, checkpoint, append a history row.

`c_end` and `r_end` are **per parameter**, in that parameter's own units, so a parameter
measured in 1024ths of a ply and one measured in centipawns share a run without either
drowning the other.

`result` is wins-minus-losses rather than Elo: at 8–16 games per iteration every Elo
estimate is noise, and win-minus-loss is the raw signal the gain is scaled for. It is read
out of the **PGN's `[Result]` tags**, not the fastchess console summary — the summary's
format is a moving target across releases, the `[Result]` tag is not.

### The parameter list is read, never copied

Names, defaults and ranges come from `mf_search::SEARCH_PARAMETERS`. A config naming a
parameter the engine does not advertise, or bounding one outside what the engine accepts,
is **rejected at load** with the offending name — not clamped at run time, which would
spend hours measuring a value the engine never played. `mf-tune init` generates a starter
config from that same table, so the config in this directory cannot go stale against the
engine.

---

## 2. Config format

`lmr-cluster.toml` in this directory is the generated config for M5-F5's recommended
subset (the top of M5-F3's sensitivity ranking), produced by:

```
mf-tune init --params "LmrCoefficient,LmrBase,LmrTtPvReduction,LmrHistoryNumerator,RfpMarginPerDepth,FutilityBaseMargin" --out experiments\MSN-M5-F4-spsa\lmr-cluster.toml
```

```toml
engine = "target/release/manifold.exe"
fastchess = "tools/fastchess/fastchess.exe"
book = "tools/books/UHO_4060_v4.epd"
iterations = 1000          # the gain HORIZON, N
games_per_iteration = 8
time_control = "5+0.05"
hash = 16
threads = 1
seed = 20260807

[[param]]
name = "LmrCoefficient"
value = 2872               # optional; defaults to the engine's shipped default
min = 1000                 # optional; defaults to (and may not exceed) the engine's range
max = 6000
c_end = 250.0              # optional; defaults to 5% of the range
r_end = 0.002              # optional; fishtest's default
```

Everything except `engine`, `fastchess`, `book`, `iterations` and one `[[param]]` name is
optional. Unknown keys are rejected rather than ignored — a config with `cend = 4.0`
should fail loudly, not silently tune with the default.

### `--iterations` shortens the budget, not the horizon

This distinction is load-bearing and was found by a failing test. `c` and `a` are both
derived from `N`, so an invocation that shortened `iterations` to run a smoke test would
step with gains derived from a 2-iteration horizon — roughly a hundred times larger than
the session it is supposed to stand in for. `--iterations` therefore sets a separate
**budget**, and `config.schedule` is left alone; a short run is a genuine *prefix* of the
configured one. `set_budget` refuses a value above the horizon.

---

## 3. Resume behaviour

**There is no resume flag.** `mf-tune run --out <dir>` continues from
`<dir>/checkpoint.toml` if one is there, and starts fresh if not. A run that had to be
*told* to resume would eventually be restarted by someone who forgot, silently discarding
hours of games.

- **Written every iteration**, atomically: full write to `checkpoint.toml.partial`, then
  rename over `checkpoint.toml`. A kill during the write cannot leave a truncated
  checkpoint, which would resume from a half-parsed theta and quietly tune the wrong point.
- **Theta is stored at full `f64` precision.** SPSA lives between the integers; a
  checkpoint that rounded to spins would discard every iteration's sub-spin progress since
  the last whole step.
- **The checkpoint is written before the history row**, deliberately. A history row for an
  iteration the checkpoint does not know about is merely duplicated on resume; the reverse
  ordering would silently replay a batch.
- **A checkpoint from a different parameter set is refused, not remapped.** Names must
  match the config position for position, or each value would land on the wrong axis —
  silent and catastrophic.
- **Perturbation signs are reproducible from `(seed, iteration)` alone** (`Rng::for_index`,
  the same splitmix64 generator `mf-datagen` uses). A resumed run replays exactly the signs
  the uninterrupted run would have drawn, without the checkpoint storing generator state.
  This is asserted directly: a 400-iteration run cut into three pieces lands on
  *bit-identical* theta to one that was never interrupted.

### Ctrl+C

`SetConsoleCtrlHandler` installs a handler that only raises a flag; the loop checks it
between iterations and exits with the checkpoint current. The default handler would
terminate the process wherever it happened to be — very likely mid-checkpoint-write.

**No orphan cleanup is attempted, and none is needed.** On Windows, Ctrl+C is delivered to
every process attached to the console, so fastchess receives it at the same instant and
tears down its own engines; a kill here would be racing a process that is already exiting.
What must not happen is the tuner dying first and leaving fastchess to finish a batch
nobody is waiting for — which is exactly what the flag prevents. `Get-Process
manifold,fastchess,stockfish` was empty after every run recorded below.

### A forfeited batch stops the run

If any game in a batch ends in a time forfeit the loop **errors out** rather than learning
from it. A batch with a forfeit is not measuring strength, and stepping theta on a harness
fault would move the run on noise. The checkpoint is current, so the session resumes after
the cause is fixed.

---

## 4. Harness rules: the documented exception

`AGENTS.md`'s two match-harness rules apply verbatim. Tuning batches **bypass
`harness/run_match.ps1`** — thousands of few-second batches would spend most of their wall
clock in PowerShell startup, SHA-256 binary hashing and 5×200 ms CPU-load sampling per
batch — so the affinity rule is **reimplemented in `batch.rs` in the same refusing form**,
derived from the thread count and **not reachable from the config**:

| Threads (both arms) | `-use-affinity` | `-concurrency` |
|---|---|---|
| 1 | **yes** | 8 |
| >1 | **no** | 1 |

This is `affinity_policy`, and it is unit-tested in both directions
(`a_single_threaded_batch_pins_and_a_multi_threaded_one_must_not`). Both arms of an SPSA
iteration are the same binary with different spins, so one setting decides it for both
sides and the two engines can never disagree.

**What is knowingly given up versus `run_match.ps1`:** per-batch `run-metadata.txt`
provenance, the memory pre-flight, and the post-match forfeit/crash cross-check against
the console. Provenance is recorded once per session instead of once per batch (config +
`history.csv` + the per-iteration PGNs). Memory is bounded by config review — the M5-F5
session's `hash = 16` gives 16 × 16 = 256 MiB against ~12 GB free, versus `run_match.ps1`'s
70%-of-free budget. Forfeits are **not** given up: they are counted from the PGN and abort
the run, which is the guarantee that mattered.

Every **measurement** match, including M5-F5's validation, still goes through
`run_match.ps1` under the 300-game cap. This exception covers tuning batches only, as
authorised in mission `AGENTS.md` rule 2.

---

## 5. The smoke run

`crates/mf-tune/tests/smoke_run.rs`, `#[ignore]`d like `mf-nnue`'s 10k parity gate because
it plays real games. It runs the **real `mf-tune` binary** against the **release** engine
and real fastchess:

```
cargo test -p mf-tune --release --test smoke_run -- --ignored --nocapture
```

Release specifically: a debug engine at a 5+0.05 tuning TC loses on time, and the run would
then fail for a reason unrelated to the tuner.

**Result: passed in 64.4 s** — 2 parameters, 2 iterations of 4 games, then a resume to a
third iteration. The test asserts, from the artifacts rather than from the tuner's own
claims:

- `checkpoint.toml` reads `completed = 2`, `games_played = 8`, both parameter names;
- `history.csv` has a header plus exactly one row per iteration;
- each `iteration-NNNNNN.pgn` holds exactly 4 `[Result ]` tags, contains **both**
  `[White "plus"]` and `[White "minus"]`, and contains **no** `time forfeit`;
- the resume prints `resuming at iteration 3`, does **not** print `iteration 1/3`, and
  **appends** one history row rather than restarting the log.

Artifacts committed here as `smoke-checkpoint.toml` and `smoke-history.csv`:

```
iteration,wins,losses,draws,score,LmrCoefficient,LmrCoefficient_spin,RfpMarginPerDepth,RfpMarginPerDepth_spin
1,0,0,4,0,2872.0000,2872,105.0000,105
2,1,1,2,0,2872.0000,2872,105.0000,105
3,0,3,1,-3,2869.5827,2870,104.7583,105
```

Iterations 1 and 2 scored 0 and left theta exactly where it was — the correct behaviour,
and visible confirmation that a null batch is not a random walk. Iteration 3's `-3` moved
`LmrCoefficient` by 2.4, i.e. **theta moved before the spin did** (2872 → 2870 while
`RfpMarginPerDepth`'s spin stayed at 105 despite theta moving to 104.76). That is the
fractional-theta property the checkpoint has to preserve, observed end to end.

### Confirming the spins reach the engine's tree

The unit tests prove the tuner *emits* the right spins; they cannot prove the engine
*acts* on them. A deliberate 2-game probe at the extreme perturbation
(`LmrCoefficient` `value = 3500`, `c_end = 2500`, so the arms played 6000 vs 1000), with
depths read out of the PGN annotations:

| Arm | LmrCoefficient | plies annotated | median depth | min | max |
|---|---:|---:|---:|---:|---:|
| plus | 6000 | 135 | **14** | 10 | 19 |
| minus | 1000 | 136 | **11** | 8 | 15 |

A 3-ply median difference at equal time. The tuner's `setoption` writes reach the search
of a real time-managed game, not merely of `bench`.

---

## 6. Throughput, measured (for M5-F5's budget)

The feature description estimated ~1,500–1,800 games/hour at 5+0.05. **Measured on this
machine it is 1,100–1,500, and it depends strongly on the batch size** — worth knowing
before M5-F5 commits to a wall-clock budget. Each batch is a fresh fastchess process plus
16 engine spawns, every one loading the 106 MiB net, so a fixed cost is paid per
*iteration* rather than per game. 6 parameters, `lmr-cluster.toml`, 64 games per row:

| games/iteration | iterations | wall | s/batch | **games/hour** |
|---:|---:|---:|---:|---:|
| 8 | 8 | 209.3 s | 26.2 | **1,101** |
| 16 | 4 | 159.8 s | 39.9 | **1,442** |
| 32 | 2 | 153.7 s | 76.9 | **1,499** |

A repeat of the 16-game row on a separate invocation gave 1,347 games/hour, so treat these
as ±7%. The implied marginal cost is ~2.2 s/game with several seconds of fixed per-batch
overhead.

**Guidance for M5-F5.** `games_per_iteration = 8` throws away ~25% of the machine's
throughput to per-batch overhead. But SPSA's convergence comes from the *number of
iterations*, not the number of games per iteration, so the tradeoff is real rather than
free: 16 games/iteration buys ~30% more games per hour at the cost of half as many
gradient estimates per hour. `16` is the recommended compromise; at ~1,400 games/hour an
8,000-game session is ~5.7 hours and 500 iterations, and a 12,000-game session ~8.6 hours
and 750 iterations. The horizon `iterations` in the config must be set to the number of
iterations actually intended, since it is what the gains are derived from.

---

## 7. Tests

**54 unit tests + 1 integration smoke.** The ones that pin a property rather than a
behaviour:

| File | Test | What it pins |
|---|---|---|
| `spsa.rs` | `the_update_converges_on_a_synthetic_quadratic_objective` | 4,000 iterations against a two-axis paraboloid with 100× different curvature per axis close ≥75% of the gap on **both** axes |
| | `convergence_does_not_depend_on_one_lucky_seed` | the same, ≥50%, over four seeds |
| | `a_positive_result_moves_theta_along_the_flip_and_a_negative_one_against_it` | sign of the update, and that ±result move theta by equal and opposite amounts |
| | `a_measurement_of_zero_leaves_theta_exactly_where_it_was` | a null batch is not a random walk |
| | `perturbations_and_theta_never_leave_the_advertised_range` | with `c_end` wider than the whole range, **no emitted spin is ever outside it** |
| | `the_gains_decay_and_the_final_learning_rate_is_r_end_times_c_end` | the schedule is fishtest's, at both ends |
| | `the_same_seed_and_iteration_always_draw_the_same_signs` | the property resume correctness rests on |
| | `both_signs_are_drawn_and_neither_dominates` | 800–1200 positive out of 2000 |
| | `a_malformed_run_is_rejected_rather_than_started` | incl. non-finite `c_end`, which would NaN the whole run |
| `run.rs` | `a_resumed_run_continues_from_the_checkpoint_and_reproduces_the_uninterrupted_result` | **400 iterations cut into 3 pieces == 400 uninterrupted, bit-identical theta**; every iteration ran exactly once |
| | `an_interrupt_stops_between_iterations_and_the_checkpoint_resumes_it_exactly` | Ctrl+C is a clean stop; the resume replays nothing |
| | `a_forfeited_batch_stops_the_run_and_leaves_the_last_good_checkpoint` | the forfeited iteration is **not** recorded as completed |
| | `a_run_whose_budget_is_already_met_does_no_games_and_reports_finished` | a finished run does not replay its last iteration |
| | `a_checkpoint_from_another_parameter_set_stops_the_run_rather_than_mismapping_it` | |
| `batch.rs` | `a_single_threaded_batch_pins_and_a_multi_threaded_one_must_not` | the AGENTS.md 4.451 guardrail, both directions |
| | `scoring_counts_the_plus_arm_from_both_colours` | |
| | `real_fastchess_headers_from_this_repo_are_parsed` | against a header block copied from `MSN-M5-sweep`'s PGN |
| | `an_empty_or_headerless_pgn_scores_zero_games_rather_than_panicking` | a killed batch's half-written game is not counted as a draw |
| `checkpoint.rs` | `a_fractional_theta_survives_exactly_so_a_resume_is_not_a_rounding_event` | |
| | `writing_over_an_existing_checkpoint_replaces_it_and_leaves_no_partial_file` | |
| `config.rs` | `shortening_a_run_moves_the_budget_and_leaves_the_gain_schedule_alone` | the section-2 distinction |
| | `a_range_wider_than_the_engine_accepts_is_rejected_rather_than_clamped` | |
| | `generating_a_config_for_every_advertised_parameter_produces_a_valid_config` | `init` covers all 36 |

Two design defects were found **by failing tests**, not by review, and both were fixed in
the design rather than in the assertion:

1. `--iterations` re-derived the gain schedule (section 2). Caught by the three-piece
   resume test landing on different theta than the uninterrupted run.
2. The loop read a process-wide interrupt flag directly, which no test could set without
   stopping every other test in the binary. The stop signal is now a parameter, which is
   what made `an_interrupt_stops_between_iterations...` possible at all.

---

## 8. Gates

| Command | Result |
|---|---|
| `cargo test --workspace` (debug) | green |
| `cargo test -p mf-tune` | 54 passed, 1 ignored (the smoke) |
| `cargo test -p mf-tune --release --test smoke_run -- --ignored` | 1 passed, 64.4 s |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `manifold bench` ×2 | `41588` / `41588` — unchanged from M5-F3 |
| `Get-Process manifold,fastchess,stockfish` after every run | empty |

---

## 9. Decision

**KEEP.** The tuner is the deliverable; there is nothing to revert. The engine is
byte-unchanged (`bench 41,588`, no anchor re-pinned), so no match was run — per the
engine-worker rule that a bit-exact change needs no match.

**Handover to M5-F5.**

- `lmr-cluster.toml` here is the ready-to-run config for the recommended 6-parameter
  subset. Set `iterations` to the number of iterations actually intended before launching:
  it is the gain horizon, not a stopping condition.
- Prefer `games_per_iteration = 16` (section 6): ~1,400 games/hour, ~5.7 h for 8,000 games
  / 500 iterations.
- The session is interruptible and resumable at any point — rerun the identical command.
- Take the final spins from the tuner's closing summary or from the last `history.csv` row
  (the `_spin` columns), and validate with **one** 300-game `run_match.ps1` match at
  8+0.08, tuned spins via `-AOptions` against shipped defaults.
- If a batch ever forfeits, the run stops by design; investigate before resuming, and do
  not quote the session as clean until it finishes without one.
