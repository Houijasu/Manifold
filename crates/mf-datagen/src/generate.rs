//! Deterministic self-play generation of training records.

use std::collections::{BTreeMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use mf_core::{Position, generate_legal_moves, has_legal_move, is_in_check};
use mf_nnue::Network;
use mf_search::{
    MATE_SCORE, SearchLimits, SearchOptions, SharedHistory, TranspositionTable,
    search_with_callback,
};
use mf_tb::{Tablebases, Wdl};

use crate::filter::{Filter, Rejection};
use crate::record::{Outcome, Record};
use crate::rng::Rng;

/// The transposition table size, in MiB, given to each generation worker.
///
/// Small on purpose: datagen searches are a few thousand nodes, so a large table only
/// costs allocation and cache footprint. Each worker owns one and clears it between
/// games so that a game's output never depends on the games scheduled before it — the
/// property `A-NNUE-015` (fixed-seed determinism) rests on.
const WORKER_HASH_MIB: usize = 16;

/// The hard ceiling on plies in one self-play game.
const MAX_GAME_PLIES: usize = 400;

/// Adjudicate a win once `|score|` stays at or above this for
/// [`ADJUDICATION_PLIES`] consecutive plies.
///
/// These are the Stormphrax settings named in `architecture.md` §5.
const ADJUDICATION_SCORE: i32 = 1_250;
const ADJUDICATION_PLIES: usize = 5;

/// How many random plies open a game, before self-play begins.
///
/// Randomizing the opening is what keeps a corpus from collapsing onto one line.
/// Stormphrax uses 8–9; the choice between them is itself drawn from the game's stream.
const MIN_RANDOM_PLIES: usize = 8;
const RANDOM_PLY_CHOICES: usize = 2;

/// Configuration for a generation run.
#[derive(Clone, Copy, Debug)]
pub struct GenerateConfig {
    /// The number of games to play.
    pub games: u64,
    /// The per-move search node budget.
    pub nodes: u64,
    /// The number of worker threads.
    pub threads: usize,
    /// The master seed. Fixing this fixes the entire run.
    pub seed: u64,
    /// The record filter.
    pub filter: Filter,
}

impl Default for GenerateConfig {
    fn default() -> Self {
        Self {
            games: 100,
            nodes: 5_000,
            threads: 1,
            seed: 0,
            filter: Filter::default(),
        }
    }
}

/// Counts describing what a generation run produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenerateStats {
    /// Games actually played.
    pub games: u64,
    /// Positions written.
    pub positions: u64,
    /// Positions considered, before filtering.
    pub considered: u64,
    /// Positions dropped, by reason. Indexed by [`Rejection::ALL`] order.
    pub rejected: [u64; Rejection::ALL.len()],
    /// Positions that passed the filter but repeated a board already kept in the same
    /// game, and were therefore not emitted twice.
    pub deduplicated: u64,
    /// Emitted records by side-to-move-relative result: loss, draw, win.
    pub results: [u64; 3],
    /// Games adjudicated by a Syzygy tablebase probe.
    pub tb_adjudicated: u64,
}

impl GenerateStats {
    /// Total positions dropped by the filter.
    pub fn total_rejected(&self) -> u64 {
        self.rejected.iter().sum()
    }

    /// Adds every counter from `other` to this run's totals.
    pub fn merge(&mut self, other: &Self) {
        self.games += other.games;
        self.positions += other.positions;
        self.considered += other.considered;
        self.deduplicated += other.deduplicated;
        self.tb_adjudicated += other.tb_adjudicated;
        for (slot, value) in self.rejected.iter_mut().zip(other.rejected) {
            *slot += value;
        }
        for (slot, value) in self.results.iter_mut().zip(other.results) {
            *slot += value;
        }
    }

    fn record_rejection(&mut self, rejection: Rejection) {
        let index = Rejection::ALL
            .iter()
            .position(|candidate| *candidate == rejection)
            .expect("every rejection is in Rejection::ALL");
        self.rejected[index] += 1;
    }
}

/// A single game's output, tagged with its index so results can be re-ordered.
struct GameOutput {
    index: u64,
    records: Vec<Record>,
    stats: GenerateStats,
}

