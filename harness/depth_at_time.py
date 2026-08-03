"""Compare the DEPTH two engines reach in the same wall-clock time.

Move quality is decided by how deep the search actually gets before the clock runs
out, not by how many nodes it visited. This driver gives both engines the identical
`go movetime` budget on the identical positions and reports the depth each finished,
which is the quantity that converts search efficiency into move strength.

Both engines are driven with stdin held OPEN and stdout polled until `bestmove`.

Usage:
    py -3 harness/depth_at_time.py --engine A=<exe> --engine B=<exe> \
        --epd tools/books/UHO_4060_v4.epd --count 30 --movetime 1000
"""

from __future__ import annotations

import argparse
import queue
import re
import statistics
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

    def search(self, fen: str, movetime_ms: int) -> tuple[int, int]:
        """Return (final_depth, seldepth) for a fixed-time search."""
        self._send("ucinewgame")
        self._send("isready")
        self._wait("readyok", 120)
        self._send(f"position fen {fen}")
        self.last_infos = []
        self._send(f"go movetime {movetime_ms}")
        self._wait("bestmove", 900)
        depth = 0
        seldepth = 0
        for line in self.last_infos:
            match = re.search(r"^info depth (\d+)", line)
            if match:
                depth = max(depth, int(match.group(1)))
            sel = re.search(r"\bseldepth (\d+)", line)
            if sel:
                seldepth = max(seldepth, int(sel.group(1)))
        return depth, seldepth

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self._send("quit")
                self.process.wait(timeout=15)
            except Exception:
                self.process.kill()


def load_positions(epd: Path | None, count: int) -> list[str]:
    if epd is None:
        return ["rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"]
    positions = []
    with epd.open(encoding="utf-8", errors="ignore") as handle:
        for line in handle:
            fields = line.split()
            if len(fields) < 4:
                continue
            positions.append(" ".join(fields[:4]) + " 0 1")
            if len(positions) >= count:
                break
    return positions


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", action="append", required=True, help="NAME=PATH")
    parser.add_argument("--epd", type=Path, default=None)
    parser.add_argument("--count", type=int, default=30)
    parser.add_argument("--movetime", type=int, default=1000)
    parser.add_argument("--hash", type=int, default=64, dest="hash_mb")
    args = parser.parse_args()

    specs = [spec.split("=", 1) for spec in args.engine]
    positions = load_positions(args.epd, args.count)

    results: dict[str, list[int]] = {}
    for name, path in specs:
        engine = Engine(name, Path(path), args.hash_mb)
        try:
            # One discarded search so neither engine pays process warmup inside a
            # measured region (a cold-start comparison is unfair by A-EONEGO-004).
            engine.search(positions[0], args.movetime)
            depths = [engine.search(fen, args.movetime)[0] for fen in positions]
            results[name] = depths
        finally:
            engine.close()

    print(f"\n=== movetime {args.movetime}ms, {len(positions)} positions, Hash={args.hash_mb} ===")
    for name, depths in results.items():
        print(
            f"{name:<12} mean depth {statistics.mean(depths):5.2f}  "
            f"median {statistics.median(depths):5.1f}  min {min(depths)}  max {max(depths)}"
        )
    if len(results) == 2:
        (first, a), (second, b) = results.items()
        diff = statistics.mean(a) - statistics.mean(b)
        wins = sum(1 for x, y in zip(a, b) if x > y)
        ties = sum(1 for x, y in zip(a, b) if x == y)
        print(
            f"\n{first} minus {second}: {diff:+.2f} plies mean; "
            f"{first} deeper in {wins}/{len(a)}, equal {ties}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
