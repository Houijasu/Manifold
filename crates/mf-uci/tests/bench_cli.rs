use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BENCH_NODE_COUNT: u64 = 175_944;
const BENCH_NODES: &str = "Nodes searched: 175944";

/// The default-context `UseLMR=false` arm.
///
/// M4-F1 deliberately moved this anchor from `367_369` to `400_404` by hoisting
/// `improving`, `history_score`, `reduction`, and `effective_depth` out from under
/// `use_lmr`. Those are shared derived values that futility (`search.rs` frontier
/// futility) and SEE pruning both read, so gating them on `use_lmr` made roughly 35%
/// of the apparent LMR effect actually be futility and SEE getting weaker. See
/// mission AGENTS.md 4.4 item 3.
///
/// Attribution measured in isolation at the same commit:
///   * `367_369` — both couplings present (M3 baseline `9fd3035`)
///   * `330_310` — `effective_depth` hoisted, quiet-history reads still gated
///   * `400_404` — both hoisted (this anchor)
///
/// The all-on signature (`175_944`) and the all-off signature (`3_768_488`) are
/// UNCHANGED by the split, which proves only gating moved and the search core did not.
const BENCH_NODE_COUNT_WITHOUT_LMR: u64 = 400_404;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_manifold"))
        .args(args)
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

fn run_uci_session(script: &str, label: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_manifold"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("manifold binary should start");
    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        stdin
            .write_all(script.as_bytes())
            .expect("UCI commands should be written");
        stdin.flush().expect("UCI commands should be flushed");
    }
    drop(child.stdin.take());

    let deadline = Instant::now() + Duration::from_secs(300);
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
            panic!("{label} did not exit within 300 seconds");
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
        "setoption name UseNMP value false\n\
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
    let output = run_uci_bench_ablation_session();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let nodes = metrics(stdout, "Nodes searched: ");
    assert_eq!(nodes.len(), 14);
    let baseline = nodes[0];
    assert_eq!(
        baseline, 4_961_681,
        "disabling the ten named selectivity techniques must leave check extensions enabled"
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
        nodes[13], 3_768_488,
        "UseCheckExt=false must restore the GHI-safe all-selectivity-off signature"
    );
}

/// `UseLMR` must gate ONLY the LMR reduction application.
///
/// This is the regression test for mission AGENTS.md 4.4 item 3. `improving`,
/// `history_score`, `reduction`, and `effective_depth` are shared derived values that
/// futility and SEE pruning read. If any of them is ever moved back under `use_lmr`,
/// the `UseLMR=false` arm silently weakens futility and SEE too, and every LMR ablation
/// becomes invalid. That regression shows up here as `400_404` drifting back toward the
/// old confounded `367_369`.
///
/// The all-on and all-off signatures are asserted alongside it because an honest split
/// changes ONLY the ablation arm; if either of those moves, the search core changed and
/// this is not a gating fix.
#[test]
fn disabling_lmr_does_not_also_weaken_futility_and_see_pruning() {
    let output = run_uci_session(
        "bench\n\
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
        nodes[0], BENCH_NODE_COUNT,
        "the split must not move the shipped all-on signature"
    );
    assert_eq!(
        nodes[1], BENCH_NODE_COUNT_WITHOUT_LMR,
        "UseLMR=false must reduce exactly the LMR reduction and nothing else"
    );
    assert_eq!(
        nodes[2], 187_700,
        "the Futility+SEE-off arm is independent of the split"
    );
    assert_eq!(
        nodes[3], 322_887,
        "with futility and SEE already off, UseLMR=false is unchanged by the split"
    );

    // With the couplings present the LMR-off arm was 2.088x the all-on count, of which
    // roughly 35% was futility and SEE getting weaker rather than LMR itself. Once
    // hoisted, the LMR-off arm must be STRICTLY LARGER than the old confounded figure:
    // futility and SEE now stay at full strength, so LMR alone has to account for the
    // whole difference.
    assert!(
        nodes[1] > 367_369,
        "hoisting effective_depth must strengthen, not weaken, the Fut+SEE arms in the \
         LMR-off ablation: got {}",
        nodes[1]
    );
}

#[test]
fn bench_rejects_arguments_helpfully() {
    let output = run(&["bench", "extra"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("bench does not accept arguments"));
    assert!(stderr.contains("Usage: manifold bench"));
}
