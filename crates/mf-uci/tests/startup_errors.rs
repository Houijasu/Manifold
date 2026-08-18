#![cfg(not(feature = "embedded-net"))]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let unique = format!(
            "manifold-startup-errors-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir(&path).expect("isolated temp root should be created");
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
}

#[test]
fn missing_network_is_a_clean_process_level_startup_error() {
    let temp = TempDirectory::new();
    let executable_directory = temp.0.join("bin");
    let working_directory = temp.0.join("cwd");
    fs::create_dir(&executable_directory).expect("isolated executable directory should exist");
    fs::create_dir(&working_directory).expect("isolated working directory should exist");

    let repository = canonical(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .as_path(),
    );
    let isolated = canonical(&temp.0);
    assert!(!isolated.starts_with(&repository));
    assert!(!repository.starts_with(&isolated));

    let source = PathBuf::from(env!("CARGO_BIN_EXE_manifold"));
    let executable = executable_directory.join(
        source
            .file_name()
            .expect("cargo binary path should have a file name"),
    );
    fs::copy(&source, &executable)
        .expect("test executable should be copied outside the repository");

    let mut child = Command::new(&executable)
        .current_dir(&working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("copied engine should start");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"uci\n")
        .expect("startup command should be writable");
    let output = child
        .wait_with_output()
        .expect("startup failure should terminate");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "startup must fail before processing UCI commands"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("startup diagnostic should be UTF-8");
    assert!(stderr.starts_with("UCI startup error:"));
    assert!(stderr.contains("no NNUE network found"));
    let diagnostic = stderr.to_ascii_lowercase();
    assert!(!diagnostic.contains("panicked"));
    assert!(!diagnostic.contains("backtrace"));
}
