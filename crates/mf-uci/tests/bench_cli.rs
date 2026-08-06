use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The all-on bench signature.
///
/// The HCE milestones moved this from `175_944` to `138_600` (-21.2%) by adding butterfly and
/// capture history to move ordering. M4-F2 then moved it to `135_257` (-2.4%) by
/// adding continuation history at 1/2/4/6 ply. Both moves are expected: better
/// ordering means more cutoffs on the first move tried.
///
/// The change is attributable ENTIRELY to history. With the history toggles off, both
/// all-off anchors below reproduce their M3 values bit-for-bit (`4_961_681` and
/// `3_768_488`), which proves the search core was not touched. `UseContHistory=false`
/// additionally reproduces the M4-F1 signature `138_600` exactly, which is pinned in
/// the historical continuation-history control.
///
/// The bench delta understates this feature badly, and deliberately is not the
/// evidence for it (mission AGENTS.md 4.53). Bench is depth 7; continuation history
/// feeds LMR and pruning, which matter at real depths. From startpos at `go depth 14`
/// the same change is -25.7% nodes (`469_349` -> `348_683`).
///
/// M4-F3 then moved it to `131_333` (-2.9%) by adding correction history: a learned
/// residual applied to the static eval, keyed on pawn structure, minor-piece and
/// major-piece placement, material, and our own two previous moves. Attribution is
/// pinned by the historical correction-history control, which
/// reproduces `135_257` exactly.
///
/// NNUE search integration intentionally invalidates those HCE anchors. The current
/// normalized-centipawn NNUE signature is pinned below.
///
/// The M7 search work (quiescence TT and delta pruning, the soft/hard time-management
/// split, widened SEE pruning windows, and score-scaled aspiration windows) then moved
/// this from `64_756` to `45_036`, a further -30.5%. Every anchor in this file moved
/// with it, so the M4-era attribution controls below pin new values: the properties
/// they guard are unchanged, but the numbers they guard them with are not. Their
/// commentary is kept because it records WHY each control exists, which is the part
/// that outlives any particular signature.
///
/// M3-F1 added quiet checks in quiescence and left this anchor exactly where M7 put it,
/// because the technique SHIPS OFF: enabling it cost +12.3% bench nodes on that build
/// (`45_036` -> `50_569`) and 0.12 plies of depth at equal time, and it measured
/// -12.75 +/- 23.01 Elo over 300 games. The enabled signature is pinned in
/// `qsearch_checks_ships_disabled_and_is_wired_through_to_the_search`, not here.
///
/// M3-F2 added capture LMR and left this anchor where M7 put it, because that
/// measurement said ship it OFF: -8.11 +/- 20.67 Elo over 300 games despite saving
/// -5.8% here and -24.7% / -33.1% / -21.6% at fixed depths 10 / 12 / 14. A large node
/// saving is not a strength result, and this file records two independent
/// demonstrations of that.
///
/// M3-F4 is the FIRST M3 feature to move this constant, from `45_036` to `44_737`
/// (-0.66%), because it is the first one whose measurement said ship it ON. It lets the
/// LMR verification re-search depth respond to how far the reduced scout beat the
/// incumbent best score instead of always paying full `child_depth`.
///
/// M4-F1b moved it again, from `44_737` to `41_588` (-7.0%), by turning capture LMR ON
/// after re-measuring it on top of that band: **+11.59 +/- 22.22 Elo** vs the M3 kept
/// build, zero forfeits. That order is the whole point. M3-F2's write-up named the
/// always-full-depth verification re-search as the reason its node saving would not
/// convert, M3-F4 removed exactly that constraint, and the same technique measured ~20
/// Elo higher afterwards. Neither interval excludes zero, so what is established is the
/// ORDER of the two measurements, not a proven gain from either.
///
/// The consequence for this file is that the two features now form an attribution
/// SQUARE rather than a chain, and all four corners are pinned:
///
///   UseCaptureLMR   UsePostLMRDepth   signature   pinned in
///   on  (shipped)   on  (shipped)     41_588      here
///   off             on                44_737      `capture_lmr_ships_enabled_...`
///   on              off               42_409      `post_lmr_depth_ships_enabled_...`
///   off             off               45_036      `the_shipped_search_decomposes_...`
///
/// So `44_737` and `45_036` are not lost, and neither feature can be blamed for the
/// other's contribution: any drift identifies which corner moved.
///
/// This constant NOT moving is otherwise still an assertion. A feature that ships
/// disabled must leave the shipped signature bit-for-bit unchanged, so if adding one
/// ever moves this number, the toggle is not gating everything it claims to gate.
const BENCH_NODE_COUNT: u64 = 41_588;
const BENCH_NODES: &str = "Nodes searched: 41588";

/// The signature with `UsePostLMRDepth=false`, capture LMR still on.
///
/// This is the number M3-F2 measured its -8.11 Elo with, which is not a coincidence:
/// that build was capture LMR without the verification-depth band, and this arm
/// reconstructs it exactly.
const BENCH_NODE_COUNT_WITHOUT_POST_LMR_DEPTH: u64 = 42_409;

/// The signature with `UsePostLMRContHist=true`.
///
/// Pinned so the disabled half of the M3-F4 package stays measurable without a rebuild.
const BENCH_NODE_COUNT_WITH_POST_LMR_CONTHIST: u64 = 43_134;

/// The signature with `UseCaptureLMR=false`: the M3 shipped signature, bit-for-bit.
///
/// The attribution proof that M4-F1b moved the signature by flipping one default and
/// nothing else. If `44_737` does not come back exactly, something other than capture
/// LMR moved the tree.
const BENCH_NODE_COUNT_WITHOUT_CAPTURE_LMR: u64 = 44_737;

