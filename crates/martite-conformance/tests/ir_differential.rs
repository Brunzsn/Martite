//! Full-corpus IR-vs-snapshot differential (#112).
//!
//! The checked-in records were captured from the retired AST interpreter and
//! remain the independent oracle for the IR evaluator. The one record changed
//! by #118 was re-recorded deliberately; see `src/snapshot.rs`.

use std::path::{Path, PathBuf};

use martite_conformance::differential::{CorpusReport, run_all};
use martite_conformance::snapshot::read_snapshot_dir;

fn snapshot_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots/v1")
}

#[test]
fn every_fixture_matches_the_snapshots() {
    let expected = read_snapshot_dir(&snapshot_root());
    run_all(&expected).assert_ok();
}

#[test]
fn a_skipped_fixture_fails_the_report() {
    let report = CorpusReport {
        covered: 0,
        skipped: 1,
        failures: Vec::new(),
    };
    let result = std::panic::catch_unwind(|| report.assert_ok());
    assert!(
        result.is_err(),
        "a skipped fixture must not pass as covered"
    );
}
