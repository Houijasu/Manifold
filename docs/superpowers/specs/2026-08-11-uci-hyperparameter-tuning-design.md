# UCI hyperparameter tuning design

## Goal

Create a comprehensive, safety-classified tuning surface for search and clock
management without turning the normal `manifold` handshake into a research console.
Expose Tier 0, Tier 1, and approved Tier 2 values now; record weaker or poorly isolated
runtime values as Tier 3 instead of pretending they are ready for SPSA. The engine
remains deterministic at shipped defaults. `mf-tune` learns its parameter contract from
the exact binary it will launch, generates legal SPSA arms, and can resume after
interruption without mixing incompatible campaigns or losing an observed batch.

## Scope

Included:

- the 39 existing `SearchParameters` spins;
- approved runtime-safe search thresholds, margins, depths, weights, and
  time-management coefficients, plus an explicit Tier 3 inventory of deferred values;
- the existing `Use*` check options as activation metadata and pinned campaign settings;
- a dedicated tuning engine binary and canonical machine-readable manifest;
- live manifest and UCI-handshake attestation;
- constraint-aware, integer-grid SPSA;
- immutable campaign fingerprints, append-only iteration journals, recoverable checkpoints, fault detection, and memory preflight;
- `mf-tune` campaign generation from live metadata.

Excluded:

- NNUE architecture, feature layout, quantization, training, or network contents;
- mate/tablebase/evaluation score bands and sentinels;
- `MAX_SEARCH_PLY`, TT depth encoding, table sizes, hash masks, cache alignment, move-buffer capacity, and other compile-time or memory-layout constants;
- `Hash`, `Threads`, `MultiPV`, `Move Overhead`, `EvalFile`, `SyzygyPath`, `Ponder`, or `UCI_Chess960` as SPSA dimensions. `Hash`, `Threads`, and `Move Overhead` are fixed campaign settings; `EvalFile` may be a hashed setting; `MultiPV` is fixed at 1; `Ponder` and `UCI_Chess960` are fixed false; `SyzygyPath` is rejected;
- automatic Elo claims. Tuning produces candidates; evidence matches remain a separate step through `harness/run_match.ps1`.

## Chosen approach

Three approaches were considered.

1. Advertise every parameter from the production engine. This is simple, but it adds dozens of research-only options to every GUI and makes accidental configuration part of the supported production interface.
2. Add a `TuneMode` check to reveal extra options. UCI option discovery occurs during `uci`; changing the option surface afterwards is ambiguous, fragile in GUIs, and difficult for fastchess to attest.
3. Build a dedicated tuning engine from the same `mf-uci` and `mf-search` implementation. It advertises the expanded surface and emits a canonical manifest. The normal binary keeps its current handshake.

Use approach 3. There is one search implementation and one declarative registry, but two presentations:

```text
mf-search registry
       |
       +--> manifold.exe              production-visible options only
       |
       +--> manifold-tune-engine.exe  production + Tier 1/2 tuning options
                                              |
                                              +--> tune-manifest
                                              +--> UCI handshake
                                                       |
                                                       v
                                                    mf-tune
```

The dedicated binary is not a fork. It is a thin launcher selecting `UciFlavor::Tuning` over the same engine loop.

## Authoritative registry

Move search-option metadata out of the handwritten UCI response and into one declarative module in `mf-search`. Keep two generated collections in that module:

```rust
pub enum OptionVisibility {
    Production,
    Tuning,
}

pub enum ParameterScale {
    Linear,
    Log,
}

pub enum ParameterTier {
    Tier0,
    Tier1,
    Tier2,
}

pub struct SearchParameterSpec {
    pub name: &'static str,
    pub default: i32,
    pub legal_min: i32,
    pub legal_max: i32,
    pub tune_min: i32,
    pub tune_max: i32,
    pub quantum: i32,
    pub scale: ParameterScale,
    pub tier: ParameterTier,
    pub group: &'static str,
    pub visibility: OptionVisibility,
    pub activation: ActivationExpr,
    pub provenance: &'static str,
    get: fn(&SearchParameters) -> i32,
    set: fn(&mut SearchParameters, i32),
}

pub struct SearchCheckSpec {
    pub name: &'static str,
    pub default: bool,
    pub group: &'static str,
    pub provenance: &'static str,
    get: fn(&SearchOptions) -> bool,
    set: fn(&mut SearchOptions, bool),
}
```

Activation metadata is a small closed expression tree, not a flat list:

```rust
pub enum ActivationExpr {
    Always,
    Check { name: &'static str, value: bool },
    Setting { name: &'static str, value: i32 },
    ClockManaged,
    All(&'static [ActivationExpr]),
    Any(&'static [ActivationExpr]),
}
```

This represents real predicates such as `UseSingularExt || UseMultiCut`,
clock-managed single-PV searches, and the LMR effective-depth consumers that remain
active when `UseLMR=false`. Do not add a general expression language.

`legal_min..=legal_max` is the engine safety contract. A legal value must not panic, divide by zero, overflow, violate an array bound, or invalidate search state. `tune_min..=tune_max` is the recommended campaign domain. It may be narrowed in a config but never widened past the legal range.

