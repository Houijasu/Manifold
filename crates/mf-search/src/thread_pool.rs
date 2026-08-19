use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use mf_core::{Move, Position};
use mf_nnue::Network;
use mf_tb::Tablebases;

use crate::history::SharedHistory;
#[cfg(test)]
use crate::search::SearchAgainObservation;
use crate::search::{
    NodeCounter, PonderState, RootMoveInfo, WorkerParameters,
    search_worker_with_history_callback_options,
};
use crate::vote::select_best_result;
use crate::{IterationInfo, SearchLimits, SearchOptions, SearchResult, TranspositionTable};

pub struct PoolSearchResult {
    pub result: SearchResult,
    pub selected_worker: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError {
    Busy,
    WorkerUnavailable,
}

impl fmt::Display for PoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(formatter, "search pool is already active"),
            Self::WorkerUnavailable => write!(formatter, "a search worker is unavailable"),
        }
    }
}

impl std::error::Error for PoolError {}

pub struct SearchPool {
    workers: Vec<WorkerHandle>,
    node_counters: Arc<[NodeCounter]>,
    /// Published tablebase-hit counters, one per worker, mirroring `node_counters`.
    tb_hit_counters: Arc<[NodeCounter]>,
    /// Shared across every worker. Thread-private history tables are obsolete: one
    /// worker reuses the ordering knowledge another worker already paid for, and the
    /// table is sized to the pool so the per-thread capacity stays constant.
    history: Arc<SharedHistory>,
    active: AtomicBool,
    generation: AtomicU8,
    #[cfg(test)]
    search_again_test_control: Mutex<Option<SearchAgainTestControl>>,
}

#[cfg(test)]
struct SearchAgainTestControl {
    increase_depth: bool,
    initial_counters: Vec<u32>,
    observations: mpsc::Sender<SearchAgainObservation>,
}