/// The signature with `UseCaptureLMR=false` AND `UsePostLMRDepth=false`: the M7/M2
/// signature both features were measured against, bit-for-bit.
const BENCH_NODE_COUNT_WITHOUT_EITHER_LMR_FEATURE: u64 = 45_036;

/// The all-on signature with `UseQSearchChecks=true`.
///
/// Pinned so the disabled technique stays measurable without a rebuild, and so a change
/// to the quiet-check generator is still caught by the suite even though nothing in the
/// shipped search reaches it.
const BENCH_NODE_COUNT_WITH_QSEARCH_CHECKS: u64 = 44_860;

/// The NNUE signature reproduced exactly by `UseCorrHistory=false`.
const BENCH_NODE_COUNT_WITHOUT_CORRECTION: u64 = 38_858;

/// The NNUE signature reproduced exactly by `UseContHistory=false`.
///
/// This is measured with correction history off so the anchor isolates continuation
/// history rather than folding correction-history changes into the same number.
const BENCH_NODE_COUNT_WITHOUT_CONTINUATION: u64 = 37_032;

/// The default-context `UseLMR=false` arm.
///
/// M4-F1 moved this anchor twice, both deliberately:
///
/// 1. The toggle split hoisted `improving`, `history_score`, `reduction`, and
///    `effective_depth` out from under `use_lmr`. Those are shared derived values that
///    frontier futility and SEE pruning both read, so gating them on `use_lmr` made
///    roughly 35% of the apparent LMR effect actually be futility and SEE getting
///    weaker (mission AGENTS.md 4.4 item 3). Attribution measured in isolation at the
///    same commit: `367_369` (both couplings) -> `330_310` (`effective_depth` hoisted
///    only) -> `400_404` (both hoisted).
/// 2. Adding history to move ordering then moved it from `400_404` to `265_786`, for
///    the same reason the all-on signature moved.
///
/// The HCE M4-F2 build moved it again, from `265_786` to `320_602`. That direction is INTENDED and
/// is the clearest single confirmation that continuation history reaches LMR: the
/// `statScore` term that scales the reduction now reads continuation history, so
/// removing LMR removes more search than it used to. The all-on arm got cheaper while
/// the LMR-off arm got dearer, which is only possible if the new signal is being
/// consumed by LMR rather than by move ordering alone.
///
/// M7 moved it again, from `142_487` to `124_323`, in the same direction as every other
/// anchor here.
const BENCH_NODE_COUNT_WITHOUT_LMR: u64 = 124_323;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bench_network_path() -> Option<std::path::PathBuf> {
    std::env::var_os("MF_NNUE_TEST_NET")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let path = workspace_root().join("nets/main.nnue");
            path.is_file().then_some(path)
        })
}

macro_rules! require_bench_network {
    () => {
        if bench_network_path().is_none() {
            eprintln!("SKIPPED: NNUE bench tests require MF_NNUE_TEST_NET or nets/main.nnue");
            return;
        }
    };
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_manifold"))
        .args(args)
        .current_dir(workspace_root())
        .output()
        .expect("manifold binary should start")
}

fn metric<'a>(output: &'a str, prefix: &str) -> &'a str {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .expect("benchmark metric should be present")
}

fn metrics(output: &str, prefix: &str) -> Vec<u64> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .map(|value| value.parse::<u64>().expect("metric should be an integer"))
        .collect()
}

/// How long one UCI session may take before it is treated as hung.
///
/// This is a watchdog against a genuinely stuck engine, not a performance assertion,
/// so it is scaled to the build the way the perft anchors are depth-gated on
/// `cfg!(debug_assertions)`. In release all thirteen tests in this file finish in about
/// forty seconds; an unoptimised build is roughly an order of magnitude slower, and the
/// two multi-session ablations —
/// `disabling_history_restores_the_m3_all_selectivity_off_signatures` and
/// `each_selectivity_toggle_changes_the_isolated_bench_node_count_by_two_percent` —
/// blew the flat 300-second deadline under a debug `cargo test --workspace`. They
/// failed as timeouts, never as signature mismatches, so the deadline was measuring the
/// optimiser rather than the engine. A red suite that everyone has learned to ignore is
/// how a real regression gets missed, so the watchdog is scaled rather than deleted.
fn session_deadline() -> Duration {
    if cfg!(debug_assertions) {
        Duration::from_secs(3_000)
    } else {
        Duration::from_secs(300)
    }
}

/// Runs one scripted UCI session against the built binary and returns its full output.
///
/// Both child pipes are drained CONCURRENTLY, on their own threads, for the whole session.
/// Collecting them afterwards via `wait_with_output` deadlocks: a Windows pipe buffers about
/// 64 KiB, and a session that emits more than that blocks the engine inside its own write
/// while this function waits for an exit that can never happen. The watchdog then kills the
/// child and the failure surfaces as `assertion failed: output.status.success()`, which reads
/// like an engine crash but is this harness starving the engine of pipe space. It was
/// observed once at 2 failures in 13 under machine load and green on an idle re-run, which is
/// exactly the profile of a buffer race rather than a signature defect.
fn run_uci_session(script: &str, label: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("manifold binary should start");

    let mut stdout = child.stdout.take().expect("stdout should be piped");
    let mut stderr = child.stderr.take().expect("stderr should be piped");
    let stdout_reader = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let stderr_reader = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });

    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        if let Some(path) = bench_network_path() {
            writeln!(stdin, "setoption name EvalFile value {}", path.display())
                .expect("NNUE test network should be selected");
        }
        stdin
            .write_all(script.as_bytes())
            .expect("UCI commands should be written");
        stdin.flush().expect("UCI commands should be flushed");
    }
    drop(child.stdin.take());

    let limit = session_deadline();
    let deadline = Instant::now() + limit;
    loop {
        if child
            .try_wait()
            .expect("process status should be readable")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("{label} did not exit within {} seconds", limit.as_secs());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let status = child.wait().expect("process status should be readable");
    Output {
        status,
        stdout: stdout_reader.join().expect("stdout reader should finish"),
        stderr: stderr_reader.join().expect("stderr reader should finish"),
    }
}

