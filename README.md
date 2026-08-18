# Manifold

Manifold is a UCI chess engine written in Rust (2024 edition). It is a pure NNUE
engine: PEXT/magic move generation, alpha-beta search with modern pruning and a lazy-SMP
thread pool, and a halfkp/halfka-style network trained through the workspace's own
`mf-datagen` pipeline. Chess960 (Fischer random) is first-class throughout.

This is version 0.0.1, a pre-release. The engine plays legal, stable games and is
measured against Stockfish in the repo's gauntlet harness, but it is not yet
competitive with top engines. Interfaces and defaults may change without notice.

## Build

Requires a stable Rust toolchain recent enough for edition 2024.

```
cargo build --release
```

The binary is `target/release/manifold` (`manifold.exe` on Windows).

Two build details matter:

- `.cargo/config.toml` compiles with `target-cpu=native`, so the release binary is
  tuned to the machine that built it and will not run on older CPUs.
- The default `embedded-net` feature embeds the network into the binary, so a GUI can
  launch the engine from any working directory. `EvalFile` (below) overrides it at
  runtime. A build with `--no-default-features` has *no* fallback evaluation and cannot
  search without an `EvalFile`.

## Run

```
manifold           # UCI mode (this is what a GUI launches)
manifold bench     # fixed-depth benchmark; prints a deterministic node signature
manifold mtbench   # the same benchmark at 1, 2, 4, and 8 threads
manifold perft 5 [--fen <FEN>] [--chess960]
```

Implemented commands: `uci`, `isready`, `ucinewgame`, `setoption`, `position`, `go`
time/depth/nodes/mate/searchmoves/ponder/infinite/perft forms, `ponderhit`, `stop`, `d`,
`eval`, `bench`, `mtbench`, and `quit`. On otherwise recognized `go` commands,
unsupported or invalid arguments are diagnosed and ignored; wholly unrecognized argument
lists are ignored as malformed.

Key options (the `uci` handshake lists everything, including the search-tunable spins):

| Option | Default | Notes |
| --- | --- | --- |
| `Threads` | 1 | Lazy SMP; 1-256. |
| `Hash` | 16 MiB | Range adapts to the machine's memory. |
| `MultiPV` | 1 | Number of principal variations; 1-256. |
| `Ponder` | false | Search while waiting for the predicted reply. |
| `UCI_Chess960` | false | X-FEN and king-takes-rook castling notation. |
| `EvalFile` | *(empty)* | Path to a `.nnue` network; overrides the embedded net. |
| `SyzygyPath` | *(empty)* | Path list used to discover and probe Syzygy WDL/DTZ tables. |

`position fen` rejects unreachable material (pawns on the back rank, side-not-to-move
in check, illegal promotion material) rather than crashing or searching garbage.

## Development

The authoritative gates, all of which must stay green:

```
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

`bench` has a pinned node signature asserted by `bench_cli` tests. Any change that
moves the signature is a strength change -- deliberate or bug -- and must be justified
and re-pinned explicitly, never silently.

Perft anchors run shallow under debug and full depth under `cargo test --release`.
`cargo test -p mf-core --features force-magic` exercises the black-magic sliding-attack
backend instead of PEXT.

```
crates/mf-core     board, move generation, hashing, FEN/X-FEN, perft
crates/mf-nnue     network loading, accumulators (Finny), threat features
crates/mf-search   alpha-beta, move ordering, TT, thread pool, tunables
crates/mf-uci      protocol surface and the `manifold` binary
crates/mf-datagen  self-play data generation for network training
crates/mf-tb       Syzygy WDL/DTZ discovery and probing for UCI/search/datagen
crates/mf-tune     SPSA tuner with checkpoint/resume and process-driven matches
crates/mf-lab      corrhist-regression and experiment tooling
research/          design notes; research/_src is read-only Stockfish reference
experiments/       match results and mission write-ups
```

`AGENTS.md` carries the detailed repository guidelines, including the fastchess
harness rules (affinity and concurrency settings are load-bearing on this machine's
hybrid CPU).

Run matches through `harness/run_match.ps1` with an explicit
`-OutDir experiments/<run-name>` so command metadata, console output, PGN, and the
result write-up stay together. There is no live root `config.json` contract.

## License

GPL-3.0-only. See `LICENSE`.