`quantum` is the integer lattice step. Most parameters use `1`; fixed-point factors use their advertised integer unit. `scale=Log` means SPSA operates in logarithmic coordinates before quantization, which is appropriate for positive divisors spanning orders of magnitude.

The registry also emits a closed constraint list:

```rust
pub enum SearchConstraintSpec {
    LessEqual { left: &'static str, right: &'static str },
    StrictLess { left: &'static str, right: &'static str },
    Positive { name: &'static str },
}
```

Do not add an expression language. The current constraints fit these three forms. Add another enum variant only when a concrete parameter family needs it.

The registry generates defaults, getters, clamping setters, production/tuning iteration, manifest records, and UCI option lines. Handwritten `Use*` lines disappear from `UCI_RESPONSE`. Resource and protocol options remain handwritten because they are not tuning dimensions.

### Check options recorded in the registry

The check registry contains the existing search feature gates:

`UseNMP`, `UseRFP`, `UseRazoring`, `UseLMR`, `UseLMP`, `UseFutility`, `UseSEEPruning`, `UseQSearchTT`, `UseQSearchDeltaPruning`, `UseQSearchChecks`, `UseCaptureLMR`, `UsePostLMRDepth`, `UsePostLMRContHist`, `UseSingularExt`, `UseCheckExt`, `UseMultiCut`, `UseIIR`, `UseProbCut`, `UseButterflyHistory`, `UseCaptureHistory`, `UsePawnHistory`, `UseContHistory`, `UseTtMoveHistory`, `UseLowPlyHistory`, `UseHistoryPruning`, `UseCorrHistory`, `UseCorrHistPawn`, `UseCorrHistMinor`, `UseCorrHistMajor`, `UseCorrHistMaterial`, `UseCorrHistCont`, `UseCorrplexity`, `UseTimeEffort`, `UseInterpolatedTimeManagement`, and `UseSearchAgainDepth`.

A tuning config pins checks; SPSA does not perturb booleans. Separate A/B campaigns remain the right tool for deciding whether a technique should exist.

## Parameter tiers

### Tier 0: shipped spins

Tier 0 is the existing 39-parameter production surface. Legal ranges stay unchanged. Recommended ranges are deliberately narrower than the crash-safe ranges so a starter campaign does not compare crippled engines.

| Parameter | Default | Legal | Recommended | Scale | Group | Activation |
|---|---:|---:|---:|---|---|---|
| RfpMarginPerDepth | 95 | 20..300 | 60..160 | linear | rfp | UseRFP |
| RfpTtPvMargin | 22 | 0..150 | 0..80 | linear | rfp | UseRFP |
| RfpCorrplexityDivisor | 198435 | 4096..1000000 | 65536..524288 | log | corrplexity | UseRFP, UseCorrHistory, UseCorrplexity |
| RazorBaseMargin | 224 | 50..600 | 120..360 | linear | razoring | UseRazoring |
| RazorMarginPerDepth | 202 | 50..600 | 100..350 | linear | razoring | UseRazoring |
| FutilityBaseMargin | 125 | 20..400 | 60..220 | linear | futility | UseFutility |
| FutilityMarginPerDepth | 106 | 20..400 | 50..200 | linear | futility | UseFutility |
| LmpBase | 9 | 1..40 | 4..20 | linear | lmp | UseLMP |
| LmrCoefficient | 2754 | 1000..6000 | 2000..3600 | linear | lmr | UseLMR or any effective-depth consumer |
| LmrBase | 996 | -1024..3072 | 256..1792 | linear | lmr | UseLMR or any effective-depth consumer |
| LmrNonImprovingNumerator | 197 | 0..1024 | 0..512 | linear | lmr | UseLMR or any effective-depth consumer |
| LmrCutNodeBonus | 1024 | 0..3072 | 256..2048 | linear | lmr | UseLMR or any effective-depth consumer |
| LmrTtPvReduction | 1028 | 0..3072 | 256..2048 | linear | lmr | UseLMR or any effective-depth consumer |
| LmrHistoryNumerator | 459 | 50..1500 | 200..900 | linear | lmr | any effective-depth consumer and the relevant history readers |
| LmrCorrplexityDivisor | 26310 | 4096..1000000 | 8192..131072 | log | corrplexity | any effective-depth consumer, UseCorrHistory, UseCorrplexity |
| CaptureStatMaterialWeight | 873 | 0..3000 | 256..1536 | linear | capture-lmr | UseLMR, UseCaptureLMR |
| NmpMarginPerDepth | 13 | 0..60 | 0..30 | linear | nmp | UseNMP |
| NmpMarginBase | 100 | 0..400 | 40..220 | linear | nmp | UseNMP |
| NmpReductionBase | 5 | 1..10 | 3..7 | linear | nmp | UseNMP |
| NmpReductionDepthDivisor | 3 | 1..10 | 2..6 | linear | nmp | UseNMP |
| NmpEvalReductionDivisor | 200 | 50..800 | 100..400 | log | nmp | UseNMP |
| NmpEvalReductionMax | 3 | 0..8 | 1..5 | linear | nmp | UseNMP |
| QuietSeeMarginPerDepth | 26 | 1..150 | 10..70 | linear | see-pruning | UseSEEPruning |
| CaptureSeeMarginPerDepth | 99 | 1..400 | 40..180 | linear | see-pruning | UseSEEPruning |
| CaptureSeeHistoryNumerator | 34 | 0..256 | 0..96 | linear | see-pruning | UseSEEPruning, UseCaptureHistory |
| AspirationInitialDelta | 8 | 1..60 | 4..24 | linear | aspiration | depth at least 5 |
| AspirationScoreDivisor | 16053 | 1000..60000 | 8000..32000 | log | aspiration | depth at least 5 |
| AspirationMaxDelta | 512 | 16..2048 | 128..1024 | log | aspiration | depth at least 5 |
| SingularBetaBase | 59 | 10..150 | 30..100 | linear | singular | UseSingularExt or UseMultiCut |
| SingularBetaTtPvBonus | 66 | 0..200 | 20..120 | linear | singular | UseSingularExt or UseMultiCut |
| SingularDoubleMargin | 16 | 0..100 | 0..48 | linear | singular | UseSingularExt |
| SingularDoubleMarginPvBonus | 16 | 0..100 | 0..48 | linear | singular | UseSingularExt |
| SingularDoubleMarginQuietBonus | 8 | 0..100 | 0..32 | linear | singular | UseSingularExt |
| SingularCorrplexityDivisor | 198368 | 4096..1000000 | 65536..524288 | log | corrplexity | UseSingularExt, UseCorrHistory, UseCorrplexity |
| PostLmrDeeperMargin | 53 | 0..300 | 0..120 | linear | post-lmr | UseLMR, UsePostLMRDepth |
| PostLmrShallowerMargin | 8 | 0..150 | 0..60 | linear | post-lmr | UseLMR, UsePostLMRDepth |
| PostLmrContinuationBonus | 1334 | 0..4096 | 256..2304 | linear | post-lmr | UseLMR, UsePostLMRContHist and a continuation-history reader |
| ProbCutBaseMargin | 241 | 50..600 | 120..360 | linear | probcut | UseProbCut |
| ProbCutImprovingMargin | 64 | 0..300 | 0..140 | linear | probcut | UseProbCut |