fn run_uci_bench_session() -> Output {
    run_uci_session(
        "setoption name Threads value 4\n\
         bench\n\
         bench\n\
         bench\n\
         setoption name Hash value 64\n\
         ucinewgame\n\
         bench\n\
         quit\n",
        "UCI bench session",
    )
}

fn run_uci_bench_ablation_session() -> Output {
    run_uci_session(
        // The history tables are disabled for the whole of this session. They are a
        // move-ordering input that every other technique's isolated delta is measured
        // through, so leaving them on would fold history's effect into all eleven
        // numbers below. The NNUE all-off signatures (`3_473_717` and `2_848_247`)
        // are pinned separately. History gets its own isolation context in
        // `history_toggles_have_pinned_nnue_signatures`.
        //
        // Correction history is disabled for the same reason and is load-bearing here
        // for a second one: it feeds the STATIC EVAL, which RFP, razoring, futility,
        // ProbCut, and the improving flag all threshold against. Leaving it on would
        // put an eval correction inside every one of those isolated deltas.
        "setoption name UseButterflyHistory value false\n\
             setoption name UseCaptureHistory value false\n\
             setoption name UseContHistory value false\n\
             setoption name UseCorrHistory value false\n\
             setoption name UseNMP value false\n\
             setoption name UseRFP value false\n\
             setoption name UseRazoring value false\n\
             setoption name UseLMR value false\n\
             setoption name UseLMP value false\n\
             setoption name UseFutility value false\n\
             setoption name UseSEEPruning value false\n\
             setoption name UseSingularExt value false\n\
             setoption name UseMultiCut value false\n\
             setoption name UseIIR value false\n\
             setoption name UseProbCut value false\n\
             bench\n\
             setoption name UseNMP value true\n\
             bench\n\
             setoption name UseNMP value false\n\
             setoption name UseRFP value true\n\
             bench\n\
             setoption name UseRFP value false\n\
             setoption name UseRazoring value true\n\
             bench\n\
             setoption name UseRazoring value false\n\
             setoption name UseLMR value true\n\
             bench\n\
             setoption name UseLMR value false\n\
             setoption name UseLMP value true\n\
             bench\n\
             setoption name UseLMP value false\n\
             SeToPtIoN NaMe UsEfUtIlItY VaLuE TrUe\n\
             bench\n\
             setoption name UseFutility value false\n\
             setoption name UseSEEPruning value true\n\
             bench\n\
             setoption name UseSEEPruning value false\n\
             setoption name UseSingularExt value true\n\
             bench\n\
             setoption name UseSingularExt value false\n\
             setoption name UseRFP value true\n\
             setoption name UseSingularExt value true\n\
             bench\n\
             setoption name UseIIR value true\n\
             bench\n\
             setoption name UseIIR value false\n\
             setoption name UseRFP value false\n\
             setoption name UseSingularExt value false\n\
             SeToPtIoN NaMe UsEpRoBcUt VaLuE TrUe\n\
             bench\n\
             setoption name UseProbCut value false\n\
             setoption name UseMultiCut value true\n\
             bench\n\
             setoption name UseMultiCut value false\n\
             setoption name UseCheckExt value false\n\
             bench\n\
             quit\n",
        "UCI bench ablation session",
    )
}

/// A session emitting more than one pipe buffer of stdout still completes.
///
/// Guards `run_uci_session` against the deadlock it used to have: it drained the child's
/// pipes only after the child exited, so a session past the ~64 KiB Windows pipe buffer left
/// the engine blocked in `write` and this harness blocked waiting for an exit that could not
/// arrive. The watchdog then killed the engine and the whole thing surfaced as a bogus
/// `status.success()` assertion on the ablation sessions.
///
/// The output is generated by alternating `setoption name Threads`, each of which echoes one
/// `info string threads set to N` line. That is deliberate: it produces well over a buffer's
/// worth of stdout in under a second, so the guard costs the suite almost nothing, whereas
/// reaching the same volume through searches would cost tens of seconds per run. This test
/// hangs until the watchdog fires on the old implementation and passes on the concurrent-drain
/// one.
#[test]
fn a_session_exceeding_one_pipe_buffer_of_output_still_exits_cleanly() {
    const ECHOES: usize = 2_500;
    let mut script = String::new();
    for index in 0..ECHOES {
        let threads = 1 + index % 2;
        script.push_str(&format!("setoption name Threads value {threads}\n"));
    }
    script.push_str("quit\n");

    let output = run_uci_session(&script, "UCI pipe-buffer session");

    assert!(output.status.success());
    assert!(
        output.stdout.len() > 64 * 1024,
        "the session must exceed one pipe buffer to exercise the deadlock, got {} bytes",
        output.stdout.len()
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("info string threads set to "))
            .count(),
        ECHOES
    );
}

#[test]
fn bench_reports_deterministic_nodes_time_and_nps() {
    require_bench_network!();
    let first = run(&["bench"]);
    let second = run(&["bench"]);
    assert!(first.status.success());
    assert!(second.status.success());
    assert!(first.stderr.is_empty());
    assert!(second.stderr.is_empty());

    for output in [&first, &second] {
        let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
        assert!(stdout.lines().any(|line| line == "Positions: 6"));
        assert!(stdout.lines().any(|line| line == BENCH_NODES));
        metric(stdout, "Time (ms): ")
            .parse::<u128>()
            .expect("time should be an integer");
        assert!(
            metric(stdout, "NPS: ")
                .parse::<u64>()
                .expect("NPS should be an integer")
                > 0
        );
    }
}

