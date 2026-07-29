use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use mf_core::Position;

use crate::search::{WorkerParameters, search_worker_with_history_callback_options};
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
    node_counters: Arc<[AtomicU64]>,
    active: AtomicBool,
    generation: AtomicU8,
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
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>()
            .into();
        Ok(Self {
            workers,
            node_counters,
            active: AtomicBool::new(false),
            generation: AtomicU8::new(0),
        })
    }

    pub fn thread_count(&self) -> usize {
        self.workers.len()
    }

    pub fn clear(&self, table: Arc<TranspositionTable>) -> Result<(), PoolError> {
        let _active = self.acquire()?;
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

    #[allow(clippy::too_many_arguments)]
    pub fn search_with_history_callback_options<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
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
            DispatchMode::AllWorkers,
            on_iteration,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn search_fixed_depth_with_history_callback_options<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
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
            DispatchMode::WorkerZeroOnly,
            on_iteration,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn search_fixed_depth_smp_with_history_callback_options<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
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
            DispatchMode::AllWorkers,
            on_iteration,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn search_impl<F>(
        &self,
        position: &Position,
        history: &[u64],
        table: Arc<TranspositionTable>,
        limits: SearchLimits,
        options: SearchOptions,
        stop: Arc<AtomicBool>,
        dispatch_mode: DispatchMode,
        mut on_iteration: F,
    ) -> Result<PoolSearchResult, PoolError>
    where
        F: FnMut(&IterationInfo),
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
            DispatchMode::WorkerZeroOnly => Arc::<[AtomicU64]>::from([AtomicU64::new(0)]),
            DispatchMode::AllWorkers => Arc::clone(&self.node_counters),
        };
        for counter in counters.iter() {
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
        let (events, event_receiver) = mpsc::channel();
        let mut dispatched = 0;

        for worker_id in 0..participating_workers {
            let worker_limits = if worker_id == 0 {
                limits
            } else {
                SearchLimits {
                    soft_time: None,
                    hard_time: None,
                    ..limits
                }
            };
            let job = SearchJob {
                worker_id,
                generation,
                position: position.clone(),
                history: Arc::clone(&history),
                table: Arc::clone(&table),
                limits: worker_limits,
                options,
                stop: Arc::clone(&stop),
                counters: Arc::clone(&counters),
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
            DispatchMode::AllWorkers => select_best_result(&results),
        };
        let worker_zero = &results[0];
        let mut result = results[selected_worker].clone();
        result.nodes = results.iter().fold(0_u64, |total, worker_result| {
            total.saturating_add(worker_result.nodes)
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
    table: Arc<TranspositionTable>,
    limits: SearchLimits,
    options: SearchOptions,
    stop: Arc<AtomicBool>,
    counters: Arc<[AtomicU64]>,
    events: mpsc::Sender<WorkerEvent>,
}

enum WorkerEvent {
    Progress(IterationInfo),
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
                let result = search_worker_with_history_callback_options(
                    &job.position,
                    &job.history,
                    &job.table,
                    job.limits,
                    job.options,
                    &job.stop,
                    WorkerParameters::new(job.worker_id, job.generation, &job.counters),
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
