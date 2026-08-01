use std::io::Write;
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
const BENCH_NODE_COUNT: u64 = 64_756;
const BENCH_NODES: &str = "Nodes searched: 64756";

/// The NNUE signature reproduced exactly by `UseCorrHistory=false`.
const BENCH_NODE_COUNT_WITHOUT_CORRECTION: u64 = 62_781;

/// The NNUE signature reproduced exactly by `UseContHistory=false`.
///
/// This is measured with correction history off so the anchor isolates continuation
/// history rather than folding correction-history changes into the same number.
const BENCH_NODE_COUNT_WITHOUT_CONTINUATION: u64 = 74_544;

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
const BENCH_NODE_COUNT_WITHOUT_LMR: u64 = 142_487;

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

fn run_uci_session(script: &str, label: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
        .current_dir(workspace_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("manifold binary should start");
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
    child
        .wait_with_output()
        .expect("UCI session output should be readable")
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
        // numbers below. The NNUE all-off signatures (`5_157_465` and `3_967_988`)
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
        baseline, 5_157_465,
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
        nodes[13], 3_967_988,
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

    assert_eq!(nodes, [62_781, 76_593, 64_008, 74_544]);

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

    assert_eq!(nodes, [BENCH_NODE_COUNT, 58_835, 65_025]);
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
        nodes[0], 5_157_465,
        "ten selectivity toggles off with history off must match the NNUE anchor exactly"
    );
    assert_eq!(
        nodes[13], 3_967_988,
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
/// The bench delta is now +0.14% (`135_257` -> `135_443`): enabling it no longer even
/// saves nodes, so there is no longer a favourable bench number tempting anyone to
/// flip the default.
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
/// (**-1.74 +/- 20.98 Elo**, 600 games, LLR -1.22, inconclusive at the cap). It costs
/// 2.0% bench nodes (`135_257` -> `137_940`). See
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
    for name in ["UsePawnHistory", "UseHistoryPruning"] {
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("option name {name} type check default false")),
            "{name} must advertise default false"
        );
    }
    for name in ["UseButterflyHistory", "UseCaptureHistory", "UseContHistory"] {
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
        // which values `use_lmr` gates, and all four anchors below are M4-F2 values
        // held fixed across M4-F3 so that a future coupling regression is still read
        // off the same numbers. Correction history feeds the static eval that futility
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
        nodes[2], 83_622,
        "the Futility+SEE-off arm is independent of the split"
    );
    assert_eq!(
        nodes[3], 109_200,
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