#[test]
fn uci_bench_matches_cli_and_clears_all_search_state() {
    require_bench_network!();
    let uci = run_uci_bench_session();
    assert!(uci.status.success());
    assert!(uci.stderr.is_empty());

    let stdout = std::str::from_utf8(&uci.stdout).expect("stdout should be UTF-8");
    assert!(
        stdout
            .lines()
            .any(|line| line == "info string threads set to 4")
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == "Positions: 6")
            .count(),
        4
    );
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT; 4],
        "three consecutive benches and one after Hash=64 plus ucinewgame should match"
    );
    assert_eq!(metrics(stdout, "Time (ms): ").len(), 4);
    assert!(
        metrics(stdout, "NPS: ")
            .into_iter()
            .all(|nodes_per_second| nodes_per_second > 0)
    );

    let cli = run(&["bench"]);
    assert!(cli.status.success());
    assert!(cli.stderr.is_empty());
    let cli_stdout = std::str::from_utf8(&cli.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metric(cli_stdout, "Nodes searched: ")
            .parse::<u64>()
            .expect("CLI node count should be an integer"),
        BENCH_NODE_COUNT
    );
}

#[test]
fn each_selectivity_toggle_changes_the_isolated_bench_node_count_by_two_percent() {
    require_bench_network!();
    let output = run_uci_bench_ablation_session();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 14);
    let baseline = nodes[0];
    assert_eq!(
        baseline, 3_473_717,
        "the NNUE all-selectivity-off signature must leave check extensions enabled"
    );
    for (name, enabled) in [
        "UseNMP",
        "UseRFP",
        "UseRazoring",
        "UseLMR",
        "UseLMP",
        "UseFutility",
        "UseSEEPruning",
        "UseSingularExt",
    ]
    .into_iter()
    .zip(&nodes[1..=8])
    {
        let difference = baseline.abs_diff(*enabled);
        assert!(
            difference.saturating_mul(100) >= baseline.saturating_mul(2),
            "{name} changed bench nodes by less than 2%: base={baseline}, enabled={enabled}"
        );
    }

    let iir_baseline = nodes[9];
    let iir_enabled = nodes[10];
    assert!(
        iir_baseline.abs_diff(iir_enabled).saturating_mul(100) >= iir_baseline.saturating_mul(2),
        "UseIIR changed bench nodes by less than 2% in its isolated context: \
         base={iir_baseline}, enabled={iir_enabled}"
    );

    let probcut_enabled = nodes[11];
    assert!(
        baseline.abs_diff(probcut_enabled).saturating_mul(100) >= baseline.saturating_mul(2),
        "UseProbCut changed bench nodes by less than 2%: \
         base={baseline}, enabled={probcut_enabled}"
    );

    assert_ne!(
        nodes[12], baseline,
        "UseMultiCut must have an independently observable bench effect"
    );

    assert_eq!(
        nodes[13], 2_848_247,
        "UseCheckExt=false must reproduce the NNUE all-selectivity-off signature"
    );
}

/// Pins each shipped history toggle under NNUE.
///
/// Correction history is disabled so these four signatures isolate move-ordering
/// history. Exact anchors are used because the capture-history delta is just under
/// two percent on NNUE; weakening a percentage guard would hide future drift.
#[test]
fn history_toggles_have_pinned_nnue_signatures() {
    require_bench_network!();
    let output = run_uci_session(
        // Correction history is off for this whole session, so the three deltas below
        // are the ordering tables in isolation rather than a correction-history mix.
        "setoption name UseCorrHistory value false\n\
         bench\n\
         setoption name UseButterflyHistory value false\n\
         bench\n\
         setoption name UseButterflyHistory value true\n\
         setoption name UseCaptureHistory value false\n\
         bench\n\
         setoption name UseCaptureHistory value true\n\
         setoption name UseContHistory value false\n\
         bench\n\
         quit\n",
        "UCI history ablation session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 4);

    assert_eq!(nodes, [38_858, 37_593, 41_436, 37_032]);

    assert!(
        nodes[2] > nodes[0],
        "capture history must SAVE nodes when enabled, not cost them: \
         base={}, disabled={}",
        nodes[0],
        nodes[2]
    );
}

/// `UseContHistory=false` must reproduce its NNUE signature bit-for-bit.
///
/// This is the control that proves M4-F2 moved the shipped signature by adding
/// continuation history and by nothing else. Continuation history is threaded through
/// `OrderingContext`, the LMR `statScore`, and history pruning, so a stray change to
/// any shared ordering weight would show up here as drift even with the new tables
/// switched off.
#[test]
fn continuation_history_off_reproduces_the_nnue_signature() {
    require_bench_network!();
    let output = run_uci_session(
        "setoption name UseCorrHistory value false\n\
         setoption name UseContHistory value false\n\
         bench\n\
         quit\n",
        "UCI continuation control session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT_WITHOUT_CONTINUATION],
        "disabling continuation history must restore the exact NNUE bench signature"
    );
}

/// Turning correction history off must reproduce its NNUE signature bit-for-bit.
///
/// This is the proof that M4-F3 moved the shipped bench signature by adding correction
/// history and by nothing else. Correction history touches the static eval on both the
/// `pvs` and the qsearch standing-pat path, and the raw eval still has to be what goes
/// into the TT, so a mistake in either place would show up here as drift even with the
/// feature switched off.
#[test]
fn correction_history_off_reproduces_the_nnue_signature() {
    require_bench_network!();
    let output = run_uci_session(
        "setoption name UseCorrHistory value false\n\
         bench\n\
         quit\n",
        "UCI correction control session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT_WITHOUT_CORRECTION],
        "disabling correction history must restore the exact NNUE bench signature"
    );
}

/// Major-piece and material correction history must stay OFF by default.
///
/// These two variants are the reason this feature needed per-variant toggles. They read
/// BETTER than the shipped default on bench -- `123_045` and `126_109` against
/// `131_333` -- and dramatically worse at real depth: enabling both takes startpos
/// `go depth 14` from `142_873` nodes to `470_678`, a 3.3x regression, and drifts the
/// score +25 cp. Both were added to Stockfish and later removed ("Remove material
/// corrHist", "Remove major corrhist"); `research/search-and-eval-sota.md:1482` says to
/// skip them.
///
/// The NNUE evaluator changes the shallow bench direction of the material variant, so
/// this test pins both exact signatures rather than pretending both still save nodes.
/// The match evidence, not the bench direction, remains the reason both ship disabled.
///
/// After M7 neither variant read better on bench any more: `49_017` and `47_522`
/// against the then-shipped `45_036`. Under M4-F1b's capture LMR the material variant
/// is tempting AGAIN -- `40_161` against the shipped `41_588`, a 3.4% saving -- while
/// the major variant stays worse at `45_188`.
///
/// That flip-flop across three searches is the point. This anchor has now shown the
/// material variant as cheaper, dearer, and cheaper again without anyone re-running the
/// depth-14 probe that actually condemned it. The decision was never the bench delta's
/// to make, and the number moving back is not new evidence.
#[test]
fn correction_variants_are_off_and_have_pinned_nnue_signatures() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UseCorrHistMajor value true\n\
         bench\n\
         setoption name UseCorrHistMajor value false\n\
         setoption name UseCorrHistMaterial value true\n\
         bench\n\
         quit\n",
        "UCI correction variant session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 3);

    assert_eq!(nodes, [BENCH_NODE_COUNT, 45_188, 40_161]);
}