impl SearchPool {
    pub fn new(thread_count: usize) -> io::Result<Self> {
        if thread_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "search pool requires at least one worker",
            ));
        }

        let mut workers = Vec::with_capacity(thread_count);
        for worker_id in 0..thread_count {
            let (sender, receiver) = mpsc::channel();
            let thread = thread::Builder::new()
                .name(format!("mf-search-{worker_id}"))
                // The search keeps its per-node state (PV lines, move lists, the move
                // picker's score arrays) on the stack, and `pvs` recurses to
                // `MAX_SEARCH_PLY` with same-ply re-entries for singular and null-move
                // verification searches. 8 MiB matches the reference engine's worker
                // stacks and leaves an order of magnitude of headroom over the
                // measured worst case.
                .stack_size(8 * 1024 * 1024)
                .spawn(move || worker_loop(receiver));
            match thread {
                Ok(handle) => workers.push(WorkerHandle {
                    sender,
                    handle: Some(handle),
                }),
                Err(error) => {
                    shutdown_workers(&mut workers);
                    return Err(error);
                }
            }
        }

        let node_counters = (0..thread_count)
            .map(|_| NodeCounter::new(0))
            .collect::<Vec<_>>()
            .into();
        let tb_hit_counters = (0..thread_count)
            .map(|_| NodeCounter::new(0))
            .collect::<Vec<_>>()
            .into();
        Ok(Self {
            workers,
            node_counters,
            tb_hit_counters,
            history: Arc::new(SharedHistory::new()),
            active: AtomicBool::new(false),
            generation: AtomicU8::new(0),
            #[cfg(test)]
            search_again_test_control: Mutex::new(None),
        })
    }

    pub fn thread_count(&self) -> usize {
        self.workers.len()
    }

    pub fn clear(&self, table: Arc<TranspositionTable>) -> Result<(), PoolError> {
        let _active = self.acquire()?;
        // A new game must not inherit the previous game's ordering statistics.
        self.history.clear();
        let cluster_count = table.cluster_count();
        let (done, acknowledgements) = mpsc::channel();
        let mut dispatched = 0;

        for (worker_id, worker) in self.workers.iter().enumerate() {
            let start_cluster = cluster_count * worker_id / self.workers.len();
            let end_cluster = cluster_count * (worker_id + 1) / self.workers.len();
            let command = WorkerCommand::Clear {
                table: Arc::clone(&table),
                start_cluster,
                end_cluster,
                done: done.clone(),
            };
            if worker.sender.send(command).is_err() {
                for _ in 0..dispatched {
                    let _ = acknowledgements.recv();
                }
                return Err(PoolError::WorkerUnavailable);
            }
            dispatched += 1;
        }
        drop(done);

        for _ in 0..dispatched {
            acknowledgements
                .recv()
                .map_err(|_| PoolError::WorkerUnavailable)?;
        }
        Ok(())
    }

    /// Lazy-SMP search: every worker searches, and their results are voted on.
    #[allow(clippy::too_many_arguments)]
    pub fn search_with_history_callback<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        network: Arc<Network>,
        tablebases: Option<Arc<Tablebases>>,
        root_moves: Option<Vec<Move>>,
        on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo),
    {
        self.search_impl(
            position,
            history,
            table,
            limits,
            options,
            stop,
            network,
            tablebases,
            root_moves,
            None,
            DispatchMode::AllWorkers,
            on_iteration,
            |_| {},
        )
    }

    /// Lazy-SMP search that also reports the root move being searched.
    ///
    /// Separate from [`Self::search_with_history_callback`] so the many callers that only
    /// want analysis lines keep their existing signature; `currmove` is a UCI display
    /// concern that only the protocol layer cares about. This is also the entry point
    /// that carries the `go ponder` latch, because pondering is a UCI protocol state and
    /// this is the method the protocol layer drives.
    #[allow(clippy::too_many_arguments)]
    pub fn search_with_history_progress<F, G>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        network: Arc<Network>,
        tablebases: Option<Arc<Tablebases>>,
        root_moves: Option<Vec<Move>>,
        ponder: Option<Arc<PonderState>>,
        on_iteration: F,
        on_current_move: G,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo),
        G: FnMut(&RootMoveInfo),
    {
        self.search_impl(
            position,
            history,
            table,
            limits,
            options,
            stop,
            network,
            tablebases,
            root_moves,
            ponder,
            DispatchMode::AllWorkers,
            on_iteration,
            on_current_move,
        )
    }

    /// Deterministic fixed-depth search on worker zero only, for reproducible node counts.
    #[allow(clippy::too_many_arguments)]
    pub fn search_fixed_depth_with_history_callback<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        network: Arc<Network>,
        tablebases: Option<Arc<Tablebases>>,
        root_moves: Option<Vec<Move>>,
        on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo),
    {
        self.search_impl(
            position,
            history,
            table,
            limits,
            options,
            stop,
            network,
            tablebases,
            root_moves,
            None,
            DispatchMode::WorkerZeroOnly,
            on_iteration,
            |_| {},
        )
    }

    /// Fixed-depth search across all workers, for measuring SMP scaling.
    #[allow(clippy::too_many_arguments)]
    pub fn search_fixed_depth_smp_with_history_callback<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        network: Arc<Network>,
        tablebases: Option<Arc<Tablebases>>,
        root_moves: Option<Vec<Move>>,
        on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo),
    {
        self.search_impl(
            position,
            history,
            table,
            limits,
            options,
            stop,
            network,
            tablebases,
            root_moves,
            None,
            DispatchMode::AllWorkers,
            on_iteration,
            |_| {},
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn search_impl<F, G>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        network: Arc<Network>,
        tablebases: Option<Arc<Tablebases>>,
        root_moves: Option<Vec<Move>>,
        ponder: Option<Arc<PonderState>>,
        dispatch_mode: DispatchMode,
        mut on_iteration: F,
        mut on_current_move: G,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo),
        G: FnMut(&RootMoveInfo),
    {
        let _active = self.acquire()?;
        stop.store(false, Ordering::Relaxed);
        let limits = if limits.infinite {
            SearchLimits {
                infinite: true,
                ..SearchLimits::default()
            }
        } else {
            limits
        };

        let participating_workers = match dispatch_mode {
            DispatchMode::WorkerZeroOnly => 1,
            DispatchMode::AllWorkers => self.workers.len(),
        };
        for counter in self.node_counters.iter() {
            counter.store(0, Ordering::Relaxed);
        }
        let counters = match dispatch_mode {
            DispatchMode::WorkerZeroOnly => Arc::<[NodeCounter]>::from([NodeCounter::new(0)]),
            DispatchMode::AllWorkers => Arc::clone(&self.node_counters),
        };
        for counter in counters.iter() {
            counter.store(0, Ordering::Relaxed);
        }
        let tb_hit_counters = match dispatch_mode {
            DispatchMode::WorkerZeroOnly => Arc::<[NodeCounter]>::from([NodeCounter::new(0)]),
            DispatchMode::AllWorkers => Arc::clone(&self.tb_hit_counters),
        };
        for counter in tb_hit_counters.iter() {
            counter.store(0, Ordering::Relaxed);
        }

        let generation = self
            .generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.wrapping_add(1) & 31)
            })
            .expect("generation update cannot fail")
            .wrapping_add(1)
            & 31;
        let history: Arc<[u64]> = history.into();
        #[cfg(test)]
        let search_again_test_control = self
            .search_again_test_control
            .lock()
            .expect("test control lock should not be poisoned")
            .take();
        #[cfg(test)]
        let increase_depth = Arc::new(AtomicBool::new(
            search_again_test_control
                .as_ref()
                .is_none_or(|control| control.increase_depth),
        ));
        #[cfg(not(test))]
        let increase_depth = Arc::new(AtomicBool::new(true));
        let (events, event_receiver) = mpsc::channel();
        let mut dispatched = 0;

        for worker_id in 0..participating_workers {
            let job = SearchJob {
                worker_id,
                generation,
                position: position.clone(),
                history: Arc::clone(&history),
                shared_history: Arc::clone(&self.history),
                table: Arc::clone(&table),
                limits,
                options,
                stop: Arc::clone(&stop),
                network: Arc::clone(&network),
                tablebases: tablebases.clone(),
                root_moves: root_moves.clone(),
                ponder: ponder.clone(),
                increase_depth: Arc::clone(&increase_depth),
                #[cfg(test)]
                search_again_counter: search_again_test_control
                    .as_ref()
                    .and_then(|control| control.initial_counters.get(worker_id))
                    .copied()
                    .unwrap_or(0),
                #[cfg(test)]
                search_again_observer: search_again_test_control
                    .as_ref()
                    .map(|control| control.observations.clone()),
                counters: Arc::clone(&counters),
                tb_hit_counters: Arc::clone(&tb_hit_counters),
                events: events.clone(),
            };
            if self.workers[worker_id]
                .sender
                .send(WorkerCommand::Search(Box::new(job)))
                .is_err()
            {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            dispatched += 1;
        }
        drop(events);

        let mut results = (0..participating_workers)
            .map(|_| None)
            .collect::<Vec<Option<SearchResult>>>();
        let finite_search = !limits.infinite;
        let mut completed = 0;
        let mut callback_panic = None;
        while completed < dispatched {
            match event_receiver.recv() {
                Ok(WorkerEvent::Progress(iteration)) => {
                    if callback_panic.is_none()
                        && let Err(payload) =
                            catch_unwind(AssertUnwindSafe(|| on_iteration(&iteration)))
                    {
                        callback_panic = Some(payload);
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                Ok(WorkerEvent::CurrentMove(root_move)) => {
                    if callback_panic.is_none()
                        && let Err(payload) =
                            catch_unwind(AssertUnwindSafe(|| on_current_move(&root_move)))
                    {
                        callback_panic = Some(payload);
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                Ok(WorkerEvent::Done { worker_id, result }) => {
                    if worker_id == 0
                        && finite_search
                        && matches!(dispatch_mode, DispatchMode::AllWorkers)
                    {
                        stop.store(true, Ordering::Relaxed);
                    }
                    let slot = results
                        .get_mut(worker_id)
                        .ok_or(PoolError::WorkerUnavailable)?;
                    if slot.replace(result).is_none() {
                        completed += 1;
                    }
                }
                Err(_) => return Err(PoolError::WorkerUnavailable),
            }
        }

        if dispatched != participating_workers {
            return Err(PoolError::WorkerUnavailable);
        }
        let results = results
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(PoolError::WorkerUnavailable)?;
        let selected_worker = match dispatch_mode {
            DispatchMode::WorkerZeroOnly => 0,
            // Only worker 0 owns the ordered MultiPV line set. Selecting a helper
            // would append its single-PV result after those lines and make bestmove
            // disagree with worker 0's line 1.
            DispatchMode::AllWorkers if options.multi_pv > 1 => 0,
            DispatchMode::AllWorkers => select_best_result(&results),
        };
        let worker_zero = &results[0];
        let mut result = results[selected_worker].clone();
        result.nodes = results.iter().fold(0_u64, |total, worker_result| {
            total.saturating_add(worker_result.nodes)
        });
        result.tbhits = results.iter().fold(0_u64, |total, worker_result| {
            total.saturating_add(worker_result.tbhits)
        });
        result.hashfull = table.hashfull_per_mille();
        result.elapsed = worker_zero.elapsed;
        result.iterations.clone_from(&worker_zero.iterations);

        let pooled = PoolSearchResult {
            result,
            selected_worker,
        };
        if let Some(payload) = callback_panic {
            resume_unwind(payload);
        }
        Ok(pooled)
    }

    fn acquire(&self) -> Result<ActiveGuard<'_>, PoolError> {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| PoolError::Busy)?;
        Ok(ActiveGuard {
            active: &self.active,
        })
    }
}

impl Drop for SearchPool {
    fn drop(&mut self) {
        shutdown_workers(&mut self.workers);
    }
}

struct ActiveGuard<'a> {
    active: &'a AtomicBool,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

struct WorkerHandle {
    sender: mpsc::Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
}

enum WorkerCommand {
    Search(Box<SearchJob>),
    Clear {
        table: Arc<TranspositionTable>,
        start_cluster: usize,
        end_cluster: usize,
        done: mpsc::Sender<()>,
    },
    Shutdown,
}

struct SearchJob {
    worker_id: usize,
    generation: u8,
    position: Position,
    history: Arc<[u64]>,
    shared_history: Arc<SharedHistory>,
    table: Arc<TranspositionTable>,
    limits: SearchLimits,
    options: SearchOptions,
    stop: Arc<AtomicBool>,
    network: Arc<Network>,
    tablebases: Option<Arc<Tablebases>>,
    root_moves: Option<Vec<Move>>,
    ponder: Option<Arc<PonderState>>,
    increase_depth: Arc<AtomicBool>,
    #[cfg(test)]
    search_again_counter: u32,
    #[cfg(test)]
    search_again_observer: Option<mpsc::Sender<SearchAgainObservation>>,
    counters: Arc<[NodeCounter]>,
    tb_hit_counters: Arc<[NodeCounter]>,
    events: mpsc::Sender<WorkerEvent>,
}

enum WorkerEvent {
    Progress(IterationInfo),
    CurrentMove(RootMoveInfo),
    Done {
        worker_id: usize,
        result: SearchResult,
    },
}

enum DispatchMode {
    WorkerZeroOnly,
    AllWorkers,
}

fn worker_loop(receiver: mpsc::Receiver<WorkerCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            WorkerCommand::Search(job) => {
                let events = job.events.clone();
                let mut parameters = WorkerParameters::new(
                    job.worker_id,
                    job.generation,
                    &job.counters,
                    &job.shared_history,
                    &job.network,
                )
                .with_increase_depth(&job.increase_depth);
                #[cfg(test)]
                if let Some(observer) = job.search_again_observer.as_ref() {
                    parameters =
                        parameters.with_search_again_test_state(job.search_again_counter, observer);
                }
                if let Some(tablebases) = job.tablebases.as_deref() {
                    parameters = parameters.with_tablebases(tablebases, &job.tb_hit_counters);
                }
                if let Some(root_moves) = job.root_moves.clone() {
                    parameters = parameters.with_root_moves(root_moves);
                }
                // Only worker 0 owns the clock, so only worker 0 needs the latch;
                // helpers already search without time limits.
                if let Some(ponder) = job.ponder.as_deref()
                    && job.worker_id == 0
                {
                    parameters = parameters.with_ponder(ponder);
                }
                // Only worker 0 reports `currmove`. Helpers search the same root moves in
                // their own order, so letting them report too would interleave
                // contradictory "now searching" updates for one depth.
                let currmove_events = job.events.clone();
                if job.worker_id == 0 {
                    parameters = parameters.with_root_move_reporter(move |root_move| {
                        let _ = currmove_events.send(WorkerEvent::CurrentMove(root_move));
                    });
                }
                let result = search_worker_with_history_callback_options(
                    &job.position,
                    &job.history,
                    &job.table,
                    job.limits,
                    job.options,
                    &job.stop,
                    parameters,
                    |iteration| {
                        if job.worker_id == 0 {
                            let _ = events.send(WorkerEvent::Progress(iteration.clone()));
                        }
                    },
                );
                let _ = job.events.send(WorkerEvent::Done {
                    worker_id: job.worker_id,
                    result,
                });
            }
            WorkerCommand::Clear {
                table,
                start_cluster,
                end_cluster,
                done,
            } => {
                table.clear_cluster_range(start_cluster, end_cluster);
                let _ = done.send(());
            }
            WorkerCommand::Shutdown => break,
        }
    }
}

