"""Paired opening-level comparison of the two Stockfish 18 anchor matches.

M1-F3 (mission-start vs SF18) and M4-F2 (mission-final vs SF18) used the SAME seed
(20260805), the same book, and the same round count, so fastchess drew the SAME opening
sequence in both runs. That makes the two matches paired at the opening level, and the
difference in Manifold's score can be estimated with a PAIRED standard error, which is
smaller than the quadrature sum of the two independent error bars.

Reads both PGNs, keys each game by its opening FEN + round, sums Manifold's points per
opening pair, and reports the per-pair difference distribution.
"""

import re
import sys
from pathlib import Path

ROOT = Path(r"C:\Users\Samaritan\Projects\Manifold")
RUNS = {
    "mission-start (M1-F3)": (
        ROOT / "experiments/MSN-F3-stockfish-baseline/games.pgn",
        "manifold-mission-start",
    ),
    "mission-final (M4-F2)": (
        ROOT / "experiments/MSN-final-stockfish/games.pgn",
        "mission-final",
    ),
}

TAG = re.compile(r'^\[(\w+)\s+"(.*)"\]\s*$')


def load(pgn_path, manifold_name):
    """-> {round: (fen, manifold_points_in_that_pair)}, and {round: fen}"""
    pairs, fens = {}, {}
    cur = {}
    with pgn_path.open(encoding="utf-8", errors="replace") as fh:
        for line in fh:
            m = TAG.match(line)
            if m:
                cur[m.group(1)] = m.group(2)
                continue
            if line.strip() and cur.get("Result"):
                rnd = cur.get("Round", "?")
                fen = cur.get("FEN", "?")
                white, black, result = cur["White"], cur["Black"], cur["Result"]
                if result == "1-0":
                    pts = 1.0 if white == manifold_name else 0.0
                elif result == "0-1":
                    pts = 1.0 if black == manifold_name else 0.0
                elif result == "1/2-1/2":
                    pts = 0.5
                else:
                    cur = {}
                    continue
                pairs[rnd] = pairs.get(rnd, 0.0) + pts
                fens.setdefault(rnd, fen)
                cur = {}
    return pairs, fens


data = {}
for label, (path, name) in RUNS.items():
    if not path.exists():
        sys.exit(f"missing {path}")
    data[label] = load(path, name)
    p, _ = data[label]
    print(f"{label}: {len(p)} opening pairs, total {sum(p.values())} points")

(a_pairs, a_fens), (b_pairs, b_fens) = data.values()
labels = list(data)

common = sorted(set(a_pairs) & set(b_pairs), key=lambda r: int(r.split(".")[0]))
print(f"\ncommon rounds: {len(common)}")

same_fen = sum(1 for r in common if a_fens[r] == b_fens[r])
print(f"rounds whose opening FEN is IDENTICAL in both runs: {same_fen}/{len(common)}")
if same_fen != len(common):
    print("  -> openings DIFFER; the runs are NOT paired, use the independent error bars.")
    sys.exit(0)

diffs = [b_pairs[r] - a_pairs[r] for r in common]
n = len(diffs)
mean = sum(diffs) / n
var = sum((d - mean) ** 2 for d in diffs) / (n - 1)
se = (var / n) ** 0.5

print(f"\nper-pair score difference ({labels[1]} minus {labels[0]}), out of 2 points/pair:")
print(f"  mean  {mean:+.4f}  sd {var ** 0.5:.4f}  se {se:.4f}  (n={n} pairs)")
print(f"  as a score-percentage difference: {mean / 2 * 100:+.2f} pp +/- {se / 2 * 100 * 1.96:.2f} pp (95%)")

better = sum(1 for d in diffs if d > 0)
worse = sum(1 for d in diffs if d < 0)
print(f"  pairs where mission-final scored MORE: {better}, LESS: {worse}, EQUAL: {n - better - worse}")

sa, sb = sum(a_pairs[r] for r in common), sum(b_pairs[r] for r in common)
print(f"\ntotals over the {n} common pairs ({2 * n} games):")
print(f"  {labels[0]}: {sa} pts = {sa / (2 * n) * 100:.2f}%")
print(f"  {labels[1]}: {sb} pts = {sb / (2 * n) * 100:.2f}%")
