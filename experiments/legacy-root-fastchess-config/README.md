# Legacy root fastchess config

This directory preserves the exact tracked `config.json` snapshot that previously lived
at the repository root.

The JSON is historical experiment evidence, not a reusable tournament template. It
contains machine-specific paths and completed-run state from the environment that
produced it. Do not copy it back to the root or use it for a new match.

Run new matches through `harness/run_match.ps1` and place each run under a named
`experiments/<run-name>/` directory so the command, metadata, console output, and result
write-up stay together.
