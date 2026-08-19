//! Profiles NNUE incremental-update work over real searches.
//!
//! Reports, per position: the split of NNUE time between changed-threat discovery, the
//! accumulator update itself, and the forward pass; and the frequency of the two full-rebuild
//! paths (king moves and changed-threat buffer overflow) per 1000 searched nodes.
//!
//! Usage: `nnue_update_profile [depth]` (default 7, matching `manifold bench`).

use std::path::PathBuf;
use std::time::Instant;

use mf_core::Position;
use mf_nnue::{Network, reset_update_counters, update_counters};
use mf_search::{SearchLimits, SearchOptions, TranspositionTable, search};

const DEFAULT_DEPTH: u32 = 7;
const HASH_MIB: usize = 16;

/// The six `manifold bench` positions, in bench order.
const BENCH_CASES: [&str; 6] = [
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
    "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
];

/// Deeper single searches, so the profile is not dominated by shallow-tree behaviour.
const DEEP_CASES: [(&str, u32); 3] = [
    (
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        14,
    ),
    (
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        13,
    ),
    (
        "2rq1rk1/pb2bppp/np2pn2/3p4/3P4/1P2PN2/PB1NBPPP/R2Q1RK1 w - - 0 1",
        13,
    ),
];

fn main() {
    let depth = std::env::args().nth(1).map_or(DEFAULT_DEPTH, |value| {
        value
            .parse::<u32>()
            .ok()
            .filter(|&depth| depth > 0)
            .expect("depth must be a positive integer")
    });
    assert!(
        std::env::args().nth(2).is_none(),
        "usage: nnue_update_profile [depth]"
    );

    let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    let network = Network::load(&path)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", path.display()));

    println!("net={} hash={HASH_MIB}MiB threads=1", path.display());
    print_header();

    let mut bench = Totals::default();
    for (index, fen) in BENCH_CASES.iter().enumerate() {
        bench.add(&profile(
            &network,
            &format!("bench{}", index + 1),
            fen,
            depth,
        ));
    }
    // Reported separately because at the default depth its node total is exactly the
    // `manifold bench` signature, so its NPS is directly comparable to an uninstrumented
    // `manifold bench` run and measures what the counters themselves cost.
    print_row("BENCH", depth, &bench.row());

    let mut totals = bench;
    for (fen, deep_depth) in DEEP_CASES {
        totals.add(&profile(
            &network,
            &format!("deep-d{deep_depth}"),
            fen,
            deep_depth,
        ));
    }
    totals.print();
}

struct Row {
    nodes: u64,
    wall_ns: f64,
    counters: mf_nnue::UpdateCounters,
}

fn profile(network: &Network, label: &str, fen: &str, depth: u32) -> Row {
    let position = Position::from_fen(fen, false).expect("profile FEN should parse");
    let table = TranspositionTable::new(HASH_MIB).expect("profile Hash should allocate");

    reset_update_counters();
    let started = Instant::now();
    let result = search(
        &position,
        &table,
        SearchLimits {
            depth: Some(depth),
            ..SearchLimits::default()
        },
        SearchOptions::default(),
        network,
    );
    let wall_ns = started.elapsed().as_secs_f64() * 1e9;
    let row = Row {
        nodes: result.nodes,
        wall_ns,
        counters: update_counters(),
    };
    print_row(label, depth, &row);
    row
}

fn print_header() {
    println!(
        "{:<10} {:>3} {:>10} {:>10} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>11}",
        "position",
        "d",
        "nodes",
        "pushes",
        "nulls",
        "thr%",
        "upd%",
        "rbld%",
        "fwd%",
        "nnue%",
        "kingR/kn",
        "ovfR/kn",
        "edges/pu",
        "nps",
    );
}

fn print_row(label: &str, depth: u32, row: &Row) {
    let counters = &row.counters;
    let nnue_cycles = (counters.threat_discovery_cycles
        + counters.accumulator_update_cycles
        + counters.forward_cycles) as f64;
    let percent = |part: u64| {
        if nnue_cycles == 0.0 {
            0.0
        } else {
            part as f64 * 100.0 / nnue_cycles
        }
    };
    let per_kilonode = |count: u64| {
        if row.nodes == 0 {
            0.0
        } else {
            count as f64 * 1000.0 / row.nodes as f64
        }
    };
    println!(
        "{label:<10} {depth:>3} {:>10} {:>10} {:>10} {:>8.1}% {:>8.1}% {:>8.1}% {:>8.1}% {:>8.1}% {:>9.2} {:>9.2} {:>9.2} {:>11.0}",
        row.nodes,
        counters.real_pushes,
        counters.null_pushes,
        percent(counters.threat_discovery_cycles),
        percent(counters.accumulator_update_cycles),
        percent(counters.rebuild_cycles),
        percent(counters.forward_cycles),
        nnue_share_of_wall(row) * 100.0,
        per_kilonode(counters.king_rebuilds),
        per_kilonode(counters.overflow_rebuilds),
        if counters.real_pushes == 0 {
            0.0
        } else {
            counters.changed_threat_edges as f64 / counters.real_pushes as f64
        },
        if row.wall_ns == 0.0 {
            0.0
        } else {
            row.nodes as f64 * 1e9 / row.wall_ns
        },
    );
}

