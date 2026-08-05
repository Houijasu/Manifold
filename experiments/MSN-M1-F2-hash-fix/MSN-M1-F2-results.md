# M1-F2 — Large-Hash allocation fix

## Purpose

`setoption name Hash value 8096` did not work, and did not say so in a way any GUI
surfaces. The engine advertised `option name Hash type spin default 16 min 1 max 1048576`
while `TranspositionTable::new` refused anything above a hard-coded
`MAX_EAGER_ALLOCATION_MIB = 4096`. The refusal path printed one `info string` and **kept
the table it already had** — in a fresh session the 16 MB default.

That is the whole of the user's "hashfull is wrong / too much memory" report. A GUI set
the gigabytes it was offered, the engine kept 16 MB, and `hashfull` pinned near 1000
within the first second of every search because a 16 MB table genuinely was full. Nothing
about the `hashfull` computation was wrong; it was faithfully reporting a table nobody
meant to be searching with.

This experiment is the stability check on the fix. The behavioural evidence is in the
regression tests and the UCI sessions below; the match exists only to prove a multi-
gigabyte table does not destabilise play.

## The fix

Two changes, both aimed at making "advertised" and "allocatable" the same number.

1. **The maximum is derived from the machine, not hard-coded.**
   `max_hash_mebibytes()` reads installed memory (`GlobalMemoryStatusEx` on Windows),
   takes half of it, and rounds *down* to a power of two. On this 31.6 GiB machine that
   is **8192 MB**. A machine whose memory cannot be read falls back to 4096 — the size
   the engine allocated successfully for its entire history. The handshake advertises
   exactly this number, so the offered range is one the engine will honour.

2. **An oversize request clamps instead of being refused.** `resize_hash` reports
   `info string Hash N MB exceeds the maximum of M MB; using M MB` and then allocates
   `M`. The old silent-retention path is gone: every `setoption Hash` now leaves behind a
   table whose size the engine has stated.

Half of installed memory is deliberate. The table is written eagerly, so every offered
mebibyte is a resident one; the other half holds the ~106 MiB network, search stacks, the
OS, and whatever the user is running alongside the engine.

### Why the 4096 cap was not simply raised

The cap was introduced with the table itself and had no recorded justification. The
suspected reasons were checked and none survived: allocation of 8 GB takes **1.33 s**
wall (measured below), `try_reserve_exact` on a multi-GB `Vec` succeeds on Windows, and
no chunked or lazily-zeroed strategy was needed. The constant was simply a fixed number
on a variable quantity.

## Verification

### UCI sessions (`harness/hash_session_check.ps1`, release build)

`Hash 8096` — the exact value from the report:

```
option name Hash type spin default 16 min 1 max 8192
info string hash resized to 8096 MB
Allocation wall time : 1.33 s
WorkingSet64         : 8,606,416,896 bytes (8.02 GB)
```

`hashfull` over `go movetime 10000` from startpos, after `ucinewgame`:

| depth | nodes | hashfull |
|---:|---:|---:|
| 1 | 22 | 1 |
| 10 | 39,363 | 1 |
| 18 | 1,015,472 | 1 |
| 21 | 2,271,946 | 2 |
| 22 | 3,440,314 | 3 |
| 23 | 4,464,682 | 4 |
| — | — | `bestmove e2e4` |

4‰ after 4.46 M nodes into an 8 GB table. Pre-fix, the same session ran on the 16 MB
default and saturated almost immediately — this is the reported symptom, resolved.

`Hash 1048576` — the old advertised maximum, now clamped rather than refused:

```
info string Hash 1048576 MB exceeds the maximum of 8192 MB; using 8192 MB
info string hash resized to 8192 MB
WorkingSet64: 8.11 GB
```

Memory is fully released on `quit` (process exits; no leak observed across sessions).

### Determinism

`bench` twice in one process: **45,036 both times**, unchanged from the committed anchor.
A pure allocation change must not move the search signature, and it does not.
`cargo test -p mf-uci --test bench_cli` passes all 13 anchors unmodified.

### Match — stability at Hash=4096

| | |
|---|---|
| Engine A | `hash-fix` — `target/release/manifold.exe` |
| Engine B | `mission-start` — `baselines/mission-start/manifold.exe` |
| TC / Hash / Threads | 8+0.08, **4096 MB**, 2T both sides |
| Book | `UHO_4060_v4.epd`, random order, `-repeat -games 2` |
| Harness | `harness/run_match.ps1`, affinity **disabled**, concurrency **1** |
| Seed | 20260805 |
| Length | 10 rounds = 20 games |

```
Results of hash-fix vs mission-start (8+0.08, 2t, 4096MB, UHO_4060_v4.epd):
Elo: 52.51 +/- 71.59, nElo: 115.10 +/- 152.27
Games: 20, Wins: 5, Losses: 2, Draws: 13, Points: 11.5 (57.50 %)
Ptnml(0-2): [0, 1, 5, 4, 0]

  hash-fix      time forfeits: 0  Timeouts: 0  Crashed: 0  illegal moves: 0
  mission-start time forfeits: 0  Timeouts: 0  Crashed: 0  illegal moves: 0
```

**Zero forfeits and zero crashes on both sides at Hash=4096** — which is the entire
claim being made here.

#### Why this match ran at Threads=2 rather than 1T

This is a deviation from the mission default and is recorded rather than glossed.

The harness mandates `-use-affinity -concurrency 8` for 1T matches, which means **16
concurrent engine processes**. At Hash=4096 that is 16 × 4 GB ≈ **65.7 GB of eagerly
written table against 19.7 GB of free memory**. The machine would page continuously, and
the symptom of paging in a chess match is *engines losing on time* — the precise signal
AGENTS.md 4.451 designates as the one thing separating a broken harness configuration
from a real engine defect. Such a run would have been inadmissible whatever it produced.

Setting Threads=2 moves the run onto the multi-thread rules (no affinity, concurrency 1),
so exactly two engine processes exist and the memory budget is 8.4 GB — comfortably
resident. Both engines are configured identically, so the comparison stays single-
variable.

**The Elo figure here is not evidence of anything.** ±71.59 over 20 games is far wider
than any effect a pure allocation change could have, and the fix does not touch search.
Only the forfeit and crash counts are being read off this run.

## Decision

**KEEP.**

- The advertised maximum (8192 on this machine) is genuinely allocatable — pinned by a
  test that reads the number out of the handshake and hands it straight back.
- An oversize request clamps loudly and leaves the clamped table behind; the silent
  fallback to the old table is gone, and its regression test
  (`failed_hash_resize_preserves_the_existing_usable_table`, which asserted the defect as
  intended behaviour) has been replaced.
- `hashfull` behaves sanely with a multi-gigabyte table: 4‰ after 4.46 M nodes.
- Bench signature unchanged (45,036), so no anchors needed re-pinning.
- 20 games at Hash=4096 with zero forfeits and zero crashes.

## Not done

Generation-aware `hashfull` counting (Stockfish semantics) was listed as optional-if-
cheap and was **not** attempted. It is a change to what `hashfull` *means* rather than to
allocation, so it would have to carry its own before/after evidence, and the reported
symptom is already resolved without it. The current sampled count is correct for what it
measures.

## Artifacts

- `run-metadata.txt` — provenance, SHA-256 of both binaries, full fastchess command
- `console.txt`, `games.pgn`, `fastchess.log`
- `harness/hash_session_check.ps1` — the UCI session driver used above, re-runnable
