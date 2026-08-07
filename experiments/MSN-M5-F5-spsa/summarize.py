"""Summarise a tuning session's history.csv: final spins, drift, and stability."""

import csv
import statistics
import sys

path = sys.argv[1] if len(sys.argv) > 1 else r"experiments\MSN-M5-F5-spsa\session\history.csv"
rows = list(csv.DictReader(open(path)))
names = [k[:-5] for k in rows[0] if k.endswith("_spin")]

defaults = {
    "LmrCoefficient": 2872,
    "LmrBase": 982,
    "LmrTtPvReduction": 1024,
    "LmrHistoryNumerator": 439,
    "RfpMarginPerDepth": 105,
    "RfpTtPvMargin": 21,
    "FutilityBaseMargin": 124,
    "FutilityMarginPerDepth": 109,
}

print("iterations:", len(rows), rows[0]["iteration"], "->", rows[-1]["iteration"])
games = sum(int(r["wins"]) + int(r["losses"]) + int(r["draws"]) for r in rows)
print("games:", games, " sum(score):", sum(int(r["score"]) for r in rows))
print()
header = "{:<26}{:>8}{:>8}{:>8}{:>11}{:>11}{:>9}"
print(header.format("param", "default", "final", "delta", "mean-last50", "sd-last50", "range%"))
for n in names:
    v = [float(r[n]) for r in rows]
    final = int(round(v[-1]))
    d = defaults[n]
    span = max(v) - min(v)
    print(
        header.format(
            n,
            d,
            final,
            final - d,
            round(statistics.mean(v[-50:]), 1),
            round(statistics.pstdev(v[-50:]), 2),
            round(100.0 * span / max(abs(d), 1), 1),
        )
    )
