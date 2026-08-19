//! Universal Chess Interface protocol handling for Manifold.

mod datagen_cli;

pub use datagen_cli::run_datagen_subcommand;

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mf_core::{
    Move, Piece, PieceKind, Position, Square, format_uci_move, generate_legal_moves,
    parse_uci_move, perft_divide,
};
use mf_nnue::{Network, NetworkSource, production_forward_mode, resolve_network};
use mf_search::{
    IterationInfo, PonderState, PoolError, PoolSearchResult, RootMoveInfo, SEARCH_PARAMETERS,
    SearchLimits, SearchOptions, SearchParameterSpec, SearchPool, SearchResult, SharedHistory,
    TranspositionTable, clamp_centipawn_score, max_hash_mebibytes, score_to_uci_mate,
    search_parameter, search_with_shared_history,
};
use mf_tb::Tablebases;

const DEFAULT_HASH_MIB: usize = 16;
const MIN_HASH_MIB: i128 = 1;
/// The handshake, apart from the `Hash` line and the tunable spins.
///
/// `Hash` is not here because its advertised range is a property of the machine rather
/// than of the build: see [`hash_option_line`]. The search parameters are not here
/// because they are generated from [`mf_search::SEARCH_PARAMETERS`], which is what makes
/// an advertised default and the constant behind it the same number by construction.
const UCI_RESPONSE: &[&str] = &[
    "id name Manifold",
    "id author Houijasu",
    "option name Threads type spin default 1 min 1 max 256",
    "option name MultiPV type spin default 1 min 1 max 256",
    "option name Clear Hash type button",
    "option name Move Overhead type spin default 10 min 0 max 2000",
    "option name Ponder type check default false",
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
    "option name UsePostLMRDepth type check default true",
    "option name UsePostLMRContHist type check default false",
    "option name UseSingularExt type check default true",
    "option name UseCheckExt type check default true",
    "option name UseMultiCut type check default true",
    "option name UseIIR type check default true",
    "option name UseProbCut type check default true",
    "option name UseButterflyHistory type check default true",
    "option name UseCaptureHistory type check default true",
    "option name UsePawnHistory type check default false",
    "option name UseContHistory type check default true",
    "option name UseTtMoveHistory type check default false",
    "option name UseLowPlyHistory type check default true",
    "option name UseHistoryPruning type check default false",
    "option name UseCorrHistory type check default true",
    "option name UseCorrHistPawn type check default true",
    "option name UseCorrHistMinor type check default true",
    "option name UseCorrHistMajor type check default false",
    "option name UseCorrHistMaterial type check default false",
    "option name UseCorrHistCont type check default true",
    "option name UseCorrplexity type check default false",
    "option name UseTimeEffort type check default false",
    "option name UseInterpolatedTimeManagement type check default false",
    "option name UseSearchAgainDepth type check default false",
    "option name EvalFile type string default <empty>",
    "option name SyzygyPath type string default <empty>",
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
/// Default `Move Overhead`; the advertised option line must agree with these three.
const TIME_OVERHEAD_MILLIS: u64 = 10;
const MIN_MOVE_OVERHEAD_MILLIS: u64 = 0;
const MAX_MOVE_OVERHEAD_MILLIS: u64 = 2000;
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

/// The `option name ... type spin` line for one tunable search parameter.
///
/// Generated rather than written out, because an SPSA tuner discovers a parameter's
/// range from this line and then writes values back with `setoption`. A hand-maintained
/// list would eventually advertise a default that no longer matched the constant, and
/// the engine would change strength the first time a GUI echoed back the value it was
/// just told.
fn search_parameter_option_line(spec: &SearchParameterSpec) -> String {
    format!(
        "option name {} type spin default {} min {} max {}",
        spec.name, spec.default, spec.min, spec.max
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
    /// Syzygy tablebases loaded via `SyzygyPath`; `None` until a path is set.
    tablebases: Option<Arc<Tablebases>>,
    /// Milliseconds withheld from every clock budget, set via `Move Overhead`.
    move_overhead_millis: u64,
    /// The advisory `Ponder` option: a GUI's declaration that it will ponder.
    ///
    /// Stored but not consulted outside tests: `go ponder` is honoured whenever it
    /// arrives, and time management does not yet spend differently for a pondering
    /// opponent.
    #[allow(dead_code)]
    ponder_enabled: bool,
}

impl EngineState {
    fn try_new() -> Result<Self, String> {
        Self::try_new_with_network(default_network_resolution())
    }

    fn try_new_with_network(
        network: Result<SharedNetworkResolution, String>,
    ) -> Result<Self, String> {
        let network = network
            .map_err(|error| format!("manifold requires an NNUE network to evaluate: {error}"))?;
        let position = Position::startpos();
        Ok(Self {
            position_history: vec![position.repetition_key()],
            position,
            position_is_stale: false,
            chess960: false,
            network: network.network,
            network_source: network.source,
            search_pool: Arc::new(
                SearchPool::new(1)
                    .map_err(|error| format!("unable to start default search worker: {error}"))?,
            ),
            search_options: SearchOptions::default(),
            transposition_table: Arc::new(
                TranspositionTable::new(DEFAULT_HASH_MIB)
                    .map_err(|error| format!("unable to allocate default Hash: {error}"))?,
            ),
            tablebases: None,
            move_overhead_millis: TIME_OVERHEAD_MILLIS,
            ponder_enabled: false,
        })
    }
}

#[derive(Clone)]
struct SharedNetworkResolution {
    network: Arc<Network>,
    source: NetworkSource,
}

/// Resolves the automatic network once per process.
///
fn default_network_resolution() -> Result<SharedNetworkResolution, String> {
    static RESOLUTION: OnceLock<Result<SharedNetworkResolution, String>> = OnceLock::new();
    RESOLUTION
        .get_or_init(|| {
            resolve_network(None)
                .map(|resolved| {
                    let (network, source) = resolved.into_parts();
                    SharedNetworkResolution {
                        network: Arc::new(network),
                        source,
                    }
                })
                .map_err(|error| error.to_string())
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
    /// The `go ponder` latch, present only while this search was started pondering.
    ponder: Option<Arc<PonderState>>,
    handle: JoinHandle<io::Result<()>>,
}

impl ActiveSearch {
    fn stop_and_join(self) -> io::Result<()> {
        // A ponder miss: end the ponder wait WITHOUT re-basing the clock, so the
        // search thread prints the deferred bestmove instead of spinning forever.
        // The stop flag alone cannot do this, because the pool sets that same flag
        // itself when worker 0 completes while still pondering.
        if let Some(ponder) = &self.ponder {
            ponder.abort();
        }
        while !self.handle.is_finished() {
            self.stop.store(true, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(1));
        }
        self.stop.store(true, Ordering::Relaxed);
        self.handle
            .join()
            .map_err(|_| io::Error::other("search thread panicked"))?
    }
}

/// Serves UCI commands until `quit` or end-of-file.
///
/// Startup resolves the NNUE network and allocates the default search worker and hash
/// table before reading any commands. A failure in any of those steps is returned as an
/// [`io::Error`], so callers can report it without entering the UCI command loop.
pub fn run<R, W>(reader: R, writer: W) -> io::Result<()>
where
    R: BufRead,
    W: Write + Send + 'static,
{
    let mut state = EngineState::try_new().map_err(io::Error::other)?;
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
                for spec in SEARCH_PARAMETERS {
                    writeln!(writer, "{}", search_parameter_option_line(spec))?;
                }
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
            stop_active_search(&mut active_search)?;
        } else if keyword.eq_ignore_ascii_case("ponderhit") && has_no_arguments {
            // The predicted move was played: the ponder search becomes the real one.
            // The latch flip re-bases the clock, so the budget computed at `go ponder`
            // starts counting from now; the search itself is NOT stopped. Without an
            // active ponder search there is nothing to convert, and a stray
            // `ponderhit` is silently ignored per the protocol's tolerance for
            // out-of-sequence commands.
            if let Some(ponder) = active_search
                .as_ref()
                .and_then(|search| search.ponder.as_ref())
            {
                ponder.ponderhit();
            }
        } else if keyword.eq_ignore_ascii_case("quit") && has_no_arguments {
            stop_active_search(&mut active_search)?;
            break;
        } else if keyword.eq_ignore_ascii_case("ucinewgame") && has_no_arguments {
            stop_active_search(&mut active_search)?;
            if let Err(error) = state.new_game() {
                let mut writer = writer
                    .lock()
                    .expect("UCI writer lock should not be poisoned");
                writeln!(writer, "info string unable to clear Hash: {error}")?;
                writer.flush()?;
            }
        } else if keyword.eq_ignore_ascii_case("bench") && has_no_arguments {
            stop_active_search(&mut active_search)?;
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            write_bench(&mut *writer, state.search_options, &state.network)
                .map_err(io::Error::other)?;
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("d") && has_no_arguments {
            stop_active_search(&mut active_search)?;
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            write_position_diagram(&mut *writer, &state.position, state.chess960)?;
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("eval") && has_no_arguments {
            stop_active_search(&mut active_search)?;
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            if state.position_is_stale {
                writeln!(
                    writer,
                    "info string refusing to evaluate: the last position command failed, \
                     so the engine does not know the current position"
                )?;
            } else {
                writeln!(
                    writer,
                    "NNUE evaluation: {} cp (from the side to move's perspective)",
                    state.network.evaluate_production(&state.position)
                )?;
            }
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("setoption") {
            stop_active_search(&mut active_search)?;
            let mut writer = writer
                .lock()
                .expect("UCI writer lock should not be poisoned");
            handle_setoption(command, &mut state, &mut *writer)?;
            writer.flush()?;
        } else if keyword.eq_ignore_ascii_case("position") {
            stop_active_search(&mut active_search)?;
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
            stop_active_search(&mut active_search)?;
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
                    let root_moves = parameters.root_moves(&state.position, state.chess960);
                    let ponder = parameters.ponder.then(|| Arc::new(PonderState::new()));
                    active_search = Some(start_search(
                        state.position.clone(),
                        state.position_history.clone(),
                        Arc::clone(&state.search_pool),
                        Arc::clone(&state.transposition_table),
                        parameters.search_limits(&state.position, state.move_overhead_millis),
                        state.search_options,
                        network,
                        state.tablebases.clone(),
                        root_moves,
                        None,
                        ponder,
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
                    let root_moves = parameters.root_moves(&state.position, state.chess960);
                    let ponder = parameters.ponder.then(|| Arc::new(PonderState::new()));
                    active_search = Some(start_search(
                        state.position.clone(),
                        state.position_history.clone(),
                        Arc::clone(&state.search_pool),
                        Arc::clone(&state.transposition_table),
                        parameters.search_limits(&state.position, state.move_overhead_millis),
                        state.search_options,
                        network,
                        state.tablebases.clone(),
                        root_moves,
                        parameters.mate,
                        ponder,
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

    stop_active_search(&mut active_search)
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

fn stop_active_search(active_search: &mut Option<ActiveSearch>) -> io::Result<()> {
    if let Some(search) = active_search.take() {
        search.stop_and_join()?;
    }
    Ok(())
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
    tablebases: Option<Arc<Tablebases>>,
    root_moves: Option<Vec<Move>>,
    mate: Option<u32>,
    ponder: Option<Arc<PonderState>>,
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
    let search_ponder = ponder.clone();
    let first_write_error = Arc::new(Mutex::new(None));
    let handle = thread::spawn(move || {
        write_search_output(&writer, &first_write_error, &search_stop, |writer| {
            writeln!(writer, "{evaluator_diagnostic}")
        });
        let mate_stop = Arc::clone(&search_stop);
        let on_iteration = |iteration: &IterationInfo| {
            write_search_output(&writer, &first_write_error, &search_stop, |writer| {
                write_iteration_info(writer, &position, iteration, chess960)
            });
            // `go mate N`: the requested mate (or shorter) for the side to move ends
            // the search. The search's own mate-score exit usually fires first; this
            // is what makes a bare `go mate N` terminate instead of running unbounded.
            if let Some(n) = mate
                && score_to_uci_mate(iteration.score)
                    .is_some_and(|moves| moves > 0 && moves as u32 <= n)
            {
                mate_stop.store(true, Ordering::Relaxed);
            }
        };
        let on_current_move = |root_move: &RootMoveInfo| {
            write_search_output(&writer, &first_write_error, &search_stop, |writer| {
                write_current_move_info(writer, &position, root_move, chess960)
            });
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
                tablebases,
                root_moves,
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
                tablebases,
                root_moves,
                search_ponder.clone(),
                on_iteration,
                on_current_move,
            )
        };
        // A search that completed while still pondering -- it hit the depth ceiling,
        // or the root was terminal -- must hold its answer until `ponderhit` or
        // `stop`, both of which unarm the latch. The shared stop flag cannot gate
        // this wait: the pool sets it internally when worker 0 completes.
        while search_ponder
            .as_ref()
            .is_some_and(|ponder| ponder.is_pondering())
        {
            thread::sleep(Duration::from_millis(1));
        }
        while wait_for_stop && !search_stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(1));
        }
        write_search_output(
            &writer,
            &first_write_error,
            &search_stop,
            |writer| match result {
                Ok(result) => write_pool_search_tail(writer, &position, &result, chess960),
                Err(error) => write_search_failure(writer, &position, error, chess960),
            },
        );
        match first_write_error
            .lock()
            .expect("search write error lock should not be poisoned")
            .take()
        {
            Some((kind, message)) => Err(io::Error::new(kind, message)),
            None => Ok(()),
        }
    });
    ActiveSearch {
        stop,
        ponder,
        handle,
    }
}

fn write_search_output<W: Write>(
    writer: &Mutex<W>,
    first_write_error: &Mutex<Option<(io::ErrorKind, String)>>,
    search_stop: &AtomicBool,
    write: impl FnOnce(&mut W) -> io::Result<()>,
) {
    if first_write_error
        .lock()
        .expect("search write error lock should not be poisoned")
        .is_some()
    {
        return;
    }
    let result = {
        let mut writer = writer
            .lock()
            .expect("UCI writer lock should not be poisoned");
        write(&mut writer).and_then(|()| writer.flush())
    };
    record_search_write_error(result, first_write_error, search_stop);
}

fn record_search_write_error(
    result: io::Result<()>,
    first_write_error: &Mutex<Option<(io::ErrorKind, String)>>,
    search_stop: &AtomicBool,
) {
    if let Err(error) = result {
        let mut first_write_error = first_write_error
            .lock()
            .expect("search write error lock should not be poisoned");
        if first_write_error.is_none() {
            *first_write_error = Some((error.kind(), error.to_string()));
        }
        search_stop.store(true, Ordering::Relaxed);
    }
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
                    None,
                    None,
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
        return default_network_resolution();
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
            // Bench is the change-detection signature: never probe tablebases here.
            None,
            None,
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
    if tokens.len() < 3
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
        // Button options carry no `value` token: `setoption name Clear Hash` is the
        // whole command. Everything else without a value stays ignored.
        let name = tokens[2..].join(" ");
        if name.eq_ignore_ascii_case("Clear Hash") {
            handle_clear_hash(state, writer)?;
        }
        return Ok(());
    };
    let name = tokens[2..value_index].join(" ");
    let value = tokens[value_index + 1..].join(" ");

    // Check-type options accept exactly true|false, and GUIs and tuners also speak a
    // numeric-bool dialect (`value 1`). A silently ignored write leaves the tuner
    // measuring a value the engine never adopted, so an unparseable value is reported
    // the same way the numeric options report theirs. A missing `value` token never
    // reaches here: some GUIs send a bare `setoption name X`.
    if let Some(canonical) = check_option_name(&name)
        && parse_check_option(&value).is_none()
    {
        writeln!(
            writer,
            "info string invalid {canonical} value '{value}' (expected true|false)"
        )?;
        return Ok(());
    }

    if name.eq_ignore_ascii_case("UCI_Chess960") {
        if let Some(enabled) = parse_check_option(&value) {
            state.chess960 = enabled;
        }
    } else if name.eq_ignore_ascii_case("Ponder") {
        if let Some(enabled) = parse_check_option(&value) {
            state.ponder_enabled = enabled;
        }
    } else if name.eq_ignore_ascii_case("MultiPV") {
        let Ok(requested) = value.parse::<i128>() else {
            writeln!(writer, "info string invalid MultiPV value '{value}'")?;
            return Ok(());
        };
        state.search_options.multi_pv = requested.clamp(1, 256) as u32;
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
    } else if name.eq_ignore_ascii_case("UsePostLMRDepth") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_post_lmr_depth = enabled;
        }
    } else if name.eq_ignore_ascii_case("UsePostLMRContHist") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_post_lmr_conthist = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseTimeEffort") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_time_effort = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseInterpolatedTimeManagement") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_interpolated_time_management = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseSearchAgainDepth") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_search_again_depth = enabled;
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
    } else if name.eq_ignore_ascii_case("UseTtMoveHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_tt_move_history = enabled;
        }
    } else if name.eq_ignore_ascii_case("UseLowPlyHistory") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_low_ply_history = enabled;
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
    } else if name.eq_ignore_ascii_case("UseCorrplexity") {
        if let Some(enabled) = parse_check_option(&value) {
            state.search_options.use_corrplexity = enabled;
        }
    } else if let Some(spec) = search_parameter(&name) {
        // A tuner writes these hundreds of times per session, so an unparseable value
        // is reported rather than swallowed: a silently ignored write leaves the tuner
        // measuring a value the engine never adopted.
        let Ok(requested) = value.parse::<i32>() else {
            writeln!(writer, "info string invalid {} value '{value}'", spec.name)?;
            return Ok(());
        };
        spec.set(&mut state.search_options.parameters, requested);
    } else if name.eq_ignore_ascii_case("EvalFile") {
        handle_eval_file(&value, state, writer)?;
    } else if name.eq_ignore_ascii_case("SyzygyPath") {
        handle_syzygy_path(&value, state, writer)?;
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
    } else if name.eq_ignore_ascii_case("Move Overhead") {
        let Ok(requested) = value.parse::<i128>() else {
            writeln!(writer, "info string invalid Move Overhead value '{value}'")?;
            return Ok(());
        };
        state.move_overhead_millis = requested.clamp(
            MIN_MOVE_OVERHEAD_MILLIS as i128,
            MAX_MOVE_OVERHEAD_MILLIS as i128,
        ) as u64;
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

/// The `Clear Hash` button: wipes the transposition table and search history in place.
///
/// Uses the same parallel [`SearchPool::clear`] path `ucinewgame` does rather than
/// reallocating, so the table keeps its size and address. A failure is reported as an
/// `info string`, matching the `ucinewgame` convention.
fn handle_clear_hash<W: Write>(state: &EngineState, writer: &mut W) -> io::Result<()> {
    if let Err(error) = state
        .search_pool
        .clear(Arc::clone(&state.transposition_table))
    {
        writeln!(writer, "info string unable to clear Hash: {error}")?;
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
    handle_eval_file_with_automatic_resolution(value, state, writer, default_network_resolution)
}

fn handle_eval_file_with_automatic_resolution<W: Write>(
    value: &str,
    state: &mut EngineState,
    writer: &mut W,
    automatic_resolution: impl FnOnce() -> Result<SharedNetworkResolution, String>,
) -> io::Result<()> {
    let automatic = value.is_empty() || value.eq_ignore_ascii_case("<empty>");
    if automatic {
        return match automatic_resolution() {
            Ok(resolution) => {
                state.network = resolution.network;
                state.network_source = resolution.source;
                clear_eval_dependent_search_state(state, writer, "EvalFile")?;
                write_network_selection(writer, state, "automatic resolution")
            }
            Err(error) => writeln!(
                writer,
                "info string unable to load EvalFile automatic resolution: {error}; keeping {}",
                state.network_source
            ),
        };
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

fn handle_syzygy_path<W: Write>(
    value: &str,
    state: &mut EngineState,
    writer: &mut W,
) -> io::Result<()> {
    if value.is_empty() || value.eq_ignore_ascii_case("<empty>") {
        state.tablebases = None;
        return writeln!(writer, "info string Syzygy tablebases disabled");
    }

    // A path that fails to open leaves the previous tablebases in place: the engine
    // keeps searching with whatever it already had, and says so.
    match Tablebases::new(value) {
        Ok(tablebases) => {
            writeln!(
                writer,
                "info string Syzygy tablebases loaded: {} WDL and {} DTZ tables, up to {} pieces",
                tablebases.wdl_table_count(),
                tablebases.dtz_table_count(),
                tablebases.max_pieces()
            )?;
            state.tablebases = Some(Arc::new(tablebases));
            Ok(())
        }
        Err(error) => writeln!(writer, "info string unable to load SyzygyPath: {error}"),
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

/// Resolves an option name to its advertised spelling when it names a check-type
/// option.
///
/// Derived from `UCI_RESPONSE` so the set of options diagnosed here can never drift
/// from the set the engine actually advertises.
fn check_option_name(name: &str) -> Option<&'static str> {
    UCI_RESPONSE
        .iter()
        .filter_map(|line| line.strip_prefix("option name "))
        .find_map(|rest| {
            let (option, option_type) = rest.split_once(" type ")?;
            (option_type.starts_with("check") && name.eq_ignore_ascii_case(option))
                .then_some(option)
        })
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
    /// Stop once a mate in this many moves (or fewer) for the side to move is found.
    mate: Option<u32>,
    /// Raw `searchmoves` notation, resolved against the position when the search starts.
    searchmoves: Vec<String>,
    /// `go ponder`: search the predicted position, but defer `bestmove` until
    /// `ponderhit` (convert to a normal timed search) or `stop` (ponder miss).
    ponder: bool,
    infinite: bool,
    /// A recognized finite keyword had no parseable value.
    malformed_finite_value: bool,
    /// At least one side clock (`wtime` or `btime`) was supplied.
    clock_keyword_seen: bool,
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
        let mut finite_keyword_seen = false;
        let mut finite_value_parsed = false;
        let mut index = 0;
        while index < tokens.len() {
            let key = tokens[index];
            index += 1;
            if key.eq_ignore_ascii_case("infinite") {
                parameters.infinite = true;
                recognized = true;
                continue;
            }
            // `ponder` takes no value; the clock tokens travel alongside it and are
            // parsed as usual, because they describe the time budget the search will
            // run under once `ponderhit` converts it.
            if key.eq_ignore_ascii_case("ponder") {
                parameters.ponder = true;
                recognized = true;
                continue;
            }
            // `searchmoves` is a trailing list of moves, not a key/value pair.
            if key.eq_ignore_ascii_case("searchmoves") {
                while index < tokens.len() && !is_go_keyword(tokens[index]) {
                    parameters.searchmoves.push(tokens[index].to_string());
                    index += 1;
                }
                recognized = true;
                continue;
            }
            if key.eq_ignore_ascii_case("mate") {
                finite_keyword_seen = true;
                match tokens.get(index).copied().and_then(parse_go_value) {
                    Some(parsed) => {
                        index += 1;
                        parameters.mate = Some(parsed.min(u64::from(u32::MAX)) as u32);
                        finite_value_parsed = true;
                    }
                    None => {
                        ignored.push(key.to_string());
                        parameters.malformed_finite_value = true;
                    }
                }
                recognized = true;
                continue;
            }

            let clock_keyword =
                key.eq_ignore_ascii_case("wtime") || key.eq_ignore_ascii_case("btime");
            parameters.clock_keyword_seen |= clock_keyword;
            let finite_keyword = key.eq_ignore_ascii_case("depth")
                || key.eq_ignore_ascii_case("nodes")
                || key.eq_ignore_ascii_case("movetime")
                || clock_keyword;
            if finite_keyword {
                finite_keyword_seen = true;
                recognized = true;
            }
            let Some(value) = tokens.get(index).copied() else {
                ignored.push(key.to_string());
                parameters.malformed_finite_value |= finite_keyword;
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
                    Some(parsed) => {
                        *slot = Some(parsed);
                        finite_value_parsed |= finite_keyword;
                    }
                    None => {
                        ignored.push(format!("{key} {value}"));
                        parameters.malformed_finite_value |= finite_keyword;
                    }
                }
            } else if key.eq_ignore_ascii_case("depth") {
                index += 1;
                recognized = true;
                match parse_go_value(value) {
                    Some(parsed) => {
                        parameters.depth = Some(parsed.min(u64::from(u32::MAX)) as u32);
                        finite_value_parsed = true;
                    }
                    None => {
                        ignored.push(format!("{key} {value}"));
                        parameters.malformed_finite_value = true;
                    }
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

        if finite_keyword_seen && !finite_value_parsed && !parameters.infinite {
            parameters.nodes = Some(1);
        }
        // A `go` carrying no budget at all -- bare `go`, or one whose only arguments
        // were `ponder`/`searchmoves` -- is an unbounded analysis request. UCI
        // requires a `bestmove` for every `go`, so treat it as infinite and let `stop`
        // end it rather than silently ignoring the command and hanging the GUI.
        // `mate N` is a budget of its own: the search stops itself once the mate is
        // found, so it must stay non-infinite for the mate-score early exit to fire.
        if parameters.depth.is_none()
            && parameters.nodes.is_none()
            && parameters.movetime.is_none()
            && parameters.wtime.is_none()
            && parameters.btime.is_none()
            && parameters.mate.is_none()
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

    /// Resolves the `searchmoves` list against the current position.
    ///
    /// Uses the same notation parsing as `position ... moves`, so both castling
    /// dialects and case-insensitive promotions are accepted. Illegal or unknown
    /// moves are skipped, and a list that resolves to nothing places no restriction:
    /// answering from the full move set still beats never answering.
    fn root_moves(&self, position: &Position, chess960: bool) -> Option<Vec<Move>> {
        let resolved: Vec<Move> = self
            .searchmoves
            .iter()
            .filter_map(|notation| parse_uci_move(position, notation, chess960))
            .collect();
        if resolved.is_empty() {
            None
        } else {
            Some(resolved)
        }
    }

    fn search_limits(&self, position: &Position, move_overhead_millis: u64) -> SearchLimits {
        let (soft_time, hard_time, use_clock_management) = if self.infinite {
            (None, None, false)
        } else if self.depth.is_some() || self.nodes.is_some() {
            // A node or depth budget stays the primary stop, but a clock sent alongside
            // still supplies a hard safety deadline: a huge node budget on a slow node
            // rate must not flag the engine. Only the hard limit applies -- no soft
            // limit, no governor -- and `clock_limits` yields no deadline when no clock
            // tokens were sent, so clock-less bounded runs are exactly what they were.
            let hard_time = self.clock_limits(position, move_overhead_millis).1;
            (None, hard_time, false)
        } else if let Some(millis) = self.movetime {
            // Move Overhead is the engine's share of the sender's budget, so it comes
            // off the requested time before both limits are built; the floor keeps a
            // tiny request from becoming a zero budget.
            let budget = millis.saturating_sub(move_overhead_millis).max(1);
            (
                Some(Duration::from_millis(budget)),
                Some(Duration::from_millis(budget)),
                false,
            )
        } else {
            let (soft_time, hard_time) = self.clock_limits(position, move_overhead_millis);
            (soft_time, hard_time, soft_time.is_some())
        };
        let emergency_nodes = (self.malformed_finite_value || self.clock_keyword_seen)
            && !self.infinite
            && self.depth.is_none()
            && self.nodes.is_none()
            && self.movetime.is_none()
            && self.mate.is_none()
            && soft_time.is_none()
            && hard_time.is_none();
        SearchLimits {
            depth: if self.infinite { None } else { self.depth },
            nodes: if self.infinite {
                None
            } else if emergency_nodes {
                Some(1)
            } else {
                self.nodes
            },
            soft_time,
            hard_time,
            infinite: self.infinite,
            use_clock_management,
        }
    }

    fn clock_limits(
        &self,
        position: &Position,
        move_overhead_millis: u64,
    ) -> (Option<Duration>, Option<Duration>) {
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
            .saturating_sub(move_overhead_millis)
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
    write_bestmove(
        writer,
        position,
        pooled.result.best_move,
        &pooled.result.pv,
        chess960,
    )
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
            "info depth {} seldepth {} multipv 1 score {} nodes {} nps 0 hashfull {} tbhits {} time {} pv {}",
            result.depth,
            result.seldepth,
            score,
            result.nodes,
            result.hashfull,
            result.tbhits,
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
        "info depth {} seldepth {} multipv 1 score {} nodes {} nps {} hashfull {} tbhits {} time {} pv {}",
        result.depth,
        result.seldepth,
        score,
        result.nodes,
        nps,
        result.hashfull,
        result.tbhits,
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
    write_bestmove(writer, position, fallback, &[], chess960)
}

fn write_bestmove<W: Write>(
    writer: &mut W,
    position: &Position,
    best_move: Option<mf_core::Move>,
    pv: &[mf_core::Move],
    chess960: bool,
) -> io::Result<()> {
    let bestmove = best_move
        .map(|mv| format_uci_move(position, mv, chess960))
        // UCI represents "no legal move" with the null-move token. `0000`
        // is accepted by strict GUIs that reject the older `(none)` spelling.
        .unwrap_or_else(|| NULL_BESTMOVE.to_string());
    // The second PV move is the reply the engine expects, which is what a pondering
    // GUI ponders on. Emitted whenever the winning PV carries one -- unconditional
    // emission is spec-legal and a non-pondering GUI ignores the field -- but only
    // when the PV actually starts with the best move: a helper-selected result's PV
    // and best move always agree, so a mismatch would mean the suggestion belongs to
    // a different line than the move being played.
    if let Some(best) = best_move
        && pv.first() == Some(&best)
        && let Some(&reply) = pv.get(1)
    {
        let mut after_best = position.clone();
        after_best.make_move(best);
        return writeln!(
            writer,
            "bestmove {bestmove} ponder {}",
            format_uci_move(&after_best, reply, chess960)
        );
    }
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
        // `multipv` is the field GUIs read to decide which analysis row a line belongs
        // to, and some hide lines that lack it. Stockfish emits it unconditionally in
        // this slot, between `seldepth` and `score`. `tbhits` sits between `hashfull`
        // and `time`, matching the reference engine's keyword order.
        "info depth {} seldepth {} multipv {} score {} nodes {} nps {} hashfull {} tbhits {} time {} pv {}",
        iteration.depth,
        iteration.seldepth,
        iteration.multipv_index,
        score,
        iteration.nodes,
        nps,
        iteration.hashfull,
        iteration.tbhits,
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

/// The `d` debug command: an ASCII board diagram, the FEN, and the Zobrist key.
fn write_position_diagram<W: Write>(
    writer: &mut W,
    position: &Position,
    chess960: bool,
) -> io::Result<()> {
    for rank in (0..8).rev() {
        let mut row = String::with_capacity(16);
        for file in 0..8 {
            if file > 0 {
                row.push(' ');
            }
            let square = Square::new(rank * 8 + file).expect("file and rank are in 0..8");
            row.push(position.piece_at(square).map_or('.', piece_letter));
        }
        writeln!(writer, "{row}")?;
    }
    writeln!(writer, "Fen: {}", position.to_fen(chess960))?;
    writeln!(writer, "Key: {:016X}", position.zobrist().main())
}

fn piece_letter(piece: Piece) -> char {
    let letter = match piece.kind() {
        PieceKind::Pawn => 'p',
        PieceKind::Knight => 'n',
        PieceKind::Bishop => 'b',
        PieceKind::Rook => 'r',
        PieceKind::Queen => 'q',
        PieceKind::King => 'k',
    };
    match piece.color() {
        mf_core::Color::White => letter.to_ascii_uppercase(),
        mf_core::Color::Black => letter,
    }
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

    fn test_network_path() -> std::path::PathBuf {
        if let Some(path) = std::env::var_os("MF_NNUE_TEST_NET").map(std::path::PathBuf::from) {
            assert!(
                path.is_file(),
                "MF_NNUE_TEST_NET requires an existing network file: {}",
                path.display()
            );
            return path;
        }
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue")
    }

    struct BrokenWriter;

    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test pipe closed",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test pipe closed",
            ))
        }
    }

    #[test]
    fn search_output_failure_is_returned_from_run() {
        let input = io::Cursor::new(b"position startpos\ngo depth 8\nquit\n");
        let error = run(input, BrokenWriter).expect_err("search output failure must escape run");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn engine_construction_returns_automatic_network_errors() {
        let error = match EngineState::try_new_with_network(Err(
            "fixture automatic network failure".to_string(),
        )) {
            Ok(_) => panic!("construction must fail"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            "manifold requires an NNUE network to evaluate: fixture automatic network failure"
        );
    }

    #[test]
    fn automatic_evalfile_reset_keeps_the_previous_network_on_resolution_failure() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        let previous_network = Arc::clone(&state.network);
        let previous_source = state.network_source.clone();
        let mut output = Vec::new();

        handle_eval_file_with_automatic_resolution("<empty>", &mut state, &mut output, || {
            Err("fixture automatic network failure".to_string())
        })
        .expect("the diagnostic should be writable");

        assert!(Arc::ptr_eq(&state.network, &previous_network));
        assert_eq!(state.network_source, previous_source);
        assert!(
            String::from_utf8(output)
                .expect("diagnostic should be UTF-8")
                .starts_with("info string unable to load EvalFile automatic resolution:")
        );
    }

    #[test]
    fn ucinewgame_clears_the_transposition_table_without_changing_its_size() {
        let mut state = EngineState {
            search_pool: Arc::new(
                SearchPool::new(4).expect("four test search workers should start"),
            ),
            ..EngineState::try_new().expect("copied automatic network should load")
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
                tbhits: 0,
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
                "info depth 5 seldepth 8 multipv 1 score cp 42 nodes 1234 nps 61700 hashfull 17 tbhits 0 time 20 pv {}",
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
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
    fn multipv_spin_clamps_values_below_the_minimum() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");

        handle_setoption(
            "setoption name MultiPV value -100",
            &mut state,
            &mut Vec::new(),
        )
        .expect("MultiPV write should be writable");

        assert_eq!(state.search_options.multi_pv, 1);
    }

    #[test]
    fn multipv_spin_clamps_values_above_the_maximum() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");

        handle_setoption(
            "setoption name MultiPV value 1000",
            &mut state,
            &mut Vec::new(),
        )
        .expect("MultiPV write should be writable");

        assert_eq!(state.search_options.multi_pv, 256);
    }

    #[test]
    fn malformed_multipv_spin_preserves_the_existing_value_and_reports_it() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        state.search_options.multi_pv = 7;
        let mut output = Vec::new();

        handle_setoption(
            "setoption name MultiPV value banana",
            &mut state,
            &mut output,
        )
        .expect("invalid MultiPV diagnostic should be writable");

        assert_eq!(state.search_options.multi_pv, 7);
        assert_eq!(
            String::from_utf8(output).expect("protocol output should be UTF-8"),
            "info string invalid MultiPV value 'banana'\n"
        );
    }
    #[test]
    fn malformed_check_values_report_a_diagnostic_and_preserve_the_existing_value() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        state.search_options.use_rfp = false;

        // The numeric-bool dialect some GUIs and tuners speak is not silently dropped:
        // the write is reported as invalid so a tuner never measures a value the
        // engine never adopted.
        let mut output = Vec::new();
        handle_setoption(
            "setoption name UseRFP value banana",
            &mut state,
            &mut output,
        )
        .expect("invalid check diagnostic should be writable");
        assert!(!state.search_options.use_rfp);
        assert_eq!(
            String::from_utf8(output.clone()).expect("protocol output should be UTF-8"),
            "info string invalid UseRFP value 'banana' (expected true|false)\n"
        );

        handle_setoption("setoption name UseRFP value 1", &mut state, &mut output)
            .expect("numeric-bool diagnostic should be writable");
        assert_eq!(
            String::from_utf8(output.clone()).expect("protocol output should be UTF-8"),
            "info string invalid UseRFP value 'banana' (expected true|false)\n\
             info string invalid UseRFP value '1' (expected true|false)\n"
        );

        // A value token with nothing after it is still a rejected write, matching the
        // numeric options' handling of the same shape.
        handle_setoption("setoption name UseNMP value", &mut state, &mut output)
            .expect("empty check diagnostic should be writable");
        assert!(
            String::from_utf8(output.clone())
                .expect("protocol output should be UTF-8")
                .contains("info string invalid UseNMP value '' (expected true|false)")
        );

        // Absence of a `value` token stays silent: some GUIs send a bare
        // `setoption name X`, and it must not produce a diagnostic.
        output.clear();
        handle_setoption("setoption name UseNMP", &mut state, &mut output)
            .expect("bare setoption should be writable");
        handle_setoption("setoption name Clear Hash", &mut state, &mut output)
            .expect("button setoption should be writable");
        assert!(
            !String::from_utf8(output)
                .expect("protocol output should be UTF-8")
                .contains("invalid"),
            "commands without a value token must stay silent"
        );
    }

    #[test]
    fn interpolated_time_management_check_option_persists_and_rejects_malformed_values() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        assert!(!state.search_options.use_interpolated_time_management);

        handle_setoption(
            "SeToPtIoN NaMe UsEiNtErPoLaTeDtImEmAnAgEmEnT VaLuE TrUe",
            &mut state,
            &mut Vec::new(),
        )
        .expect("mixed-case check write should be accepted");
        assert!(state.search_options.use_interpolated_time_management);

        state
            .new_game()
            .expect("new game should clear search state without resetting options");
        assert!(state.search_options.use_interpolated_time_management);

        let mut output = Vec::new();
        handle_setoption(
            "setoption name UseInterpolatedTimeManagement value banana",
            &mut state,
            &mut output,
        )
        .expect("malformed check write should be writable");
        assert!(state.search_options.use_interpolated_time_management);
        assert_eq!(
            String::from_utf8(output).expect("protocol output should be UTF-8"),
            "info string invalid UseInterpolatedTimeManagement value 'banana' (expected true|false)\n"
        );

        handle_setoption(
            "setoption name UseInterpolatedTimeManagement value FALSE",
            &mut state,
            &mut Vec::new(),
        )
        .expect("false check write should be accepted");
        assert!(!state.search_options.use_interpolated_time_management);
    }

    #[test]
    fn search_again_depth_check_option_persists_and_rejects_malformed_values() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        assert!(!state.search_options.use_search_again_depth);

        handle_setoption(
            "SeToPtIoN NaMe UsEsEaRcHaGaInDePtH VaLuE TrUe",
            &mut state,
            &mut Vec::new(),
        )
        .expect("mixed-case check write should be accepted");
        assert!(state.search_options.use_search_again_depth);

        state
            .new_game()
            .expect("new game should clear search state without resetting options");
        assert!(state.search_options.use_search_again_depth);

        let mut output = Vec::new();
        handle_setoption(
            "setoption name UseSearchAgainDepth value banana",
            &mut state,
            &mut output,
        )
        .expect("malformed check write should be writable");
        assert!(state.search_options.use_search_again_depth);
        assert_eq!(
            String::from_utf8(output).expect("protocol output should be UTF-8"),
            "info string invalid UseSearchAgainDepth value 'banana' (expected true|false)\n"
        );

        handle_setoption(
            "setoption name UseSearchAgainDepth value FALSE",
            &mut state,
            &mut Vec::new(),
        )
        .expect("false check write should be accepted");
        assert!(!state.search_options.use_search_again_depth);
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
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        // ttMove history and corrplexity default to false, so setting them false
        // would prove nothing about parsing. Set them TRUE and assert they flipped.
        handle_setoption(
            "setoption name UseTtMoveHistory value true",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "SeToPtIoN NaMe UsElOwPlYhIsToRy VaLuE FaLsE",
            &mut state,
            &mut output,
        )
        .expect("setoption output should be writable");
        handle_setoption(
            "setoption name UseCorrplexity value true",
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
        assert!(state.search_options.use_tt_move_history);
        assert!(!state.search_options.use_low_ply_history);
        assert!(state.search_options.use_corrplexity);
    }

    #[test]
    fn the_evaluator_diagnostic_always_reports_nnue_and_its_source() {
        let state = EngineState::try_new().expect("copied automatic network should load");

        let diagnostic = active_evaluator_diagnostic(&state);

        assert!(
            diagnostic.starts_with("info string evaluation NNUE from "),
            "the engine has no non-NNUE evaluator to report: {diagnostic}"
        );
        assert!(diagnostic.contains("backend"));
    }

    #[test]
    fn bad_explicit_eval_file_preserves_the_previous_network_arc() {
        let valid = test_network_path();
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let path = test_network_path();
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let path = test_network_path();
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let path = test_network_path();
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let path = test_network_path();
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let mut state = EngineState::try_new().expect("copied automatic network should load");
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
        let path = test_network_path();
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
    fn movetime_search_limits_subtract_the_move_overhead() {
        let (parameters, _) =
            GoParameters::parse(&["movetime", "100"]).expect("movetime should parse");
        let limits = parameters.search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS);

        // The sender's budget is the requested time minus the engine's Move Overhead
        // share; spending the full request plus I/O latency is how movetime flags.
        let expected = Duration::from_millis(100 - TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.soft_time, Some(expected));
        assert_eq!(limits.hard_time, Some(expected));
        assert!(!limits.use_clock_management);
    }

    #[test]
    fn movetime_below_the_overhead_clamps_to_one_millisecond() {
        let (parameters, _) =
            GoParameters::parse(&["movetime", "5"]).expect("movetime should parse");
        let limits = parameters.search_limits(&Position::startpos(), 10);

        // A saturating subtraction alone would yield a zero budget, which no search
        // can satisfy; the floor keeps the engine answering.
        assert_eq!(limits.soft_time, Some(Duration::from_millis(1)));
        assert_eq!(limits.hard_time, Some(Duration::from_millis(1)));
        assert!(!limits.use_clock_management);
    }

    #[test]
    fn node_and_depth_limited_searches_keep_only_a_hard_clock_deadline() {
        for arguments in [
            &["nodes", "100000", "wtime", "2000", "btime", "2000"][..],
            &["depth", "10", "wtime", "2000", "btime", "2000"][..],
        ] {
            let (parameters, _) =
                GoParameters::parse(arguments).expect("bounded go with clock should parse");
            let limits = parameters.search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS);
            assert_eq!(
                limits.hard_time,
                parameters
                    .clock_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS)
                    .1,
                "{arguments:?}: the clock's hard limit becomes the safety deadline"
            );
            assert_eq!(
                limits.soft_time, None,
                "{arguments:?}: the soft-limit governor must stay out of bounded searches"
            );
            assert!(!limits.use_clock_management);
        }

        // Without clock tokens the bounded forms must be exactly what they were: no
        // deadline at all, so node- and depth-limited determinism is preserved.
        for arguments in [&["nodes", "100000"][..], &["depth", "10"][..]] {
            let (parameters, _) = GoParameters::parse(arguments).expect("bounded go should parse");
            let limits = parameters.search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS);
            assert_eq!(
                limits.hard_time, None,
                "{arguments:?}: no clock, no deadline"
            );
            assert_eq!(limits.soft_time, None);
        }
    }

    #[test]
    fn non_clock_go_forms_disable_clock_management() {
        for arguments in [
            &["depth", "5"][..],
            &["nodes", "1000"][..],
            &["infinite"][..],
            &["movetime", "100"][..],
        ] {
            let (parameters, _) =
                GoParameters::parse(arguments).expect("bounded go form should parse");
            assert!(
                !parameters
                    .search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS)
                    .use_clock_management,
                "{arguments:?} must not activate clock management"
            );
        }
    }

    #[test]
    fn invalid_finite_values_use_an_emergency_node_budget_instead_of_infinite_search() {
        for arguments in [
            &["depth", "banana"][..],
            &["nodes", "banana"][..],
            &["movetime", "banana"][..],
            &["mate", "banana"][..],
        ] {
            let (parameters, ignored) =
                GoParameters::parse(arguments).expect("finite keyword is recognized");
            assert_eq!(parameters.nodes, Some(1), "{arguments:?}");
            assert!(!parameters.infinite, "{arguments:?}");
            assert!(!ignored.is_empty(), "{arguments:?}");
        }
    }

    #[test]
    fn one_valid_finite_value_wins_over_an_invalid_sibling() {
        let (parameters, _) = GoParameters::parse(&["depth", "banana", "nodes", "20"]).unwrap();
        assert_eq!(parameters.nodes, Some(20));
        assert!(!parameters.infinite);
    }

    #[test]
    fn malformed_side_to_move_clock_uses_an_emergency_node_budget_for_either_color() {
        let white_to_move = Position::startpos();
        let mut black_to_move = white_to_move.clone();
        let e2e4 = parse_uci_move(&black_to_move, "e2e4", false).expect("e2e4 is legal");
        black_to_move.make_move(e2e4);

        for (arguments, position) in [
            (&["wtime", "banana", "btime", "1000"][..], &white_to_move),
            (&["btime", "banana", "wtime", "1000"][..], &black_to_move),
        ] {
            let (parameters, ignored) =
                GoParameters::parse(arguments).expect("clock arguments should parse");
            assert!(parameters.malformed_finite_value, "{arguments:?}");
            assert!(!ignored.is_empty(), "{arguments:?}");

            let limits = parameters.search_limits(position, TIME_OVERHEAD_MILLIS);
            assert_eq!(limits.nodes, Some(1), "{arguments:?}");
            assert_eq!(limits.depth, None, "{arguments:?}");
            assert_eq!(limits.soft_time, None, "{arguments:?}");
            assert_eq!(limits.hard_time, None, "{arguments:?}");
            assert!(!limits.infinite, "{arguments:?}");
        }
    }

    #[test]
    fn wrong_side_only_clock_uses_an_emergency_node_budget_for_either_color() {
        let white_to_move = Position::startpos();
        let mut black_to_move = white_to_move.clone();
        let e2e4 = parse_uci_move(&black_to_move, "e2e4", false).expect("e2e4 is legal");
        black_to_move.make_move(e2e4);

        for (arguments, position) in [
            (&["btime", "1000"][..], &white_to_move),
            (&["wtime", "1000"][..], &black_to_move),
        ] {
            let (parameters, ignored) =
                GoParameters::parse(arguments).expect("clock arguments should parse");
            assert!(parameters.clock_keyword_seen, "{arguments:?}");
            assert!(ignored.is_empty(), "{arguments:?}");

            let limits = parameters.search_limits(position, TIME_OVERHEAD_MILLIS);
            assert_eq!(limits.nodes, Some(1), "{arguments:?}");
            assert_eq!(limits.depth, None, "{arguments:?}");
            assert_eq!(limits.soft_time, None, "{arguments:?}");
            assert_eq!(limits.hard_time, None, "{arguments:?}");
            assert!(!limits.infinite, "{arguments:?}");
        }
    }

    #[test]
    fn usable_finite_sibling_wins_over_a_wrong_side_clock() {
        let white_to_move = Position::startpos();

        let (nodes, _) = GoParameters::parse(&["btime", "1000", "nodes", "20"]).unwrap();
        let limits = nodes.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, Some(20));

        let (depth, _) = GoParameters::parse(&["btime", "1000", "depth", "5"]).unwrap();
        let limits = depth.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.depth, Some(5));
        assert_eq!(limits.nodes, None);

        let (movetime, _) = GoParameters::parse(&["btime", "1000", "movetime", "100"]).unwrap();
        let limits = movetime.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);
        assert!(limits.soft_time.is_some());
        assert!(limits.hard_time.is_some());

        let (mate, _) = GoParameters::parse(&["btime", "1000", "mate", "3"]).unwrap();
        let limits = mate.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);

        let (clock, _) = GoParameters::parse(&["btime", "1000", "wtime", "1000"]).unwrap();
        let limits = clock.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);
        assert!(limits.soft_time.is_some());
    }

    #[test]
    fn wrong_side_clock_preserves_explicit_and_implicit_infinite_requests() {
        let white_to_move = Position::startpos();
        let (explicit, _) = GoParameters::parse(&["infinite", "btime", "1000"]).unwrap();
        let limits = explicit.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert!(limits.infinite);
        assert_eq!(limits.nodes, None);
        assert_eq!(limits.soft_time, None);
        assert_eq!(limits.hard_time, None);

        for arguments in [&[][..], &["ponder"][..], &["searchmoves", "e2e4"][..]] {
            let (parameters, _) = GoParameters::parse(arguments).unwrap();
            assert!(parameters.infinite, "{arguments:?}");
            let limits = parameters.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
            assert!(limits.infinite, "{arguments:?}");
            assert_eq!(limits.nodes, None, "{arguments:?}");
        }
    }

    #[test]
    fn usable_finite_sibling_wins_over_a_malformed_clock() {
        let white_to_move = Position::startpos();
        let mut black_to_move = white_to_move.clone();
        let e2e4 = parse_uci_move(&black_to_move, "e2e4", false).expect("e2e4 is legal");
        black_to_move.make_move(e2e4);

        let (nodes, _) =
            GoParameters::parse(&["wtime", "banana", "btime", "1000", "nodes", "20"]).unwrap();
        let limits = nodes.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, Some(20));

        let (depth, _) =
            GoParameters::parse(&["wtime", "banana", "btime", "1000", "depth", "5"]).unwrap();
        let limits = depth.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.depth, Some(5));
        assert_eq!(limits.nodes, None);

        let (movetime, _) =
            GoParameters::parse(&["wtime", "banana", "btime", "1000", "movetime", "100"]).unwrap();
        let limits = movetime.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);
        assert!(limits.soft_time.is_some());
        assert!(limits.hard_time.is_some());

        let (white_clock, _) = GoParameters::parse(&["btime", "banana", "wtime", "1000"]).unwrap();
        let limits = white_clock.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);
        assert!(limits.soft_time.is_some());

        let (black_clock, _) = GoParameters::parse(&["wtime", "banana", "btime", "1000"]).unwrap();
        let limits = black_clock.search_limits(&black_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);
        assert!(limits.soft_time.is_some());

        let (mate, _) =
            GoParameters::parse(&["wtime", "banana", "btime", "1000", "mate", "3"]).unwrap();
        let limits = mate.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert_eq!(limits.nodes, None);

        let (infinite, _) =
            GoParameters::parse(&["infinite", "wtime", "banana", "btime", "1000"]).unwrap();
        let limits = infinite.search_limits(&white_to_move, TIME_OVERHEAD_MILLIS);
        assert!(limits.infinite);
        assert_eq!(limits.nodes, None);
        assert_eq!(limits.soft_time, None);
        assert_eq!(limits.hard_time, None);
    }

    #[test]
    fn clock_limits_reserve_a_safety_margin_and_let_the_hard_limit_borrow_from_later_moves() {
        let (parameters, _) = GoParameters::parse(&["wtime", "60000", "winc", "600"])
            .expect("clock parameters should parse");
        let limits = parameters.search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS);
        let soft = limits.soft_time.expect("a clock implies a soft limit");
        let hard = limits.hard_time.expect("a clock implies a hard limit");
        assert!(limits.use_clock_management);

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
        let limits = parameters.search_limits(&position, TIME_OVERHEAD_MILLIS);
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
            far.search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS)
                .soft_time,
            clamped
                .search_limits(&Position::startpos(), TIME_OVERHEAD_MILLIS)
                .soft_time
        );
    }

    #[test]
    fn searchmoves_resolves_legal_moves_and_skips_illegal_ones() {
        let (parameters, _) = GoParameters::parse(&["searchmoves", "e2e4", "d2d4", "depth", "3"])
            .expect("searchmoves should parse");
        assert_eq!(parameters.searchmoves, ["e2e4", "d2d4"]);
        assert_eq!(parameters.depth, Some(3));

        let position = Position::startpos();
        let resolved = parameters
            .root_moves(&position, false)
            .expect("two legal moves should resolve");
        assert_eq!(resolved.len(), 2);
        assert!(
            resolved
                .iter()
                .all(|&mv| generate_legal_moves(&position).contains(&mv))
        );

        let (mixed, _) =
            GoParameters::parse(&["searchmoves", "e2e5", "zzzz", "e2e4", "depth", "3"])
                .expect("a partially illegal searchmoves list should parse");
        let resolved = mixed
            .root_moves(&position, false)
            .expect("the one legal move should resolve");
        assert_eq!(resolved.len(), 1);
        assert_eq!(format_uci_move(&position, resolved[0], false), "e2e4");
    }

    #[test]
    fn an_empty_or_fully_illegal_searchmoves_list_places_no_restriction() {
        let position = Position::startpos();
        let (empty, _) = GoParameters::parse(&["searchmoves", "depth", "3"])
            .expect("an empty searchmoves list should parse");
        assert!(empty.searchmoves.is_empty());
        assert_eq!(empty.root_moves(&position, false), None);

        let (illegal, _) = GoParameters::parse(&["searchmoves", "e2e5", "zzzz", "depth", "3"])
            .expect("an illegal searchmoves list should parse");
        assert_eq!(illegal.root_moves(&position, false), None);
    }

    #[test]
    fn searchmoves_accepts_chess960_castling_notation() {
        let position = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w HAha - 0 1", true)
            .expect("Chess960 castling FEN should parse");
        let (parameters, _) = GoParameters::parse(&["searchmoves", "e1h1", "depth", "3"])
            .expect("Chess960 searchmoves should parse");
        let resolved = parameters
            .root_moves(&position, true)
            .expect("king-takes-rook notation should resolve");
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].flag().is_castling());
    }

    #[test]
    fn go_mate_parses_its_move_count_and_stays_non_infinite() {
        let (parameters, ignored) =
            GoParameters::parse(&["mate", "3"]).expect("go mate should parse");
        assert_eq!(parameters.mate, Some(3));
        assert!(!parameters.infinite);
        assert!(ignored.is_empty());

        // A bare `mate` without a value is malformed: report it and use the same
        // emergency budget as every other malformed finite request.
        let (bare, ignored) = GoParameters::parse(&["mate"]).expect("bare mate should parse");
        assert_eq!(bare.mate, None);
        assert_eq!(bare.nodes, Some(1));
        assert!(!bare.infinite);
        assert_eq!(ignored, ["mate"]);
    }

    #[test]
    fn go_ponder_parses_alongside_clock_tokens() {
        let (parameters, ignored) = GoParameters::parse(&[
            "ponder", "wtime", "60000", "btime", "59000", "winc", "1000", "binc", "1000",
        ])
        .expect("go ponder with clocks should parse");
        assert!(parameters.ponder);
        assert!(
            !parameters.infinite,
            "the clock tokens are the budget the search converts to at ponderhit"
        );
        assert_eq!(parameters.wtime, Some(60_000));
        assert_eq!(parameters.btime, Some(59_000));
        assert!(ignored.is_empty());

        // A bare `go ponder` carries no budget: legal, and unbounded until
        // ponderhit/stop.
        let (bare, ignored) = GoParameters::parse(&["ponder"]).expect("bare ponder should parse");
        assert!(bare.ponder);
        assert!(bare.infinite);
        assert!(ignored.is_empty());
    }

    #[test]
    fn bestmove_gains_a_ponder_suggestion_when_the_pv_has_a_reply() {
        let position = Position::startpos();
        let first = parse_uci_move(&position, "e2e4", false).expect("e2e4 is legal");
        let mut after_first = position.clone();
        after_first.make_move(first);
        let reply = parse_uci_move(&after_first, "e7e5", false).expect("e7e5 is legal");

        let mut output = Vec::new();
        write_bestmove(&mut output, &position, Some(first), &[first, reply], false)
            .expect("bestmove should be writable");
        assert_eq!(
            String::from_utf8(output).expect("protocol output should be UTF-8"),
            "bestmove e2e4 ponder e7e5\n"
        );

        // A single-move PV has no reply to suggest.
        let mut output = Vec::new();
        write_bestmove(&mut output, &position, Some(first), &[first], false)
            .expect("bestmove should be writable");
        assert_eq!(
            String::from_utf8(output).expect("protocol output should be UTF-8"),
            "bestmove e2e4\n"
        );

        // A PV that does not begin with the played move belongs to a different line,
        // so no suggestion is attached.
        let other = parse_uci_move(&position, "d2d4", false).expect("d2d4 is legal");
        let mut output = Vec::new();
        write_bestmove(&mut output, &position, Some(other), &[first, reply], false)
            .expect("bestmove should be writable");
        assert_eq!(
            String::from_utf8(output).expect("protocol output should be UTF-8"),
            "bestmove d2d4\n"
        );
    }

    #[test]
    fn the_ponder_option_is_stored_on_the_engine_state() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        let mut output = Vec::new();
        assert!(!state.ponder_enabled);

        handle_setoption("setoption name Ponder value true", &mut state, &mut output)
            .expect("Ponder write should be writable");
        assert!(state.ponder_enabled);

        handle_setoption("setoption name Ponder value false", &mut state, &mut output)
            .expect("Ponder write should be writable");
        assert!(!state.ponder_enabled);
    }

    #[test]
    fn move_overhead_writes_are_clamped_to_the_advertised_bounds() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        let mut output = Vec::new();

        handle_setoption(
            "setoption name Move Overhead value 500",
            &mut state,
            &mut output,
        )
        .expect("Move Overhead write should be writable");
        assert_eq!(state.move_overhead_millis, 500);

        handle_setoption(
            "setoption name Move Overhead value 5000",
            &mut state,
            &mut output,
        )
        .expect("oversize Move Overhead should be writable");
        assert_eq!(state.move_overhead_millis, MAX_MOVE_OVERHEAD_MILLIS);

        handle_setoption(
            "setoption name Move Overhead value -3",
            &mut state,
            &mut output,
        )
        .expect("negative Move Overhead should be writable");
        assert_eq!(state.move_overhead_millis, MIN_MOVE_OVERHEAD_MILLIS);

        handle_setoption(
            "setoption name Move Overhead value banana",
            &mut state,
            &mut output,
        )
        .expect("invalid Move Overhead diagnostic should be writable");
        assert_eq!(state.move_overhead_millis, MIN_MOVE_OVERHEAD_MILLIS);
        assert!(
            String::from_utf8(output)
                .expect("protocol output should be UTF-8")
                .contains("info string invalid Move Overhead value 'banana'")
        );
    }

    #[test]
    fn move_overhead_is_subtracted_from_the_clock_budget() {
        let (parameters, _) = GoParameters::parse(&["wtime", "60000", "winc", "600"])
            .expect("clock parameters should parse");
        let limits = parameters.search_limits(&Position::startpos(), 500);

        // 60000 - 500 overhead - 1200 safety = 58300 available; 58300/30 + 450 = 2393.
        assert_eq!(limits.soft_time, Some(Duration::from_millis(2_393)));
    }

    #[test]
    fn clear_hash_button_without_a_value_clears_the_table_in_place() {
        let mut state = EngineState::try_new().expect("copied automatic network should load");
        let key = state.position.zobrist().main();
        let allocated_bytes = state.transposition_table.allocated_bytes();
        state.transposition_table.store(
            key,
            EntryData {
                best_move: generate_legal_moves(&state.position).first().copied(),
                score: 7,
                static_eval: 3,
                depth: 9,
                bound: Bound::Exact,
                age: 1,
                pv: false,
            },
        );
        let mut output = Vec::new();

        handle_setoption("setoption name Clear Hash", &mut state, &mut output)
            .expect("Clear Hash button should be writable");

        assert_eq!(state.transposition_table.probe(key), None);
        assert_eq!(state.transposition_table.allocated_bytes(), allocated_bytes);
        assert!(output.is_empty(), "a successful clear reports nothing");
    }

    #[test]
    fn the_position_diagram_round_trips_its_own_fen() {
        let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
        let position = Position::from_fen(fen, false).expect("test FEN should parse");
        let mut output = Vec::new();

        write_position_diagram(&mut output, &position, false)
            .expect("position diagram should be writable");

        let output = String::from_utf8(output).expect("protocol output should be UTF-8");
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 10, "eight ranks, the FEN, and the key");
        assert_eq!(lines[0], "r . . . k . . r");
        assert_eq!(lines[8], format!("Fen: {fen}"));
        assert_eq!(lines[9], format!("Key: {:016X}", position.zobrist().main()));
    }

    #[test]
    fn mtbench_parser_defaults_to_depth_ten_and_standard_thread_rows() {
        let options = parse_mtbench_arguments(std::iter::empty::<String>())
            .expect("default mtbench arguments should parse");

        assert_eq!(options.threads, [1, 2, 4, 8]);
        assert_eq!(options.depth, 10);
    }
}
