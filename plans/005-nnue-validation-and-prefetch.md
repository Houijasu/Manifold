# Plan 005: Validate net hashes on load and prefetch the NNUE king-move hot paths

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If
> anything in the "STOP conditions" section occurs, stop and report. When
> done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- crates/mf-nnue/src/network.rs crates/mf-nnue/src/accumulator.rs crates/mf-nnue/src/finny.rs crates/mf-nnue/src/simd.rs`
> Written against commit `b9d15bf` **plus its uncommitted working tree**. Excerpt mismatch = STOP.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW (validation is load-time only; prefetches are hardware hints; the row hoist is arithmetic-identical)
- **Depends on**: none
- **Category**: security + perf
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

## Why this matters

Four small, independent mf-nnue items found by audit:

1. **Network hash fields are parsed but never validated** (`network.rs`): `architecture_hash` and `feature_transformer_hash` are read and stored, but no expected-value comparison exists anywhere — only `version` is checked. An `EvalFile` with the right version but a different architecture whose layer byte-lengths happen to line up loads silently and evaluates garbage with no diagnostic. Stockfish validates both hashes on load.
2. **The mirror-flip threat rebuild streams cold rows with no prefetch** (`accumulator.rs`): `rebuild_threats_onto` computes all threat feature indices, then immediately applies scattered 1 KiB rows from the ~60 MB threat table — each `add_i8_row` waits on a cold miss a prefetch issued during index computation would have hidden. Both sibling paths already prefetch; this one (30.4% of Finny-served king moves, ~1122 ns each) does not.
3. **Finny refreshes apply 2 KiB HalfKA rows without prefetch** (`finny.rs`): same pattern, addresses known before the apply loops.
4. **The fused AVX2 update kernel re-resolves weight rows once per tile** (`simd.rs`): inside the `for tile in (0..L1).step_by(128)` loop, each delta feature re-executes `network.half_ka_weights().row(feature).expect(...)` — a bounds check + panic path repeated 8× per feature for all four delta lists, though the row address is loop-invariant.

## Current state

- `crates/mf-nnue/src/network.rs` lines ~132-180 — hashes read, never compared:

```rust
        let architecture_hash = cursor.read_u32().map_err(LoadError::from)?;
        ...
        let feature_transformer_hash = cursor.read_u32().map_err(LoadError::from)?;
```

  The version check to mirror (lines ~121-127):

```rust
        if version != VERSION {
            return Err(LoadError::UnexpectedVersion {
                found: version,
                expected: VERSION,
            });
        }
