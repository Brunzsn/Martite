//! Full-corpus IR-vs-snapshot differential (#112).
//!
//! The checked-in records were captured from the retired AST interpreter and
//! remain the independent oracle for the IR evaluator.

use std::path::{Path, PathBuf};

use martite_conformance::differential::run_all;
use martite_conformance::snapshot::read_snapshot_dir;

fn snapshot_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots/v1")
}

#[test]
fn every_fixture_matches_the_snapshots() {
    let expected = read_snapshot_dir(&snapshot_root());
    run_all(&expected).assert_ok();
}
