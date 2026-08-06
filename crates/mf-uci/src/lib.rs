//! Universal Chess Interface protocol handling for Manifold.

mod datagen_cli;

pub use datagen_cli::run_datagen_subcommand;

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mf_core::{Position, format_uci_move, generate_legal_moves, parse_uci_move, perft_divide};
use mf_nnue::{Network, NetworkSource, production_forward_mode, resolve_network};
use mf_search::{
    IterationInfo, PoolError, PoolSearchResult, RootMoveInfo, SearchLimits, SearchOptions,
    SearchPool, SearchResult, SharedHistory, TranspositionTable, clamp_centipawn_score,
    max_hash_mebibytes, score_to_uci_mate, search_with_shared_history,
};

const DEFAULT_HASH_MIB: usize = 16;
const MIN_HASH_MIB: i128 = 1;
/// The handshake, apart from the `Hash` line.
///
/// `Hash` is not here because its advertised range is a property of the machine rather
/// than of the build: see [`hash_option_line`].
const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Houijasu",
    "option name Threads type spin default 1 min 1 max 256",
    "option name UCI_Chess960 type check default false",
    "option name UseNMP type check default true",
    "option name UseRFP type check default true",
    "option name UseRazoring type check default true",
    "option name UseLMR type check default true",
    "option name UseLMP type check default true",
    "option name UseFutility type check default true",
    "option name UseSEEPruning type check default true",
    "option name UseQSearchTT type check default true",
    "option name UseQSearchDeltaPruning type check default true",
    "option name UseQSearchChecks type check default false",
    "option name UseCaptureLMR type check default false",
    "option name UseSingularExt type check default true",
    "option name UseCheckExt type check default true",
    "option name UseMultiCut type check default true",
    "option name UseIIR type check default true",
    "option name UseProbCut type check default true",
    "option name UseButterflyHistory type check default true",
    "option name UseCaptureHistory type check default true",
    "option name UsePawnHistory type check default false",
    "option name UseContHistory type check default true",
    "option name UseHistoryPruning type check default false",
    "option name UseCorrHistory type check default true",
    "option name UseCorrHistPawn type check default true",
    "option name UseCorrHistMinor type check default true",
    "option name UseCorrHistMajor type check default false",
    "option name UseCorrHistMaterial type check default false",
    "option name UseCorrHistCont type check default true",
    "option name UseTimeEffort type check default false",
    "option name EvalFile type string default <empty>",
];

const BENCH_CASES: [&str; 6] = [
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
    "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
];
const BENCH_DEPTH: u32 = 7;
const BENCH_HASH_MIB: usize = 16;
const MTBENCH_DEFAULT_THREADS: [usize; 4] = [1, 2, 4, 8];
const MTBENCH_DEFAULT_DEPTH: u32 = 10;
const MTBENCH_HASH_MIB: usize = 64;
const MTBENCH_MAX_THREADS: usize = 256;
const DEFAULT_MOVES_TO_GO: u64 = 30;
/// Upper bound on `movestogo`, so a tournament that announces a very distant time
/// control does not shrink each move's budget to nothing.
const MAX_MOVES_TO_GO: u64 = 50;
const TIME_OVERHEAD_MILLIS: u64 = 10;
/// Fraction of the increment folded into the per-move budget, in percent.
const INCREMENT_FRACTION_PERCENT: u64 = 75;
/// Fraction of the remaining clock held back as a safety reserve, in percent.
const CLOCK_SAFETY_PERCENT: u64 = 2;
/// Ceiling on a single move's hard limit as a fraction of the remaining clock, in
/// percent.
const HARD_LIMIT_CLOCK_PERCENT: u64 = 40;
/// Ceiling on a single move's hard limit as a multiple of its soft limit.
///
/// This is the whole point of having two limits. At 5/4 the hard limit sat so close to
/// the soft one that a position whose score was collapsing could not be given materially
/// more time than a quiet one, which makes the soft/hard split decorative.
const HARD_LIMIT_SOFT_MULTIPLE: u64 = 4;
const NULL_BESTMOVE: &str = "0000";

/// The `Hash` option line, whose advertised maximum is what this machine can allocate.
///
/// A fixed maximum here is a promise the engine cannot keep on every machine it runs on,
/// and the failure mode is silent: the GUI sets the size it was offered, allocation is
/// refused, and the engine keeps searching with the table it already had.
fn hash_option_line() -> String {
    format!(
        "option name Hash type spin default {DEFAULT_HASH_MIB} min {MIN_HASH_MIB} max {}",
        max_hash_mebibytes()
    )
}

struct EngineState {
    position: Position,
    position_history: Vec<u64>,
    /// Set when a `position` command failed, meaning `position` no longer describes the
    /// board the GUI is on. A search started from it would return a move that is illegal
    /// in the real position, so `go` declines until a `position` command succeeds.
    position_is_stale: bool,
    chess960: bool,
    /// The engine evaluates only with NNUE, so a network is always loaded. A default
    /// build embeds one, which is why this can be an unconditional value rather than
    /// something the search has to check on every node.
    network: Arc<Network>,
    network_source: NetworkSource,
    search_pool: Arc<SearchPool>,
    search_options: SearchOptions,
    transposition_table: Arc<TranspositionTable>,
}

impl Default for EngineState {
    fn default() -> Self {
        let position = Position::startpos();
        let network = default_network_resolution();
        Self {
            position_history: vec![position.repetition_key()],
            position,
            position_is_stale: false,
            chess960: false,
            network: network.network,
            network_source: network.source,
            search_pool: Arc::new(
                SearchPool::new(1).expect("the default search worker should start"),
            ),
            search_options: SearchOptions::default(),
            transposition_table: Arc::new(
                TranspositionTable::new(DEFAULT_HASH_MIB)
                    .expect("the default transposition table should allocate"),
            ),
        }
    }
}

#[derive(Clone)]
struct SharedNetworkResolution {
    network: Arc<Network>,
    source: NetworkSource,
}

/// Resolves the automatic network once per process.
///
/// Automatic resolution ends in the embedded network, so it cannot come up empty in a
/// default build. If it fails anyway -- a corrupt explicit override of the discovery
/// path, or an `embedded-net`-less build with nothing on disk -- the engine cannot
/// evaluate a single position, so there is nothing useful to degrade to.
fn default_network_resolution() -> SharedNetworkResolution {
    static RESOLUTION: OnceLock<SharedNetworkResolution> = OnceLock::new();
    RESOLUTION
        .get_or_init(|| match resolve_network(None) {
            Ok(resolved) => {
                let (network, source) = resolved.into_parts();
                SharedNetworkResolution {
                    network: Arc::new(network),
                    source,
                }
            }
            Err(error) => panic!("manifold requires an NNUE network to evaluate: {error}"),
        })
        .clone()
}

impl EngineState {
    fn new_game(&mut self) -> Result<(), PoolError> {
        self.position = Position::startpos();
        self.position_history = vec![self.position.repetition_key()];
        // A known board again, so searching is safe even if the previous game ended on a
        // rejected `position` command.
        self.position_is_stale = false;
        self.search_pool
            .clear(Arc::clone(&self.transposition_table))
    }
}