```

  `LoadError` lives in the same file; add a variant in its style.

- `crates/mf-nnue/src/accumulator.rs` lines ~794-811 — the existing prefetch helper (note: takes `&[u32]`):

```rust
#[inline]
fn prefetch_threat_rows(network: &Network, features: &[u32]) {
    #[cfg(target_arch = "x86_64")]
    for &feature in features {
        if let Some(row) = network.threat_weights().row(feature as usize) {
            use core::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
            unsafe { _mm_prefetch::<_MM_HINT_T0>(row.as_ptr().cast()) };
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (network, features);
}
```

- `crates/mf-nnue/src/accumulator.rs` lines ~813-846 — `rebuild_threats_onto` fills `active_threats: [usize; MAX_ACTIVE]` via `append_active_threats`, then immediately loops `add_i8_row` + `add_psqt_row` per row with no prefetch between (sibling paths at ~221-222 and ~277-278 call `prefetch_threat_rows`).
- `crates/mf-nnue/src/finny.rs` lines ~74-165 — `refresh` computes removal/addition feature indices into fixed arrays, then subtracts/adds 2 KiB `i16` rows; no `_mm_prefetch` in the file.
- `crates/mf-nnue/src/simd.rs` lines ~1029-1105 — the fused update kernel; per-tile `row(...)` resolution at ~1040/~1053 and the threat loops.
- Architecture facts (for expected-hash constants): HalfKAv2_hm input set 22,528 dims with `[i16; 1024]` rows; FullThreats 60,720 dims with `[i8; 1024]` rows; L1 = 1024; PSQT buckets = 8; screamer activation; 8 layer stacks.
- Parity/oracle nets: `crates/mf-nnue/tests/` (accumulator_stack, lazy_updates, eonego_parity, evaluation, ...) all load `nets/main.nnue` via `test_support` and skip when absent. These are the correctness net for items 2-4: accumulator values must remain **bit-identical**.
- Conventions: `#[inline]` on small hot helpers; `Aligned64` weight rows (`network.rs` ~26-56); all kernels in `simd.rs` behind `SimdBackend` dispatch.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| NNUE tests | `cargo test -p mf-nnue` | all pass (bit-identical accumulators) |
| Gates | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` | exit 0 |
| Bench (must NOT move) | `cargo run --release -p mf-uci --bin manifold -- bench` | unchanged node signature — prefetches and row hoisting change no values |
| NPS | `cargo run --release -p mf-uci --bin manifold -- mtbench --threads 1` | record before/after |

## Scope

**In scope**:
- `crates/mf-nnue/src/network.rs`
- `crates/mf-nnue/src/accumulator.rs`
- `crates/mf-nnue/src/finny.rs`
- `crates/mf-nnue/src/simd.rs`
- `crates/mf-nnue/tests/` (new load-validation tests)

**Out of scope**:
- Any change to accumulator **values**, activation math, or quantization.
- `threats.rs` (its scan is already characterized; the mirror-cache redesign is future work, not this plan).
- The `rebase_halfka` scalar loop and any AVX-512 backend — audit left both as *investigate* items; deliberately excluded.

## Git workflow

- One commit: `Validate net hashes and prefetch the king-move NNUE paths` (or two: validation / prefetch+hoist).

## Steps

### Step 1: Validate the hashes

1. Add expected constants `ARCHITECTURE_HASH` / `FEATURE_TRANSFORMER_HASH` to `network.rs`. Derive them **from the shipped net**: load `nets/main.nnue` once (a scratch test print is fine) and hard-code the observed values as the expected constants — they identify the architecture this build was compiled against.
2. Reject mismatches right after reading each field, with a `LoadError` variant mirroring `UnexpectedVersion` (e.g. `UnexpectedArchitectureHash { found, expected }`).

**Verify**: new test in `crates/mf-nnue/tests/` — take the real net's bytes, corrupt only the architecture-hash field, assert the new error; same for the transformer hash; unmodified bytes still load. `cargo test -p mf-nnue` → pass.

### Step 2: Prefetch in `rebuild_threats_onto`

Add a `usize`-accepting prefetch (either a second helper or collect into a small `u32` buffer — smaller diff wins) and call it over `active_threats[..threat_count]` between `append_active_threats` and the add loop. Prefetch the PSQT rows too if `add_psqt_row`'s source row is cheaply addressable (there is a `threat_psqt_row` accessor).

**Verify**: `cargo test -p mf-nnue` → pass (bit-identical); bench signature unchanged.

### Step 3: Prefetch in Finny `refresh`

Between index computation and the removal/addition apply loops, prefetch `network.half_ka_weights().row(idx)` (and the PSQT rows) for every computed index, reusing the `_MM_HINT_T0` pattern (add an `i16`-row variant next to `prefetch_threat_rows`).

**Verify**: `cargo test -p mf-nnue` → pass; bench signature unchanged.

### Step 4: Hoist row resolution out of the tile loop

In the fused AVX2 kernel, resolve each delta feature's row once into small stack arrays (`[&[i16; L1]; N]`-style) before the tile loop, then index them per tile. Keep the `expect` at resolution time (it still fires for the same impossible condition, now once). Only the AVX2 path needs this; leave the scalar twin untouched.

**Verify**: `cargo test -p mf-nnue` → pass; `cargo clippy --workspace --all-targets -- -D warnings` → exit 0; bench signature unchanged.

### Step 5: Measure and gate

Record 1-thread mtbench NPS before/after. Prefetch wins are expected small (1-3% each); the plan's value is that none of it can regress correctness. If NPS regresses, drop the offending prefetch (they are hints — keep the code simplest that measures ≥ baseline).

**Verify**: all gates exit 0; numbers recorded.

## Test plan

- Load-validation tests (step 1) in `crates/mf-nnue/tests/`, modeled on existing loader tests (`rg -n "from_bytes" crates/mf-nnue/tests/` for the buffer-manipulation pattern).
- Bit-identity: the entire existing mf-nnue suite (accumulator_stack, lazy_updates, eonego_parity) is the oracle for steps 2-4.
- Bench signature as the end-to-end no-change check.

## Done criteria

- [ ] All gates exit 0; `cargo test -p mf-nnue` green
- [ ] Corrupted-hash nets rejected with named errors; real net loads
- [ ] `rebuild_threats_onto` and Finny `refresh` prefetch before their apply loops
- [ ] Fused kernel resolves rows once per feature, not once per tile
- [ ] Bench signature unchanged; NPS before/after recorded

## STOP conditions

- Any accumulator/evaluation parity test fails (values must be bit-identical; a failure means a prefetch/hoist changed arithmetic — almost certainly the hoist).
- The shipped net's hash fields turn out to be zero/absent (format difference from Stockfish's) — report; validation constants then need a format-specific decision.
- Determining expected hashes requires changing the file format or adding a dependency.

## Maintenance notes

- When the network is next retrained, the expected-hash constants MUST be updated in the same change — this is now the load contract.
- The AVX-512 backend and `rebase_halfka` vectorization remain open investigate items (see plans/README.md); do not fold them in here.