fn shutdown_workers(workers: &mut [WorkerHandle]) {
    for worker in workers.iter() {
        let _ = worker.sender.send(WorkerCommand::Shutdown);
    }
    for worker in workers {
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn local_network() -> Option<Arc<Network>> {
        let explicit_path = std::env::var_os("MF_NNUE_TEST_NET");
        let path = explicit_path.clone().map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
            PathBuf::from,
        );
        if !path.is_file() {
            assert!(
                explicit_path.is_none(),
                "MF_NNUE_TEST_NET requires an existing network file: {}",
                path.display()
            );
            eprintln!(
                "SKIPPED: search-again dispatch test needs {}",
                path.display()
            );
            return None;
        }
        Some(Arc::new(Network::load(&path).unwrap_or_else(|error| {
            panic!("test NNUE network {}: {error}", path.display())
        })))
    }

    #[test]
    fn search_pool_dispatches_one_shared_decision_with_independent_worker_counters() {
        let Some(network) = local_network() else {
            return;
        };
        let pool = SearchPool::new(2).expect("test workers should start");
        let (observations, received) = mpsc::channel();
        *pool
            .search_again_test_control
            .lock()
            .expect("test control lock should not be poisoned") = Some(SearchAgainTestControl {
            increase_depth: false,
            initial_counters: vec![0, 4],
            observations,
        });
        let position = Position::startpos();
        let table = Arc::new(TranspositionTable::new(1).expect("test TT should allocate"));

        pool.search_with_history_callback(
            &position,
            &[position.repetition_key()],
            table,
            SearchLimits {
                depth: Some(1),
                // A zero soft budget makes worker 0's post-iteration
                // `increase_depth` store deterministically `false`
                // (`elapsed <= soft / 2` cannot hold), so worker 1 observes the
                // injected value no matter how the two workers interleave. With a
                // generous budget a slow scheduler can let worker 0 finish its
                // iteration and overwrite the flag before worker 1's first read.
                soft_time: Some(Duration::ZERO),
                hard_time: Some(Duration::from_secs(4)),
                use_clock_management: true,
                ..SearchLimits::default()
            },
            SearchOptions {
                use_search_again_depth: true,
                ..SearchOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
            network,
            None,
            None,
            |_| {},
        )
        .expect("test search should complete");

        let mut observations = received.try_iter().collect::<Vec<_>>();
        observations.sort_by_key(|observation| observation.worker_id);
        assert_eq!(observations.len(), 2);
        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.increase_depth)
                .collect::<Vec<_>>(),
            [false, false]
        );
        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.search_again_counter)
                .collect::<Vec<_>>(),
            [1, 5]
        );
        assert_eq!(
            observations[0].increase_depth_address,
            observations[1].increase_depth_address
        );
    }
}