struct ActiveSearch {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl ActiveSearch {
    fn stop_and_join(self) {
        while !self.handle.is_finished() {
            self.stop.store(true, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(1));
        }
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

/// Serves UCI commands until `quit` or end-of-file.
pub fn run<R, W>(reader: R, writer: W) -> io::Result<()>
where
    R: BufRead,
    W: Write + Send + 'static,
{
    let mut state = EngineState::default();
    let writer = Arc::new(Mutex::new(writer));
    let mut active_search = None;

    for line in reader.lines() {
        let command = line?;
        let command = command.trim();
        let tokens: Vec<_> = command.split_whitespace().collect();
        let keyword = tokens.first().copied().unwrap_or_default();
        let has_no_arguments = tokens.len() == 1;

        if keyword.eq_ignore_ascii_case("uci") && has_no_arguments {
            {
                let mut writer = writer
                    .lock()
                    .expect("UCI writer lock should not be poisoned");
                for response in UCI_RESPONSE {
                    writeln!(writer, "{response}")?;
                }
                writeln!(writer, "{}", hash_option_line())?;
                writeln!(writer, "uciok")?;
                writer.flush()?;
            }
        } else if keyword.eq_ignore_ascii_case("isready") && has_no_arguments {
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            writeln!(writer, "readyok")?;
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("stop") && has_no_arguments {
            stop_active_search(&mut active_search);
        } else if keyword.eq_ignore_ascii_case("quit") && has_no_arguments {
            stop_active_search(&mut active_search);
            break;
        } else if keyword.eq_ignore_ascii_case("ucinewgame") && has_no_arguments {
            stop_active_search(&mut active_search);
            if let Err(error) = state.new_game() {
                let mut writer = writer
                    .lock()
                    .expect("UCI writer lock should not be poisoned");
                writeln!(writer, "info string unable to clear Hash: {error}")?;
                writer.flush()?;
            }
        } else if keyword.eq_ignore_ascii_case("bench") && has_no_arguments {
            stop_active_search(&mut active_search);
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            write_bench(&mut *writer, state.search_options, &state.network)
                .map_err(io::Error::other)?;
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("setoption") {
            stop_active_search(&mut active_search);
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            handle_setoption(command, &mut state, &mut *writer)?;
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("position") {
            stop_active_search(&mut active_search);
            match handle_position(command, &mut state) {
                Ok(()) => state.position_is_stale = false,
                Err(error) => {
                    // The command named a board this engine could not construct, so
                    // whatever `state.position` still holds is NOT what the GUI is
                    // showing. Remember that, so the next `go` refuses to answer from it.
                    state.position_is_stale = true;
                    let mut writer = writer
                        .lock()
                        .expect("UCI writer lock should not be poisoned");
                    writeln!(writer, "info string invalid position command: {error}")?;
                    writer.flush()?;
                }
            }
        } else if keyword.eq_ignore_ascii_case("go") {
            let Some(request) = GoRequest::parse(&tokens) else {
                continue;
            };
            stop_active_search(&mut active_search);
            // Answering from a board the GUI is not showing produces a move that is
            // illegal there, which most GUIs score as an immediate loss. UCI still
            // requires a reply to every `go`, so emit the null move rather than a
            // confidently wrong one, and say why.
            if state.position_is_stale && !matches!(request, GoRequest::Perft(_)) {
                let mut writer = writer
                    .lock()
                    .expect("UCI writer lock should not be poisoned");
                writeln!(
                    writer,
                    "info string refusing to search: the last position command failed, \
                     so the engine does not know the current position"
                )?;
                writeln!(writer, "bestmove {NULL_BESTMOVE}")?;
                writer.flush()?;
                continue;
            }
            match request {
                GoRequest::Perft(depth) => {
                    let mut writer = writer
                        .lock()
                        .expect("UCI writer lock should not be poisoned");
                    write_perft(&mut *writer, &mut state.position, depth, state.chess960)?;
                    writer.flush()?;
                }
                GoRequest::Search(parameters, ignored) if parameters.infinite => {
                    report_ignored_go_arguments(&writer, &ignored)?;
                    let network = Arc::clone(&state.network);
                    let evaluator_diagnostic = active_evaluator_diagnostic(&state);
                    active_search = Some(start_search(
                        state.position.clone(),
                        state.position_history.clone(),
                        Arc::clone(&state.search_pool),
                        Arc::clone(&state.transposition_table),
                        parameters.search_limits(&state.position),
                        state.search_options,
                        network,
                        evaluator_diagnostic,
                        state.chess960,
                        Arc::clone(&writer),
                        true,
                        false,
                    ));
                }
                GoRequest::Search(parameters, ignored) => {
                    report_ignored_go_arguments(&writer, &ignored)?;
                    let fixed_depth = parameters.depth.is_some();
                    let network = Arc::clone(&state.network);
                    let evaluator_diagnostic = active_evaluator_diagnostic(&state);
                    active_search = Some(start_search(
                        state.position.clone(),
                        state.position_history.clone(),
                        Arc::clone(&state.search_pool),
                        Arc::clone(&state.transposition_table),
                        parameters.search_limits(&state.position),
                        state.search_options,
                        network,
                        evaluator_diagnostic,
                        state.chess960,
                        Arc::clone(&writer),
                        false,
                        fixed_depth,
                    ));
                }
            }
        }
    }

    stop_active_search(&mut active_search);
    Ok(())
}

/// Reports `go` arguments the engine could not act on, so a dialect gap stays visible.
fn report_ignored_go_arguments<W: Write>(
    writer: &Arc<Mutex<W>>,
    ignored: &[String],
) -> io::Result<()> {
    if ignored.is_empty() {
        return Ok(());
    }
    let mut writer = writer
        .lock()
        .expect("UCI writer lock should not be poisoned");
    writeln!(
        writer,
        "info string ignoring unrecognized go arguments: {}",
        ignored.join(", ")
    )?;
    writer.flush()
}

fn stop_active_search(active_search: &mut Option<ActiveSearch>) {
    if let Some(search) = active_search.take() {
        search.stop_and_join();
    }
}

fn active_evaluator_diagnostic(state: &EngineState) -> String {
    let mode = production_forward_mode();
    format!(
        "info string evaluation NNUE from {}; network \"{}\"; backend {:?}; sparse FC0 {}",
        state.network_source,
        state.network.description(),
        mode.backend(),
        mode.sparse_fc0()
    )
}

#[allow(clippy::too_many_arguments)]
fn start_search<W>(
    position: Position,
    position_history: Vec<u64>,
    search_pool: Arc<SearchPool>,
    transposition_table: Arc<TranspositionTable>,
    limits: SearchLimits,
    options: SearchOptions,
    network: Arc<Network>,
    evaluator_diagnostic: String,
    chess960: bool,
    writer: Arc<Mutex<W>>,
    wait_for_stop: bool,
    fixed_depth: bool,
) -> ActiveSearch
where
    W: Write + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let search_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        if let Ok(mut writer) = writer.lock() {
            let _ = writeln!(writer, "{evaluator_diagnostic}");
            let _ = writer.flush();
        }
        let on_iteration = |iteration: &IterationInfo| {
            if let Ok(mut writer) = writer.lock() {
                let _ = write_iteration_info(&mut *writer, &position, iteration, chess960);
                let _ = writer.flush();
            }
        };
        let on_current_move = |root_move: &RootMoveInfo| {
            if let Ok(mut writer) = writer.lock() {
                let _ = write_current_move_info(&mut *writer, &position, root_move, chess960);
                let _ = writer.flush();
            }
        };
        let result = if fixed_depth {
            search_pool.search_fixed_depth_with_history_callback(
                &position,
                &position_history,
                Arc::clone(&transposition_table),
                limits,
                options,
                Arc::clone(&search_stop),
                Arc::clone(&network),
                on_iteration,
            )
        } else {
            search_pool.search_with_history_progress(
                &position,
                &position_history,
                Arc::clone(&transposition_table),
                limits,
                options,
                Arc::clone(&search_stop),
                network,
                on_iteration,
                on_current_move,
            )
        };
        while wait_for_stop && !search_stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(1));
        }
        if let Ok(mut writer) = writer.lock() {
            match result {
                Ok(result) => {
                    let _ = write_pool_search_tail(&mut *writer, &position, &result, chess960);
                }
                Err(error) => {
                    let _ = write_search_failure(&mut *writer, &position, error, chess960);
                }
            }
            let _ = writer.flush();
        }
    });
    ActiveSearch { stop, handle }
}

/// Runs the standalone `perft` subcommand arguments.
pub fn run_perft_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let mut arguments = arguments.into_iter().map(Into::into);
    let Some(depth) = arguments.next() else {
        return Err(perft_usage("missing perft depth"));
    };
    if matches!(depth.as_str(), "-h" | "--help") {
        writeln!(writer, "{}", perft_help()).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let depth = depth
        .parse::<u32>()
        .map_err(|_| perft_usage(&format!("invalid perft depth '{depth}'")))?;

    let mut chess960 = false;
    let mut fen = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--chess960" => chess960 = true,
            "--fen" => {
                fen = Some(
                    arguments
                        .next()
                        .ok_or_else(|| perft_usage("--fen requires a FEN value"))?,
                );
            }
            unknown => {
                return Err(perft_usage(&format!("unknown perft argument '{unknown}'")));
            }
        }
    }

    let mut position = match fen {
        Some(fen) => Position::from_fen(&fen, chess960)
            .map_err(|error| perft_usage(&format!("invalid FEN: {error}")))?,
        None => Position::startpos(),
    };
    write_perft(&mut writer, &mut position, depth, chess960).map_err(|error| error.to_string())
}

/// Runs the deterministic standalone search `bench` subcommand.
pub fn run_bench_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    if !arguments.is_empty() {
        return Err(bench_usage("bench does not accept arguments"));
    }

    let resolution = benchmark_network_resolution()?;
    write_bench(&mut writer, SearchOptions::default(), &resolution.network)
}

