//! Full-corpus AST-vs-IR differential (#112).
//!
//! `LiveAst` runs both evaluators; `Snapshots` keeps the IR-vs-snapshot mode
//! that stays valid after the AST interpreter is retired (#116).

use std::path::{Path, PathBuf};

use martite_conformance::differential::{Oracle, run_all};
use martite_conformance::snapshot::read_snapshot_dir;

fn snapshot_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots/v1")
}

fn check(oracle: Oracle) {
    let expected = read_snapshot_dir(&snapshot_root());
    run_all(oracle, &expected).assert_ok();
}

#[test]
fn every_fixture_matches_the_ast_interpreter() {
    check(Oracle::LiveAst);
}

#[test]
fn every_fixture_matches_the_snapshots() {
    check(Oracle::Snapshots);
}
