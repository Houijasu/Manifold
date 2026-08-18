# Reliability foundation validation

## Provenance

- Engine source commit profiled:
  `bdb42a2ac643fb330b6fb3e9e82941d41f4d0813`
- NNUE fixture:
  `C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation\nets\main.nnue`
- NNUE size: `111,261,604` bytes
- NNUE SHA-256:
  `E8449B689E26E40DFD8FAC0423E7825377AFDE8B7D40FC14BFB96DFA32FF908A`
- Working directory:
  `C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation`
- Shell: PowerShell 7

The captured command and environment were:

```powershell
$env:MF_NNUE_TEST_NET = 'C:\Users\Samaritan\AppData\Local\Temp\manifold-reliability-foundation\nets\main.nnue'
cargo run --release -p mf-search --features instrumentation --example search_profile -- 7 |
    Tee-Object 'experiments\2026-08-18-reliability-foundation\search-profile.txt'
```

`search-profile.txt` contains exactly the six stable `key=value` profile rows emitted
on standard output. Cargo diagnostics were emitted separately on standard error.

## Final local gates

All commands below used the same `MF_NNUE_TEST_NET` value shown above.

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `cargo test --workspace` | Passed |
| `cargo test -p mf-core --features force-magic` | Passed |
| `cargo test --release -p mf-core --test perft` | Passed: 8/8 tests |
| First uninstrumented `cargo run --release -p mf-uci --bin manifold -- bench` | Passed: 37,420 nodes |
| Second uninstrumented `cargo run --release -p mf-uci --bin manifold -- bench` | Passed: 37,420 nodes |
| `pwsh -NoProfile -File harness/provenance.tests.ps1` | Passed |
| `pwsh -NoProfile -File harness/build_pgo.tests.ps1` | Passed |
| `pwsh -NoProfile -File harness/build_portable.tests.ps1` | Passed |
| `git diff --check` | Passed |

The branch has not been pushed. The Windows and Ubuntu CI jobs have therefore not run,
and their plan checkboxes remain open.

## Aggregate profile

The six searches produced exactly `37,420` nodes:

| Metric | Aggregate |
|---|---:|
| Interior nodes | 29,594 |
| Qsearch nodes | 19,155 |
| Checked interior nodes | 1,553 |
| Checked interior fraction | 5.248% of interior nodes |
| Interior static evaluations | 7,041 |
| Qsearch static evaluations | 14,714 |
| Total NNUE forward evaluations | 21,755 |
| TT cutoffs | 5,703 |
| SEE calls | 51,432 |
| SEE cycles | 13,406,143 |
| NNUE forward cycles | 11,919,312 |
| LMR reductions | 5,785 |
| Reduced-search fail-highs | 19 |
| Full-depth re-searches | 17 |

Static-evaluation densities were:

- interior static evaluations: `237.920` per 1,000 interior nodes and `188.161`
  per 1,000 reported total nodes;
- qsearch static evaluations: `768.155` per 1,000 qsearch nodes and `393.212`
  per 1,000 reported total nodes;
- all NNUE forward evaluations: `581.374` per 1,000 reported total nodes.

The event counters have deliberately different boundaries, so `interior_nodes +
qsearch_nodes` is not used as a substitute for the profile's reported `nodes` value.

## Per-position profile

`Int eval/int` and `Q eval/q` are evaluations per 1,000 corresponding nodes.
`Int eval/total` and `Q eval/total` use each row's reported `nodes`.

| Position | Nodes | Interior | Qsearch | Checked interior | Int eval/int | Int eval/total | Q eval/q | Q eval/total | SEE calls | SEE cycles | NNUE forwards | Forward cycles |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| bench1 | 6,085 | 5,759 | 3,170 | 74 (1.285%) | 148.984 | 141.002 | 814.511 | 424.322 | 3,791 | 739,399 | 3,440 | 1,664,717 |
| bench2 | 13,484 | 8,783 | 7,562 | 480 (5.465%) | 322.441 | 210.027 | 772.547 | 433.254 | 25,340 | 7,387,884 | 8,674 | 4,916,304 |
| bench3 | 3,125 | 2,940 | 1,379 | 223 (7.585%) | 143.878 | 135.360 | 622.190 | 274.560 | 2,443 | 316,065 | 1,281 | 751,209 |
| bench4 | 6,006 | 4,542 | 3,001 | 187 (4.117%) | 280.273 | 211.955 | 761.080 | 380.286 | 8,881 | 2,047,028 | 3,557 | 2,084,758 |
| bench5 | 2,972 | 2,775 | 1,295 | 263 (9.477%) | 151.351 | 141.319 | 815.444 | 355.316 | 2,200 | 477,766 | 1,476 | 827,988 |
| bench6 | 5,748 | 4,795 | 2,748 | 326 (6.799%) | 257.560 | 214.857 | 761.281 | 363.953 | 8,777 | 2,438,001 | 3,327 | 1,674,336 |

Additional search-event rows:

| Position | TT cutoffs | LMR reductions | Reduced fail-highs | Full-depth re-searches |
|---|---:|---:|---:|---:|
| bench1 | 1,475 | 937 | 10 | 7 |
| bench2 | 1,514 | 1,793 | 0 | 0 |
| bench3 | 835 | 480 | 1 | 0 |
| bench4 | 786 | 1,141 | 0 | 0 |
| bench5 | 316 | 450 | 4 | 5 |
| bench6 | 777 | 984 | 4 | 5 |

