"""Re-pins bench_cli.rs anchors to the M5-F5 tuned-default vectors."""

PATH = r"crates\mf-uci\tests\bench_cli.rs"

SUBS = [
    ("const BENCH_NODE_COUNT: u64 = 41_588;", "const BENCH_NODE_COUNT: u64 = 34_516;"),
    ('const BENCH_NODES: &str = "Nodes searched: 41588";',
     'const BENCH_NODES: &str = "Nodes searched: 34516";'),
    ("const BENCH_NODE_COUNT_WITHOUT_POST_LMR_DEPTH: u64 = 42_409;",
     "const BENCH_NODE_COUNT_WITHOUT_POST_LMR_DEPTH: u64 = 35_199;"),
    ("const BENCH_NODE_COUNT_WITH_POST_LMR_CONTHIST: u64 = 43_134;",
     "const BENCH_NODE_COUNT_WITH_POST_LMR_CONTHIST: u64 = 36_581;"),
    ("const BENCH_NODE_COUNT_WITHOUT_CAPTURE_LMR: u64 = 44_737;",
     "const BENCH_NODE_COUNT_WITHOUT_CAPTURE_LMR: u64 = 38_620;"),
    ("const BENCH_NODE_COUNT_WITHOUT_EITHER_LMR_FEATURE: u64 = 45_036;",
     "const BENCH_NODE_COUNT_WITHOUT_EITHER_LMR_FEATURE: u64 = 39_272;"),
    ("const BENCH_NODE_COUNT_WITH_QSEARCH_CHECKS: u64 = 44_860;",
     "const BENCH_NODE_COUNT_WITH_QSEARCH_CHECKS: u64 = 38_871;"),
    ("const BENCH_NODE_COUNT_WITHOUT_CORRECTION: u64 = 38_858;",
     "const BENCH_NODE_COUNT_WITHOUT_CORRECTION: u64 = 39_134;"),
    ("const BENCH_NODE_COUNT_WITHOUT_CONTINUATION: u64 = 37_032;",
     "const BENCH_NODE_COUNT_WITHOUT_CONTINUATION: u64 = 36_360;"),
    ("const BENCH_NODE_COUNT_WITHOUT_LMR: u64 = 124_323;",
     "const BENCH_NODE_COUNT_WITHOUT_LMR: u64 = 78_365;"),
    ("        vec![80_425; 3],", "        vec![67_005; 3],"),
    ("        nodes[2], 58_272,", "        nodes[2], 56_833,"),
    ("        nodes[3], 151_903,", "        nodes[3], 135_316,"),
    ("    assert_eq!(nodes, [BENCH_NODE_COUNT, 45_188, 40_161]);",
     "    assert_eq!(nodes, [BENCH_NODE_COUNT, 45_249, 37_519]);"),
]

text = open(PATH, encoding="utf-8").read()
for old, new in SUBS:
    assert text.count(old) == 1, (old, text.count(old))
    text = text.replace(old, new)
open(PATH, "w", encoding="utf-8", newline="").write(text)
print("re-pinned", len(SUBS), "anchors")