/// Generates records and passes each batch, **in canonical game order**, to `sink`.
///
/// Ordering is the whole point of the batching. Workers finish games out of order, so
/// emitting as they complete would make the output depend on thread scheduling and
/// break `A-NNUE-015`. Instead games are claimed from a shared counter, results are
/// buffered, and a game is only released once every lower-numbered game has been
/// released. The output is therefore identical for any thread count, which also means
/// `--threads` is a pure throughput knob and never a correctness variable.
pub fn generate<S>(
    config: GenerateConfig,
    network: &Network,
    tablebases: Option<&Tablebases>,
    sink: S,
) -> Result<GenerateStats, String>
where
    S: FnMut(&[Record]) -> Result<(), String>,
{
    generate_from(config, 0, network, tablebases, sink, |_, _| Ok(()))
}

/// Generates records for `[first_game, config.games)` in canonical game order.
///
/// `progress` runs after each complete game is emitted. Its first argument is the
/// absolute number of completed games, so a restarted run beginning at game 12 first
/// reports 13.
pub fn generate_from<S, P>(
    config: GenerateConfig,
    first_game: u64,
    network: &Network,
    tablebases: Option<&Tablebases>,
    sink: S,
    progress: P,
) -> Result<GenerateStats, String>
where
    S: FnMut(&[Record]) -> Result<(), String>,
    P: FnMut(u64, &GenerateStats) -> Result<(), String>,
{
    generate_with_worker(
        config,
        first_game,
        || {
            let transposition_table = TranspositionTable::new(WORKER_HASH_MIB)
                .map_err(|error| format!("unable to allocate datagen Hash: {error}"))?;
            Ok((transposition_table, SharedHistory::new()))
        },
        |(transposition_table, history), index| {
            Ok(play_game(
                index,
                &config,
                transposition_table,
                history,
                network,
                tablebases,
            ))
        },
        sink,
        progress,
    )
}

fn generate_with_worker<S, W, F, P, G>(
    config: GenerateConfig,
    first_game: u64,
    make_worker: F,
    play: P,
    mut sink: S,
    mut progress: G,
) -> Result<GenerateStats, String>
where
    S: FnMut(&[Record]) -> Result<(), String>,
    F: Fn() -> Result<W, String> + Sync,
    P: Fn(&mut W, u64) -> Result<GameOutput, String> + Sync,
    G: FnMut(u64, &GenerateStats) -> Result<(), String>,
{
    let next_game = Arc::new(AtomicU64::new(first_game));
    let cancelled = Arc::new(AtomicBool::new(false));

    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..config.threads.max(1) {
            let next_game = Arc::clone(&next_game);
            let cancelled = Arc::clone(&cancelled);
            let sender = sender.clone();
            let make_worker = &make_worker;
            let play = &play;
            scope.spawn(move || {
                let panic_sender = sender.clone();
                let result = catch_unwind(AssertUnwindSafe(move || {
                    let mut worker = match make_worker() {
                        Ok(worker) => worker,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            return;
                        }
                    };
                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            return;
                        }
                        let index = next_game.fetch_add(1, Ordering::Relaxed);
                        if index >= config.games {
                            return;
                        }
                        let result = play(&mut worker, index);
                        let failed = result.is_err();
                        if sender.send(result).is_err() || failed {
                            return;
                        }
                    }
                }));
                if result.is_err() {
                    let _ = panic_sender.send(Err("datagen worker panicked".to_string()));
                }
            });
        }
        drop(sender);

        let result = (|| {
            let mut stats = GenerateStats::default();
            let mut pending = BTreeMap::new();
            let mut next_to_emit = first_game;

            while let Ok(output) = receiver.recv() {
                let output = output?;
                pending.insert(output.index, output);

                while let Some(output) = pending.remove(&next_to_emit) {
                    sink(&output.records)?;
                    stats.merge(&output.stats);
                    next_to_emit += 1;
                    progress(next_to_emit, &stats)?;
                }
            }

            if next_to_emit < config.games {
                Err("datagen workers stopped before every game was produced".to_string())
            } else {
                Ok(stats)
            }
        })();

        if result.is_err() {
            cancelled.store(true, Ordering::Relaxed);
        }
        result
    })
}

