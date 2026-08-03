"""Isolate the node-count effect of individual UCI toggles on one binary.

Runs the SAME engine binary once per arm, changing only the named options, so the
toggle is the single variable. Stdin is held OPEN and stdout is polled until
`bestmove`: piping a script that ends in `quit` aborts the search
(validation-contract.md 0.1) and yields a truncated node count.

Usage:
    py -3 harness/isolate_options.py --engine target/release/manifold.exe \
        --position endgame --depth 12 \
        --arm "all-on" --arm "no-qs-tt:UseQSearchTT=false"
"""

from __future__ import annotations

import argparse
import queue
import re
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
    def __init__(self, executable: Path, hash_mb: int, options: list[str]):
        self.process = subprocess.Popen(
            [str(executable)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.lines: queue.Queue[str] = queue.Queue()
        self.last_infos: list[str] = []
        threading.Thread(target=self._pump, daemon=True).start()
        self._send("uci")
        self._wait("uciok", 60)
        self._send(f"setoption name Hash value {hash_mb}")
        self._send("setoption name Threads value 1")
        for option in options:
            name, value = option.split("=", 1)
            self._send(f"setoption name {name} value {value}")
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
                    raise RuntimeError(f"engine exited while waiting for {prefix!r}")
                continue
            self.last_infos.append(line)
            if line.startswith(prefix):
                return line
        raise TimeoutError(f"engine did not emit {prefix!r} within {timeout_s}s")

    def search(self, fen: str, depth: int) -> tuple[int, str, float]:
        self._send("ucinewgame")
        self._send("isready")
        self._wait("readyok", 60)
        self._send(f"position fen {fen}")
        self.last_infos = []
        start = time.perf_counter()
        self._send(f"go depth {depth}")
        bestmove = self._wait("bestmove", 900)
        wall = time.perf_counter() - start
        nodes = 0
        for line in self.last_infos:
            match = re.search(r"\bnodes (\d+)", line)
            if match:
                nodes = max(nodes, int(match.group(1)))
        return nodes, bestmove, wall

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self._send("quit")
                self.process.wait(timeout=10)
            except Exception:
                self.process.kill()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--position", action="append", default=None)
    parser.add_argument("--depth", type=int, default=12)
    parser.add_argument("--hash", type=int, default=64, dest="hash_mb")
    parser.add_argument(
        "--arm",
        action="append",
        required=True,
        help="LABEL or LABEL:OPT=VAL[,OPT=VAL...]",
    )
    args = parser.parse_args()

    labels = args.position or list(POSITIONS)
    baseline: dict[str, int] = {}
    for spec in args.arm:
        label, _, option_spec = spec.partition(":")
        options = [o for o in option_spec.split(",") if o]
        engine = Engine(args.engine, args.hash_mb, options)
        try:
            for position in labels:
                nodes, bestmove, wall = engine.search(POSITIONS[position], args.depth)
                baseline.setdefault(position, nodes)
                ratio = nodes / baseline[position] if baseline[position] else 0.0
                print(
                    f"{label:<18} {position:<9} nodes={nodes:>12,} "
                    f"({ratio:5.2f}x)  {bestmove}  {wall:6.2f}s"
                )
        finally:
            engine.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
