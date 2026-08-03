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
    IterationInfo, PoolError, PoolSearchResult, SearchLimits, SearchOptions, SearchPool,
    SearchResult, SharedHistory, TranspositionTable, clamp_centipawn_score, score_to_uci_mate,
    search_with_shared_history_network_options,
};

const DEFAULT_HASH_MIB: usize = 16;
const MIN_HASH_MIB: i128 = 1;
const MAX_HASH_MIB: i128 = 1_048_576;
const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Houijasu",
    "option name Hash type spin default 16 min 1 max 1048576",
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
    "option name UseNnue type check default true",
    "option name EvalFile type string default <empty>",
    "uciok",
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
const TIME_OVERHEAD_MILLIS: u64 = 10;
const NULL_BESTMOVE: &str = "0000";

struct EngineState {
    position: Position,
    position_history: Vec<u64>,
    chess960: bool,
    use_nnue: bool,
    network: Option<Arc<Network>>,
    network_source: Option<NetworkSource>,
    network_resolution_error: Option<Arc<str>>,
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
            chess960: false,
            use_nnue: true,
            network: network.network,
            network_source: network.source,
            network_resolution_error: network.error,
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
    network: Option<Arc<Network>>,
    source: Option<NetworkSource>,
    error: Option<Arc<str>>,
}

fn default_network_resolution() -> SharedNetworkResolution {
    static RESOLUTION: OnceLock<SharedNetworkResolution> = OnceLock::new();
    RESOLUTION
        .get_or_init(|| match resolve_network(None) {
            Ok(Some(resolved)) => {
                let (network, source) = resolved.into_parts();
                SharedNetworkResolution {
                    network: Some(Arc::new(network)),
                    source: Some(source),
                    error: None,
                }
            }
            Ok(None) => SharedNetworkResolution {
                network: None,
                source: None,
                error: None,
            },
            Err(error) => SharedNetworkResolution {
                network: None,
                source: None,
                error: Some(Arc::from(error.to_string())),
            },
        })
        .clone()
}