/// Fraction of wall time the counted NNUE regions account for.
///
/// `rdtsc` ticks at the invariant TSC rate, so the ratio of counted ticks to elapsed ticks is
/// the fraction of wall time, whatever the core's actual clock was.
fn nnue_share_of_wall(row: &Row) -> f64 {
    let nnue_cycles = (row.counters.threat_discovery_cycles
        + row.counters.accumulator_update_cycles
        + row.counters.forward_cycles) as f64;
    let elapsed_cycles = row.wall_ns * tsc_ghz();
    if elapsed_cycles == 0.0 {
        0.0
    } else {
        nnue_cycles / elapsed_cycles
    }
}

/// Measures the invariant-TSC rate once, by timing a fixed sleep.
fn tsc_ghz() -> f64 {
    use std::sync::OnceLock;
    static RATE: OnceLock<f64> = OnceLock::new();
    *RATE.get_or_init(|| {
        let started_tsc = rdtsc();
        let started = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let elapsed_ns = started.elapsed().as_secs_f64() * 1e9;
        (rdtsc() - started_tsc) as f64 / elapsed_ns
    })
}

fn rdtsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `_rdtsc` is unconditionally available on x86_64 and only reads a counter.
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    0
}

#[derive(Default)]
struct Totals {
    nodes: u64,
    wall_ns: f64,
    counters: mf_nnue::UpdateCounters,
}

impl Totals {
    fn add(&mut self, row: &Row) {
        self.nodes += row.nodes;
        self.wall_ns += row.wall_ns;
        let counters = &row.counters;
        self.counters.real_pushes += counters.real_pushes;
        self.counters.null_pushes += counters.null_pushes;
        self.counters.forward_evaluations += counters.forward_evaluations;
        self.counters.king_rebuilds += counters.king_rebuilds;
        self.counters.overflow_rebuilds += counters.overflow_rebuilds;
        self.counters.changed_threat_edges += counters.changed_threat_edges;
        self.counters.sliders_scanned += counters.sliders_scanned;
        self.counters.threat_discovery_cycles += counters.threat_discovery_cycles;
        self.counters.accumulator_update_cycles += counters.accumulator_update_cycles;
        self.counters.rebuild_cycles += counters.rebuild_cycles;
        self.counters.finny_king_updates += counters.finny_king_updates;
        self.counters.finny_threat_rebuilds += counters.finny_threat_rebuilds;
        self.counters.finny_cycles += counters.finny_cycles;
        self.counters.finny_refreshes += counters.finny_refreshes;
        self.counters.finny_delta_rows += counters.finny_delta_rows;
        self.counters.finny_threat_rebuild_cycles += counters.finny_threat_rebuild_cycles;
        self.counters.threat_scan_cycles += counters.threat_scan_cycles;
        self.counters.threat_scan_edges += counters.threat_scan_edges;
        self.counters.forward_cycles += counters.forward_cycles;
        self.counters.deferred_pushes_skipped += counters.deferred_pushes_skipped;
    }

    fn row(&self) -> Row {
        Row {
            nodes: self.nodes,
            wall_ns: self.wall_ns,
            counters: self.counters,
        }
    }

