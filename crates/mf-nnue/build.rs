use std::path::{Component, Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBEDDED_NET");

    let manifest_directory = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo must provide CARGO_MANIFEST_DIR to mf-nnue's build script"),
    );
    let network_path = normalize(&manifest_directory.join("../../nets/main.nnue"));
    println!("cargo:rerun-if-changed={}", network_path.display());

    if std::env::var_os("CARGO_FEATURE_EMBEDDED_NET").is_none() {
        return;
    }

    match std::fs::metadata(&network_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => panic!(
            "mf-nnue feature `embedded-net` requires a network file at {}, but that path is not a file",
            network_path.display()
        ),
        Err(error) => panic!(
            "mf-nnue feature `embedded-net` requires a network file at {}: {error}",
            network_path.display()
        ),
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