## Pruning attempts and cutoffs

Rates are `cutoffs / attempts`. A zero attempt denominator is reported as `N/A`, not
as zero percent.

| Counter | Aggregate cutoffs / attempts | Aggregate rate |
|---|---:|---:|
| Razoring | 215 / 12,938 | 1.662% |
| Reverse futility pruning | 8,295 / 13,945 | 59.484% |
| Null-move pruning | 652 / 1,977 | 32.979% |
| ProbCut | 320 / 1,329 | 24.078% |
| Late-move pruning | 19,690 / 53,577 | 36.751% |
| Futility pruning | 23,178 / 31,886 | 72.690% |
| History pruning | 0 / 0 | N/A |
| SEE pruning | 4,191 / 14,020 | 29.893% |

Per-position cutoff rates:

| Position | Razor | RFP | NMP | ProbCut | LMP | Futility | History | SEE pruning |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| bench1 | 5/1,709 (0.293%) | 289/1,741 (16.600%) | 8/140 (5.714%) | 0/132 (0.000%) | 4,340/8,313 (52.207%) | 371/2,782 (13.336%) | 0/0 (N/A) | 461/2,465 (18.702%) |
| bench2 | 41/4,621 (0.887%) | 3,360/5,017 (66.972%) | 189/846 (22.340%) | 200/657 (30.441%) | 8,609/21,746 (39.589%) | 10,690/13,105 (81.572%) | 0/0 (N/A) | 2,137/5,839 (36.599%) |
| bench3 | 34/964 (3.527%) | 442/1,020 (43.333%) | 78/152 (51.316%) | 1/78 (1.282%) | 675/3,079 (21.923%) | 825/2,014 (40.963%) | 0/0 (N/A) | 217/1,259 (17.236%) |
| bench4 | 43/2,355 (1.826%) | 1,875/2,536 (73.935%) | 89/301 (29.568%) | 36/212 (16.981%) | 2,295/6,976 (32.899%) | 3,402/4,609 (73.812%) | 0/0 (N/A) | 637/2,162 (29.463%) |
| bench5 | 19/1,117 (1.701%) | 927/1,249 (74.219%) | 11/73 (15.068%) | 16/62 (25.806%) | 2,059/4,372 (47.095%) | 1,542/2,135 (72.225%) | 0/0 (N/A) | 130/700 (18.571%) |
| bench6 | 73/2,172 (3.361%) | 1,402/2,382 (58.858%) | 277/465 (59.570%) | 67/188 (35.638%) | 1,712/9,091 (18.832%) | 6,348/7,241 (87.667%) | 0/0 (N/A) | 609/1,595 (38.182%) |

These rates describe the existing event boundaries and are not direct estimates of Elo
or net search benefit.

## Cycle-counter interpretation

The current run recorded `13,406,143` SEE cycles across `51,432` calls and
`11,919,312` NNUE forward cycles across `21,755` forward evaluations. These values are
useful for identifying work within this one run. They are not portable timing claims:
cycle values and NPS vary with CPU, clock behavior, scheduling, compiler, and build
environment, and should not be compared as absolute performance numbers across CPUs.

## Experiment decision

### Checked-node evaluation

Checked interior nodes were a nontrivial `5.248%` of interior nodes. The current
counters do not distinguish checked nodes that performed a real NNUE forward from
checked nodes that reused a TT static evaluation. Therefore `1,553` is only an upper
bound on avoidable checked-node forwards. That upper bound is `22.056%` of the 7,041
interior forwards and `7.139%` of all 21,755 forwards in this profile.

Decision: the measured ceiling is large enough to justify requesting approval for a
separate, default-off, same-binary toggle experiment. It does not justify changing the
default or claiming a speed or strength win. Any experiment still needs its own approved
design and plan, deterministic tests, bench/NPS evidence, and fixed-time paired matches.
No strength change or experiment plan is included here.

### Threshold SEE

The profile establishes substantial existing SEE activity: `51,432` calls and
`13,406,143` run-local cycles. It does not separate threshold-capable call sites, record
the threshold distribution, or estimate how much exchange work an early exit would
avoid. The previous king-only legality shortcut also failed exact oracle equivalence.

Decision: a threshold-SEE implementation is not yet justified by this profile alone.
Further work requires explicit approval and a separate plan that first proves
`see_ge(position, move, threshold)` exactly equivalent to
`static_exchange_evaluation(position, move) >= threshold` across the existing exhaustive
and random oracle, without the rejected shortcut. No such plan or implementation was
created on this branch.

## Final review

The committed range `943617a045b9cdfd7c102debd59e67e579326da8...bdb42a2ac643fb330b6fb3e9e82941d41f4d0813`
was reviewed against `AGENTS.md`, the approved reliability design, and the implementation
plan.

- Standards review: no remaining high-confidence findings.
- Specification review: no remaining local implementation findings. Remote Windows and
  Ubuntu CI execution remains objectively pending because the branch is not pushed.
- No changed path is under `target/`, `nets/`, `tools/books/`, or `tools/fastchess/`.
- `cargo tree -p mf-uci -e features` reported zero `instrumentation` matches.
- The default `mf-search` feature remains empty, and both ordinary release benches retained
  the 37,420-node signature.