/// `UseQSearchChecks` ships OFF, and this test records WHY in an executable form.
///
/// M3-F1 implemented quiet checks at the first quiescence ply -- the largest search
/// feature the audit found missing -- and measured it single-variable against the M2
/// kept build over 300 games at 8+0.08, Threads=1, `-use-affinity -concurrency 8`, with
/// zero forfeits on both sides:
///
///   * enabled: **-12.75 +/- 23.01 Elo**, Ptnml [5,38,74,29,4], LOS 13.8%
///
/// The error bar covers zero, so the honest reading is "not shown to help" rather than
/// "shown to hurt". It ships off because the feature's stated criterion was a positive
/// point estimate, and a technique with no demonstrated gain has no claim on being the
/// default.
///
/// The mechanism was measured rather than assumed. At `movetime 1000` over 24 book
/// positions the widening reaches **0.12 plies LESS depth** (15.96 vs 16.08, deeper in
/// only 7 of 24) while costing bench nodes (+12.3% when measured against the M2 build;
/// +7.9% against the current one). A quiet check resolves no material,
/// so the qsearch grows without the standing pat converging any faster, and the time
/// comes out of the iterative deepening that actually finds moves. Full write-up in
/// `experiments/MSN-S1-qchecks/results.md`.
///
/// This is deliberately the SAME shape as the pawn-history and history-pruning tests
/// above: pin that the toggle ships off, and that enabling it still reaches the search.
/// The enabled anchor is exact rather than a bare inequality so the disabled technique
/// stays measurable -- a change to the quiet-check generator is caught here even though
/// nothing in the shipped search reaches it.
#[test]
fn qsearch_checks_ships_disabled_and_is_wired_through_to_the_search() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UseQSearchChecks value true\n\
         bench\n\
         quit\n",
        "UCI qsearch checks session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT, BENCH_NODE_COUNT_WITH_QSEARCH_CHECKS],
        "quiet checks must be OFF in the shipped default and must reach the search when on"
    );
}

/// `UseCaptureLMR` ships ON, and turning it off must reproduce the M3 signature.
///
/// M3-F2 extended LMR to late captures: the same log-log formula quiets use, fed a
/// capture `statScore` of captured material plus capture history, with TT moves,
/// checking captures, and queen promotions exempt. It shipped OFF on that measurement
/// -- **-8.11 +/- 20.67 Elo**, Ptnml [2,37,79,30,2] -- and this test used to assert
/// exactly that.
///
/// It ships ON now, on the strength of a second match, and the reason the verdict
/// changed is not that anyone re-ran the same experiment. M3-F2's write-up diagnosed
/// the mechanism: the technique really does shrink the tree by a fifth to a third at
/// fixed depth and really does play the same moves, but converted only +0.12 plies at
/// equal time, because a reduced capture that fails high was re-searched at FULL depth
/// and captures fail high far more often than quiets at the same move index. That
/// write-up named the always-full-depth re-search as the binding constraint and named
/// `doDeeperSearch`/`doShallowerSearch` as the prerequisite for a re-measurement.
///
/// M3-F4 shipped that prerequisite. M4-F1b then spent the one authorized re-measurement
/// on top of it -- same conditions, 300 games at 8+0.08, Threads=1, `-use-affinity
/// -concurrency 8`, vs the M3 kept build, zero forfeits both sides:
///
///   * **+11.59 +/- 22.22 Elo**, Ptnml [3,31,72,41,3], LOS 84.7%, PairsRatio 1.29
///
/// Both intervals cover zero and they overlap heavily, so the claim this test encodes
/// is deliberately modest: the point estimate moved ~20 Elo in the direction the
/// diagnosis predicted once the constraint it named was removed. That is why the
/// disabled anchor here is `BENCH_NODE_COUNT_WITHOUT_CAPTURE_LMR` rather than a bare
/// inequality -- it is the attribution control proving M4-F1b moved the shipped
/// signature by flipping this one default and nothing else.
///
/// Write-ups: `experiments/MSN-S2-capture-lmr/results.md` and
/// `experiments/MSN-S7-capture-lmr-v2/results.md`.
#[test]
fn capture_lmr_ships_enabled_and_reproduces_the_m3_signature_when_disabled() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UseCaptureLMR value false\n\
         bench\n\
         quit\n",
        "UCI capture LMR session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 2);

    assert_eq!(
        nodes,
        [BENCH_NODE_COUNT, BENCH_NODE_COUNT_WITHOUT_CAPTURE_LMR],
        "capture LMR must be ON in the shipped default and must restore the exact M3 \
         signature when disabled"
    );
}

