# Fixed-depth node counts for one engine with one UCI option flipped per run.
# Deterministic single-thread search, so one run per arm is exact.
# Usage: py -3 harness/toggle_nodes.py <exe> <depth> [Name=value ...]
import re
import subprocess
import sys

POSITIONS = {
    "kiwipete": "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "endgame": "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
}


def nodes(exe: str, depth: int, option: str | None) -> dict[str, int]:
    out = {}
    for label, fen in POSITIONS.items():
        lines = ["uci"]
        if option:
            for one in option.split(","):
                name, value = one.split("=", 1)
                lines.append(f"setoption name {name} value {value}")
        lines += [f"position fen {fen}", f"go depth {depth}"]
        proc = subprocess.Popen(
            [exe], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        proc.stdin.write("\n".join(lines) + "\n")
        proc.stdin.flush()
        best = 0
        for line in proc.stdout:
            match = re.search(r"\bnodes (\d+)", line)
            if match:
                best = max(best, int(match.group(1)))
            if line.startswith("bestmove"):
                break
        proc.stdin.write("quit\n")
        proc.stdin.flush()
        proc.wait(timeout=10)
        out[label] = best
    return out


def main() -> None:
    exe, depth = sys.argv[1], int(sys.argv[2])
    baseline = nodes(exe, depth, None)
    print(f"{'arm':<40}" + "".join(f"{p:>12}" for p in POSITIONS) + "   ratio-vs-default")
    print(f"{'default':<40}" + "".join(f"{baseline[p]:>12,}" for p in POSITIONS))
    for option in sys.argv[3:]:
        row = nodes(exe, depth, option)
        ratios = " ".join(f"{p}={row[p] / baseline[p]:.2f}x" for p in POSITIONS)
        print(f"{option:<40}" + "".join(f"{row[p]:>12,}" for p in POSITIONS) + f"   {ratios}")


if __name__ == "__main__":
    main()