/// Runs the standalone multi-thread search scaling benchmark.
pub fn run_mtbench_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let options = parse_mtbench_arguments(arguments)?;
    let network = benchmark_network_resolution()?.network;
    let positions = BENCH_CASES
        .iter()
        .map(|fen| {
            Position::from_fen(fen, false)
                .map_err(|error| format!("invalid built-in bench FEN: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    writeln!(writer, "Threads\tDepth\tNodes\tTime (ms)\tNPS").map_err(|error| error.to_string())?;
    for thread_count in options.threads {
        let pool = SearchPool::new(thread_count).map_err(|error| {
            format!("unable to create {thread_count}-thread mtbench search pool: {error}")
        })?;
        let transposition_table = Arc::new(
            TranspositionTable::new(MTBENCH_HASH_MIB)
                .map_err(|error| format!("unable to allocate mtbench Hash: {error}"))?,
        );

        let started = Instant::now();
        let mut total_nodes = 0u64;
        for position in &positions {
            pool.clear(Arc::clone(&transposition_table))
                .map_err(|error| format!("unable to clear mtbench search state: {error}"))?;
            let history = [position.repetition_key()];
            let pooled = pool
                .search_fixed_depth_smp_with_history_callback(
                    position,
                    &history,
                    Arc::clone(&transposition_table),
                    SearchLimits {
                        depth: Some(options.depth),
                        ..SearchLimits::default()
                    },
                    SearchOptions::default(),
                    Arc::new(AtomicBool::new(false)),
                    Arc::clone(&network),
                    |_| {},
                )
                .map_err(|error| format!("mtbench search failed: {error}"))?;
            total_nodes = total_nodes
                .checked_add(pooled.result.nodes)
                .ok_or_else(|| "mtbench node count overflowed u64".to_string())?;
        }
        let elapsed = started.elapsed();
        let nps = ((u128::from(total_nodes) * 1_000_000_000) / elapsed.as_nanos().max(1)) as u64;
        writeln!(
            writer,
            "{thread_count}\t{}\t{total_nodes}\t{}\t{nps}",
            options.depth,
            elapsed.as_millis()
        )
        .map_err(|error| error.to_string())?;
    }

    Ok(())
}

/// The network the `bench` and `mtbench` subcommands measure with.
///
/// `MF_NNUE_TEST_NET` overrides it so a test can pin a specific network; otherwise this
/// is the same automatic resolution the engine plays with.
fn benchmark_network_resolution() -> Result<SharedNetworkResolution, String> {
    let Some(path) = std::env::var_os("MF_NNUE_TEST_NET") else {
        return Ok(default_network_resolution());
    };
    match resolve_network(Some(Path::new(&path))) {
        Ok(resolved) => {
            let (network, source) = resolved.into_parts();
            Ok(SharedNetworkResolution {
                network: Arc::new(network),
                source,
            })
        }
        Err(error) => Err(format!(
            "unable to resolve benchmark NNUE network from MF_NNUE_TEST_NET: {error}"
        )),
    }
}

struct MtbenchOptions {
    threads: Vec<usize>,
    depth: u32,
}

fn parse_mtbench_arguments<I, S>(arguments: I) -> Result<MtbenchOptions, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut arguments = arguments.into_iter().map(Into::into);
    let mut threads = None;
    let mut depth = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--threads" => {
                if threads.is_some() {
                    return Err(mtbench_usage("duplicate mtbench argument '--threads'"));
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| mtbench_usage("--threads requires a comma-separated list"))?;
                threads = Some(parse_mtbench_thread_list(&value)?);
            }
            "--depth" => {
                if depth.is_some() {
                    return Err(mtbench_usage("duplicate mtbench argument '--depth'"));
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| mtbench_usage("--depth requires a value"))?;
                let parsed = value
                    .parse::<u32>()
                    .map_err(|_| mtbench_usage(&format!("invalid mtbench depth '{value}'")))?;
                if parsed == 0 {
                    return Err(mtbench_usage(&format!(
                        "invalid mtbench depth '{value}': minimum is 1"
                    )));
                }
                depth = Some(parsed);
            }
            unknown => {
                return Err(mtbench_usage(&format!(
                    "unknown mtbench argument '{unknown}'"
                )));
            }
        }
    }

    Ok(MtbenchOptions {
        threads: threads.unwrap_or_else(|| MTBENCH_DEFAULT_THREADS.to_vec()),
        depth: depth.unwrap_or(MTBENCH_DEFAULT_DEPTH),
    })
}

fn parse_mtbench_thread_list(value: &str) -> Result<Vec<usize>, String> {
    let mut threads = Vec::new();
    for item in value.split(',') {
        let thread_count = item
            .parse::<usize>()
            .map_err(|_| mtbench_usage(&format!("invalid mtbench thread list '{value}'")))?;
        if !(1..=MTBENCH_MAX_THREADS).contains(&thread_count) || threads.contains(&thread_count) {
            return Err(mtbench_usage(&format!(
                "invalid mtbench thread list '{value}': values must be unique integers from 1 to \
                 {MTBENCH_MAX_THREADS}"
            )));
        }
        threads.push(thread_count);
    }
    if threads.is_empty() {
        return Err(mtbench_usage(&format!(
            "invalid mtbench thread list '{value}'"
        )));
    }
    Ok(threads)
}

fn write_bench<W: Write>(
    writer: &mut W,
    options: SearchOptions,
    network: &Network,
) -> Result<(), String> {
    let positions = BENCH_CASES
        .iter()
        .map(|fen| {
            Position::from_fen(fen, false)
                .map_err(|error| format!("invalid built-in bench FEN: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let transposition_table = TranspositionTable::new(BENCH_HASH_MIB)
        .map_err(|error| format!("unable to allocate bench Hash: {error}"))?;
    // Constructed ONCE, outside the timed region, and cleared between positions.
    // `search_with_options` builds a fresh `SharedHistory` per call, so bench used to
    // allocate and zero the whole history zoo six times inside its own timing and
    // therefore misreported the throughput cost of adding tables -- by ~3 percentage
    // points in the WRONG direction on M4-F2 (mission AGENTS.md 4.54). Match play
    // allocates once per game; this now matches it. The node signature is unchanged
    // because a cleared table is bit-identical to a fresh one.
    let shared_history = SharedHistory::new();

    // Only the searches are timed. The per-position table resets are setup, not search
    // work, and leaving them inside the measurement is what made bench NPS unusable.
    let mut elapsed = Duration::ZERO;
    let mut total = 0u64;
    for position in &positions {
        transposition_table.clear();
        shared_history.clear();
        let started = Instant::now();
        let result = search_with_shared_history(
            position,
            &transposition_table,
            SearchLimits {
                depth: Some(BENCH_DEPTH),
                ..SearchLimits::default()
            },
            options,
            &shared_history,
            network,
        );
        elapsed += started.elapsed();
        total = total
            .checked_add(result.nodes)
            .ok_or_else(|| "bench node count overflowed u64".to_string())?;
    }
    let nanos = elapsed.as_nanos().max(1);
    let nps = ((u128::from(total) * 1_000_000_000) / nanos) as u64;

    writeln!(writer, "Positions: {}", positions.len()).map_err(|error| error.to_string())?;
    writeln!(writer, "Nodes searched: {total}").map_err(|error| error.to_string())?;
    writeln!(writer, "Time (ms): {}", elapsed.as_millis()).map_err(|error| error.to_string())?;
    writeln!(writer, "NPS: {nps}").map_err(|error| error.to_string())
}

fn handle_setoption<W: Write>(
    command: &str,
    state: &mut EngineState,
    writer: &mut W,
) -> io::Result<()> {
    let tokens: Vec<_> = command.split_whitespace().collect();
    if tokens.len() < 4
        || !tokens[0].eq_ignore_ascii_case("setoption")
        || !tokens[1].eq_ignore_ascii_case("name")
    {
        return Ok(());
    }
    let Some(value_index) = tokens[2..]
        .iter()
        .position(|token| token.eq_ignore_ascii_case("value"))
        .map(|index| index + 2)
    else {
        return Ok(());
    };
    let name = tokens[2..value_index].join(" ");
    let value = tokens[value_index + 1..].join(" ");

    if name.eq_ignore_ascii_case("UCI_Chess960") {
        if let Some(enabled) = parse_check_option(&value) {
            state.chess960 = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseNMP") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_nmp = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseRFP") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_rfp = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseRazoring") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_razoring = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseLMR") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_lmr = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseLMP") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_lmp = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseFutility") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_futility = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseSEEPruning") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_see_pruning = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseQSearchTT") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_qsearch_tt = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseQSearchDeltaPruning") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_qsearch_delta_pruning = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseQSearchChecks") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_qsearch_checks = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseCaptureLMR") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_capture_lmr = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseTimeEffort") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_time_effort = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseSingularExt") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_singular_ext = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseCheckExt") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_check_ext = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseMultiCut") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_multicut = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseIIR") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_iir = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseProbCut") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_probcut = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseButterflyHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_butterfly_history = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseCaptureHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_capture_history = enabled;
        }
    } else if name.eq_ignore_ascii_case("UsePawnHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_pawn_history = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseContHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_continuation_history = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseHistoryPruning") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_history_pruning = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseCorrHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_correction_history = enabled;
        }
    } else if let Some(source) = correction_source_option(&name) {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_correction_sources[source] = enabled;
        }
    } else if name.eq_ignore_ascii_case("EvalFile") {
        handle_eval_file(&value, state, writer)?;
    } else if name.eq_ignore_ascii_case("Hash") {
        let Ok(requested) = value.parse::<i128>() else {
            writeln!(writer, "info string invalid Hash value '{value}'")?;
            return Ok(());
        };
        if requested < MIN_HASH_MIB {
            writeln!(
                writer,
                "info string invalid Hash value '{value}': minimum is {MIN_HASH_MIB}"
            )?;
            return Ok(());
        }

        resize_hash(requested, max_hash_mebibytes(), state, writer)?;
    } else if name.eq_ignore_ascii_case("Threads") {
        let Ok(requested) = value.parse::<i128>() else {
            writeln!(writer, "info string invalid Threads value '{value}'")?;
            return Ok(());
        };
        let thread_count = requested.clamp(1, 256) as usize;
        match SearchPool::new(thread_count) {
            Ok(pool) => {
                state.search_pool = Arc::new(pool);
                writeln!(writer, "info string threads set to {thread_count}")?;
            }
            Err(error) => {
                writeln!(
                    writer,
                    "info string unable to create {thread_count}-thread search pool: {error}"
                )?;
            }
        }
    }
    Ok(())
}

