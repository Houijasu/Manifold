mod common;

use std::path::Path;

#[test]
fn ethereal_chess960_suite_positions_721_through_960_are_exact() {
    let depth = if cfg!(debug_assertions) { 3 } else { 5 };
    common::suite_range(
        &Path::new(common::TESTDATA).join("ethereal_fischer.epd"),
        depth,
        true,
        720,
        usize::MAX,
    );
}