/// `UsePostLMRDepth` ships ON, and turning it off must reproduce M3-F2's build.
///
/// M3-F4 was the first M3 feature to ship enabled, and this is its attribution control.
/// The arm it reproduces is now the more interesting one: with capture LMR also on,
/// `UsePostLMRDepth=false` is exactly the build M3-F2 measured its -8.11 Elo with.
///
/// The mechanism: a reduced scout that beats alpha is re-searched one ply DEEPER when
/// it cleared the incumbent best score by more than 53, one ply SHALLOWER when it
/// cleared it by less than 8, and at the unchanged full depth in between. M3-F2's
/// write-up identified that always-full-depth re-search as the reason a 25-33%
/// fixed-depth node saving converted to +0.12 plies at equal time, and M4-F1b's
/// re-measurement on top of this band is what turned that diagnosis into a shipped
/// default.
#[test]
fn post_lmr_depth_ships_enabled_and_reproduces_the_m3_f2_build() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UsePostLMRDepth value false\n\
         bench\n\
         quit\n",
        "UCI post-LMR depth session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT, BENCH_NODE_COUNT_WITHOUT_POST_LMR_DEPTH],
        "the verification-depth band must be ON by default and must restore the exact \
         M3-F2 signature when disabled"
    );
}

/// The two shipped LMR features must decompose to the M7 signature they were measured
/// against.
///
/// `44_737` and `45_036` are the anchors M3 and M2/M7 shipped, and both are still
/// reachable from the current default by switching off one or both of the LMR features
/// added since. Together with the two tests above this closes the attribution square:
/// each corner is pinned, so a drift in the shipped `41_588` identifies WHICH feature's
/// contribution moved rather than merely that something did.
#[test]
fn the_shipped_search_decomposes_to_its_two_predecessor_signatures() {
    require_bench_network!();
    let output = run_uci_session(
        "setoption name UseCaptureLMR value false\n\
         setoption name UsePostLMRDepth value false\n\
         bench\n\
         quit\n",
        "UCI both-LMR-features-off session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT_WITHOUT_EITHER_LMR_FEATURE],
        "with both LMR features off the search must be the M7 build bit-for-bit"
    );
}

/// `UsePostLMRContHist` ships OFF, and this test records WHY in an executable form.
///
/// M3-F4 was specified as ONE package of two sub-mechanisms hanging off the same LMR
/// fail-high, "unless the worker finds cause to split". There was cause. Measured
/// separately against the bit-identical both-off control over 24 book positions at
/// fixed depth, they move the tree in OPPOSITE directions:
///
///   arm             d12 total   d12 median   d14 total   d14 median
///   depth-only         +0.57%        0.935      -0.98%        0.960
///   conthist-only      +5.92%        1.068      +1.30%        0.986
///   both               +9.47%        1.053      +9.87%        1.061
///
/// A single toggle would therefore have measured their DIFFERENCE and called it the
/// package. The bonus also fails the composition test M3-F3 established: it adds a
/// fourth writer to a continuation table three tuned consumers already read (the LMR
/// statScore, move ordering, and pruning history), with a bonus magnitude imported from
/// an engine whose other history sites all use different ones.
///
/// The enabled anchor is exact rather than a bare inequality so the disabled half stays
/// measurable without a rebuild. Full write-up in
/// `experiments/MSN-S4-postlmr/results.md`.
#[test]
fn post_lmr_conthist_ships_disabled_and_is_wired_through_to_the_search() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UsePostLMRContHist value true\n\
         bench\n\
         quit\n",
        "UCI post-LMR continuation history session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT, BENCH_NODE_COUNT_WITH_POST_LMR_CONTHIST],
        "the post-LMR continuation bonus must be OFF in the shipped default and must \
         reach the search when on"
    );
}

