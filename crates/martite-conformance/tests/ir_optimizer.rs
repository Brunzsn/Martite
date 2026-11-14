//! Passes-off versus passes-on differential (#123).
//!
//! Every executable snapshot case is observed twice: once from the lowered
//! program and once after the default `Preserve` optimizer. The full
//! observable record, exact fuel and the three recorded thresholds must match.

use std::path::{Path, PathBuf};

use martite_conformance::differential::run_optimized;
use martite_conformance::snapshot::read_snapshot_dir;

fn snapshot_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots/v1")
}

#[test]
fn every_optimized_fixture_matches_the_lowered_observation() {
    let expected = read_snapshot_dir(&snapshot_root());
    run_optimized(&expected).assert_ok();
}