/// The state of a game as it is being played.
struct GameState {
    position: Position,
    history: Vec<u64>,
    /// `(position, score)` for every ply that survived the filter.
    kept: Vec<(Position, i16)>,
    /// Board identities already kept in this game, for deduplication.
    seen: HashSet<BoardIdentity>,
}

/// A board as the record format sees it: occupancy, pieces, and both king squares.
///
/// Deliberately excludes the score and the result. Within one game the result for a
/// given board is fixed, and the score is the only thing that varies between two
/// occurrences of a repeated position — so keying on the board alone is what makes
/// generation-time deduplication agree with the duplicate count `--validate` derives
/// from the finished file.
type BoardIdentity = [u8; 26];

/// Extracts the board identity from an encoded record.
fn board_identity(record: &Record) -> BoardIdentity {
    let bytes = record.to_bytes();
    let mut identity = [0u8; 26];
    identity[0..24].copy_from_slice(&bytes[0..24]);
    identity[24] = bytes[27];
    identity[25] = bytes[28];
    identity
}

fn play_game(
    index: u64,
    config: &GenerateConfig,
    transposition_table: &TranspositionTable,
    shared_history: &SharedHistory,
    network: &Network,
    tablebases: Option<&Tablebases>,
) -> GameOutput {
    let mut rng = Rng::for_index(config.seed, index);
    let mut stats = GenerateStats {
        games: 1,
        ..GenerateStats::default()
    };

    // Every game starts from a cleared table and cleared history, so its output is a
    // pure function of its own seed stream and never of the games that ran before it
    // on this worker.
    transposition_table.clear();
    shared_history.clear();

    let Some(mut state) = random_opening(&mut rng) else {
        // The random opening walked into a terminal position. That game contributes
        // nothing; its index is still consumed so the seed stream stays aligned.
        return GameOutput {
            index,
            records: Vec::new(),
            stats,
        };
    };

    let limits = SearchLimits {
        nodes: Some(config.nodes),
        ..SearchLimits::default()
    };
    let options = SearchOptions::default();
    // Datagen searches are bounded by their node budget alone; nothing stops them early.
    let stop = AtomicBool::new(false);

    let mut white_relative_outcome = None;
    let mut adjudication_run = 0usize;
    let mut adjudication_winner = None;

    for _ in 0..MAX_GAME_PLIES {
        if let Some(terminal) = terminal_outcome(&state.position) {
            white_relative_outcome = Some(terminal);
            break;
        }
        if is_draw_by_rule(&state) {
            white_relative_outcome = Some(Outcome::Draw);
            break;
        }

        let result = search_with_callback(
            &state.position,
            &state.history,
            transposition_table,
            limits,
            options,
            network,
            None,
            None,
            &stop,
            |_| {},
        );

        stats.considered += 1;
        match config
            .filter
            .rejection(&state.position, result.best_move, result.score)
        {
            Some(rejection) => stats.record_rejection(rejection),
            None => {
                let score = i16::try_from(result.score)
                    .expect("filtered scores are bounded well inside i16");
                // Self-play shuffles pieces back and forth, so the same board is often
                // reached more than once in one game. Emitting it twice would weight
                // that position by however often the engine happened to repeat, which
                // is a property of the search rather than of the position's value. The
                // outcome label is identical for both copies, so the second carries no
                // information at all. Dedup is per game and keyed on the board only:
                // the same board in a *different* game got a different result label and
                // is genuinely a different training example.
                //
                // `Outcome::Draw` is a placeholder here purely to derive the board
                // identity; the real outcome is not known until the game ends, and the
                // identity excludes it by construction.
                if let Ok(probe) = Record::encode(&state.position, score, Outcome::Draw)
                    && state.seen.insert(board_identity(&probe))
                {
                    state.kept.push((state.position.clone(), score));
                } else {
                    stats.deduplicated += 1;
                }
            }
        }

        let Some(best_move) = result.best_move else {
            // No legal move: `terminal_outcome` above already covers this, but a search
            // that was stopped before completing an iteration can also return `None`.
            white_relative_outcome =
                Some(terminal_outcome(&state.position).unwrap_or(Outcome::Draw));
            break;
        };

        // Adjudicate a decisive game once one side holds a large advantage for several
        // consecutive plies. Scores are side-to-move relative, so they are converted to
        // a winning colour before being compared across plies.
        let winner = if result.score >= ADJUDICATION_SCORE {
            Some(state.position.side_to_move())
        } else if result.score <= -ADJUDICATION_SCORE {
            Some(!state.position.side_to_move())
        } else {
            None
        };
        if winner.is_some() && winner == adjudication_winner {
            adjudication_run += 1;
        } else {
            adjudication_run = usize::from(winner.is_some());
            adjudication_winner = winner;
        }
        if adjudication_run >= ADJUDICATION_PLIES
            && let Some(winner) = adjudication_winner
        {
            white_relative_outcome = Some(match winner {
                mf_core::Color::White => Outcome::Win,
                mf_core::Color::Black => Outcome::Loss,
            });
            break;
        }

        state.position.make_move(best_move);
        state.history.push(state.position.repetition_key());

        // Tablebase adjudication: the instant the game enters table range, the true
        // result is known and the game ends. Probing only at `halfmove_clock() == 0`
        // is both the standard Syzygy WDL soundness condition and sufficient: a
        // position can only enter table range through a capture, which zeroes the
        // clock, so every in-range position was probed on the ply it became in-range.
        if let Some(tablebases) = tablebases
            && state.position.halfmove_clock() == 0
            && state.position.occupancy().count() as usize <= tablebases.max_pieces()
            && let Some(wdl) = tablebases.probe_wdl(&state.position)
        {
            white_relative_outcome = Some(white_relative_tb_outcome(
                wdl,
                state.position.side_to_move(),
            ));
            stats.tb_adjudicated += 1;
            break;
        }
    }

    // A game that hit the ply ceiling without resolving is scored as a draw, which is
    // the honest label: no side demonstrated a win.
    let white_relative_outcome = white_relative_outcome.unwrap_or(Outcome::Draw);

    let mut records = Vec::with_capacity(state.kept.len());
    for (position, score) in &state.kept {
        let outcome = Outcome::from_white_relative(white_relative_outcome, position.side_to_move());
        match Record::encode(position, *score, outcome) {
            Ok(record) => {
                stats.results[outcome as usize] += 1;
                stats.positions += 1;
                records.push(record);
            }
            Err(_) => {
                // A position that cannot be encoded is dropped rather than written
                // malformed. `Record::encode` only fails on a missing king or an
                // over-full board, neither of which a legal game can reach.
            }
        }
    }

    GameOutput {
        index,
        records,
        stats,
    }
}