/// Neither post-LMR mechanism may reach the tree while `UseLMR` is off.
///
/// Both sit behind `reduced_depth < child_depth`, which cannot happen when nothing is
/// reduced. The continuation bonus writes to a table move ordering and the LMR
/// statScore both read, so a leak would make the `UseLMR=false` arm stop being the
/// clean control every other selectivity anchor in this file is read against
/// (mission AGENTS.md 4.4).
#[test]
fn post_lmr_handling_cannot_reach_the_tree_without_lmr() {
    require_bench_network!();
    let output = run_uci_session(
        "setoption name UseLMR value false\n\
         bench\n\
         setoption name UsePostLMRDepth value false\n\
         bench\n\
         setoption name UsePostLMRContHist value true\n\
         bench\n\
         quit\n",
        "UCI post-LMR without LMR session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 3);
    // Not `BENCH_NODE_COUNT_WITHOUT_LMR`: that anchor is measured with correction
    // history OFF, and this session leaves the shipped defaults alone so the inertness
    // is asserted about the search that actually plays games. What the test needs is
    // that all three readings AGREE, and the exact value pins that they agree at the
    // default-context LMR-off tree rather than at some third thing.
    assert_eq!(
        nodes,
        vec![80_425; 3],
        "post-LMR handling must be completely inert while UseLMR is off"
    );
}

/// `UseTimeEffort` must be INVISIBLE to bench in BOTH toggle positions.
///
/// M3-F3 scales the soft time limit by the best root move's share of the tree. Bench is
/// a fixed-DEPTH search with no soft limit, so the term has nothing to act on there.
/// This is the reason this test asserts EQUALITY where every other toggle test asserts
/// a difference: an ablation anchor proves a toggle reaches the search, while this one
/// proves it cannot.
///
/// It therefore also stands in for the ablation anchors `UseQSearchChecks` and
/// `UseCaptureLMR` carry. Those two toggles pin a DIFFERENT number in their off
/// position; this one ships off (-17.39 +/- 18.99 Elo at 8+0.08, -34.86 +/- 44.35 at
/// 30+0.3) and pins the SAME number in both positions, because it does not touch the
/// tree at all -- only the clock.
///
/// The per-root-move node accounting the term needs IS performed on every search,
/// bench included -- only its consumer is time-gated. If that accounting ever acquired
/// a side effect on the tree (a move-ordering read, a different TT store, an extra
/// node visit), this is where it surfaces, and the M3 chain of "a feature that ships
/// disabled leaves the signature bit-for-bit unchanged" would break silently without
/// it. `crates/mf-search/tests/search_invariants.rs` makes the same assertion at the
/// library level against fixed-node searches too.
#[test]
fn the_time_effort_term_cannot_move_the_fixed_depth_bench_signature() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UseTimeEffort value true\n\
         bench\n\
         setoption name UseTimeEffort value false\n\
         bench\n\
         quit\n",
        "UCI time effort session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        metrics(stdout, "Nodes searched: "),
        vec![BENCH_NODE_COUNT, BENCH_NODE_COUNT, BENCH_NODE_COUNT],
        "the time-effort term must not reach a fixed-depth search in either position"
    );
}

/// Turning history off must reproduce the pinned NNUE all-off signatures bit-for-bit.
///
/// This is the proof that M4-F1 moved the shipped bench signature by adding history
/// and by nothing else. If the search core had changed as well, these two numbers
/// would drift even with every history table disabled.
#[test]
fn disabling_history_reproduces_nnue_all_selectivity_off_signatures() {
    require_bench_network!();
    let output = run_uci_bench_ablation_session();
    assert!(output.status.success());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");

    assert_eq!(
        nodes[0], 3_473_717,
        "ten selectivity toggles off with history off must match the NNUE anchor exactly"
    );
    assert_eq!(
        nodes[13], 2_848_247,
        "all selectivity off with history off must match the NNUE anchor exactly"
    );
}

/// History pruning ships OFF, and this test records WHY in an executable form.
///
/// M4-F1 measured it as the one place where a favourable bench delta and a match
/// result point in opposite directions:
///
///   * enabled : 133_126 bench nodes (-3.95%) and **-103.68 +/- 46.31 Elo**
///   * disabled: 138_600 bench nodes and **+133.61 +/- 44.43 Elo**
///
/// Both arms vs `baselines/M3/manifold.exe` at 8+0.08, Threads=1, `-use-affinity
/// -concurrency 8`. See `experiments/M4-F1-history/`.
///
/// M4-F1's diagnosis was that a lone butterfly statistic is too noisy to decide that a
/// quiet move is unsearchable, and predicted the technique would become viable once
/// continuation history existed to thicken the signal. M4-F2 acted on that: the
/// threshold now reads `OrderingContext::pruning_history`, which sums 1-ply and 2-ply
/// continuation history with pawn history instead of the single butterfly entry.
///
/// It STILL ships off. Re-measured on the new signal against the same build with the
/// toggle off, it was SPRT-REJECTED at **-45.63 +/- 32.58 Elo** (268 games, LLR -2.96,
/// H0 accepted); see `experiments/M4-F2-conthist/history-pruning/`. The thicker signal
/// did help — the regression shrank from -103.68 to -45.63 — but it is still a
/// regression, so the prediction that continuation history alone would make history
/// pruning viable is DISPROVEN, not merely unconfirmed.
///
/// The bench delta has moved repeatedly since. M4-F2 measured +0.14% (`135_257` ->
/// `135_443`), i.e. enabling it no longer even saved nodes. Under the M7 search it was
/// favourable again, -4.6% (`45_036` -> `42_959`), and under M4-F1b's it still is,
/// -3.7% (`41_588` -> `40_046`). That is precisely the trap this comment exists to
/// disarm: the technique has now shown a tempting bench number under three different
/// searches and lost a match under two of them. Bench is not the evidence.
///
/// This test therefore pins only that the toggle ships off and still reaches the
/// search. The old "must save nodes" assertion is deliberately NOT retained: it
/// guarded an implementation (lone-butterfly threshold) that no longer exists, and
/// keeping it would have meant asserting a property of dead code.
#[test]
fn history_pruning_ships_disabled_after_being_re_measured_on_the_continuation_signal() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UseHistoryPruning value true\n\
         bench\n\
         quit\n",
        "UCI history pruning session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 2);

    assert_eq!(
        nodes[0], BENCH_NODE_COUNT,
        "history pruning must be OFF in the shipped default"
    );
    assert_ne!(
        nodes[1], nodes[0],
        "enabling history pruning must reach the search and change the tree"
    );
}

