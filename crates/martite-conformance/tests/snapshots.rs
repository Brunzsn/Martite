use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use martite_conformance::snapshot::{SnapshotRecord, read_snapshot_dir, write_records};
use martite_conformance::snapshot_cases;

fn snapshot_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots/v1")
}

#[test]
fn snapshots_match_the_ir_evaluator() {
    let snapshots = read_snapshot_dir(&snapshot_root());
    for case in snapshot_cases::all_cases() {
        let Some(expected) = snapshots.get(case.id()) else {
            panic!(
                "missing snapshot for {}; run:\n  cargo test -p martite-conformance --test snapshots -- --ignored update_snapshots",
                case.id()
            );
        };
        case.verify(expected);
    }
    let known: std::collections::BTreeSet<_> = snapshot_cases::all_cases()
        .into_iter()
        .map(|case| case.id().to_string())
        .collect();
    for id in snapshots.keys() {
        assert!(
            known.contains(id),
            "snapshot {} has no case; remove it or restore the case",
            id
        );
    }
}

#[test]
fn threshold_notes_are_only_accepted_for_limit_traps() {
    let snapshots = read_snapshot_dir(&snapshot_root());
    let case = snapshot_cases::all_cases()
        .into_iter()
        .find(|case| case.id() == "e2e/hello_world")
        .expect("hello_world case");
    let mut forged = snapshots
        .get("e2e/hello_world")
        .expect("hello_world snapshot")
        .clone();
    assert!(forged.thresholds.note.is_none());
    forged.thresholds.note = Some("limit-driven outcome".into());
    let error = case
        .verify_ir_thresholds(&forged)
        .expect_err("a note on a successful outcome must be rejected");
    assert!(
        error
            .iter()
            .any(|mismatch| mismatch.field == "thresholds.note"),
        "{error:?}"
    );

    let case = snapshot_cases::all_cases()
        .into_iter()
        .find(|case| case.id() == "limits/traps/out-of-fuel")
        .expect("out-of-fuel case");
    let mut forged = snapshots
        .get("limits/traps/out-of-fuel")
        .expect("out-of-fuel snapshot")
        .clone();
    assert!(forged.thresholds.note.is_some());
    forged.thresholds.fuel = Some(1);
    let error = case
        .verify_ir_thresholds(&forged)
        .expect_err("a note must not hide numeric levels");
    assert!(
        error.iter().any(|mismatch| mismatch.field == "thresholds"),
        "{error:?}"
    );
}

#[test]
fn limit_trap_fixtures_are_deterministic() {
    for id in [
        "limits/traps/out-of-fuel",
        "limits/traps/out-of-memory",
        "limits/traps/depth-exceeded",
        "limits/traps/deadline",
    ] {
        let case = snapshot_cases::all_cases()
            .into_iter()
            .find(|case| case.id() == id)
            .unwrap_or_else(|| panic!("missing case {id}"));
        assert_eq!(case.observe(), case.observe(), "{id} is not deterministic");
    }
}

#[test]
#[ignore = "regenerates the checked-in behavior snapshots"]
fn update_snapshots() {
    let root = snapshot_root();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create snapshot directory");
    let mut groups: BTreeMap<String, Vec<SnapshotRecord>> = BTreeMap::new();
    for case in snapshot_cases::all_cases() {
        let record = case.record();
        groups
            .entry(case.group().to_string())
            .or_default()
            .push(record);
    }
    for (group, mut records) in groups {
        records.sort_by(|left, right| left.id.cmp(&right.id));
        write_records(&root.join(format!("{group}.jsonl")), &records);
    }
}