/// Resizes the transposition table, clamping a request past `maximum` instead of
/// refusing it.
///
/// Refusal left the engine searching with the table it already had -- in a fresh session
/// the 16 MB default -- while the GUI believed it had configured gigabytes. The only
/// notice was an `info string` that no GUI surfaces, so the visible symptom was a
/// `hashfull` that saturated within the first second of every search and an engine that
/// played like it had no memory. Clamping keeps the size the engine reports and the size
/// it actually searches with the same number.
///
/// `maximum` is a parameter rather than a direct call to [`max_hash_mebibytes`] so the
/// clamp can be tested at a size that does not require the machine's memory to exercise.
fn resize_hash<W: Write>(
    requested_mib: i128,
    maximum_mib: usize,
    state: &mut EngineState,
    writer: &mut W,
) -> io::Result<()> {
    let clamped = requested_mib.min(maximum_mib as i128) as usize;
    if requested_mib > maximum_mib as i128 {
        writeln!(
            writer,
            "info string Hash {requested_mib} MB exceeds the maximum of {maximum_mib} MB; using {clamped} MB"
        )?;
    }
    match TranspositionTable::new(clamped) {
        Ok(table) => {
            state.transposition_table = Arc::new(table);
            writeln!(writer, "info string hash resized to {clamped} MB")?;
        }
        Err(error) => {
            writeln!(
                writer,
                "info string unable to allocate Hash {clamped} MB: {error}"
            )?;
        }
    }
    Ok(())
}

fn clear_eval_dependent_search_state<W: Write>(
    state: &EngineState,
    writer: &mut W,
    option: &str,
) -> io::Result<()> {
    if let Err(error) = state
        .search_pool
        .clear(Arc::clone(&state.transposition_table))
    {
        writeln!(
            writer,
            "info string unable to clear search state after {option}: {error}"
        )?;
    }
    Ok(())
}

fn handle_eval_file<W: Write>(
    value: &str,
    state: &mut EngineState,
    writer: &mut W,
) -> io::Result<()> {
    let automatic = value.is_empty() || value.eq_ignore_ascii_case("<empty>");
    if automatic {
        let resolution = default_network_resolution();
        state.network = resolution.network;
        state.network_source = resolution.source;
        clear_eval_dependent_search_state(state, writer, "EvalFile")?;
        return write_network_selection(writer, state, "automatic resolution");
    }

    // An explicit path that fails to load leaves the previous network in place: the
    // engine keeps playing at full strength with the net it already had, and says so.
    match resolve_network(Some(Path::new(value))) {
        Ok(resolved) => {
            let (network, source) = resolved.into_parts();
            state.network = Arc::new(network);
            state.network_source = source;
            clear_eval_dependent_search_state(state, writer, "EvalFile")?;
            write_network_selection(writer, state, "EvalFile")
        }
        Err(error) => writeln!(
            writer,
            "info string unable to load EvalFile {error}; keeping {}",
            state.network_source
        ),
    }
}

fn write_network_selection<W: Write>(
    writer: &mut W,
    state: &EngineState,
    selection: &str,
) -> io::Result<()> {
    let network = &state.network;
    let source = &state.network_source;
    let mode = production_forward_mode();
    writeln!(
        writer,
        "info string {selection} loaded from {source}; network \"{}\"; backend {:?}; sparse FC0 {}",
        network.description(),
        mode.backend(),
        mode.sparse_fc0()
    )
}

/// Maps a per-variant correction-history option name to its `use_correction_sources` index.
///
/// The trailing slot is continuation correction history, which is a differently-shaped
/// table and so is not one of the hash-keyed `CORRECTION_*` sources.
fn correction_source_option(name: &str) -> Option<usize> {
    const OPTIONS: [(&str, usize); 5] = [
        ("UseCorrHistPawn", mf_search::CORRECTION_PAWN),
        ("UseCorrHistMinor", mf_search::CORRECTION_MINOR),
        ("UseCorrHistMajor", mf_search::CORRECTION_MAJOR),
        ("UseCorrHistMaterial", mf_search::CORRECTION_MATERIAL),
        ("UseCorrHistCont", mf_search::CORRECTION_SOURCES),
    ];
    OPTIONS
        .iter()
        .find(|(option, _)| name.eq_ignore_ascii_case(option))
        .map(|(_, source)| *source)
}