The `SEARCH_PARAMETERS.len() <= 40` test is replaced with metadata completeness and uniqueness tests. Parameter count is no longer a policy.

### Tier 1: direct hidden dimensions

Tier 1 contains isolated runtime-safe numbers with a clear behavioral meaning. These are tuning-visible only.

| Parameter | Default | Legal | Recommended | Group | Activation |
|---|---:|---:|---:|---|---|
| NmpMinDepth | 3 | 1..12 | 2..6 | nmp-depth | UseNMP |
| NmpVerificationDepth | 6 | 2..16 | 4..10 | nmp-depth | UseNMP |
| RfpMaxDepth | 6 | 1..16 | 4..10 | rfp-depth | UseRFP |
| RazorMaxDepth | 3 | 1..8 | 2..5 | razor-depth | UseRazoring |
| LmpMaxDepth | 8 | 1..16 | 5..12 | lmp-depth | UseLMP |
| FutilityMaxEffectiveDepth | 6 | 1..16 | 4..10 | futility-depth | UseFutility |
| QuietSeeMaxEffectiveDepth | 7 | 1..16 | 4..10 | see-depth | UseSEEPruning |
| CaptureSeeMaxDepth | 6 | 1..16 | 4..10 | see-depth | UseSEEPruning |
| SingularMinDepth | 6 | 2..16 | 4..10 | singular-depth | UseSingularExt or UseMultiCut |
| IirMinDepth | 4 | 1..12 | 2..8 | iir | UseIIR |
| ProbCutMinDepth | 3 | 1..12 | 2..7 | probcut-depth | UseProbCut |
| HistoryPruningMaxDepth | 3 | 1..12 | 2..6 | history-pruning | UseHistoryPruning |
| HistoryPruningSlope | -1000 | -4000..-100 | -2000..-400 | history-pruning | UseHistoryPruning |
| QSearchSeeThreshold | 0 | -300..300 | -80..80 | qsearch | UseSEEPruning |
| QSearchDeltaMargin | 196 | 0..800 | 80..360 | qsearch | UseQSearchDeltaPruning |
| QSearchCheckSeeThreshold | 0 | -300..300 | -80..80 | qsearch | UseQSearchChecks |
| ProbCutNonImprovingDepthReduction | 3 | 0..10 | 1..6 | probcut-depth | UseProbCut |
| ProbCutImprovingDepthReduction | 5 | 0..12 | 2..8 | probcut-depth | UseProbCut |
| AspirationActivationDepth | 5 | 2..12 | 4..8 | aspiration | always |
| AspirationMaxDepthReduction | 3 | 0..8 | 1..5 | aspiration | always |
| AspirationWideningDivisor | 3 | 1..8 | 2..5 | aspiration | always |
| TimeStabilityCap | 6 | 1..16 | 3..10 | legacy-time | normal clock search with UseInterpolatedTimeManagement=false |
| TimeStabilityBasePercent | 110 | 50..200 | 80..140 | legacy-time | normal clock search with UseInterpolatedTimeManagement=false |
| TimeStabilityStepPercent | 5 | 0..30 | 2..12 | legacy-time | normal clock search with UseInterpolatedTimeManagement=false |
| TimeFallingScoreStep | 50 | 1..300 | 20..100 | legacy-time | normal clock search with UseInterpolatedTimeManagement=false |
| TimeFallingStepPercent | 20 | 0..100 | 5..40 | legacy-time | normal clock search with UseInterpolatedTimeManagement=false |
| TimeScaleMaxPercent | 180 | 100..400 | 130..240 | time | normal clock search |
| TimeEffortLowPermille | 500 | 0..999 | 300..700 | effort-time | UseTimeEffort and UseInterpolatedTimeManagement=false |
| TimeEffortHighPermille | 900 | 1..1000 | 700..980 | effort-time | UseTimeEffort and UseInterpolatedTimeManagement=false |
| TimeEffortLowPercent | 110 | 25..250 | 80..140 | effort-time | UseTimeEffort and UseInterpolatedTimeManagement=false |
| TimeEffortHighPercent | 90 | 25..250 | 60..120 | effort-time | UseTimeEffort and UseInterpolatedTimeManagement=false |
| DefaultMovesToGo | 30 | 1..100 | 20..50 | clock-allocation | clock search |
| MaxMovesToGo | 50 | 1..200 | 30..80 | clock-allocation | clock search |
| IncrementFractionPercent | 75 | 0..200 | 50..110 | clock-allocation | clock search |
| ClockSafetyPercent | 2 | 0..20 | 1..6 | clock-allocation | clock search |
| HardLimitClockPercent | 40 | 1..90 | 25..60 | clock-allocation | clock search |
| HardLimitSoftMultiple | 4 | 1..12 | 2..7 | clock-allocation | clock search |
| SearchAgainReductionPermille | 750 | 0..2000 | 400..1200 | search-again | UseSearchAgainDepth |
| SearchAgainGrowthCutoffPermille | 500 | 1..1000 | 300..750 | search-again | UseSearchAgainDepth |

