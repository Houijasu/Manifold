# Plan 004: Harden the Syzygy parser against malformed table files

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If
> anything in the "STOP conditions" section occurs, stop and report. When
> done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b9d15bf..HEAD -- crates/mf-tb/src/probe.rs`
> Written against commit `b9d15bf` **plus its uncommitted (untracked) `crates/mf-tb/`**. Excerpt mismatch = STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW (valid tables must load identically; only malformed-file handling changes)
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `b9d15bf` + working tree, 2026-08-15

## Why this matters

`SyzygyPath` points the engine at user-supplied binary files, parsed at runtime. Two robustness gaps in `crates/mf-tb/src/probe.rs`:

1. **`calc_sym_len` recurses without a depth bound.** The recursion follows symbol-DAG edges whose indices are file-controlled 12-bit values (max 4095), with only a `visited` guard against *revisits*, not *depth*. A crafted `.rtbw` with a degenerate symbol chain drives ~4096 nested frames; the UCI main thread on Windows has ~1 MiB of stack, and the workspace compiles with `panic = "abort"` — a stack overflow aborts the process, killing the engine mid-game (a forfeit in any match or GUI session).
2. **`load_table` accumulates section offsets from untrusted header sizes with a single post-hoc check.** Nine to eighteen `ptr += size[...]` operations execute with sizes derived from file headers (up to ~2^62 each; with a 4-subtable pawn table plus split encoding the sum can exceed `usize::MAX`), and only then does `if ptr > data.len() + 0x3f` run. In release builds (overflow-checks off) the sum can wrap past that check. Reads afterward go through bounds-checked helpers (`rd_u8` uses `data.get(at).copied().unwrap_or(0)`), so there is no memory-unsafety — but the engine will happily probe garbage WDL/DTZ values from a malformed file, and debug builds panic on the overflowing `+=`.

## Current state

- `crates/mf-tb/src/probe.rs` lines ~555-571 — the unbounded recursion:

```rust
fn calc_sym_len(data: &[u8], sym_pat: usize, sym_len: &mut [u8], visited: &mut [bool], s: usize) {
    if s >= sym_len.len() || visited[s] {
        return;
    }
    visited[s] = true;
    let w = sym_pat + 3 * s;
    let s2 = ((rd_u8(data, w + 2) as usize) << 4) | ((rd_u8(data, w + 1) as usize) >> 4);
    if s2 == 0x0fff {
        sym_len[s] = 0;
    } else {
        let s1 = (((rd_u8(data, w + 1) as usize) & 0xf) << 8) | rd_u8(data, w) as usize;
        calc_sym_len(data, sym_pat, sym_len, visited, s1);
        calc_sym_len(data, sym_pat, sym_len, visited, s2);
        let left = sym_len.get(s1).copied().unwrap_or(0);
        let right = sym_len.get(s2).copied().unwrap_or(0);
        sym_len[s] = left.wrapping_add(right).wrapping_add(1);
    }
}
```

  Called in a loop over up to 65535 symbols (~lines 634-638) during table load, which happens on the UCI thread when `SyzygyPath` is set.

- `crates/mf-tb/src/probe.rs` lines ~829-872 — the accumulation and its single check:

```rust
    for t in 0..num {
        ...
        ptr += size[t][0][0] as usize;
        if split {
            ...
            ptr += size[t][1][0] as usize;
        }
    }
    ... // two more such loops (size_table, data sections), plus
    ... // ptr += 1 + rd_u8(...) at ~line 815 and ptr += 2 + 2 * rd_le_u16(...) at ~line 821
    if ptr > data.len() + 0x3f {
        return None;
    }
```

- Reads are bounds-checked by design: `rd_u8(data, at)` = `data.get(at).copied().unwrap_or(0)` (~lines 280-286). Keep that property.
- Failure contract: `load_table` returning `None` makes the table unavailable and the engine plays on without tablebases (graceful). That is the correct behavior for a malformed file.
- Conventions: this crate has no external dependencies; errors are `Option`/`Result` returns, no panics on data. `cargo test -p mf-tb` runs its suite.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Crate tests | `cargo test -p mf-tb` | all pass |
| Gates | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` | exit 0, all pass |
| Bench (must NOT move) | `cargo run --release -p mf-uci --bin manifold -- bench` | unchanged signature (no SyzygyPath set) |

## Scope

**In scope**:
- `crates/mf-tb/src/probe.rs`
- `crates/mf-tb/tests/` (new malformed-file tests; create the directory/file if absent)

**Out of scope**:
- `crates/mf-tb/src/chess.rs` and `lib.rs` (audit found them clean).
- Any change to probing logic or WDL/DTZ semantics for valid files.
- File I/O strategy (eager `fs::read` is deliberate).

## Git workflow

- One commit: `Harden the Syzygy parser against malformed tables`.

## Steps

### Step 1: Bound the symbol-length recursion

Thread a `depth: u32` parameter through `calc_sym_len` (or convert to an iterative worklist — pick whichever is the smaller diff). Past a modest cap (64 is generous; valid tables have shallow symbol trees), return a sentinel that makes the caller treat the table as invalid (e.g. `sym_len[s] = u8::MAX` meaning "poisoned", and the caller checks for it after the loop, returning `None` from `load_table`). Update the call loop at ~634-638 to pass the initial depth and check the poison.

**Verify**: `cargo test -p mf-tb` → pass. New test: construct a minimal table buffer whose symbol table forms a long chain (each entry's `s1` points to the next) and assert `load` fails gracefully rather than overflowing — model the buffer construction on existing tests if present, else on the header layout in `setup_pairs`/`load_table`.

### Step 2: Check every offset step

After **every** `ptr` advance in `load_table` (the three section loops, the two map-loop advances at ~815/~821, and each `(ptr + 0x3f) & !0x3f` alignment), verify `ptr <= data.len() + 0x3f` using `checked_add` so wraparound is impossible, and `return None` on the first violation. Remove the now-redundant final check (or keep it as a belt-and-braces assert — either is fine, say which in the commit).

**Verify**: new test — a header declaring an enormous `num_blocks`/`real_num_blocks` (wraparound-sized) must yield `None`, not a wrapped pointer; `cargo test -p mf-tb` → pass.

### Step 3: Full gates

**Verify**: `cargo test --workspace` → all pass; bench signature unchanged.

## Test plan

- The two malformed-file tests from steps 1-2, in `crates/mf-tb/tests/`.
- Regression: existing mf-tb tests and any Syzygy integration tests under `crates/mf-uci/tests/` (find via `rg -n "Syzygy" crates/mf-uci/tests/`) must pass unchanged — valid-file behavior is untouched.

## Done criteria

- [ ] All gates exit 0; `cargo test -p mf-tb` green with the two new tests
- [ ] No `ptr +=` in `load_table` without an immediately-following bounds check (`rg -n "ptr \+=" crates/mf-tb/src/probe.rs` — every hit followed by a check)
- [ ] `calc_sym_len` carries an explicit depth bound
- [ ] Bench signature unchanged

## STOP conditions

- Valid-table tests break (behavior for legitimate files must be bit-identical).
- The poison-propagation for over-deep symbols cannot reach `load_table`'s `None` without touching `lib.rs`/`chess.rs` — report.

## Maintenance notes

- This crate parses untrusted input forever; any new header-driven arithmetic must use `checked_*` by convention. Reviewers should grep new diffs for bare `+=` on offsets.
- A future 7-piece or DTZ-map extension will add more offset walks — they inherit this rule.
