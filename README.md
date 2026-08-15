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

Implemented commands: `uci`, `isready`, `ucinewgame`, `setoption`, `position`,
`go depth|nodes|movetime|wtime/btime|infinite`, `stop`, and `quit`.
Unsupported commands are ignored, so pondering is effectively limited to ordinary search
for now.

Key options (the `uci` handshake lists everything, including the search-tunable spins):

| Option | Default | Notes |
| --- | --- | --- |
| `Threads` | 1 | Lazy SMP; 1-256. |
| `Hash` | 16 MiB | Range adapts to the machine's memory. |
| `UCI_Chess960` | false | X-FEN and king-takes-rook castling notation. |
| `EvalFile` | *(empty)* | Path to a `.nnue` network; overrides the embedded net. |

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
crates/mf-tune     SPSA tuner for the search parameters
crates/mf-lab      experiment scaffolding (stub)
research/          design notes; research/_src is read-only Stockfish reference
experiments/       match results and mission write-ups
```

`AGENTS.md` carries the detailed repository guidelines, including the fastchess
harness rules (affinity and concurrency settings are load-bearing on this machine's
hybrid CPU).

## License

GPL-3.0-only. See `LICENSE`.