The two search-again values replace the literal `3/4` and `1/2` with one fixed-point integer each. This avoids tuning coupled numerators and denominators separately.

Clock-allocation parameters live in `mf-search` metadata even though `mf-uci` consumes them. The registry is the contract; the consuming module is not the source of truth.

### Tier 2: coupled families

Tier 2 values are runtime-safe but should be tuned only in named groups. `mf-tune init --tier 2` must still require `--group`; it never emits every Tier 2 coordinate into one campaign.

| Group | Parameters and defaults | Legal domain | Recommended domain |
|---|---|---|---|
| interpolated-time-falling | intercept 1148, previous-average weight 230, older-score weight 110, divisor 10000, clamp 576..1728 permille | intercept/weights 0..5000, divisor 1000..50000, clamps 100..5000 | intercept 500..2000, weights 0..500, divisor 5000..20000, clamp low 300..1000, high 1000..3000 |
| interpolated-time-stability | depth anchors 496 and 1879 centiplies, factors 639 and 1712 permille, output clamp 629..1544 permille | anchors 0..6400, factors/clamps 100..5000 | anchors 200..2600, factors/clamps 300..2500 |
| interpolated-time-instability | base 1077 and slope 2229 permille | base 100..3000, slope 0..8000 | base 500..1600, slope 500..4000 |
| interpolated-time-effort | node-effort anchors 75800 and 104510, factors 969 and 714 permille, clamp 693..838 permille | anchors 0..1000000, factors/clamps 100..3000 | anchors 40000..140000, factors/clamps 400..1300 |
| tt-move-history | hit 918, miss -747, multicut base -421, per-depth 110, margin numerator 1175, divisor 114178 | bonuses/maluses -8192..8192, per-depth 0..1024, numerator 0..8192, divisor 4096..1000000 | bonuses/maluses -2048..2048, per-depth 0..512, numerator 256..4096, divisor 32768..262144 |
| continuation-history | update weights 1040,780,502,418 | each 0..4096 | each 128..2048, constrained non-increasing |
| ordering-history | butterfly multiplier 2, pawn weight 2, low-ply weight 8, continuation ordering weights 1/1/1/1 | butterfly/pawn 0..16, low-ply 0..64, continuation 0..8 | butterfly/pawn 0..8, low-ply 0..24, continuation 0..4 |
| lmr-stat-history | butterfly weight 2048, continuation weights 1126/1093 | each 0..4096 | butterfly 1024..3072, continuation 256..2048 |
| quiet-ordering-scores | primary killer 20000, secondary killer 19000, castling 1000 | each 0..100000 | primary 10000..40000, secondary 9000..35000, castling 100..5000; preserve primary > secondary > castling |
| capture-ordering | SEE 32, victim 16, attacker -1, promotion/history 1 | SEE/victim 0..256, attacker -64..64, promotion/history 0..32 | SEE 8..64, victim 4..32, attacker -8..0, promotion/history 0..4 |
| correction-blend | source weights 15341/10569/12906/12906, continuation 8761 | each 0..65536 | each 0..32768; keep correction scale fixed |
| correction-update | source weights 128/150/186/186, continuation 130/70, fail-high 12, fail-low 18, depth divisor 128, final gain 1061/1024 | source/update 0..1024, fail scale 1..128, depth divisor 1..1024, final-gain numerator 0..4096 | source/update 32..256, fail scale 4..32, depth divisor 64..256, final-gain numerator 512..2048 over fixed denominator 1024 |
| singular-outcomes | TT tolerance 3, verification divisor 2, single +1, double +2, multicut -3, cut-node -2 | tolerance 0..16, divisor 1..8, extensions 0..4, reductions -16..0 | tolerance 1..6, divisor 1..4, extensions 0..3, reductions -5..0 |

