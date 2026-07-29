## M4 — History & Move Ordering Overhaul

Goal: replace Manifold's primitive quiet ordering (killers + PSQT delta) with a full history stack, and close the two open M3 blocking findings (singular extensions measuring negative, WAC 261/300).

### Testing policy (the change from M3)

M3 spent ~10,000 games; ~6,000 went to SPRTs that never resolved. New policy:

1. **Free screen first.** Every phase must move `manifold bench` node count and/or WAC score before any games are spent. No movement → no SPRT, feature dropped.
2. **SPRT bounds `elo0=0 elo1=20`, `alpha=beta=0.05`, hard cap 600 games.** Real gainers resolved in 104–462 games in M3. Non-resolution at the cap = rejection, not "run 2,000 more".
3. Standard harness (unchanged, per `research/infra-readiness.md`): `tc=8+0.08`, `-concurrency 8`, `-use-affinity`, `UHO_4060_v4.epd`, `option.Threads=1`, fixed `-srand` per run, `-report penta=true`.
4. Each phase records: bench signature, WAC/300, SPRT block, timeout/crash counts. Archived to `experiments/M4-*/`.

Budget: ~3,000 games total.

---

### Phase 0 — Prelude (no games)

**FEN validation** (`crates/mf-core/src/fen.rs`). Add a legality pass to `from_fen`:
- ≤16 pieces per side, ≤8 pawns per side, ≤ 8 total of any material kind (fixes the `TABLES.material[..][17]` out-of-bounds abort — currently reachable from UCI `position fen` and fatal under `panic = "abort"`)
- no pawns on rank 1 or 8
- side-not-to-move not in check

**Corpus audit + workaround deletion.** The new validation rejects `search_invariants.rs:13` (`8/8/8/8/8/2k5/1q6/K7 b - - 0 1` — black to move, white not in check... verify each corpus entry). Every rejected entry gets replaced with a legal equivalent. Then delete the mate-declining branch in `root_search` (`search.rs:186-195`) that exists solely to tolerate those illegal positions — including its per-root-move `generate_legal_moves` cost.

**TT aging.** Add `generation: u8` to `SearchContext`, increment per `go`, thread it to all 7 `age: 0` store sites (`search.rs:520, 784, 1102, 1862, 1919, 1935, 2177`). `relative_age` is currently always 0, so replacement has degenerated to depth-preferred only, and `clear()` runs only on `ucinewgame`.

**Two honesty one-liners** (strike if unwanted):
- Remove `option name Threads` from `UCI_RESPONSE` — parsed into `EngineState::threads`, never read anywhere.
- Change advertised `Hash ... max 1048576` to `max 4096` to match `MAX_EAGER_ALLOCATION_MIB`. Update `failed_hash_resize_preserves_the_existing_usable_table`, which currently asserts the mismatch as intended.

Verify: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p mf-core --features force-magic`, record new bench signature. Add a regression test for the >16-piece FEN.

---

### Phase 1 — Non-allocating staged move picker

`MovePicker::new` currently copies a 514-byte `MoveList`, builds three `Vec`s, and calls `static_exchange_evaluation` once per move to classify then again O(n log n) times inside `sort_unstable_by_key` (which does not cache keys).

- Replace the three `Vec`s with one fixed-capacity array of `(Move, i32)` scored entries, SEE evaluated exactly once per move.
- Replace full sorts with lazy partial selection sort in `next()` — most nodes never consume past the first few moves.
- Same treatment for `quiescence_moves`, which repeats the double-SEE pattern.
- Const 64-entry lookup tables for `knight_attacks` / `king_attacks` / `pawn_attacks` (`mf-core/src/attacks.rs:3-26, 81-89`), replacing the per-call 8-iteration delta loop in movegen, `is_square_attacked`, and `SeeState::attackers_to`.

**Correctness invariant: bench node count must be bit-identical to Phase 0.** Move order is unchanged, so any node-count delta is a bug. This is the whole test.

Screen: bench nps (expect a material rise from ~1.7 Mnps search). SPRT vs Phase 0 binary — speed-only changes are exactly where a single SPRT is worth it.

---

### Phase 2 — Butterfly history in ordering + capture history

- Wire the existing `quiet_history` into `quiet_score`. It is currently read only by `late_move_reduction` (`search.rs:816-822`) and never influences move order at all.
- Add capture history `[piece][to][captured_kind]`, applied to capture ordering and updated on capture cutoffs/maluses.
- Keep the existing gravity update rule (`update_quiet_history` already implements `bonus - current*|bonus|/MAX` correctly).
- New UCI toggles `UseHistoryOrdering`, `UseCaptureHistory`, following the established isolated-toggle pattern from commit a97c994 (each option gates exactly the technique it names — no shared gating).

Screen: bench nodes + WAC/300. SPRT `[0,20]`, cap 600.

---

### Phase 3 — Continuation history

- 1/2/4/6-ply continuation tables indexed by (previous piece, previous to) × (piece, to). `SearchContext::current_moves` already tracks the move stack, including the null-move hole.
- Counter-moves fall out of the 1-ply table — no separate structure.
- Feed continuation scores into both ordering and the existing `history_score` input to `late_move_reduction`.
- Toggle `UseContHistory`.

Screen: bench nodes + WAC/300. SPRT `[0,20]`, cap 600.

---

### Phase 4 — Correction history

`ZobristKeys` already exposes `pawn()`, `minor()`, `major()`, `non_pawn_material()` — these exist for corrhist and are currently unused by search.

- Correction tables keyed on each, blended into static eval before RFP/razoring/futility/NMP consume it.
- Update on nodes where the search value contradicts static eval, per the standard rule.
- Toggle `UseCorrHistory`.

Screen: bench nodes + WAC/300. SPRT `[0,20]`, cap 600.

---

### Phase 5 — Validation & M3 closure

1. **Re-run the singular ablation** (`UseSingularExt` on/off, seed distinct from 20260744/20260748). M3 measured −10.08 ± 17.57 STC and −24.36 ± 38.57 LTC and flagged it as a probable search bug elsewhere. Better ordering feeding singular verification is the leading hypothesis. If it is now positive, the finding closes; if still negative, singular ships default-off and the finding is documented as a real result rather than an open question.
2. **WAC gate** at 1,000 ms / Hash 64. M3 default is 261/300 against a 270 threshold (270 with singular off). M4 must clear 270 with defaults on.
3. **Cumulative M4 vs M3** — SPRT `[0,10]` at `tc=8+0.08`, then LTC `tc=60+0.6` with `UHO_Lichess_4852_v1.epd`.
4. Archive `baselines/M4/manifold.exe` + `build-metadata.txt`, write `experiments/M4-validation/M4-validation-results.md` in the same format as the M3 doc.

---

### Deliverables

- `docs/superpowers/specs/2026-07-29-m4-history-move-ordering-design.md` (this design, committed first)
- Per-phase commits following repo style (short imperative sentence-case subjects, bench signature deltas stated in the body)
- `experiments/M4-*/run-metadata.txt` with exact fastchess commands for every SPRT
- `baselines/M4/`

### Out of scope

NNUE (`mf-nnue`, `mf-datagen` stay stubs), Lazy SMP, `EvalFile`, streamed `info` during search, and the remaining Medium review findings (duplicate `piece_value`, per-store `evaluate()` on TT writes, flaky wall-clock tests).
