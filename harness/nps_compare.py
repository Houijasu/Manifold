"""Fair NPS comparison between two UCI engines at identical positions and depths.

A-EONEGO-004 requires that Eonego be measured *after warmup*, because a cold-start
measurement would be unfair and is explicitly disallowed. This driver therefore:

  * keeps ONE engine process alive for all measurements (process startup, and for
    Eonego the ~0.9 s embedded-net load, is paid once and is never inside a timed
    region);
  * runs `--warmup` discarded searches per position before any timed search;
  * runs `--repeat` timed searches per position and reports the MEDIAN, so a single
    scheduling hiccup cannot decide the number.

Both engines are driven with stdin held OPEN and stdout polled until `bestmove`.
Piping a script that ends in `quit` aborts the search (validation-contract.md 0.1);
Eonego in particular then returns `bestmove a2a3` with 0 nodes.

Usage:
    py -3.14 harness/nps_compare.py --engine Manifold=<exe> --engine Eonego=<exe> \
        --depth 12 --hash 64 --warmup 1 --repeat 3
"""

from __future__ import annotations

import argparse
import json
import queue
import re
import statistics
import subprocess
import threading
import time
from pathlib import Path

POSITIONS = {
    "startpos": "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "kiwipete": "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "midgame": "2rq1rk1/pb2bppp/np2pn2/3p4/3P4/1P2PN2/PB1NBPPP/R2Q1RK1 w - - 0 1",
    "endgame": "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
}


class Engine:
    def __init__(self, name: str, executable: Path, hash_mb: int):
        self.name = name
        self.process = subprocess.Popen(
            [str(executable)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.lines: queue.Queue[str] = queue.Queue()
        threading.Thread(target=self._pump, daemon=True).start()
        self._send("uci")
        self._wait("uciok", 60)
        self._send(f"setoption name Hash value {hash_mb}")
        self._send("setoption name Threads value 1")
        self._send("isready")
        self._wait("readyok", 60)

    def _pump(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self.lines.put(line.rstrip())

    def _send(self, command: str) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def _wait(self, prefix: str, timeout_s: float) -> str:
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                line = self.lines.get(timeout=0.05)
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError(f"{self.name} exited while waiting for {prefix!r}")
                continue
            self.last_infos.append(line) if hasattr(self, "last_infos") else None
            if line.startswith(prefix):
                return line
        raise TimeoutError(f"{self.name} did not emit {prefix!r} within {timeout_s}s")

    def search(self, fen: str, depth: int) -> tuple[int, float]:
        """Return (nodes, wall_seconds) for a fixed-depth search from a fresh TT."""
        self._send("ucinewgame")
        self._send("isready")
        self._wait("readyok", 60)
        self._send(f"position fen {fen}")
        self.last_infos = []
        start = time.perf_counter()
        self._send(f"go depth {depth}")
        self._wait("bestmove", 900)
        wall = time.perf_counter() - start
        nodes = 0
        for line in self.last_infos:
            match = re.search(r"\bnodes (\d+)", line)
            if match:
                nodes = max(nodes, int(match.group(1)))
        return nodes, wall

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self._send("quit")
                self.process.wait(timeout=10)
            except Exception:
                self.process.kill()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", action="append", required=True, help="NAME=PATH")
    parser.add_argument("--depth", type=int, default=12)
    parser.add_argument("--hash", type=int, default=64, dest="hash_mb")
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--json", type=Path, default=None)
    args = parser.parse_args()

    specs = []
    for spec in args.engine:
        name, path = spec.split("=", 1)
        specs.append((name, Path(path)))

    results: dict[str, dict] = {}
    for name, path in specs:
        engine = Engine(name, path, args.hash_mb)
        try:
            per_position = {}
            for label, fen in POSITIONS.items():
                for _ in range(args.warmup):
                    engine.search(fen, args.depth)  # discarded: warmup, never timed
                samples = [engine.search(fen, args.depth) for _ in range(args.repeat)]
                nodes = [n for n, _ in samples]
                nps = [n / w for n, w in samples if w > 0]
                per_position[label] = {
                    "nodes": statistics.median(nodes),
                    "nodes_all": nodes,
                    "nps_median": statistics.median(nps),
                    "nps_all": [round(x) for x in nps],
                }
                print(
                    f"{name:10s} {label:9s} d{args.depth} "
                    f"nodes={statistics.median(nodes):>12,.0f} "
                    f"nps_median={statistics.median(nps):>12,.0f} "
                    f"nps_samples={[round(x) for x in nps]}",
                    flush=True,
                )
            results[name] = per_position
        finally:
            engine.close()

    print()
    print(f"=== depth {args.depth}, Hash={args.hash_mb}, Threads=1, "
          f"warmup={args.warmup} discarded search(es)/position, {args.repeat} timed repeats, median reported ===")
    header = f"{'position':10s}" + "".join(f"{n + ' nodes':>18s}{n + ' NPS':>16s}" for n, _ in specs)
    print(header)
    for label in POSITIONS:
        row = f"{label:10s}"
        for name, _ in specs:
            row += f"{results[name][label]['nodes']:>18,.0f}{results[name][label]['nps_median']:>16,.0f}"
        print(row)

    if len(specs) == 2:
        a, b = specs[0][0], specs[1][0]
        print()
        for label in POSITIONS:
            ra, rb = results[a][label], results[b][label]
            print(f"{label:10s} NPS ratio {a}/{b} = {ra['nps_median'] / rb['nps_median']:.2f}x   "
                  f"nodes-to-depth ratio {a}/{b} = {ra['nodes'] / rb['nodes']:.2f}x")
        gm_nps = statistics.geometric_mean(
            [results[a][l]["nps_median"] / results[b][l]["nps_median"] for l in POSITIONS])
        gm_nodes = statistics.geometric_mean(
            [results[a][l]["nodes"] / results[b][l]["nodes"] for l in POSITIONS])
        print(f"\ngeometric mean NPS ratio {a}/{b}   = {gm_nps:.2f}x")
        print(f"geometric mean nodes ratio {a}/{b} = {gm_nodes:.2f}x "
              f"(<1 means {a} needs FEWER nodes for the same depth)")

    if args.json:
        args.json.write_text(json.dumps(results, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