Interpolated-time coefficients are represented as fixed-point integers in the registry. Search converts them to `f64` only at the existing calculation seam. The manifest states each unit, for example `permille`, `centiply`, or `hundredth`.

Tier 2 campaign generation validates all group-specific ordering constraints. Individual values may be named explicitly for diagnostics, but the CLI warns and requires `--allow-partial-group` before generating a partial group.

### Tier 3: research-only, not initially exposed

Keep these documented in the registry design notes but out of the first tuning manifest:

- aspiration worker-jitter bucket count;
- RFP cutoff return blend;
- Syzygy TT depth bonus;
- default API maximum depth;
- time-check and node-publication cadence;
- draw jitter;
- shuffle-suppression thresholds;
- score-history window length and smoothing topology;
- history saturation bounds;
- quiet-history quadratic bonus/malus coefficients and clamps;
- pawn-history update bonus/malus scales;
- low-ply history prior and update coefficient;
- the LMP improving divisor;
- the RFP improving effective-depth refund;
- the null-move verification-span fraction;
- correction scale;
- compile-time generated piece-square ordering coefficients.

These are operational constants, weakly isolated signals, or values whose change affects cost and semantics more than strength. Promote one only with a dedicated experiment and a legal runtime representation.

### Forbidden parameters

Never add these to the tuning registry:

- `MAX_SEARCH_PLY`, `MAX_MOVES`, LMR table size, PV capacity, history bucket counts/masks, low-ply slot count, continuation table shape, and alignment values;
- `MATE_SCORE`, `EVALUATION_LIMIT`, `INFINITY`, `TABLEBASE_SCORE`, `TABLEBASE_WIN_IN_MAX_PLY`, `UNEVALUATED_STATIC_EVAL`;
- qsearch TT domains, `TT_DEPTH_OFFSET`, TT key/value encoding, rule-50 cutoff-safety constants;
- bench and mtbench constants;
- NNUE dimensions, accumulator layout, feature indices, quantization, or training settings;
- resource and protocol options listed in the scope exclusions.

## Constraints and activation

The initial manifest encodes these constraints:

- all divisors are positive;
- `AspirationInitialDelta <= AspirationMaxDelta`;
- `PostLmrShallowerMargin <= PostLmrDeeperMargin`;
- `NmpMinDepth < NmpVerificationDepth`;
- `TimeEffortLowPermille < TimeEffortHighPermille`;
- `TimeEffortLowPercent >= TimeEffortHighPercent`;
- `DefaultMovesToGo <= MaxMovesToGo`;
- every interpolation low anchor is strictly below its high anchor;
- every clamp low is at most its clamp high;
- continuation-history weights are non-increasing by lookback distance;
- quiet-ordering score constants preserve primary killer > secondary killer > castling.

Activation predicates are checked before a campaign starts. A dimension that is inert under the pinned checks is an error, not a warning. Examples:

- `CaptureStatMaterialWeight` requires `UseLMR=true` and `UseCaptureLMR=true`;
- correction complexity divisors require correction history plus their consumer gates;
- interpolated time parameters require `UseInterpolatedTimeManagement=true`, normal clock management, and `MultiPV=1`;
- search-again parameters require `UseSearchAgainDepth=true`, normal clock management, and `MultiPV=1`.

The current LMR table also feeds effective-depth consumers when `UseLMR=false`. The manifest records that nuance; the tuner must not claim the LMR family is wholly inert when futility, SEE, or history pruning remains enabled.

For activation purposes, an LMR effective-depth consumer is:

```text
UseLMR || UseFutility || UseSEEPruning || UseHistoryPruning
```

Individual LMR terms then add their own history/correction gates where applicable.
Legacy-time and `UseTimeEffort` parameters additionally require
`UseInterpolatedTimeManagement=false`.

## Tuning engine and manifest

Add `manifold-tune-engine` as a second binary in `mf-uci`. With no arguments it speaks UCI and advertises Tier 0, Tier 1, and Tier 2 spins. `manifold` continues to advertise Tier 0 only.

`manifold-tune-engine tune-manifest` writes a canonical UTF-8 text format:

```text
manifold-tuning-manifest 1
registry-revision 1
parameter name=LmrCoefficient default=2754 legal=1000..6000 tune=2000..3600 quantum=1 scale=linear tier=0 group=lmr visibility=production unit=1 activation=any(check(UseLMR,true);check(UseFutility,true);check(UseSEEPruning,true);check(UseHistoryPruning,true))
check name=UseLMR default=true group=lmr
constraint le left=AspirationInitialDelta right=AspirationMaxDelta
end
```

Records are sorted in registry order. Values contain no whitespace; provenance is
omitted from the compact stream and identified by the emitted stable registry revision.
The exact bytes are deterministic for a given binary.

