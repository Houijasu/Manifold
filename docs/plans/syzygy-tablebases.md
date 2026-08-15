# Plan: Syzygy tablebase probing (WDL + DTZ)

Add Syzygy endgame tablebase support to Manifold: a new pure-Rust `mf-tb` crate (vendored
Pyrrhic-derived probe code), search-time WDL probing, root DTZ probing, a `SyzygyPath` UCI
option, and `tbhits` in info output. Local test data: `C:\Syzygy\{Syzygy345WDL,Syzygy345DTZ,
Syzygy6WDL,Syzygy6DTZ}` (145 + 145 + 365 + 365 files, complete 3-4-5 and 6-man sets).

## Constraints (from repo invariants)

- **No external runtime dependencies, no C toolchain.** Reckless's approach (C Fathom via FFI)
  is rejected. Instead, vendor a pure-Rust Pyrrhic/Fathom-derived implementation into
  `crates/mf-tb` with a provenance header on each vendored file and a notice in
  `THIRD_PARTY_NOTICES/` (existing pattern: `THIRD_PARTY_NOTICES/Eonego.txt`).
  - Primary source candidate: the `pyrrhic-rs` Rust port (engine-agnostic, pure Rust).
    Worker must verify its license is GPL-3.0-compatible (MIT/Apache/BSD) before vendoring;
    if unsuitable, port from Pyrrhic C (MIT, Ethereal project) directly.
- **Search hot paths must not allocate.** WDL probe must be allocation-free after init.
- **Chess960 first-class:** Syzygy TBs assume no castling rights; probe only when
  `castling_rights` are empty (all four `rook(color, side)` are `None`). This is correct for
  both standard and 960.
- **`panic = "abort"` in release:** probe code must not panic on corrupt/truncated files at
  init time; return errors. (Probe-time invariants may use debug asserts.)

## Architecture

```
mf-core  ←  mf-tb (new)  ←  mf-search  ←  mf-uci
```

`mf-tb` depends only on `mf-core` (Position, Bitboard, Square, Color, PieceKind, Move).
Public API (small, engine-facing — adapt vendored internals behind it):

```rust
pub enum Wdl { Loss, BlessedLoss, Draw, CursedWin, Win }
pub struct Tablebases { /* mmap-free: pread-style file handles or read-into-Vec tables */ }
impl Tablebases {
    pub fn new(paths: &str) -> Result<Self, TbError>; // ';'-separated dirs (Windows convention)
    pub fn max_pieces(&self) -> usize;                // largest men count found
    pub fn probe_wdl(&self, pos: &Position) -> Option<Wdl>;
    pub fn probe_root(&self, pos: &Position) -> Option<RootProbe>; // DTZ: best move + WDL per move
}
```

File access: memory-map is the standard approach (Fathom/Stockfish use it); on Windows use
`CreateFileMapping`/`MapViewOfFile` via minimal `windows`-free raw bindings is NOT allowed
(no external deps, and hand-rolled FFI to Win32 is its own liability). Lazy choice:
**read each table file fully into `Vec<u8>` at first use** (lazy per-file load behind
`OnceLock`), guarded by a documented memory ceiling note. 3-4-5 men ≈ 1 GB, 6-man WDL ≈ 68 GB
— full preload is impossible, so per-file lazy load stays, but a fully-loaded 6-man set will
not fit in RAM. `ponytail:` comment required: file-handle + `seek_read` (std
`std::os::windows::fs::FileExt::seek_read` / unix `read_at`) per block is the upgrade path if
lazy whole-file loads prove too memory-hungry. Worker may go straight to `seek_read` block
reads if the vendored code's access pattern makes that simpler — std-only either way.
Decision recorded per file actually loaded: WDL files are the hot path; DTZ only at root.

## Steps

### 1. `mf-tb` crate with probing core (largest step)

- Add `crates/mf-tb` to workspace members; crate doc comment; dep on `mf-core` only.
- Vendor/port Pyrrhic-derived tables + probe logic: file discovery (`.rtbw`/`.rtbz`),
  header parsing, pairs decompression, WDL probe (with en-passant and "capture resolution"
  ply logic), DTZ root probe.
- Provenance: header comment on each vendored file; add `THIRD_PARTY_NOTICES/<Upstream>.txt`.
- Adapter layer maps `mf_core::Position` bitboards (`pieces(color, kind)`, `occupancy()`,
  `en_passant()`, `side_to_move()`, `halfmove_clock()`) into the probe's expected inputs.
