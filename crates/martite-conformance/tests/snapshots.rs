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