The activation field uses this versioned grammar:

```text
expr := always
      | clock
      | check(NAME,BOOL)
      | setting(NAME,INT)
      | all(EXPR;EXPR;...)
      | any(EXPR;EXPR;...)
```

For example, the LMR coefficient record emits:

```text
activation=any(check(UseLMR,true);check(UseFutility,true);check(UseSEEPruning,true);check(UseHistoryPruning,true))
```

Names may contain ASCII letters and digits only. Empty `all`/`any`, unknown operators,
bad arity, malformed nesting, and trailing text are invalid.

`mf-tune` must query both surfaces from the exact engine path:

1. run `<engine> tune-manifest` with a timeout;
2. run the engine in UCI mode, send `uci`, `isready`, `quit`, and capture the handshake;
3. require one `uciok`, one `readyok`, unique option names, and exact agreement between manifest defaults/legal bounds/check defaults and advertised tuning UCI options.

Any timeout, malformed record, duplicate, missing option, changed bound, early exit, or unexpected nonzero status stops before the campaign directory is mutated.

## `mf-tune` configuration

The config describes campaign intent, not engine facts:

```toml
engine = "target/release/manifold-tune-engine.exe"
fastchess = "tools/fastchess/fastchess.exe"
book = "tools/books/UHO_4060_v4.epd"
iterations = 1000
games_per_iteration = 8
time_control = "5+0.05"
hash = 16
threads = 1
move_overhead = 10
multi_pv = 1
ponder = false
uci_chess960 = false
# eval_file = "nets/candidate.nnue"
seed = 20260807

[[option]]
name = "UseLMR"
value = true

[[param]]
name = "LmrCoefficient"
value = 2754
min = 2200
max = 3300
c_end = 55
r_end = 0.002
```

Defaults, legal ranges, recommended ranges, quantum, scale, group, activation, and constraints come only from the live manifest. A config may narrow `min/max` and override gains. It may not widen the legal range, violate a constraint, select an inert parameter, or use a value off the declared lattice.

`mf-tune init` requires `--engine`. It supports:

- `--params Name,Name` for an explicit set;
- `--group lmr` for a complete group;
- `--tier 0` for all Tier 0 parameters;
- `--tier 1 --group legacy-time` for a Tier 1 group;
- `--checks shipped` to pin shipped check defaults, with repeated `--set-check Name=true|false` overrides.

The generated config uses recommended ranges. Default `c_end` is 5% of the selected
coordinate span, rounded up to one quantum. The sample above therefore uses
`c_end = 55`. Default `r_end` remains `0.002`. Log-scale dimensions compute the 5%
span in log space.

Unknown root keys, unknown sections, negative seeds, non-finite schedule values, and
invalid time controls are rejected. Relative paths resolve against the config file's
directory.

The non-check UCI settings above are typed root keys, not free-form option strings.
`mf-tune` applies the same resolved values to both SPSA arms and fingerprints them.
`multi_pv` must equal 1; `ponder` and `uci_chess960` must be false. If `eval_file` is
present, resolve and hash that file. There is no generic `extra_options` escape hatch.

`mf-tune init` writes engine, fastchess, and book paths either as canonical absolute
paths or as paths relative to the destination config's parent. A generated config must
remain valid when `--out` names a nested directory.

Remove `mf-search`, `mf-core`, and `mf-nnue` from `mf-tune` once live manifest loading replaces linked metadata. Keep the existing deterministic RNG source unless moving the tiny SplitMix64 implementation into `mf-tune` is smaller than retaining the `mf-datagen` dependency.

## Campaign manifest and fingerprint

Before iteration 1, write `session-manifest.txt`. It contains:

- schema, registry, SPSA-domain, and journal versions;
- the `mf-tune` executable hash, which binds update and recovery code to the session;
- canonical config and selected parameter order;
- complete resolved engine manifest and UCI handshake;
- canonical paths and SHA-256 hashes of engine, fastchess, book, and external network
  when one is enabled;
- fastchess version output;
- git commit plus dirty-state marker when the repo is available;
- schedule horizon, `alpha`, `gamma`, `A`, seed, each coordinate's start/range/gains/scale/quantum;
- pinned checks and typed non-check settings;
- games per iteration, time control, Hash, Threads;
- derived affinity and concurrency policy;
- exact fastchess command template;
- memory-preflight policy.

The campaign fingerprint is SHA-256 over the canonical semantic fields. Invocation budget is excluded, so `--iterations 20` can be a prefix of a 1000-iteration campaign. Everything that can change the games, parameter domain, or update arithmetic is included.

Reject `SyzygyPath` in tuning configs. Hashing multi-gigabyte tablebase directories on
every resume is disproportionate, and exact tablebase cutoffs introduce a second
strength surface that this search-parameter campaign is not trying to optimize.

Use one small SHA-256 dependency rather than a handwritten hash. This is provenance code, and correctness matters more than preserving a dependency-free crate. The lockfile pins the implementation.

An existing output directory is accepted only if its manifest parses and its fingerprint exactly matches the current live attestation and config. Changed binaries at the same path, changed books, changed fastchess, changed checks, reordered parameters, changed ranges/gains, changed TC, or changed schedule are hard failures.

