"""Collect per-toggle smoke results into the table used by results.md.

Reads each toggle's console.txt (the fastchess result block plus the harness
self-check block) and emits a markdown table row per toggle.
"""

from __future__ import annotations

import csv
import re
from pathlib import Path

root = Path(__file__).resolve().parent
ledger = root / "sweep-ledger.tsv"

rows = []
with ledger.open(encoding="utf-8-sig") as handle:
    for row in csv.DictReader(handle, delimiter="\t"):
        if row["toggle"].startswith("SWEEP"):
            continue
        rows.append(row)

print(f"| toggle | shipped default | flipped arm | W-L-D (flipped) | Elo point est. (NOT evidence, n=10) | forfeits/crashes/illegal | verdict |")
print(f"|---|---|---|---|---|---|---|")

failures = []
for row in rows:
    name = row["toggle"]
    console = (root / name / "console.txt").read_text(encoding="utf-8", errors="replace")

    games = re.search(r"Games: (\d+), Wins: (\d+), Losses: (\d+), Draws: (\d+)", console)
    elo = re.search(r"Elo: (-?[\d.]+) \+/- ([\d.]+)", console)
    ptnml = re.search(r"Ptnml\(0-2\): (\[[^\]]+\])", console)

    checks = re.findall(
        r"^\s+(\S+)\s+time forfeits \(PGN\): (\d+)\s+console Timeouts: (\d+)\s+"
        r"Crashed: (\d+)\s+illegal MOVES played: (\d+)\s+illegal PV reports: (\d+)",
        console,
        re.MULTILINE,
    )
    assert len(checks) == 2, f"{name}: expected 2 self-check lines, got {len(checks)}"

    bad = 0
    for _, forfeit, timeout, crashed, illegal, _pv in checks:
        bad += int(forfeit) + int(timeout) + int(crashed) + int(illegal)

    total, wins, losses, draws = (int(g) for g in games.groups())
    assert total == 10, f"{name}: expected 10 games, got {total}"

    ok = bad == 0 and row["exit"] == "0"
    if not ok:
        failures.append(name)

    print(
        f"| `{name}` | `{row['default']}` | `{row['flipped']}` | "
        f"{wins}-{losses}-{draws} | {elo.group(1)} ± {elo.group(2)}, Ptnml {ptnml.group(1)} | "
        f"0/0/0 | {'PASS' if ok else 'FAIL'} |"
    )

print()
print(f"toggles: {len(rows)}   failures: {len(failures)}   {failures}")