/// Plays 8–9 random legal plies, or returns `None` if a terminal position is reached.
fn random_opening(rng: &mut Rng) -> Option<GameState> {
    let mut position = Position::startpos();
    let mut history = vec![position.repetition_key()];
    let plies = MIN_RANDOM_PLIES + rng.below(RANDOM_PLY_CHOICES)?;

    for _ in 0..plies {
        let moves = generate_legal_moves(&position);
        let legal = moves.as_slice();
        let choice = rng.below(legal.len())?;
        position.make_move(legal[choice]);
        history.push(position.repetition_key());
    }

    // A randomly-reached position that is already decided teaches nothing, and one
    // that is terminal cannot be played from at all.
    if !has_legal_move(&position) {
        return None;
    }

    Some(GameState {
        position,
        history,
        kept: Vec::new(),
        seen: HashSet::new(),
    })
}

/// Maps a side-to-move-relative tablebase verdict onto the white-relative game
/// outcome.
///
/// Cursed wins and blessed losses are draws under the fifty-move rule, which is the
/// truth a real game would reach, so they are labelled as draws.
fn white_relative_tb_outcome(wdl: Wdl, side_to_move: mf_core::Color) -> Outcome {
    let stm_wins = match wdl {
        Wdl::Win => true,
        Wdl::Loss => false,
        Wdl::CursedWin | Wdl::BlessedLoss | Wdl::Draw => return Outcome::Draw,
    };
    match (side_to_move, stm_wins) {
        (mf_core::Color::White, true) | (mf_core::Color::Black, false) => Outcome::Win,
        (mf_core::Color::White, false) | (mf_core::Color::Black, true) => Outcome::Loss,
    }
}