## Constraint-aware SPSA

Separate schedule arithmetic from domain projection.

```rust
pub struct ParameterDomain {
    coordinates: Vec<CoordinateSpec>,
    constraints: Vec<Constraint>,
}

pub struct FeasiblePerturbation {
    pub flips: Vec<i8>,
    pub plus: Vec<i32>,
    pub minus: Vec<i32>,
    pub signed_deltas: Vec<f64>,
}

impl ParameterDomain {
    pub fn perturb(
        &self,
        theta: &[f64],
        gains: &[Gains],
        seed: u64,
        iteration: u64,
    ) -> Result<FeasiblePerturbation, DomainError>;

    pub fn bound_theta(&self, theta: &[f64]) -> Result<Vec<f64>, DomainError>;
}
```

For linear coordinates, the arm coordinate is the parameter value divided by its
quantum. For log coordinates, it is `ln(value)`. Live theta remains continuous and
bounded; only emitted plus/minus arms are mapped back to the nearest legal integer
lattice point.

For each iteration:

1. derive deterministic signs from `(seed, iteration)`;
2. build candidate plus/minus coordinates;
3. quantize to the integer lattice;
4. project both complete arms against bounds and cross-parameter constraints;
5. compute each coordinate's actual signed arm separation;
6. leave a coordinate at zero delta when quantization or constraints collapse only that
   coordinate; it receives no update for this iteration;
7. redraw the complete sign vector deterministically only when the two full option sets
   are identical;
8. fail with a clear error if no distinct legal pair exists after a bounded number of redraws.

The SPSA update divides by the actual signed separation, not the theoretical `c_k *
flip`. This prevents clipping and quantization from lying about the gradient. Keep
sub-quantum updates in continuous theta. After each update, clamp continuous theta to
bounds and project only the cross-parameter inequalities. Reject NaN, infinity, or any
checkpoint state outside the continuous domain.

Both arms must differ in at least one selected coordinate. A zero-delta coordinate is
skipped by the update; a batch comparing identical UCI option sets is refused before
fastchess starts.

## Durable run state

Make an append-only journal authoritative. `checkpoint.toml` and `history.csv` become derived views.

Each successful iteration appends and flushes three records; failed attempts append a
fourth event without advancing theta:

1. `Prepared`: fingerprint, sequence, iteration, prior theta, signs, actual plus/minus arms, seed, exact command, and artifact paths.
2. `Observed`: process exit, validated game counts, W/L/D, forfeits, crashes, illegal moves, warnings, and artifact hashes.
3. `Committed`: result applied exactly once, new theta, cumulative games.
4. `AttemptFailed`: iteration, attempt number, reason, process status when known, and
   preserved artifact hashes.

Each record has a schema version, monotonic sequence number, payload length, and SHA-256 checksum. A torn final record is ignored only when an earlier complete record remains authoritative; corruption before the tail is fatal.

Recovery rules:

- `Prepared` plus a complete clean PGN/log and a synced zero-exit status sidecar is
  validated and advanced to `Observed` without replay;
- `Prepared` without a trustworthy exit-status sidecar, or with
  incomplete/crashed/illegal evidence, appends `AttemptFailed`, preserves the artifacts,
  and permits a new attempt number for the same iteration;
- `Observed` without `Committed` applies the update once and appends `Committed`;
- committed journal state rebuilds missing or stale checkpoint/history files;
- a stale temporary checkpoint never outranks the journal;
- an already-complete invocation launches no arena process.

Never delete or overwrite the only artifact for an uncommitted iteration. Retries use an attempt suffix such as `iteration-000137-attempt-02.pgn`.

Checkpoint replacement must not delete the last good checkpoint before the replacement
is durable. Write and sync `checkpoint.next.toml`; while current remains valid, remove
an older `checkpoint.previous.toml`; rename current to previous; then rename next to
current. Readers fall back to previous if current is absent. The journal remains
authoritative if rotation is interrupted.

## Fastchess execution and validation

Keep the established harness policy:

- Threads=1: `-use-affinity -concurrency 8`;
- Threads>1: no affinity and `-concurrency 1`;
- paired openings through `-repeat -games 2` and an even game count;
- deterministic seed and random opening order;
- fixed-time comparisons when thread counts differ;
- fixed-node comparisons only at equal thread counts.

Every pinned `Use*` check is serialized explicitly as
`option.<Name>=true|false` for both fastchess arms. The typed non-check settings are
also identical on both arms; only SPSA spin values differ.

Before launch, compute:

```text
engine_processes = 2 * concurrency
required_hash_mib = engine_processes * Hash
allowed_hash_mib = floor(free_physical_memory_mib * 0.70)
```

Refuse when required hash exceeds the allowance. Record free memory and sampled CPU load in the session evidence.

Capture fastchess stdout/stderr to `iteration-NNNNNN-attempt-NN.console.txt` and request a warning log with `-log file=... level=warn append=false`. Parse the PGN and console together. A batch is admissible only when:

- fastchess exits zero;
- exactly the configured number of complete games exists;
- every game names one plus and one minus arm;
- every result and termination is valid;
- colour reversal and paired openings are present;
- no time forfeit, crash, illegal move played, or `No output from` event occurs.

