use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use mf_tb::Tablebases;

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should follow the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("manifold-{name}-{}-{nonce}", std::process::id()))
}

#[test]
fn every_semicolon_delimited_path_segment_is_trimmed() {
    let first = unique_temp_dir("syzygy-first");
    let second = unique_temp_dir("syzygy-second");
    fs::create_dir_all(&first).expect("first fixture directory should be created");
    fs::create_dir_all(&second).expect("second fixture directory should be created");

    let paths = format!("  {}  ;\t{}\t", first.display(), second.display());
    let opened = Tablebases::new(&paths);

    fs::remove_dir_all(&first).expect("first fixture directory should be removed");
    fs::remove_dir_all(&second).expect("second fixture directory should be removed");
    assert!(opened.is_ok(), "trimmed existing directories should be accepted");
}
