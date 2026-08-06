"""Enumerate `type check` toggles from the live UCI handshake.

Stdin is held OPEN and stdout polled (AGENTS.md rule 7: a piped here-string closes
stdin and aborts the engine mid-handshake).
"""

from __future__ import annotations

import queue
import subprocess
import sys
import threading
import time
from pathlib import Path

exe = Path(sys.argv[1])
proc = subprocess.Popen(
    [str(exe)],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
    bufsize=1,
)
lines: queue.Queue[str] = queue.Queue()


def pump() -> None:
    assert proc.stdout is not None
    for line in proc.stdout:
        lines.put(line.rstrip())


threading.Thread(target=pump, daemon=True).start()
assert proc.stdin is not None
proc.stdin.write("uci\n")
proc.stdin.flush()

collected: list[str] = []
deadline = time.monotonic() + 120
while time.monotonic() < deadline:
    try:
        line = lines.get(timeout=0.05)
    except queue.Empty:
        if proc.poll() is not None:
            raise SystemExit("engine exited during handshake")
        continue
    collected.append(line)
    if line.startswith("uciok"):
        break
else:
    raise SystemExit("no uciok within 120s")

proc.stdin.write("quit\n")
proc.stdin.flush()
proc.wait(timeout=30)

for line in collected:
    print(line)