`Illegal PV move` is counted and recorded but is not confused with an illegal move played. Unknown players, duplicate games, extra games, malformed headers, or incomplete PGNs are fatal. No failed batch updates theta.

After fastchess exits, sync the PGN, console, and warning log first. Only then write and
sync the exit-status sidecar. Recovery never trusts a success marker whose evidence was
not made durable first.

Use a process-runner seam with an explicit timeout and child cleanup. Redirect verbose
child output directly to artifact files or drain both pipes concurrently; never wait on
a child while undrained pipes can fill. Start the process inside a Windows Job Object or
POSIX process group and terminate the whole tree on timeout or Ctrl+C. Preserve
artifacts, sync the known exit status when available, and append `AttemptFailed`.

## Known behavior correction

Before exposing time parameters, pin and correct the existing `movetime` discrepancy. `SearchLimits::use_clock_management=false` promises exact movetime, but the legacy between-iteration governor currently still scales a soft limit. Both legacy and interpolated governors must require `use_clock_management`; `go movetime` remains unscaled.

## Validation strategy

### Registry and engine

- every parameter/check name is unique and case-insensitive lookup is unambiguous;
- defaults lie inside recommended and legal ranges and on the declared lattice;
- tuning ranges lie inside legal ranges;
- all activation references name a registered check;
- all constraints name registered parameters and hold at shipped defaults;
- production handshake is byte-for-byte unchanged except for intentional bug-fix output unrelated to option listing;
- normal engine rejects tuning-only setoptions;
- tuning engine advertises every Tier 0/1/2 spin exactly once;
- manifest is deterministic and agrees with the tuning UCI handshake;
- setting every production spin and check to its shipped default reproduces the 40,705-node release bench signature;
- setting every newly exposed parameter to its default also reproduces that signature in the tuning binary;
- toggle-off identity remains pinned for feature-gated families.

### Time management

- `go movetime` is not scaled by legacy, interpolated, effort, or search-again parameters;
- normal clock searches consume configured allocation values;
- interpolation anchors and clamps are deterministic at exact boundaries;
- joint `UseInterpolatedTimeManagement` plus `UseSearchAgainDepth` behavior is covered;
- `MultiPV>1` keeps interpolated/search-again paths inactive;
- ponder clock rebasing remains unchanged.

### Tuner arithmetic

- linear and log coordinates round to the declared lattice;
- arms remain inside bounds and satisfy every cross-parameter constraint;
- clipping uses actual arm separation in the update;
- collapsed arms redraw deterministically or fail before a match;
- resumed and uninterrupted synthetic objectives produce bit-identical theta and arm sequences;
- malformed or incompatible checkpoints are rejected.

### Durability and faults

Inject failures before process start, after `Prepared`, after complete PGN, after
`Observed`, during checkpoint rotation, and before history rebuild. Prove no
double-application. Clean observed batches recover exactly once; prepared batches
without trustworthy exit status are preserved and retried through `AttemptFailed`.

Test nonzero exit, timeout, missing PGN, short/extra PGN, unknown players, time forfeit, crash summary, illegal move, illegal-PV warning, `No output from`, stale partial files, and Ctrl+C process cleanup.

### Real end-to-end smoke

Build release `manifold-tune-engine`, query its manifest and handshake, generate a two-parameter config from the live binary, run two real fastchess iterations, resume for one more, and assert:

- manifest exists before games;
- recorded hashes match disk;
- three unique committed iterations and six paired arms are present;
- no artifacts were overwritten;
- checkpoint/history rebuild from journal;
- resume launches only the missing iteration;
- no orphaned engine, tuner, or fastchess processes remain.

Final candidate validation uses `harness/run_match.ps1` with the same tuning binary on both arms, tuned options on A and shipped defaults on B. After a candidate is accepted and baked into defaults, rerun the same validation with the production engine.

## Milestones

1. Registry and dual engine presentation. Preserve the production handshake and bench while adding complete metadata, fixed-point time parameters, constraints, tuning manifest, and the movetime correction.
2. Live campaign creation. Make `mf-tune init/run` query and attest the selected binary, consume recommended ranges, pin checks, write the immutable session manifest, and reject incompatible resumes.
3. Constraint-aware SPSA. Add linear/log coordinate transforms, integer quantization, legal arm projection, actual-delta updates, and deterministic collapse handling.
4. Durable execution. Add the journal, recovery, checkpoint/history rebuild, process capture, PGN/console validation, timeout/Ctrl+C cleanup, and memory preflight.
5. End-to-end hardening. Run fault injection, real release smoke, documentation, and a small no-Elo campaign proving the full path.

## Non-functional requirements

- No second search implementation and no duplicate parameter list in `mf-tune`.
- Normal production defaults, option names, and 40,705-node bench remain stable.
- No campaign starts unless the live binary, handshake, files, parameter domain, constraints, and memory policy attest successfully.
- Reproducible for fixed binaries, files, config, seed, and iteration horizon.
- Fail closed on ambiguity. A tuning run may stop and preserve evidence; it must never guess, clamp silently across the manifest contract, or learn from a suspect batch.
- Generated artifacts stay under the chosen output directory. They are not committed by default.
