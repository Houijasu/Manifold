mod common;

use std::path::Path;

#[test]
fn ethereal_standard_suite_is_exact() {
    let depth = if cfg!(debug_assertions) { 3 } else { 6 };
    common::suite(
        &Path::new(common::TESTDATA).join("ethereal_perft.epd"),
        depth,
        false,
    );
}