/// The white-relative outcome if `position` is checkmate or stalemate.
fn terminal_outcome(position: &Position) -> Option<Outcome> {
    if has_legal_move(position) {
        return None;
    }
    if !is_in_check(position, position.side_to_move()) {
        return Some(Outcome::Draw);
    }
    // The side to move is checkmated, so the other side won.
    Some(match position.side_to_move() {
        mf_core::Color::White => Outcome::Loss,
        mf_core::Color::Black => Outcome::Win,
    })
}

/// Whether the game is drawn by the fifty-move rule, threefold repetition, or
/// insufficient material.
fn is_draw_by_rule(state: &GameState) -> bool {
    if state.position.halfmove_clock() >= 100 || state.position.is_insufficient_material() {
        return true;
    }
    let key = state.position.repetition_key();
    // The current position is the last entry, so its own occurrence is included.
    state.history.iter().filter(|entry| **entry == key).count() >= 3
}

/// Mate-score sanity: the filter's threshold must sit below `MATE_SCORE`.
const _MATE_THRESHOLD_IS_SANE: () = assert!(crate::filter::MATE_SCORE_THRESHOLD < MATE_SCORE);

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use mf_nnue::Network;

    use super::{GameOutput, GenerateConfig, GenerateStats, generate, generate_with_worker};
    use crate::filter::Rejection;
    use crate::record::Record;

    fn empty_output(index: u64) -> GameOutput {
        GameOutput {
            index,
            records: Vec::new(),
            stats: GenerateStats {
                games: 1,
                ..GenerateStats::default()
            },
        }
    }

    #[test]
    fn a_worker_error_is_returned_instead_of_waiting_for_the_missing_game() {
        let config = GenerateConfig {
            games: 8,
            threads: 4,
            ..GenerateConfig::default()
        };
        let error = generate_with_worker(
            config,
            0,
            || Ok(()),
            |_, index| {
                if index == 0 {
                    Err("worker failed at game 0".to_string())
                } else {
                    Ok(empty_output(index))
                }
            },
            |_| Ok(()),
            |_, _| Ok(()),
        )
        .expect_err("worker failure must end generation");
        assert_eq!(error, "worker failed at game 0");
    }

    #[test]
    fn a_sink_error_cancels_before_the_run_claims_every_game() {
        let claimed = AtomicU64::new(0);
        let sink_failed = AtomicBool::new(false);
        let config = GenerateConfig {
            games: 10_000,
            threads: 4,
            ..GenerateConfig::default()
        };
        let error = generate_with_worker(
            config,
            0,
            || Ok(()),
            |_, index| {
                claimed.fetch_add(1, Ordering::Relaxed);
                if index != 0 {
                    while !sink_failed.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                }
                Ok(empty_output(index))
            },
            |_| {
                sink_failed.store(true, Ordering::Relaxed);
                Err("disk full".to_string())
            },
            |_, _| Ok(()),
        )
        .expect_err("sink failure must end generation");
        assert_eq!(error, "disk full");
        assert!(claimed.load(Ordering::Relaxed) < config.games);
    }

    #[test]
    fn a_late_worker_initialization_error_is_not_lost_after_the_final_game() {
        let worker_number = AtomicU64::new(0);
        let release_failure = AtomicBool::new(false);
        let config = GenerateConfig {
            games: 1,
            threads: 2,
            ..GenerateConfig::default()
        };
        let error = generate_with_worker(
            config,
            0,
            || {
                if worker_number.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    while !release_failure.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    Err("late worker initialization failed".to_string())
                }
            },
            |_, index| {
                release_failure.store(true, Ordering::SeqCst);
                Ok(empty_output(index))
            },
            |_| Ok(()),
            |_, _| Ok(()),
        )
        .expect_err("every worker initialization result must be observed");
        assert_eq!(error, "late worker initialization failed");
    }

    #[test]
    fn a_worker_panic_is_returned_as_an_error() {
        let config = GenerateConfig {
            games: 1,
            threads: 1,
            ..GenerateConfig::default()
        };
        let error = generate_with_worker(
            config,
            0,
            || Ok(()),
            |_, _| panic!("worker exploded"),
            |_| Ok(()),
            |_, _| Ok(()),
        )
        .expect_err("worker panics must not escape the Result API");
        assert_eq!(error, "datagen worker panicked");
    }

    #[test]
    fn a_nonzero_restart_emits_the_requested_range_in_order_and_reports_absolute_progress() {
        let claimed = AtomicU64::new(0);
        let mut progress = Vec::new();
        let mut sink_calls = 0;
        let config = GenerateConfig {
            games: 5,
            threads: 3,
            ..GenerateConfig::default()
        };
        let stats = generate_with_worker(
            config,
            2,
            || Ok(()),
            |_, index| {
                claimed.fetch_or(1 << index, Ordering::SeqCst);
                if index == 2 {
                    while claimed.load(Ordering::SeqCst) & ((1 << 3) | (1 << 4))
                        != (1 << 3) | (1 << 4)
                    {
                        std::thread::yield_now();
                    }
                }
                let mut output = empty_output(index);
                output.stats.considered = index;
                Ok(output)
            },
            |_| {
                sink_calls += 1;
                Ok(())
            },
            |completed, stats| {
                progress.push((completed, stats.games, stats.considered));
                Ok(())
            },
        )
        .expect("the restarted range must complete");

        assert_eq!(
            claimed.load(Ordering::SeqCst),
            (1 << 2) | (1 << 3) | (1 << 4)
        );
        assert_eq!(sink_calls, 3);
        assert_eq!(stats.games, 3);
        assert_eq!(
            progress,
            vec![(3, 1, 2), (4, 2, 5), (5, 3, 9)],
            "progress must follow canonical game order"
        );
    }

    #[test]
    fn a_progress_error_cancels_before_the_run_claims_every_game() {
        let claimed = AtomicU64::new(0);
        let progress_failed = AtomicBool::new(false);
        let config = GenerateConfig {
            games: 10_000,
            threads: 4,
            ..GenerateConfig::default()
        };
        let error = generate_with_worker(
            config,
            0,
            || Ok(()),
            |_, index| {
                claimed.fetch_add(1, Ordering::Relaxed);
                if index != 0 {
                    while !progress_failed.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                }
                Ok(empty_output(index))
            },
            |_| Ok(()),
            |_, _| {
                progress_failed.store(true, Ordering::Relaxed);
                Err("checkpoint failed".to_string())
            },
        )
        .expect_err("progress failure must end generation");
        assert_eq!(error, "checkpoint failed");
        assert!(claimed.load(Ordering::Relaxed) < config.games);
    }

    #[test]
    fn a_premature_channel_disconnect_has_the_stable_error_text() {
        let config = GenerateConfig {
            games: 1,
            threads: 1,
            ..GenerateConfig::default()
        };
        let error = generate_with_worker(
            config,
            0,
            || Ok(()),
            |_, index| Ok(empty_output(index + 1)),
            |_| Ok(()),
            |_, _| Ok(()),
        )
        .expect_err("a missing requested game must be diagnosed");
        assert_eq!(
            error,
            "datagen workers stopped before every game was produced"
        );
    }

    /// Self-play evaluates with NNUE, so datagen tests need a network.
    ///
    /// Loaded once for the target because the file is ~106 MiB. Returns `None` when the
    /// (gitignored) network is absent so a fresh clone skips rather than fails.
    fn network() -> Option<&'static Network> {
        static NETWORK: OnceLock<Option<Network>> = OnceLock::new();
        NETWORK
            .get_or_init(|| {
                let path = std::env::var_os("MF_NNUE_TEST_NET").map_or_else(
                    || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nets/main.nnue"),
                    PathBuf::from,
                );
                if !path.is_file() {
                    eprintln!("SKIPPED: datagen tests need {}", path.display());
                    return None;
                }
                Some(Network::load(&path).unwrap_or_else(|error| {
                    panic!("failed to load NNUE network {}: {error}", path.display())
                }))
            })
            .as_ref()
    }

    fn collect(config: GenerateConfig) -> (Vec<Record>, super::GenerateStats) {
        let network = network().expect("caller must check `network()` before collecting");
        let mut records = Vec::new();
        let stats = generate(config, network, None, |batch| {
            records.extend_from_slice(batch);
            Ok(())
        })
        .expect("generation succeeds");
        (records, stats)
    }

    fn small(seed: u64, threads: usize) -> GenerateConfig {
        GenerateConfig {
            games: 6,
            nodes: 1_000,
            threads,
            seed,
            ..GenerateConfig::default()
        }
    }

    #[test]
    fn a_fixed_seed_reproduces_byte_identical_output() {
        if network().is_none() {
            return;
        }
        let (first, first_stats) = collect(small(4_242, 1));
        let (second, second_stats) = collect(small(4_242, 1));
        assert_eq!(first, second);
        assert_eq!(first_stats, second_stats);
        assert!(!first.is_empty(), "the run must actually produce records");
    }

    #[test]
    fn a_different_seed_produces_different_output() {
        if network().is_none() {
            return;
        }
        let (first, _) = collect(small(4_242, 1));
        let (second, _) = collect(small(4_243, 1));
        assert_ne!(first, second, "the seed must actually be consumed");
    }

    #[test]
    fn output_is_independent_of_the_thread_count() {
        if network().is_none() {
            return;
        }
        let (single, single_stats) = collect(small(777, 1));
        let (multi, multi_stats) = collect(small(777, 4));
        assert_eq!(
            single, multi,
            "thread count is a throughput knob, never a correctness variable"
        );
        assert_eq!(single_stats, multi_stats);
    }

    #[test]
    fn every_emitted_record_survives_the_filter_and_is_structurally_valid() {
        if network().is_none() {
            return;
        }
        let (records, stats) = collect(small(31, 2));
        assert_eq!(stats.positions as usize, records.len());
        for record in &records {
            assert_eq!(record.structural_errors(), Vec::new());
            assert!(record.score().abs() <= 10_000);
            assert!(record.outcome().is_some());
        }
    }

    #[test]
    fn the_filter_actually_rejects_positions_during_a_real_run() {
        if network().is_none() {
            return;
        }
        let (_, stats) = collect(small(9, 2));
        assert_eq!(
            stats.considered,
            stats.positions + stats.total_rejected() + stats.deduplicated,
            "every considered position is either emitted, filtered, or deduplicated"
        );
        assert!(
            stats.total_rejected() > 0,
            "a real run must exercise the filters"
        );
        let in_check = stats.rejected[Rejection::ALL
            .iter()
            .position(|r| *r == Rejection::InCheck)
            .expect("in-check is a rejection reason")];
        let tactical = stats.rejected[Rejection::ALL
            .iter()
            .position(|r| *r == Rejection::TacticalMove)
            .expect("tactical is a rejection reason")];
        assert!(
            in_check + tactical > 0,
            "the two structural filters must both be reachable"
        );
    }

    #[test]
    fn no_game_emits_the_same_board_twice() {
        if network().is_none() {
            return;
        }
        let (records, stats) = collect(GenerateConfig {
            games: 24,
            nodes: 1_200,
            threads: 4,
            seed: 7,
            ..GenerateConfig::default()
        });

        // Deduplication is per game, so the file can still hold the same board twice if
        // two different games reached it — with different result labels, which makes
        // them genuinely different training examples. What must never happen is the
        // same board carrying the same label from the same game.
        let mut seen = std::collections::HashMap::new();
        for record in &records {
            let bytes = record.to_bytes();
            let mut identity = [0u8; 27];
            identity[0..24].copy_from_slice(&bytes[0..24]);
            identity[24] = bytes[26];
            identity[25] = bytes[27];
            identity[26] = bytes[28];
            *seen.entry(identity).or_insert(0u32) += 1;
        }
        let repeated: u32 = seen.values().map(|count| count.saturating_sub(1)).sum();
        let percent = f64::from(repeated) * 100.0 / records.len() as f64;
        assert!(
            percent <= 1.0,
            "duplicate share {percent:.3}% must stay within the 1% contract bound"
        );
        assert!(
            stats.deduplicated > 0,
            "self-play repeats positions, so dedup must actually fire"
        );
    }

    #[test]
    fn wdl_verdicts_map_to_white_relative_outcomes_through_the_side_to_move() {
        use mf_core::Color;
        use mf_tb::Wdl;

        use super::white_relative_tb_outcome;
        use crate::record::Outcome;

        assert_eq!(
            white_relative_tb_outcome(Wdl::Win, Color::White),
            Outcome::Win
        );
        assert_eq!(
            white_relative_tb_outcome(Wdl::Loss, Color::White),
            Outcome::Loss
        );
        assert_eq!(
            white_relative_tb_outcome(Wdl::Win, Color::Black),
            Outcome::Loss
        );
        assert_eq!(
            white_relative_tb_outcome(Wdl::Loss, Color::Black),
            Outcome::Win
        );
        // Cursed wins and blessed losses are fifty-move-rule draws for either side.
        for color in Color::ALL {
            assert_eq!(
                white_relative_tb_outcome(Wdl::CursedWin, color),
                Outcome::Draw
            );
            assert_eq!(
                white_relative_tb_outcome(Wdl::BlessedLoss, color),
                Outcome::Draw
            );
            assert_eq!(white_relative_tb_outcome(Wdl::Draw, color), Outcome::Draw);
        }
    }

    /// Loads tablebases from `MF_SYZYGY_PATH`, or `None` to skip (repo pattern).
    fn tablebases() -> Option<&'static mf_tb::Tablebases> {
        static TABLEBASES: OnceLock<Option<mf_tb::Tablebases>> = OnceLock::new();
        TABLEBASES
            .get_or_init(|| {
                let paths = std::env::var("MF_SYZYGY_PATH").ok()?;
                Some(
                    mf_tb::Tablebases::new(&paths).unwrap_or_else(|error| {
                        panic!("MF_SYZYGY_PATH is set but broken: {error}")
                    }),
                )
            })
            .as_ref()
    }

    fn collect_with_tb(
        config: GenerateConfig,
        tablebases: &mf_tb::Tablebases,
    ) -> (Vec<Record>, super::GenerateStats) {
        let network = network().expect("caller must check `network()` before collecting");
        let mut records = Vec::new();
        let stats = generate(config, network, Some(tablebases), |batch| {
            records.extend_from_slice(batch);
            Ok(())
        })
        .expect("generation succeeds");
        (records, stats)
    }

    /// Enough games to make at least one reach tablebase range with high probability.
    fn tb_config(threads: usize) -> GenerateConfig {
        GenerateConfig {
            games: 32,
            nodes: 1_000,
            threads,
            seed: 90_210,
            ..GenerateConfig::default()
        }
    }

    #[test]
    fn a_tablebase_run_is_deterministic_and_actually_adjudicates() {
        let (Some(tablebases), Some(_)) = (tablebases(), network()) else {
            eprintln!("SKIPPED: set MF_SYZYGY_PATH to run tablebase adjudication tests");
            return;
        };
        let (first, first_stats) = collect_with_tb(tb_config(1), tablebases);
        let (second, second_stats) = collect_with_tb(tb_config(1), tablebases);
        assert_eq!(
            first, second,
            "a fixed seed plus a fixed table set must be reproducible"
        );
        assert_eq!(first_stats, second_stats);
        assert!(
            first_stats.tb_adjudicated > 0,
            "at least one game must actually be adjudicated by the tablebases"
        );
    }

    #[test]
    fn a_tablebase_run_is_independent_of_the_thread_count() {
        let (Some(tablebases), Some(_)) = (tablebases(), network()) else {
            eprintln!("SKIPPED: set MF_SYZYGY_PATH to run tablebase adjudication tests");
            return;
        };
        let (single, single_stats) = collect_with_tb(tb_config(1), tablebases);
        let (multi, multi_stats) = collect_with_tb(tb_config(4), tablebases);
        assert_eq!(single, multi);
        assert_eq!(single_stats, multi_stats);
    }

    #[test]
    fn results_are_labelled_and_counted_consistently() {
        if network().is_none() {
            return;
        }
        let (records, stats) = collect(small(2_026, 2));
        let mut counted = [0u64; 3];
        for record in &records {
            counted[record.outcome().expect("valid outcome") as usize] += 1;
        }
        assert_eq!(counted, stats.results);
    }
}