fn parse_check_option(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn handle_position(command: &str, state: &mut EngineState) -> Result<(), String> {
    let mut tokens = command.split_whitespace();
    if !tokens
        .next()
        .is_some_and(|token| token.eq_ignore_ascii_case("position"))
    {
        return Err("missing position command".to_string());
    }
    let kind = tokens
        .next()
        .ok_or_else(|| "missing position type".to_string())?;

    // Set when the FEN branch has already consumed the `moves` separator while
    // scanning for the end of the variable-length FEN.
    let mut moves_follow = false;
    let mut position = if kind.eq_ignore_ascii_case("startpos") {
        Position::startpos()
    } else if kind.eq_ignore_ascii_case("fen") {
        // GUIs routinely omit the trailing fields: the counters a four- or five-field
        // FEN leaves out, and the en-passant dash along with them when a position-setup
        // dialog leaves every optional field blank (three fields). Take every token up
        // to the `moves` separator rather than a fixed six, then pad what is missing;
        // the en-passant field is advisory (see parse_en_passant), so a padded "-"
        // drops nothing.
        let mut fields = Vec::new();
        for token in tokens.by_ref() {
            if token.eq_ignore_ascii_case("moves") {
                moves_follow = true;
                break;
            }
            fields.push(token);
        }
        match fields.len() {
            2 => fields.extend(["-", "-", "0", "1"]),
            3 => fields.extend(["-", "0", "1"]),
            4 => fields.extend(["0", "1"]),
            5 => fields.push("1"),
            6 => {}
            count => return Err(format!("FEN requires two to six fields, found {count}")),
        }
        Position::from_fen(&fields.join(" "), state.chess960).map_err(|error| error.to_string())?
    } else {
        return Err(format!("unknown position type '{kind}'"));
    };

    if !moves_follow {
        match tokens.next() {
            None => moves_follow = false,
            Some(separator) if separator.eq_ignore_ascii_case("moves") => moves_follow = true,
            Some(separator) => {
                return Err(format!("unexpected position argument '{separator}'"));
            }
        }
    }

    let mut position_history = vec![position.repetition_key()];
    let mut rejected = None;
    if moves_follow {
        for notation in tokens {
            let Some(mv) = parse_uci_move(&position, notation, state.chess960) else {
                rejected = Some(format!("illegal move '{notation}'"));
                break;
            };
            position.make_move(mv);
            position_history.push(position.repetition_key());
        }
    }

    // Keep whatever prefix did parse. Abandoning the command entirely would leave the
    // engine on the *previous* position, so it would analyse a board the GUI is not
    // showing and answer with a move that is illegal there -- a far worse failure than
    // analysing this one a few plies early.
    state.position = position;
    state.position_history = position_history;
    match rejected {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

enum GoRequest {
    Perft(u32),
    Search(GoParameters, Vec<String>),
}

impl GoRequest {
    fn parse(tokens: &[&str]) -> Option<Self> {
        if tokens
            .first()
            .is_none_or(|token| !token.eq_ignore_ascii_case("go"))
        {
            return None;
        }
        if tokens.len() >= 3
            && tokens[1].eq_ignore_ascii_case("perft")
            && let Ok(depth) = tokens[2].parse()
        {
            return Some(Self::Perft(depth));
        }
        let (parameters, ignored) = GoParameters::parse(&tokens[1..])?;
        Some(Self::Search(parameters, ignored))
    }
}

/// Whether a token starts a new `go` argument, used to find the end of `searchmoves`.
fn is_go_keyword(token: &str) -> bool {
    const KEYWORDS: [&str; 10] = [
        "searchmoves",
        "ponder",
        "wtime",
        "btime",
        "winc",
        "binc",
        "movestogo",
        "depth",
        "nodes",
        "movetime",
    ];
    token.eq_ignore_ascii_case("infinite")
        || token.eq_ignore_ascii_case("mate")
        || KEYWORDS
            .iter()
            .any(|keyword| token.eq_ignore_ascii_case(keyword))
}

#[derive(Default)]
struct GoParameters {
    depth: Option<u32>,
    nodes: Option<u64>,
    movetime: Option<u64>,
    wtime: Option<u64>,
    btime: Option<u64>,
    winc: Option<u64>,
    binc: Option<u64>,
    movestogo: Option<u64>,
    infinite: bool,
}

/// Reads a non-negative millisecond or count value, clamping a negative one to zero.
///
/// GUIs send a negative clock when the flag has fallen (`go wtime -134 ...` from Arena,
/// CuteChess, and Banksia). Rejecting it used to abandon the whole `go`, so the engine
/// went mute in exactly the time pressure where a move matters most. Zero is the honest
/// reading and already produces an immediate move.
fn parse_go_value(value: &str) -> Option<u64> {
    value
        .parse::<i64>()
        .ok()
        .map(|value| u64::try_from(value).unwrap_or(0))
}

impl GoParameters {
    /// Parses `go` arguments, failing only when *nothing* in them was recognized.
    ///
    /// UCI requires a `bestmove` for every `go`, and the spec directs engines to ignore
    /// unexpected tokens rather than reject the command, so a recognized argument
    /// alongside an unrecognized one still searches: dropping the command silently
    /// hangs a GUI forever waiting for a reply that never comes.
    ///
    /// The one exception is a `go` whose arguments are *entirely* unrecognized, such as
    /// `go banana`. That is a malformed command rather than a dialect this engine does
    /// not speak, and treating it as an analysis request would let a typo silently
    /// abandon a running search. Bare `go` stays valid -- it has no arguments to fail.
    fn parse(tokens: &[&str]) -> Option<(Self, Vec<String>)> {
        let mut parameters = Self::default();
        let mut ignored = Vec::new();
        let mut recognized = false;
        let mut index = 0;
        while index < tokens.len() {
            let key = tokens[index];
            index += 1;
            if key.eq_ignore_ascii_case("infinite") {
                parameters.infinite = true;
                recognized = true;
                continue;
            }
            // `ponder` takes no value. Treat it as an ordinary search rather than
            // dropping the command: without real pondering the engine still owes the
            // GUI a `bestmove`, and a GUI that sent `go ponder` waits forever without
            // one.
            if key.eq_ignore_ascii_case("ponder") {
                recognized = true;
                continue;
            }
            // `searchmoves` is a trailing list of moves, not a key/value pair. The
            // search has no root-move restriction, so consume the list and search
            // normally; answering the wrong move set still beats never answering.
            if key.eq_ignore_ascii_case("searchmoves") {
                while index < tokens.len() && !is_go_keyword(tokens[index]) {
                    index += 1;
                }
                recognized = true;
                continue;
            }
            // `mate N` asks for a mate search this engine does not implement. Consume
            // the value and fall through to the unbounded-analysis default below.
            if key.eq_ignore_ascii_case("mate") {
                if index < tokens.len() && parse_go_value(tokens[index]).is_some() {
                    index += 1;
                }
                ignored.push(key.to_string());
                recognized = true;
                continue;
            }

            let Some(value) = tokens.get(index).copied() else {
                ignored.push(key.to_string());
                break;
            };

            let slot: Option<&mut Option<u64>> = if key.eq_ignore_ascii_case("nodes") {
                Some(&mut parameters.nodes)
            } else if key.eq_ignore_ascii_case("movetime") {
                Some(&mut parameters.movetime)
            } else if key.eq_ignore_ascii_case("wtime") {
                Some(&mut parameters.wtime)
            } else if key.eq_ignore_ascii_case("btime") {
                Some(&mut parameters.btime)
            } else if key.eq_ignore_ascii_case("winc") {
                Some(&mut parameters.winc)
            } else if key.eq_ignore_ascii_case("binc") {
                Some(&mut parameters.binc)
            } else if key.eq_ignore_ascii_case("movestogo") {
                Some(&mut parameters.movestogo)
            } else {
                None
            };

            if let Some(slot) = slot {
                index += 1;
                recognized = true;
                match parse_go_value(value) {
                    Some(parsed) => *slot = Some(parsed),
                    None => ignored.push(format!("{key} {value}")),
                }
            } else if key.eq_ignore_ascii_case("depth") {
                index += 1;
                recognized = true;
                match parse_go_value(value) {
                    Some(parsed) => {
                        parameters.depth = Some(parsed.min(u64::from(u32::MAX)) as u32);
                    }
                    None => ignored.push(format!("{key} {value}")),
                }
            } else {
                // An unrecognized key. Skip a numeric follower too, on the assumption it
                // was that key's value rather than the next argument.
                ignored.push(key.to_string());
                if parse_go_value(value).is_some() {
                    index += 1;
                }
            }
        }

        // A `go` carrying no budget at all -- bare `go`, or one whose only arguments
        // were `ponder`/`searchmoves`/`mate` -- is an unbounded analysis request. UCI
        // requires a `bestmove` for every `go`, so treat it as infinite and let `stop`
        // end it rather than silently ignoring the command and hanging the GUI.
        if parameters.depth.is_none()
            && parameters.nodes.is_none()
            && parameters.movetime.is_none()
            && parameters.wtime.is_none()
            && parameters.btime.is_none()
        {
            parameters.infinite = true;
        }
        // Arguments were given but none of them meant anything: a malformed command,
        // not a dialect gap.
        if !tokens.is_empty() && !recognized {
            return None;
        }
        Some((parameters, ignored))
    }

    fn search_limits(&self, position: &Position) -> SearchLimits {
        let (soft_time, hard_time) =
            if self.infinite || self.depth.is_some() || self.nodes.is_some() {
                (None, None)
            } else if let Some(millis) = self.movetime {
                (
                    Some(Duration::from_millis(millis)),
                    Some(Duration::from_millis(millis)),
                )
            } else {
                self.clock_limits(position)
            };
        SearchLimits {
            depth: if self.infinite { None } else { self.depth },
            nodes: if self.infinite { None } else { self.nodes },
            soft_time,
            hard_time,
            infinite: self.infinite,
        }
    }

    fn clock_limits(&self, position: &Position) -> (Option<Duration>, Option<Duration>) {
        let white = position.side_to_move() == mf_core::Color::White;
        let remaining = if white { self.wtime } else { self.btime };
        let Some(remaining) = remaining else {
            return (None, None);
        };
        let increment = if white {
            self.winc.unwrap_or(0)
        } else {
            self.binc.unwrap_or(0)
        };
        let moves = self
            .movestogo
            .unwrap_or(DEFAULT_MOVES_TO_GO)
            .clamp(1, MAX_MOVES_TO_GO);
        // A safety reserve is withheld from the clock BEFORE the per-move share is
        // taken, so the reserve survives being divided by `movestogo` and is still there
        // on the last move of the control.
        let safety = remaining.saturating_mul(CLOCK_SAFETY_PERCENT) / 100;
        let available = remaining
            .saturating_sub(TIME_OVERHEAD_MILLIS)
            .saturating_sub(safety)
            .max(1);
        let soft = (available / moves)
            .saturating_add(increment.saturating_mul(INCREMENT_FRACTION_PERCENT) / 100)
            .max(1)
            .min(available);
        // The hard limit is what lets one critical move borrow from the moves after it.
        // It is bounded twice: by a multiple of the soft limit, so a single move cannot
        // run away with the game, and by a fraction of the clock, so the borrowing can
        // never approach flag fall.
        let hard = soft
            .saturating_mul(HARD_LIMIT_SOFT_MULTIPLE)
            .min(remaining.saturating_mul(HARD_LIMIT_CLOCK_PERCENT) / 100)
            .clamp(1, available);
        (
            Some(Duration::from_millis(soft.min(hard))),
            Some(Duration::from_millis(hard)),
        )
    }
}

fn write_pool_search_tail<W: Write>(
    writer: &mut W,
    position: &Position,
    pooled: &PoolSearchResult,
    chess960: bool,
) -> io::Result<()> {
    if pooled.selected_worker == 0 {
        write_search_summary(writer, position, &pooled.result, chess960)?;
    } else {
        write_selected_result_info(writer, position, &pooled.result, chess960)?;
    }
    write_bestmove(writer, position, pooled.result.best_move, chess960)
}

fn write_search_summary<W: Write>(
    writer: &mut W,
    position: &Position,
    result: &SearchResult,
    chess960: bool,
) -> io::Result<()> {
    if result.iterations.is_empty() {
        let score = format_score(result.score);
        let pv = format_pv(position, &result.pv, chess960);
        writeln!(
            writer,
            "info depth {} seldepth {} multipv 1 score {} nodes {} nps 0 hashfull {} time {} pv {}",
            result.depth,
            result.seldepth,
            score,
            result.nodes,
            result.hashfull,
            result.elapsed.as_millis(),
            pv
        )?;
    } else if result
        .iterations
        .last()
        .is_some_and(|iteration| iteration.nodes != result.nodes)
    {
        let elapsed_millis = result.elapsed.as_millis() as u64;
        let nps = result
            .nodes
            .saturating_mul(1_000)
            .checked_div(elapsed_millis.max(1))
            .unwrap_or(0);
        writeln!(
            writer,
            "info nodes {} nps {} time {}",
            result.nodes, nps, elapsed_millis
        )?;
    }

    Ok(())
}

fn write_selected_result_info<W: Write>(
    writer: &mut W,
    position: &Position,
    result: &SearchResult,
    chess960: bool,
) -> io::Result<()> {
    let elapsed_millis = result.elapsed.as_millis() as u64;
    let nps = result
        .nodes
        .saturating_mul(1_000)
        .checked_div(elapsed_millis.max(1))
        .unwrap_or(0);
    let score = format_score(result.score);
    let pv = format_pv(position, &result.pv, chess960);
    // `depth`/`seldepth` must be present. This line carries the engine's final score and
    // PV whenever a helper thread wins the race, and it is frequently a different score
    // than the last line worker 0 printed. GUIs index their analysis display by depth and
    // drop an `info` line that has none, so omitting it made the true final answer
    // invisible -- the GUI kept showing a superseded score from the previous line.
    writeln!(
        writer,
        "info depth {} seldepth {} multipv 1 score {} nodes {} nps {} hashfull {} time {} pv {}",
        result.depth,
        result.seldepth,
        score,
        result.nodes,
        nps,
        result.hashfull,
        elapsed_millis,
        pv
    )
}

fn write_search_failure<W: Write>(
    writer: &mut W,
    position: &Position,
    error: PoolError,
    chess960: bool,
) -> io::Result<()> {
    writeln!(writer, "info string search failed: {error}")?;
    let fallback = generate_legal_moves(position).first().copied();
    write_bestmove(writer, position, fallback, chess960)
}

fn write_bestmove<W: Write>(
    writer: &mut W,
    position: &Position,
    best_move: Option<mf_core::Move>,
    chess960: bool,
) -> io::Result<()> {
    let bestmove = best_move
        .map(|mv| format_uci_move(position, mv, chess960))
        // UCI represents "no legal move" with the null-move token. `0000`
        // is accepted by strict GUIs that reject the older `(none)` spelling.
        .unwrap_or_else(|| NULL_BESTMOVE.to_string());
    writeln!(writer, "bestmove {bestmove}")
}

/// Writes the UCI `currmove` progress line.
///
/// Deliberately carries only `depth`, `currmove` and `currmovenumber`. A GUI treats a line
/// with `currmove` as a transient status ("searching Nf3, 2/34") rather than an analysis
/// row, so adding a score or PV here would push a half-finished evaluation into the
/// analysis display. The reference engine sends the same three fields for the same reason.
fn write_current_move_info<W: Write>(
    writer: &mut W,
    position: &Position,
    root_move: &RootMoveInfo,
    chess960: bool,
) -> io::Result<()> {
    writeln!(
        writer,
        "info depth {} currmove {} currmovenumber {}",
        root_move.depth,
        format_uci_move(position, root_move.best_move, chess960),
        root_move.move_number
    )
}

fn write_iteration_info<W: Write>(
    writer: &mut W,
    position: &Position,
    iteration: &IterationInfo,
    chess960: bool,
) -> io::Result<()> {
    let elapsed_millis = iteration.elapsed.as_millis() as u64;
    let nps = iteration
        .nodes
        .saturating_mul(1_000)
        .checked_div(elapsed_millis.max(1))
        .unwrap_or(0);
    let score = format_score(iteration.score);
    let pv = format_pv(position, &iteration.pv, chess960);
    writeln!(
        writer,
        // `multipv 1` is constant because the engine searches a single PV. It is emitted
        // anyway: it is the field GUIs read to decide which analysis row a line belongs
        // to, and some hide lines that lack it. Stockfish emits it unconditionally in
        // this slot, between `seldepth` and `score`.
        "info depth {} seldepth {} multipv 1 score {} nodes {} nps {} hashfull {} time {} pv {}",
        iteration.depth,
        iteration.seldepth,
        score,
        iteration.nodes,
        nps,
        iteration.hashfull,
        elapsed_millis,
        pv
    )
}

fn format_score(score: i32) -> String {
    score_to_uci_mate(score).map_or_else(
        || format!("cp {}", clamp_centipawn_score(score)),
        |moves| format!("mate {moves}"),
    )
}

fn format_pv(position: &Position, pv: &[mf_core::Move], chess960: bool) -> String {
    let mut replay = position.clone();
    let mut notation = Vec::with_capacity(pv.len());
    for &mv in pv {
        notation.push(format_uci_move(&replay, mv, chess960));
        replay.make_move(mv);
    }
    notation.join(" ")
}

fn write_perft<W: Write>(
    writer: &mut W,
    position: &mut Position,
    depth: u32,
    chess960: bool,
) -> io::Result<()> {
    if depth == 0 {
        writeln!(writer, "Nodes searched: 1")?;
        return Ok(());
    }

    let mut rows: Vec<_> = perft_divide(position, depth)
        .into_iter()
        .map(|(mv, nodes)| (format_uci_move(position, mv, chess960), nodes))
        .collect();
    rows.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let total = rows.iter().map(|(_, nodes)| *nodes).sum::<u64>();
    for (mv, nodes) in rows {
        writeln!(writer, "{mv}: {nodes}")?;
    }
    writeln!(writer, "Nodes searched: {total}")
}

fn perft_usage(message: &str) -> String {
    format!("{message}\n\n{}", perft_help())
}

fn bench_usage(message: &str) -> String {
    format!("{message}\n\nUsage: manifold bench")
}

fn mtbench_usage(message: &str) -> String {
    format!("{message}\n\nUsage: manifold mtbench [--threads 1,2,4,8] [--depth N]")
}

fn perft_help() -> &'static str {
    "Usage: manifold perft <depth> [--fen <FEN>] [--chess960]\n\
     \n\
     Examples:\n\
       manifold perft 6\n\
       manifold perft 5 --fen \"r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1\"\n\
       manifold perft 4 --chess960 --fen \"rk6/8/8/8/8/8/8/RK6 w Aa - 0 1\""
}

#[cfg(test)]
mod tests {
    use mf_core::generate_legal_moves;
    use mf_search::{Bound, EntryData, PoolSearchResult};

    use super::*;

    #[test]
    fn ucinewgame_clears_the_transposition_table_without_changing_its_size() {
        let mut state = EngineState {
            search_pool: Arc::new(
                SearchPool::new(4).expect("four test search workers should start"),
            ),
            ..EngineState::default()
        };
        let key = state.position.zobrist().main();
        let allocated_bytes = state.transposition_table.allocated_bytes();
        let data = EntryData {
            best_move: generate_legal_moves(&state.position).first().copied(),
            score: 31,
            static_eval: 18,
            depth: 12,
            bound: Bound::Exact,
            age: 3,
            pv: true,
        };
        state.transposition_table.store(key, data);
        assert_eq!(state.transposition_table.probe(key), Some(data));

        state
            .new_game()
            .expect("the default search pool should clear the table");

        assert_eq!(state.transposition_table.probe(key), None);
        assert_eq!(state.transposition_table.allocated_bytes(), allocated_bytes);
        assert_eq!(state.search_pool.thread_count(), 4);
    }

    #[test]
    fn helper_selected_terminal_line_reports_depth_before_bestmove() {
        let position = Position::startpos();
        let best_move = generate_legal_moves(&position)[0];
        let pooled = PoolSearchResult {
            result: SearchResult {
                best_move: Some(best_move),
                score: 42,
                depth: 5,
                seldepth: 8,
                nodes: 1_234,
                hashfull: 17,
                elapsed: Duration::from_millis(20),
                pv: vec![best_move],
                iterations: Vec::new(),
            },
            selected_worker: 2,
        };
        let mut output = Vec::new();

        write_pool_search_tail(&mut output, &position, &pooled, false)
            .expect("pool search tail should be writable");

        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!(
                "info depth 5 seldepth 8 multipv 1 score cp 42 nodes 1234 nps 61700 hashfull 17 time 20 pv {}",
                format_uci_move(&position, best_move, false)
            )
        );
        // A GUI indexes analysis output by depth and discards an `info` line without one,
        // so this line -- which carries the final score when a helper wins -- must have it.
        assert!(lines[0].split_whitespace().any(|token| token == "depth"));
        assert_eq!(
            lines[1],
            format!("bestmove {}", format_uci_move(&position, best_move, false))
        );
    }

    #[test]
    fn pool_failure_emits_error_and_deterministic_legal_fallback() {
        let position = Position::startpos();
        let expected = generate_legal_moves(&position)[0];
        let mut output = Vec::new();

        write_search_failure(&mut output, &position, PoolError::Busy, false)
            .expect("search failure should be writable");

        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                "info string search failed: search pool is already active".to_string(),
                format!("bestmove {}", format_uci_move(&position, expected, false)),
            ]
        );
    }

    #[test]
    fn invalid_threads_resize_preserves_the_existing_pool() {
        let mut state = EngineState::default();
        let mut output = Vec::new();
        handle_setoption("setoption name Threads value 4", &mut state, &mut output)
            .expect("valid Threads resize should be writable");
        assert_eq!(state.search_pool.thread_count(), 4);

        handle_setoption(
            "setoption name Threads value banana",
            &mut state,
            &mut output,
        )
        .expect("invalid Threads diagnostic should be writable");

        assert_eq!(state.search_pool.thread_count(), 4);
        assert!(
            String::from_utf8(output)
                .expect("protocol output should be UTF-8")
                .contains("info string invalid Threads value 'banana'")
        );
    }

    /// An oversize request resizes to the advertised maximum and says so.
    ///
    /// The old behaviour kept the previous table -- in a fresh session the 16 MB default
    /// -- and reported only an `info string`. A GUI that offers the advertised range then
    /// believes it configured gigabytes while the engine thrashed a table small enough to
    /// saturate in the first second of every search. Clamping is honest about the number
    /// the engine actually got, and the table it leaves behind is the one it named.
    #[test]
    fn an_oversize_hash_request_clamps_to_the_maximum() {
        let mut state = EngineState::default();
        let mut output = Vec::new();

        resize_hash(9_000, 8, &mut state, &mut output)
            .expect("setoption output should be writable");

        assert_eq!(
            state.transposition_table.allocated_bytes(),
            8 * 1024 * 1024,
            "an oversize request must leave the clamped table behind, not the old one"
        );
        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        assert!(
            output.contains("info string Hash 9000 MB exceeds the maximum of 8 MB; using 8 MB"),
            "the clamp must be announced explicitly: {output}"
        );
        assert!(
            output.contains("info string hash resized to 8 MB"),
            "the resulting size must be reported: {output}"
        );
        assert!(
            !output.contains("unable to allocate"),
            "a clamped request is not a failure: {output}"
        );
    }

    #[test]
    fn a_hash_request_within_the_maximum_is_honoured_without_a_clamp_notice() {
        let mut state = EngineState::default();
        let mut output = Vec::new();

        resize_hash(4, 8, &mut state, &mut output).expect("setoption output should be writable");

        assert_eq!(state.transposition_table.allocated_bytes(), 4 * 1024 * 1024);
        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        assert_eq!(output.trim_end(), "info string hash resized to 4 MB");
    }

    /// The advertised maximum must be the one the resize path enforces.
    ///
    /// The engine used to advertise `max 1048576` and refuse everything over 4096, which
    /// is the whole defect: the two numbers have to come from the same place.
    #[test]
    fn the_advertised_hash_maximum_is_the_one_the_engine_enforces() {
        let maximum = max_hash_mebibytes();
        assert_eq!(
            hash_option_line(),
            format!(
                "option name Hash type spin default {DEFAULT_HASH_MIB} min {MIN_HASH_MIB} max {maximum}"
            )
        );
        assert!(
            TranspositionTable::new(maximum + 1).is_err(),
            "one MB past the advertised maximum is the first refused size"
        );
    }

    #[test]
    fn successful_hash_resize_replaces_the_table_and_starts_empty() {
        let mut state = EngineState::default();
        let key = state.position.zobrist().main();
        state.transposition_table.store(
            key,
            EntryData {
                best_move: None,
                score: 1,
                static_eval: 2,
                depth: 3,
                bound: Bound::Lower,
                age: 4,
                pv: false,
            },
        );
        let mut output = Vec::new();

        handle_setoption("setoption name Hash value 3", &mut state, &mut output)
            .expect("setoption output should be writable");

        assert_eq!(state.transposition_table.allocated_bytes(), 3 * 1024 * 1024);
        assert_eq!(state.transposition_table.probe(key), None);
    }

    #[test]
    fn selectivity_options_parse_case_insensitively_and_survive_new_game() {
        let mut state = EngineState::default();
        let mut output = Vec::new();

        handle_setoption("SeToPtIoN NaMe uSeNmP VaLuE FaLsE", &mut state, &mut output)
            .expect("setoption output should be writable");
        handle_setoption("setoption name UseRFP value FALSE", &mut state, &mut output)
            .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseRazoring value false",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption("setoption name UseLMR value false", &mut state, &mut output)
            .expect("setoption output should be writable");
        handle_setoption("setoption name UseLMP value FALSE", &mut state, &mut output)
            .expect("setoption output should be writable");
        handle_setoption(
            "SeToPtIoN NaMe UsEfUtIlItY VaLuE FaLsE",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseSEEPruning value false",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseSingularExt value FALSE",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseCheckExt value false",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseMultiCut value false",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption("SeToPtIoN NaMe UsEiIr VaLuE FaLsE", &mut state, &mut output)
            .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseProbCut value false",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "SeToPtIoN NaMe UsEbUtTeRfLyHiStOrY VaLuE FaLsE",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseCaptureHistory value FALSE",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        // Pawn history defaults to false, so setting it false would prove nothing
        // about parsing. Set it TRUE and assert it flipped.
        handle_setoption(
            "setoption name UsePawnHistory value TRUE",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        // History pruning defaults to false, so setting it false would prove nothing
        // about parsing. Set it TRUE and assert it flipped.
        handle_setoption(
            "setoption name UseHistoryPruning value true",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");

        state
            .new_game()
            .expect("the default search pool should clear the table");

        assert!(!state.search_options.use_nmp);
        assert!(!state.search_options.use_rfp);
        assert!(!state.search_options.use_razoring);
        assert!(!state.search_options.use_lmr);
        assert!(!state.search_options.use_lmp);
        assert!(!state.search_options.use_futility);
        assert!(!state.search_options.use_see_pruning);
        assert!(!state.search_options.use_singular_ext);
        assert!(!state.search_options.use_check_ext);
        assert!(!state.search_options.use_multicut);
        assert!(!state.search_options.use_iir);
        assert!(!state.search_options.use_probcut);
        assert!(!state.search_options.use_butterfly_history);
        assert!(!state.search_options.use_capture_history);
        assert!(state.search_options.use_pawn_history);
        assert!(state.search_options.use_history_pruning);
    }

    #[test]
    fn the_evaluator_diagnostic_always_reports_nnue_and_its_source() {
        let state = EngineState::default();

        let diagnostic = active_evaluator_diagnostic(&state);

        assert!(
            diagnostic.starts_with("info string evaluation NNUE from "),
            "the engine has no non-NNUE evaluator to report: {diagnostic}"
        );
        assert!(diagnostic.contains("backend"));
    }

    #[test]
    fn bad_explicit_eval_file_preserves_the_previous_network_arc() {
        let valid =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let mut state = EngineState::default();
        let mut output = Vec::new();
        handle_setoption(
            &format!("setoption name EvalFile value {}", valid.display()),
            &mut state,
            &mut output,
        )
        .expect("valid setup EvalFile should load");
        let original = Arc::clone(&state.network);
        let missing = std::env::temp_dir().join(format!(
            "manifold-missing-eval-file-{}.nnue",
            std::process::id()
        ));
        output.clear();

        handle_setoption(
            &format!("setoption name EvalFile value {}", missing.display()),
            &mut state,
            &mut output,
        )
        .expect("EvalFile failure should be writable");

        assert!(
            Arc::ptr_eq(&state.network, &original),
            "a failed replacement must leave the engine playing with the network it had"
        );
        assert!(
            String::from_utf8(output)
                .expect("protocol output should be UTF-8")
                .starts_with("info string unable to load EvalFile")
        );
    }

    #[test]
    fn good_explicit_eval_file_loads_and_reports_source_description_and_backend() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let mut state = EngineState::default();
        let mut output = Vec::new();

        handle_setoption(
            &format!("setoption name EvalFile value {}", path.display()),
            &mut state,
            &mut output,
        )
        .expect("EvalFile success should be writable");

        assert_eq!(
            state.network_source,
            mf_nnue::NetworkSource::Explicit(path.clone())
        );
        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        assert!(output.starts_with("info string"));
        assert!(output.contains(&path.display().to_string()));
        assert!(output.contains(state.network.description()));
        assert!(output.contains("backend"));
    }

    #[test]
    fn changing_eval_file_clears_eval_dependent_search_state() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let mut state = EngineState::default();
        let key = state.position.zobrist().main();
        state.transposition_table.store(
            key,
            EntryData {
                best_move: None,
                score: 11,
                static_eval: 22,
                depth: 3,
                bound: Bound::Exact,
                age: 0,
                pv: false,
            },
        );
        let mut output = Vec::new();

        handle_setoption(
            &format!("setoption name EvalFile value {}", path.display()),
            &mut state,
            &mut output,
        )
        .expect("EvalFile change should be writable");

        assert_eq!(state.transposition_table.probe(key), None);
    }

    #[test]
    fn empty_marker_eval_file_value_returns_to_automatic_resolution() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let mut state = EngineState::default();
        let mut output = Vec::new();
        handle_setoption(
            &format!("setoption name EvalFile value {}", path.display()),
            &mut state,
            &mut output,
        )
        .expect("explicit EvalFile should load");
        assert!(matches!(
            state.network_source,
            mf_nnue::NetworkSource::Explicit(_)
        ));

        output.clear();
        handle_setoption(
            "setoption name EvalFile value <empty>",
            &mut state,
            &mut output,
        )
        .expect("automatic EvalFile reset should be writable");

        // Automatic resolution always yields a network, so the observable effect of the
        // reset is that the source is no longer the explicit one.
        assert!(!matches!(
            state.network_source,
            mf_nnue::NetworkSource::Explicit(_)
        ));
    }

    #[test]
    fn empty_eval_file_value_returns_to_automatic_resolution() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let mut state = EngineState::default();
        let mut output = Vec::new();
        handle_setoption(
            &format!("setoption name EvalFile value {}", path.display()),
            &mut state,
            &mut output,
        )
        .expect("explicit EvalFile should load");
        assert!(matches!(
            state.network_source,
            mf_nnue::NetworkSource::Explicit(_)
        ));

        output.clear();
        handle_setoption("setoption name EvalFile value", &mut state, &mut output)
            .expect("empty automatic EvalFile reset should be writable");

        assert!(!matches!(
            state.network_source,
            mf_nnue::NetworkSource::Explicit(_)
        ));
    }

    #[test]
    fn existing_invalid_eval_file_reports_a_strict_format_error() {
        let path = std::env::temp_dir().join(format!(
            "manifold-invalid-eval-file-{}.nnue",
            std::process::id()
        ));
        std::fs::write(&path, b"invalid NNUE fixture")
            .expect("invalid EvalFile fixture should be written");
        let mut state = EngineState::default();
        let mut output = Vec::new();

        handle_setoption(
            &format!("setoption name EvalFile value {}", path.display()),
            &mut state,
            &mut output,
        )
        .expect("strict EvalFile error should be writable");

        let _ = std::fs::remove_file(&path);
        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        assert!(output.starts_with("info string unable to load EvalFile"));
        assert!(output.contains("unexpected NNUE version"));
    }

    /// The bench node count is the repository's change-detection signature, so it has to
    /// be a pure function of the network and the search options -- never of anything
    /// left over from a previous run.
    #[test]
    fn bench_node_count_is_deterministic_for_a_given_network() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let network = Network::load(&path)
            .unwrap_or_else(|error| panic!("test NNUE network {}: {error}", path.display()));
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        write_bench(&mut first_output, SearchOptions::default(), &network)
            .expect("first bench should complete");
        write_bench(&mut second_output, SearchOptions::default(), &network)
            .expect("second bench should complete");

        let nodes = |output: Vec<u8>| {
            String::from_utf8(output)
                .expect("bench output should be UTF-8")
                .lines()
                .find_map(|line| line.strip_prefix("Nodes searched: "))
                .expect("bench nodes should be reported")
                .parse::<u64>()
                .expect("bench nodes should be numeric")
        };
        assert_eq!(nodes(first_output), nodes(second_output));
    }

    #[test]
    fn movetime_search_limits_use_the_requested_duration() {
        let (parameters, _) =
            GoParameters::parse(&["movetime", "100"]).expect("movetime should parse");
        let limits = parameters.search_limits(&Position::startpos());

        assert_eq!(limits.soft_time, Some(Duration::from_millis(100)));
        assert_eq!(limits.hard_time, Some(Duration::from_millis(100)));
    }

    #[test]
    fn clock_limits_reserve_a_safety_margin_and_let_the_hard_limit_borrow_from_later_moves() {
        let (parameters, _) = GoParameters::parse(&["wtime", "60000", "winc", "600"])
            .expect("clock parameters should parse");
        let limits = parameters.search_limits(&Position::startpos());
        let soft = limits.soft_time.expect("a clock implies a soft limit");
        let hard = limits.hard_time.expect("a clock implies a hard limit");

        // 60000 - 10 overhead - 1200 safety = 58790 available; 58790/30 + 450 = 2409.
        assert_eq!(soft, Duration::from_millis(2_409));
        // The hard limit must be a genuine multiple of the soft one, not the 5/4 that
        // made the two limits interchangeable.
        assert_eq!(hard, Duration::from_millis(2_409 * 4));
        assert!(hard <= Duration::from_millis(60_000 * 40 / 100));
    }

    #[test]
    fn a_short_clock_caps_the_hard_limit_at_a_fraction_of_what_remains() {
        let (parameters, _) =
            GoParameters::parse(&["btime", "300", "movestogo", "1"]).expect("clock should parse");
        let mut position = Position::startpos();
        position.make_move(
            mf_core::generate_legal_moves(&position)
                .first()
                .copied()
                .expect("startpos has moves"),
        );
        let limits = parameters.search_limits(&position);
        let hard = limits.hard_time.expect("a clock implies a hard limit");

        // With one move to go the soft limit takes the whole available clock, so only
        // the 40% ceiling stands between the engine and forfeiting on time.
        assert_eq!(hard, Duration::from_millis(300 * 40 / 100));
        assert!(limits.soft_time.expect("soft limit") <= hard);
    }

    #[test]
    fn a_distant_movestogo_is_clamped_so_the_per_move_budget_stays_usable() {
        let (far, _) = GoParameters::parse(&["wtime", "60000", "movestogo", "200"])
            .expect("clock should parse");
        let (clamped, _) = GoParameters::parse(&["wtime", "60000", "movestogo", "50"])
            .expect("clock should parse");

        assert_eq!(
            far.search_limits(&Position::startpos()).soft_time,
            clamped.search_limits(&Position::startpos()).soft_time
        );
    }

    #[test]
    fn mtbench_parser_defaults_to_depth_ten_and_standard_thread_rows() {
        let options = parse_mtbench_arguments(std::iter::empty::<String>())
            .expect("default mtbench arguments should parse");

        assert_eq!(options.threads, [1, 2, 4, 8]);
        assert_eq!(options.depth, 10);
    }
}
