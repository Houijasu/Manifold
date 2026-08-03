"""Compare two engines MOVE BY MOVE on the same positions at the same depth.

Node counts say how fast a search runs, not whether it picks the right move. This
driver reports, per position: the move each engine plays, the score each reports, and
whether they agree -- so a disagreement can be inspected directly instead of inferred
from an Elo number.

Positions come from an EPD/FEN file (one per line, FEN fields only) or from the
built-in set. Both engines are driven with stdin held OPEN and stdout polled until
`bestmove`; piping a script ending in `quit` aborts the search.

Usage:
    py -3 harness/compare_moves.py --engine A=<exe> --engine B=<exe> \
        --epd tools/books/UHO_4060_v4.epd --count 200 --depth 10
"""

from __future__ import annotations

import argparse
import queue
import re
import subprocess
import threading
import time
from pathlib import Path


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
        self.last_infos: list[str] = []
        threading.Thread(target=self._pump, daemon=True).start()
        self._send("uci")
        self._wait("uciok", 120)
        self._send(f"setoption name Hash value {hash_mb}")
        self._send("setoption name Threads value 1")
        self._send("isready")
        self._wait("readyok", 120)

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
            self.last_infos.append(line)
            if line.startswith(prefix):
                return line
        raise TimeoutError(f"{self.name} did not emit {prefix!r} within {timeout_s}s")

    def analyse(self, fen: str, depth: int) -> tuple[str, int | None, str | None, int]:
        """Return (best_move, score_cp, mate_in, nodes) for a fixed-depth search."""
        self._send("ucinewgame")
        self._send("isready")
        self._wait("readyok", 120)
        self._send(f"position fen {fen}")
        self.last_infos = []
        self._send(f"go depth {depth}")
        bestmove = self._wait("bestmove", 900)
        move = bestmove.split()[1] if len(bestmove.split()) > 1 else "(none)"
        score_cp: int | None = None
        mate: str | None = None
        nodes = 0
        for line in self.last_infos:
            if not line.startswith("info "):
                continue
            cp = re.search(r"score cp (-?\d+)", line)
            mt = re.search(r"score mate (-?\d+)", line)
            nd = re.search(r"\bnodes (\d+)", line)
            if cp:
                score_cp, mate = int(cp.group(1)), None
            if mt:
                mate, score_cp = mt.group(1), None
            if nd:
                nodes = max(nodes, int(nd.group(1)))
        return move, score_cp, mate, nodes

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self._send("quit")
                self.process.wait(timeout=15)
            except Exception:
                self.process.kill()


def load_positions(epd: Path | None, count: int) -> list[str]:
    if epd is None:
        return [
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "2rq1rk1/pb2bppp/np2pn2/3p4/3P4/1P2PN2/PB1NBPPP/R2Q1RK1 w - - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        ]
    positions = []
    with epd.open(encoding="utf-8", errors="ignore") as handle:
        for line in handle:
            fields = line.split()
            if len(fields) < 4:
                continue
            # EPD carries no halfmove/fullmove counters; supply the FEN defaults.
            positions.append(" ".join(fields[:4]) + " 0 1")
            if len(positions) >= count:
                break
    return positions


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", action="append", required=True, help="NAME=PATH")
    parser.add_argument("--epd", type=Path, default=None)
    parser.add_argument("--count", type=int, default=50)
    parser.add_argument("--depth", type=int, default=10)
    parser.add_argument("--hash", type=int, default=64, dest="hash_mb")
    parser.add_argument("--show", type=int, default=20, help="disagreements to print")
    args = parser.parse_args()

    specs = [spec.split("=", 1) for spec in args.engine]
    if len(specs) != 2:
        raise SystemExit("exactly two --engine arguments are required")
    positions = load_positions(args.epd, args.count)

    engines = [Engine(name, Path(path), args.hash_mb) for name, path in specs]
    try:
        agree = 0
        disagreements = []
        score_deltas = []
        for fen in positions:
            a = engines[0].analyse(fen, args.depth)
            b = engines[1].analyse(fen, args.depth)
            if a[0] == b[0]:
                agree += 1
            else:
                disagreements.append((fen, a, b))
            if a[1] is not None and b[1] is not None:
                score_deltas.append(a[1] - b[1])

        total = len(positions)
        print(f"\n=== depth {args.depth}, {total} positions, Hash={args.hash_mb} ===")
        print(f"same best move : {agree}/{total} ({100 * agree / total:.1f}%)")
        if score_deltas:
            mean = sum(score_deltas) / len(score_deltas)
            mean_abs = sum(abs(d) for d in score_deltas) / len(score_deltas)
            print(
                f"score delta    : mean {mean:+.1f} cp, mean|delta| {mean_abs:.1f} cp "
                f"({specs[0][0]} minus {specs[1][0]}, {len(score_deltas)} scored)"
            )
        for fen, a, b in disagreements[: args.show]:
            fa = f"{a[1]:+d}cp" if a[1] is not None else f"mate {a[2]}"
            fb = f"{b[1]:+d}cp" if b[1] is not None else f"mate {b[2]}"
            print(f"  {fen}")
            print(f"    {specs[0][0]:<10} {a[0]:<6} {fa:>10}   {specs[1][0]:<10} {b[0]:<6} {fb:>10}")
    finally:
        for engine in engines:
            engine.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