/// `UsePawnHistory` ships OFF because it is a measured regression on its own.
///
/// Standalone with butterfly history disabled it is 9.18% WORSE than no history at
/// all, and at every ordering weight tried (1, 2, 4, 8, 16) it cost nodes. In
/// Stockfish pawn history is never a standalone ordering signal: it is one small term
/// in a sum dominated by continuation history.
///
/// M4-F2 added that continuation history and re-measured pawn history as a term in the
/// sum rather than as a standalone ordering signal, which was M4-F1's stated condition
/// for revisiting it. It still ships OFF, but the picture improved a lot: as a
/// standalone signal M4-F1 measured it 9.18% WORSE than no history at all, whereas as
/// a term in the sum it is now statistically indistinguishable from not having it
/// (**-1.74 +/- 20.98 Elo**, 600 games, LLR -1.22, inconclusive at the cap). It cost
/// 2.0% bench nodes at M4-F2 (`135_257` -> `137_940`), 2.7% under M7 (`45_036` ->
/// `46_257`), and 5.3% under M4-F1b (`41_588` -> `43_806`). See
/// `experiments/M4-F2-conthist/pawn-history/`.
///
/// "Not measurably harmful" is not a reason to enable something, so the default stands
/// unchanged. Pawn history remains wired as a term in `pruning_history` only, where it
/// is gated off with the rest of history pruning.
///
/// This test pins only that the toggle is wired and off. That is the AGENTS.md 4.52
/// situation: the guard is NOT lowered to manufacture an observable delta.
#[test]
fn pawn_history_ships_disabled_and_is_wired_through_to_the_search() {
    require_bench_network!();
    let output = run_uci_session(
        "bench\n\
         setoption name UsePawnHistory value true\n\
         bench\n\
         quit\n",
        "UCI pawn history session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 2);

    assert_eq!(
        nodes[0], BENCH_NODE_COUNT,
        "pawn history must be OFF in the shipped default"
    );
    assert_ne!(
        nodes[1], nodes[0],
        "enabling pawn history must reach the search and change the tree"
    );
}

/// The UCI option list must advertise the real defaults.
///
/// A GUI that trusts `default true` would silently enable a measured regression.
#[test]
fn the_advertised_pawn_history_default_matches_the_shipped_default() {
    require_bench_network!();
    let output = run_uci_session("uci\nquit\n", "UCI option list session");
    assert!(output.status.success());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    for name in [
        "UsePawnHistory",
        "UseHistoryPruning",
        "UseQSearchChecks",
        "UsePostLMRContHist",
        "UseTimeEffort",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("option name {name} type check default false")),
            "{name} must advertise default false"
        );
    }
    for name in [
        "UseButterflyHistory",
        "UseCaptureHistory",
        "UseContHistory",
        "UsePostLMRDepth",
        "UseCaptureLMR",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("option name {name} type check default true")),
            "{name} must advertise default true"
        );
    }
}

/// `UseLMR` must gate ONLY the LMR reduction application.
///
/// This is the regression test for mission AGENTS.md 4.4 item 3. `improving`,
/// `history_score`, `reduction`, and `effective_depth` are shared derived values that
/// futility and SEE pruning read. If any of them is ever moved back under `use_lmr`,
/// the `UseLMR=false` arm silently weakens futility and SEE too, and every LMR ablation
/// becomes invalid. That regression shows up here as the LMR-off-to-all-on RATIO
/// falling back toward the old confounded 2.088.
#[test]
fn disabling_lmr_does_not_also_weaken_futility_and_see_pruning() {
    require_bench_network!();
    let output = run_uci_session(
        // Correction history is off for this whole session. The property under test is
        // which values `use_lmr` gates, and all four anchors below were re-pinned to the
        // M7 build so that a future coupling regression is still read off one
        // self-consistent set of numbers. Correction history feeds the static eval that futility
        // and SEE pruning threshold against, so leaving it on would move all four
        // anchors for a reason that has nothing to do with LMR gating.
        "setoption name UseCorrHistory value false\n\
         bench\n\
         setoption name UseLMR value false\n\
         bench\n\
         setoption name UseLMR value true\n\
         setoption name UseFutility value false\n\
         setoption name UseSEEPruning value false\n\
         bench\n\
         setoption name UseLMR value false\n\
         bench\n\
         quit\n",
        "UCI LMR coupling session",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 4);

    assert_eq!(
        nodes[0], BENCH_NODE_COUNT_WITHOUT_CORRECTION,
        "the split must not move the shipped all-on signature"
    );
    assert_eq!(
        nodes[1], BENCH_NODE_COUNT_WITHOUT_LMR,
        "UseLMR=false must reduce exactly the LMR reduction and nothing else"
    );
    assert_eq!(
        nodes[2], 58_272,
        "the Futility+SEE-off arm is independent of the split"
    );
    assert_eq!(
        nodes[3], 151_903,
        "with futility and SEE already off, UseLMR=false is unchanged by the split"
    );

    // A ratio guard against the pre-split 367_369/175_944 = 2.088 used to live here.
    // It is gone deliberately: history changed the composition of BOTH arms, so the
    // ratio no longer isolates the gating property it was standing in for. The four
    // exact anchors above are the real guard — re-gating any shared derived value on
    // `use_lmr` moves `nodes[1]` while leaving `nodes[0]`, `nodes[2]`, and `nodes[3]`
    // where they are, which is exactly the signature this test exists to catch.
}

#[test]
fn bench_rejects_arguments_helpfully() {
    require_bench_network!();
    let output = run(&["bench", "extra"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("bench does not accept arguments"));
    assert!(stderr.contains("Usage: manifold bench"));
}
