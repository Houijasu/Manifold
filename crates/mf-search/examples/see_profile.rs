//! Profiles static exchange evaluation work over real searches.
//!
//! Reports, per position: SEE calls per node, nanoseconds per call, and the fraction of wall
//! time spent inside `static_exchange_evaluation`. Used to size the SEE rewrite and to verify
//! each step of it against the same positions.
//!
//! Usage: `see_profile [depth]` (default 7, matching `manifold bench`).

use std::path::PathBuf;
use std::time::Instant;

use mf_core::{Position, reset_see_counters, see_counters};
use mf_search::{
    SearchLimits, SearchOptions, TranspositionTable, reset_search_counters, search, search_counters,
};

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
        "usage: see_profile [depth]"
    );

    let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
        PathBuf::from,
    );
    let network = mf_nnue::Network::load(&path)
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
    calls: u64,
    cycles: u64,
    sites: SeeSites,
}

/// Per-call-site SEE counts, so the profile can show where the aggregate SEE cost is
/// issued from. The four sites are every non-test `static_exchange_evaluation` call in
/// mf-search, so their sum must equal the mf-core `see_calls` total.
#[derive(Clone, Copy, Default)]
struct SeeSites {
    load_captures: u64,
    tt_validation: u64,
    interior_quiets_fallback: u64,
    quiet_checks: u64,
}

impl SeeSites {
    fn from_counters(counters: &mf_search::SearchCounters) -> Self {
        Self {
            load_captures: counters.see_calls_load_captures,
            tt_validation: counters.see_calls_tt_validation,
            interior_quiets_fallback: counters.see_calls_interior_quiets_fallback,
            quiet_checks: counters.see_calls_quiet_checks,
        }
    }

    fn add(&mut self, other: Self) {
        self.load_captures += other.load_captures;
        self.tt_validation += other.tt_validation;
        self.interior_quiets_fallback += other.interior_quiets_fallback;
        self.quiet_checks += other.quiet_checks;
    }
}

fn profile(network: &mf_nnue::Network, label: &str, fen: &str, depth: u32) -> Row {
    let position = Position::from_fen(fen, false).expect("profile FEN should parse");
    let table = TranspositionTable::new(HASH_MIB).expect("profile Hash should allocate");

    reset_see_counters();
    reset_search_counters();
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
    let counters = see_counters();
    let row = Row {
        nodes: result.nodes,
        wall_ns,
        calls: counters.calls,
        cycles: counters.cycles,
        sites: SeeSites::from_counters(&search_counters()),
    };
    print_row(label, depth, &row);
    row
}

fn print_header() {
    println!(
        "{:<10} {:>3} {:>10} {:>10} {:>9} {:>9} {:>8} {:>11}",
        "position", "d", "nodes", "see_calls", "calls/1kn", "ns/call", "see%", "nps",
    );
}

fn print_row(label: &str, depth: u32, row: &Row) {
    let ghz = tsc_ghz();
    println!(
        "{label:<10} {depth:>3} {:>10} {:>10} {:>9.2} {:>9.1} {:>7.1}% {:>11.0}",
        row.nodes,
        row.calls,
        if row.nodes == 0 {
            0.0
        } else {
            row.calls as f64 * 1000.0 / row.nodes as f64
        },
        if row.calls == 0 {
            0.0
        } else {
            row.cycles as f64 / row.calls as f64 / ghz
        },
        see_share_of_wall(row) * 100.0,
        if row.wall_ns == 0.0 {
            0.0
        } else {
            row.nodes as f64 * 1e9 / row.wall_ns
        },
    );
}

/// Fraction of wall time the counted SEE regions account for.
///
/// `rdtsc` ticks at the invariant TSC rate, so the ratio of counted ticks to elapsed ticks is
/// the fraction of wall time, whatever the core's actual clock was.
fn see_share_of_wall(row: &Row) -> f64 {
    let elapsed_cycles = row.wall_ns * tsc_ghz();
    if elapsed_cycles == 0.0 {
        0.0
    } else {
        row.cycles as f64 / elapsed_cycles
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
    calls: u64,
    cycles: u64,
    sites: SeeSites,
}

impl Totals {
    fn add(&mut self, row: &Row) {
        self.nodes += row.nodes;
        self.wall_ns += row.wall_ns;
        self.calls += row.calls;
        self.cycles += row.cycles;
        self.sites.add(row.sites);
    }

    fn row(&self) -> Row {
        Row {
            nodes: self.nodes,
            wall_ns: self.wall_ns,
            calls: self.calls,
            cycles: self.cycles,
            sites: self.sites,
        }
    }

    fn print(&self) {
        print_row("TOTAL", 0, &self.row());
        let ghz = tsc_ghz();
        println!();
        println!(
            "totals: nodes={} see_calls={} ({:.2}/1000 nodes)",
            self.nodes,
            self.calls,
            self.calls as f64 * 1000.0 / self.nodes as f64,
        );
        println!(
            "per call (ns, TSC {ghz:.3} GHz): {:.1}",
            self.cycles as f64 / self.calls as f64 / ghz,
        );
        println!(
            "share of wall: {:.1}%",
            see_share_of_wall(&self.row()) * 100.0,
        );
        let per_kilonode = |count: u64| count as f64 * 1000.0 / self.nodes as f64;
        println!(
            "per site (calls/1000 nodes): load_captures={:.2} tt_validation={:.2} \
             interior_quiets_fallback={:.2} quiet_checks={:.2}",
            per_kilonode(self.sites.load_captures),
            per_kilonode(self.sites.tt_validation),
            per_kilonode(self.sites.interior_quiets_fallback),
            per_kilonode(self.sites.quiet_checks),
        );
        let site_sum = self.sites.load_captures
            + self.sites.tt_validation
            + self.sites.interior_quiets_fallback
            + self.sites.quiet_checks;
        println!(
            "site sum {site_sum} vs see_calls total {} (must match)",
            self.calls
        );
    }
}