    fn print(&self) {
        print_row("TOTAL", 0, &self.row());
        let counters = &self.counters;
        println!();
        println!(
            "totals: nodes={} real_pushes={} null_pushes={} forwards={}",
            self.nodes, counters.real_pushes, counters.null_pushes, counters.forward_evaluations
        );
        println!(
            "rebuilds: king={} ({:.3}/1000 nodes) overflow={} ({:.3}/1000 nodes)",
            counters.king_rebuilds,
            counters.king_rebuilds as f64 * 1000.0 / self.nodes as f64,
            counters.overflow_rebuilds,
            counters.overflow_rebuilds as f64 * 1000.0 / self.nodes as f64,
        );
        println!(
            "finny: king_moves={} ({:.3}/1000 nodes) of which mirror-flips={} ({:.1}%) \
             rows/refresh={:.2}",
            counters.finny_king_updates,
            counters.finny_king_updates as f64 * 1000.0 / self.nodes as f64,
            counters.finny_threat_rebuilds,
            counters.finny_threat_rebuilds as f64 * 100.0 / counters.finny_king_updates as f64,
            counters.finny_delta_rows as f64 / counters.finny_refreshes as f64,
        );
        println!(
            "per real push: changed_edges={:.2} sliders_scanned={:.2}",
            counters.changed_threat_edges as f64 / counters.real_pushes as f64,
            counters.sliders_scanned as f64 / counters.real_pushes as f64,
        );
        let ghz = tsc_ghz();
        println!(
            "per real push (ns, TSC {ghz:.3} GHz): threat_discovery={:.1} accumulator_update={:.1} \
             (of which rebuild={:.1}, incremental={:.1})",
            counters.threat_discovery_cycles as f64 / counters.real_pushes as f64 / ghz,
            counters.accumulator_update_cycles as f64 / counters.real_pushes as f64 / ghz,
            counters.rebuild_cycles as f64 / counters.real_pushes as f64 / ghz,
            (counters.accumulator_update_cycles - counters.rebuild_cycles) as f64
                / counters.real_pushes as f64
                / ghz,
        );
        let rebuilds = counters.king_rebuilds + counters.overflow_rebuilds;
        if rebuilds > 0 {
            println!(
                "per rebuild (ns): {:.1}",
                counters.rebuild_cycles as f64 / rebuilds as f64 / ghz,
            );
        }
        println!(
            "per finny-served king move (ns): {:.1}",
            counters.finny_cycles as f64 / counters.finny_king_updates as f64 / ghz,
        );
        // The plan-010 step-0 gate. The mirror-flip branch rescans the board
        // (`append_active_threats`) before re-streaming every threat row; an edge cache
        // would remove the scan, so the scan's share of wall is the plan's ceiling.
        if counters.finny_threat_rebuilds > 0 {
            let flips = counters.finny_threat_rebuilds as f64;
            let per_flip_ns = counters.finny_threat_rebuild_cycles as f64 / flips / ghz;
            let scan_ns_per_flip = counters.threat_scan_cycles as f64 / flips / ghz;
            let scan_share = if counters.finny_threat_rebuild_cycles > 0 {
                counters.threat_scan_cycles as f64 * 100.0
                    / counters.finny_threat_rebuild_cycles as f64
            } else {
                0.0
            };
            let elapsed_cycles = self.wall_ns * ghz;
            let ceiling = if elapsed_cycles > 0.0 {
                counters.threat_scan_cycles as f64 * 100.0 / elapsed_cycles
            } else {
                0.0
            };
            println!(
                "flip path: flips={} ({:.1}/1000 nodes) per-flip={:.1} ns \
                 (scan={:.1} ns = {:.1}% of flip, rows+prefetch={:.1} ns) edges/scan={:.2}",
                counters.finny_threat_rebuilds,
                flips * 1000.0 / self.nodes as f64,
                per_flip_ns,
                scan_ns_per_flip,
                scan_share,
                per_flip_ns - scan_ns_per_flip,
                counters.threat_scan_edges as f64 / flips,
            );
            println!(
                "projected ceiling: scan = {:.2}% of wall (plan 010 gate: keep only if >= 0.50%)",
                ceiling,
            );
        }
        println!(
            "per forward (ns): {:.1}",
            counters.forward_cycles as f64 / counters.forward_evaluations as f64 / ghz,
        );
        // The ceiling on lazy updates: deferring work only ever saves anything on a pushed
        // state whose accumulator is never read. Counted against pushes, not nodes, because a
        // node that never evaluates still paid for its push.
        let pushes = counters.real_pushes + counters.null_pushes;
        let unread = pushes.saturating_sub(counters.forward_evaluations);
        println!(
            "unread pushes: {unread} of {pushes} ({:.1}%) -- OVERSTATES the lazy ceiling",
            unread as f64 * 100.0 / pushes as f64,
        );
        // The real ceiling. An unread push is only skippable if no descendant was evaluated
        // either: the first eval below it forces its materialization anyway, because the
        // descendant's incremental update reads its parent's accumulator.
        println!(
            "deferred pushes SKIPPED: {} of {} ({:.1}%) -- accumulator work lazy updates avoided",
            counters.deferred_pushes_skipped,
            counters.real_pushes,
            counters.deferred_pushes_skipped as f64 * 100.0 / counters.real_pushes as f64,
        );
    }
}