impl EngineState {
    fn new_game(&mut self) -> Result<(), PoolError> {
        self.position = Position::startpos();
        self.position_history = vec![self.position.repetition_key()];
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
            match active_bench_network(&state) {
                Ok(network) => {
                    write_bench(&mut *writer, state.search_options, network.as_deref())
                        .map_err(io::Error::other)?;
                }
                Err(error) => {
                    writeln!(
                        writer,
                        "info string unable to run bench: NNUE resolution failed: {error}"
                    )?;
                }
            }
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
            let _ = handle_position(command, &mut state);
        } else if keyword.eq_ignore_ascii_case("go") {
            let Some(request) = GoRequest::parse(&tokens) else {
                continue;
            };
            stop_active_search(&mut active_search);
            match request {
                GoRequest::Perft(depth) => {
                    let mut writer = writer
                        .lock()
                        .expect("UCI writer lock should not be poisoned");
                    write_perft(&mut *writer, &mut state.position, depth, state.chess960)?;
                    writer.flush()?;
                }
                GoRequest::Search(parameters) if parameters.infinite => {
                    let network = active_search_network(&state);
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
                GoRequest::Search(parameters) => {
                    let fixed_depth = parameters.depth.is_some();
                    let network = active_search_network(&state);
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

fn stop_active_search(active_search: &mut Option<ActiveSearch>) {
    if let Some(search) = active_search.take() {
        search.stop_and_join();
    }
}

fn active_search_network(state: &EngineState) -> Option<Arc<Network>> {
    state
        .use_nnue
        .then(|| state.network.as_ref().map(Arc::clone))
        .flatten()
}

fn active_bench_network(state: &EngineState) -> Result<Option<Arc<Network>>, &str> {
    if state.use_nnue
        && state.network.is_none()
        && let Some(error) = state.network_resolution_error.as_deref()
    {
        return Err(error);
    }
    Ok(active_search_network(state))
}

fn active_evaluator_diagnostic(state: &EngineState) -> String {
    if state.use_nnue
        && state.network.is_none()
        && let Some(error) = state.network_resolution_error.as_ref()
    {
        return format!("info string evaluation HCE because NNUE resolution failed: {error}");
    }
    match (
        state.use_nnue,
        state.network.as_ref(),
        state.network_source.as_ref(),
    ) {
        (true, Some(network), Some(source)) => {
            let mode = production_forward_mode();
            format!(
                "info string evaluation NNUE from {source}; network \"{}\"; backend {:?}; sparse FC0 {}",
                network.description(),
                mode.backend(),
                mode.sparse_fc0()
            )
        }
        _ => "info string evaluation HCE".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn start_search<W>(
    position: Position,
    position_history: Vec<u64>,
    search_pool: Arc<SearchPool>,
    transposition_table: Arc<TranspositionTable>,
    limits: SearchLimits,
    options: SearchOptions,
    network: Option<Arc<Network>>,
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
        let result = if fixed_depth {
            search_pool.search_fixed_depth_with_history_callback_network_options(
                &position,
                &position_history,
                Arc::clone(&transposition_table),
                limits,
                options,
                Arc::clone(&search_stop),
                network.as_ref().map(Arc::clone),
                on_iteration,
            )
        } else {
            search_pool.search_with_history_callback_network_options(
                &position,
                &position_history,
                Arc::clone(&transposition_table),
                limits,
                options,
                Arc::clone(&search_stop),
                network,
                on_iteration,
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

    let resolution = benchmark_network_resolution();
    if let Some(error) = resolution.error {
        return Err(format!("unable to resolve bench NNUE network: {error}"));
    }
    write_bench(
        &mut writer,
        SearchOptions::default(),
        resolution.network.as_deref(),
    )
}

/// Runs the standalone multi-thread search scaling benchmark.
pub fn run_mtbench_subcommand<I, S, W>(arguments: I, mut writer: W) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
{
    let options = parse_mtbench_arguments(arguments)?;
    let resolution = benchmark_network_resolution();
    if let Some(error) = resolution.error {
        return Err(format!("unable to resolve mtbench NNUE network: {error}"));
    }
    let network = resolution.network;
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
                .search_fixed_depth_smp_with_history_callback_network_options(
                    position,
                    &history,
                    Arc::clone(&transposition_table),
                    SearchLimits {
                        depth: Some(options.depth),
                        ..SearchLimits::default()
                    },
                    SearchOptions::default(),
                    Arc::new(AtomicBool::new(false)),
                    network.as_ref().map(Arc::clone),
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

fn benchmark_network_resolution() -> SharedNetworkResolution {
    let Some(path) = std::env::var_os("MF_NNUE_TEST_NET") else {
        return default_network_resolution();
    };
    match resolve_network(Some(Path::new(&path))) {
        Ok(Some(resolved)) => {
            let (network, source) = resolved.into_parts();
            SharedNetworkResolution {
                network: Some(Arc::new(network)),
                source: Some(source),
                error: None,
            }
        }
        Ok(None) => unreachable!("an explicit benchmark network never resolves to none"),
        Err(error) => SharedNetworkResolution {
            network: None,
            source: None,
            error: Some(Arc::from(error.to_string())),
        },
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
    network: Option<&Network>,
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
    let shared_history = SharedHistory::new(1);

    // Only the searches are timed. The per-position table resets are setup, not search
    // work, and leaving them inside the measurement is what made bench NPS unusable.
    let mut elapsed = Duration::ZERO;
    let mut total = 0u64;
    for position in &positions {
        transposition_table.clear();
        shared_history.clear();
        let started = Instant::now();
        let result = search_with_shared_history_network_options(
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
    } else if name.eq_ignore_ascii_case("UseNnue") {
        if let Some(enabled) = parse_check_option(&value)
            && state.use_nnue != enabled
        {
            state.use_nnue = enabled;
            clear_eval_dependent_search_state(state, writer, "UseNnue")?;
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

        let clamped = requested.min(MAX_HASH_MIB) as usize;
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
        if let Some(error) = resolution.error {
            state.network_resolution_error = Some(Arc::clone(&error));
            writeln!(
                writer,
                "info string unable to load EvalFile automatic source: {error}"
            )?;
            return Ok(());
        }

        state.network = resolution.network;
        state.network_source = resolution.source;
        state.network_resolution_error = None;
        clear_eval_dependent_search_state(state, writer, "EvalFile")?;
        return write_network_selection(writer, state, "automatic resolution");
    }

    match resolve_network(Some(Path::new(value))) {
        Ok(Some(resolved)) => {
            let (network, source) = resolved.into_parts();
            state.network = Some(Arc::new(network));
            state.network_source = Some(source);
            state.network_resolution_error = None;
            clear_eval_dependent_search_state(state, writer, "EvalFile")?;
            write_network_selection(writer, state, "EvalFile")
        }
        Ok(None) => unreachable!("an explicit EvalFile never resolves to no network"),
        Err(error) => {
            state.network_resolution_error = Some(Arc::from(error.to_string()));
            writeln!(writer, "info string unable to load EvalFile {error}")
        }
    }
}

fn write_network_selection<W: Write>(
    writer: &mut W,
    state: &EngineState,
    selection: &str,
) -> io::Result<()> {
    let Some(network) = state.network.as_ref() else {
        return writeln!(writer, "info string {selection} found no NNUE network");
    };
    let source = state
        .network_source
        .as_ref()
        .expect("a loaded network must retain its source");
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

    let mut position = if kind.eq_ignore_ascii_case("startpos") {
        Position::startpos()
    } else if kind.eq_ignore_ascii_case("fen") {
        let fen = (0..6)
            .map(|_| {
                tokens
                    .next()
                    .ok_or_else(|| "FEN requires six fields".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        Position::from_fen(&fen, state.chess960).map_err(|error| error.to_string())?
    } else {
        return Err(format!("unknown position type '{kind}'"));
    };

    let mut position_history = vec![position.repetition_key()];
    if let Some(separator) = tokens.next() {
        if !separator.eq_ignore_ascii_case("moves") {
            return Err(format!("unexpected position argument '{separator}'"));
        }
        for notation in tokens {
            let mv = parse_uci_move(&position, notation, state.chess960)
                .ok_or_else(|| format!("illegal move '{notation}'"))?;
            position.make_move(mv);
            position_history.push(position.repetition_key());
        }
    }

    state.position = position;
    state.position_history = position_history;
    Ok(())
}

enum GoRequest {
    Perft(u32),
    Search(GoParameters),
}

impl GoRequest {
    fn parse(tokens: &[&str]) -> Option<Self> {
        if tokens
            .first()
            .is_none_or(|token| !token.eq_ignore_ascii_case("go"))
        {
            return None;
        }
        if tokens.len() == 3 && tokens[1].eq_ignore_ascii_case("perft") {
            return tokens[2].parse().ok().map(Self::Perft);
        }
        GoParameters::parse(&tokens[1..]).map(Self::Search)
    }
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

impl GoParameters {
    fn parse(tokens: &[&str]) -> Option<Self> {
        let mut parameters = Self::default();
        let mut index = 0;
        while index < tokens.len() {
            let key = tokens[index];
            index += 1;
            if key.eq_ignore_ascii_case("infinite") {
                if parameters.infinite {
                    return None;
                }
                parameters.infinite = true;
                continue;
            }
            let value = *tokens.get(index)?;
            index += 1;

            if key.eq_ignore_ascii_case("depth") {
                parameters.depth = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("nodes") {
                parameters.nodes = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("movetime") {
                parameters.movetime = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("wtime") {
                parameters.wtime = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("btime") {
                parameters.btime = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("winc") {
                parameters.winc = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("binc") {
                parameters.binc = Some(value.parse().ok()?);
            } else if key.eq_ignore_ascii_case("movestogo") {
                parameters.movestogo = Some(value.parse().ok()?);
            } else {
                return None;
            }
        }

        (parameters.depth.is_some()
            || parameters.nodes.is_some()
            || parameters.movetime.is_some()
            || parameters.wtime.is_some()
            || parameters.btime.is_some()
            || parameters.infinite)
            .then_some(parameters)
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
        let moves = self.movestogo.unwrap_or(DEFAULT_MOVES_TO_GO).max(1);
        let ideal = (remaining / moves)
            .saturating_add(increment.saturating_mul(3) / 4)
            .max(1);
        let hard = ideal
            .saturating_mul(5)
            .checked_div(4)
            .unwrap_or(ideal)
            .saturating_sub(TIME_OVERHEAD_MILLIS)
            .max(1)
            .min(remaining.saturating_sub(TIME_OVERHEAD_MILLIS).max(1));
        (
            Some(Duration::from_millis(ideal.min(hard))),
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
            "info depth {} seldepth {} score {} nodes {} nps 0 hashfull {} time {} pv {}",
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
    writeln!(
        writer,
        "info score {} nodes {} nps {} hashfull {} time {} pv {}",
        score, result.nodes, nps, result.hashfull, elapsed_millis, pv
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
        "info depth {} seldepth {} score {} nodes {} nps {} hashfull {} time {} pv {}",
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
    fn helper_selected_terminal_line_omits_depth_before_bestmove() {
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
                "info score cp 42 nodes 1234 nps 61700 hashfull 17 time 20 pv {}",
                format_uci_move(&position, best_move, false)
            )
        );
        assert!(!lines[0].split_whitespace().any(|token| token == "depth"));
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

    #[test]
    fn failed_hash_resize_preserves_the_existing_usable_table() {
        let mut state = EngineState::default();
        let original_bytes = state.transposition_table.allocated_bytes();
        let mut output = Vec::new();

        handle_setoption("setoption name Hash value 1048576", &mut state, &mut output)
            .expect("setoption output should be writable");

        assert_eq!(state.transposition_table.allocated_bytes(), original_bytes);
        assert!(
            String::from_utf8(output)
                .expect("protocol output should be UTF-8")
                .contains("unable to allocate Hash 1048576 MB")
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
    fn use_nnue_parses_case_insensitively_and_survives_new_game() {
        let mut state = EngineState::default();
        let mut output = Vec::new();

        handle_setoption(
            "SeToPtIoN NaMe UsEnNuE VaLuE FaLsE",
            &mut state,
            &mut output,
        )
        .expect("UseNnue output should be writable");
        state
            .new_game()
            .expect("the default search pool should clear the table");

        assert!(!state.use_nnue);
        assert!(output.is_empty());
    }

    #[test]
    fn use_nnue_controls_search_network_without_unloading_it() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let mut state = EngineState::default();
        let mut output = Vec::new();
        handle_setoption(
            &format!("setoption name EvalFile value {}", path.display()),
            &mut state,
            &mut output,
        )
        .expect("test EvalFile should load");
        let loaded = Arc::clone(state.network.as_ref().expect("network should be loaded"));

        assert!(Arc::ptr_eq(
            active_search_network(&state)
                .as_ref()
                .expect("enabled NNUE should reach search"),
            &loaded
        ));

        handle_setoption(
            "setoption name UseNnue value false",
            &mut state,
            &mut output,
        )
        .expect("UseNnue false should be writable");
        assert!(active_search_network(&state).is_none());
        assert!(Arc::ptr_eq(
            state
                .network
                .as_ref()
                .expect("disabling NNUE must not unload it"),
            &loaded
        ));

        handle_setoption("setoption name UseNnue value true", &mut state, &mut output)
            .expect("UseNnue true should be writable");
        assert!(Arc::ptr_eq(
            active_search_network(&state)
                .as_ref()
                .expect("re-enabled NNUE should reach search"),
            &loaded
        ));
    }

    #[test]
    fn changing_use_nnue_clears_eval_dependent_search_state() {
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
            "setoption name UseNnue value false",
            &mut state,
            &mut output,
        )
        .expect("UseNnue change should be writable");

        assert_eq!(state.transposition_table.probe(key), None);
    }

    #[test]
    fn failed_default_network_resolution_is_not_reported_as_plain_hce() {
        let state = EngineState {
            use_nnue: true,
            network: None,
            network_source: None,
            network_resolution_error: Some(Arc::from("invalid discovered network")),
            ..EngineState::default()
        };

        let diagnostic = active_evaluator_diagnostic(&state);

        assert!(diagnostic.contains("NNUE resolution failed"));
        assert!(diagnostic.contains("invalid discovered network"));
    }

    #[test]
    fn bench_rejects_a_failed_required_nnue_resolution() {
        let state = EngineState {
            use_nnue: true,
            network: None,
            network_source: None,
            network_resolution_error: Some(Arc::from("invalid discovered network")),
            ..EngineState::default()
        };

        assert!(active_bench_network(&state).is_err());
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
        let original = Arc::clone(
            state
                .network
                .as_ref()
                .expect("valid setup network should resolve"),
        );
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

        assert!(Arc::ptr_eq(
            state
                .network
                .as_ref()
                .expect("failed replacement should preserve the network"),
            &original
        ));
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

        assert!(state.network.is_some());
        assert_eq!(
            state.network_source.as_ref(),
            Some(&mf_nnue::NetworkSource::Explicit(path.clone()))
        );
        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        assert!(output.starts_with("info string"));
        assert!(output.contains(&path.display().to_string()));
        assert!(
            output.contains(
                state
                    .network
                    .as_ref()
                    .expect("network should be present")
                    .description()
            )
        );
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
        assert!(state.network.is_some());

        output.clear();
        handle_setoption(
            "setoption name EvalFile value <empty>",
            &mut state,
            &mut output,
        )
        .expect("automatic EvalFile reset should be writable");

        assert!(state.network.is_none());
        assert!(state.network_source.is_none());
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
        assert!(state.network.is_some());

        output.clear();
        handle_setoption("setoption name EvalFile value", &mut state, &mut output)
            .expect("empty automatic EvalFile reset should be writable");

        assert!(state.network.is_none());
        assert!(state.network_source.is_none());
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

    #[test]
    fn bench_uses_the_supplied_nnue_network() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue");
        let network = Network::load(&path)
            .unwrap_or_else(|error| panic!("test NNUE network {}: {error}", path.display()));
        let mut nnue_output = Vec::new();
        let mut hce_output = Vec::new();

        write_bench(&mut nnue_output, SearchOptions::default(), Some(&network))
            .expect("NNUE bench should complete");
        write_bench(&mut hce_output, SearchOptions::default(), None)
            .expect("HCE bench should complete");

        let nodes = |output: Vec<u8>| {
            String::from_utf8(output)
                .expect("bench output should be UTF-8")
                .lines()
                .find_map(|line| line.strip_prefix("Nodes searched: "))
                .expect("bench nodes should be reported")
                .parse::<u64>()
                .expect("bench nodes should be numeric")
        };
        assert_ne!(nodes(nnue_output), nodes(hce_output));
    }

    #[test]
    fn movetime_search_limits_use_the_requested_duration() {
        let parameters = GoParameters::parse(&["movetime", "100"]).expect("movetime should parse");
        let limits = parameters.search_limits(&Position::startpos());

        assert_eq!(limits.soft_time, Some(Duration::from_millis(100)));
        assert_eq!(limits.hard_time, Some(Duration::from_millis(100)));
    }

    #[test]
    fn mtbench_parser_defaults_to_depth_ten_and_standard_thread_rows() {
        let options = parse_mtbench_arguments(std::iter::empty::<String>())
            .expect("default mtbench arguments should parse");

        assert_eq!(options.threads, [1, 2, 4, 8]);
        assert_eq!(options.depth, 10);
    }
}
