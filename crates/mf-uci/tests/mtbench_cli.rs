use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_manifold"))
        .args(args)
        .output()
        .expect("manifold binary should start")
}

fn table_rows(stdout: &str) -> Vec<Vec<&str>> {
    stdout
        .lines()
        .skip(1)
        .map(|line| line.split('\t').collect())
        .collect()
}

#[test]
fn mtbench_defaults_to_rows_for_one_two_four_and_eight_threads() {
    let output = run(&["mtbench", "--depth", "2"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let rows = table_rows(stdout);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter().map(|row| row[0]).collect::<Vec<_>>(),
        ["1", "2", "4", "8"]
    );
    assert!(rows.iter().all(|row| row[1] == "2"));
}

#[test]
fn mtbench_accepts_a_custom_thread_list() {
    let output = run(&["mtbench", "--threads", "1,4", "--depth", "8"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let rows = table_rows(stdout);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().map(|row| row[0]).collect::<Vec<_>>(),
        ["1", "4"]
    );
    assert!(rows.iter().all(|row| row[1] == "8"));
}

#[test]
fn mtbench_prints_stable_parseable_columns() {
    let output = run(&["mtbench", "--threads", "1", "--depth", "2"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8");
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("Threads\tDepth\tNodes\tTime (ms)\tNPS"));
    let row: Vec<_> = lines
        .next()
        .expect("one benchmark row should be present")
        .split('\t')
        .collect();
    assert_eq!(row.len(), 5);
    assert_eq!(row[0], "1");
    assert_eq!(row[1], "2");
    assert!(row[2].parse::<u64>().expect("nodes should be an integer") > 0);
    row[3]
        .parse::<u128>()
        .expect("elapsed milliseconds should be an integer");
    row[4].parse::<u64>().expect("NPS should be an integer");
    assert!(lines.next().is_none());
}

#[test]
fn mtbench_rejects_invalid_thread_lists_helpfully() {
    for threads in ["0", "1,1", "1,,4", "one", "257"] {
        let output = run(&["mtbench", "--threads", threads, "--depth", "2"]);
        assert!(
            !output.status.success(),
            "invalid thread list '{threads}' should fail"
        );
        assert!(output.stdout.is_empty());

        let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8");
        assert!(
            stderr.contains("invalid mtbench thread list"),
            "missing diagnostic for '{threads}': {stderr}"
        );
        assert!(
            stderr.contains("Usage: manifold mtbench"),
            "missing usage for '{threads}': {stderr}"
        );
    }
}

#[test]
fn mtbench_rejects_zero_depth_helpfully() {
    let output = run(&["mtbench", "--depth", "0"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("invalid mtbench depth '0'"));
    assert!(stderr.contains("Usage: manifold mtbench"));
}