- **Checks (crate tests, skip-if-absent pattern like `MF_NNUE_TEST_NET`):** env var
  `MF_SYZYGY_PATH` (tests default to `C:\Syzygy\Syzygy345WDL;C:\Syzygy\Syzygy345DTZ;C:\Syzygy\Syzygy6WDL;C:\Syzygy\Syzygy6DTZ`
  only on Windows when present; otherwise skip). Known-answer tests:
  - `KQvK` white to move → `Win`; `KvK`-adjacent draws (e.g. `KBvK`) → `Draw`;
    `KPvK` fortress draw FEN → `Draw`; a cursed-win FEN → `CursedWin`.
  - En-passant-sensitive FEN (probe must consider ep captures).
  - `probe_root` on a 5-man win returns a DTZ-optimal move that preserves the win.
  - Differential sanity: for N random reachable ≤5-man positions, `probe_wdl(pos)` must be
    consistent under make/unmake of a zeroing move (WDL child bound check).

### 2. mf-search integration

- Thread an `Option<Arc<Tablebases>>` through: `SearchPool` methods →
  `WorkerParameters` (search.rs:808) → `SearchContext` (search.rs:3446). Do **not** put it in
  `SearchOptions` (it is `Copy`).
- WDL probe in `pvs` at non-root nodes, gated on: piece count ≤ `max_pieces()`,
  `halfmove_clock() == 0`, no castling rights, depth/ply guard as per Pyrrhic-standard
  conditions. Map `Wdl` into the existing score band: `Win → TABLEBASE_SCORE - ply`,
  `Loss → -(TABLEBASE_SCORE - ply)`, cursed/blessed/draw → 0 (drawish scores), matching the
  pre-existing `value_to_tt`/`value_from_tt` handling (search.rs:2416, 3423). Store as TT
  entries with appropriate bound so the existing rule50-headroom logic applies.
- Root: when the root position itself is in TB range, `probe_root` once and restrict/rank
  root moves to DTZ-preserving ones (Fathom-standard root filtering); search continues
  normally among the filtered moves.
- `tbhits` counter: new cache-line-aligned atomic paralleling `NodeCounter` (search.rs:791),
  published on the same `NODE_PUBLISH_INTERVAL` cadence; expose through `IterationInfo`.
- Bench path (`search_with_shared_history`) gets `None` — bench signature must not change
  when no TB is loaded. **Verify bench signature unchanged before/after.**

### 3. mf-uci integration

- `SyzygyPath` string option: add to `UCI_RESPONSE` (lib.rs:31) and `handle_setoption`
  (lib.rs:784); value is the multi-token join already used by `EvalFile` (path-friendly).
  Loading errors → `info string` message, engine keeps running without TB.
- Store `Option<Arc<Tablebases>>` on `EngineState` (lib.rs:129); pass through
  `start_search` (lib.rs:423) into the pool calls.
- Add `tbhits` to `write_iteration_info` (lib.rs:1566) output; update the
  `uci_protocol.rs:343` doc comment/test that documents its omission.
- **Checks:** UCI protocol tests — `setoption name SyzygyPath value C:\Syzygy\...` accepted;
  invalid path degrades gracefully; info line contains `tbhits` when TB loaded; existing
  option tests still pass.

### 4. Verification gate (per `task_completion` memory)

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # with MF_SYZYGY_PATH set so TB tests actually run
cargo run --release -p mf-uci --bin manifold -- bench   # signature must equal pre-change
```

Manual smoke: release build, `setoption name SyzygyPath value
C:\Syzygy\Syzygy345WDL;C:\Syzygy\Syzygy345DTZ;C:\Syzygy\Syzygy6WDL;C:\Syzygy\Syzygy6DTZ`,
then `position fen 8/8/8/8/8/2k5/1q6/K7 w - - 0 1` + `go depth 10` → immediate mate-band
loss score with `tbhits > 0`; a 6-man position probe to confirm large-table decode.

## Risks

- **Port correctness** is the dominant risk: pawn-file symmetry math, LSB-first bitstream
  refill, DTZ sign/±1-ply at root, ep normalization. Mitigation: vendor a proven Rust port
  rather than translating C by hand; known-answer tests against the real `C:\Syzygy` files.
- **Memory** with 6-man lazy whole-file loads (single 6-man WDL files reach ~1-2 GB;
  the full set is ~68 GB). `seek_read` block access is the documented upgrade path and may
  be chosen up front by the implementing worker.
- **SMP**: `Tablebases` must be `Sync`; lazy per-file init behind `OnceLock` gives that
  for free.
- **Elo claims deferred**: search-integration gains are measured later via
  `harness/run_match.ps1` per `measurement_harness`; not part of this plan's gate.

## Non-goals

- No `SyzygyProbeDepth` / `Syzygy50MoveRule` / `SyzygyProbeLimit` options (add when a
  measured need appears; Reckless ships only `SyzygyPath`).
- No DTM/DTW, no 7-man support, no datagen TB adjudication (follow-up if wanted).
