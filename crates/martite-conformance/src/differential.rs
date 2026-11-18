//! IR-vs-snapshot differential harness (#112).
//!
//! The harness enumerates every snapshot case, compares the full observable
//! record and checks the IR at the recorded fuel, memory and depth thresholds.
//! The checked-in records were captured from the retired AST interpreter, so
//! they remain the independent oracle for the IR evaluator.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use martite_ir::{lower, verify};
use martite_opt::{OptimizeOptions, optimize};

use crate::snapshot::{ProgramStage, SnapshotCase, SnapshotRecord};
use crate::snapshot_cases;

/// One differing field in a snapshot record or threshold check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDifference {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// One fixture that did not match its oracle.
#[derive(Debug)]
pub struct CaseFailure {
    pub id: String,
    pub note: Option<String>,
    pub differences: Vec<FieldDifference>,
    pub ir_dump: Option<String>,
}

/// Aggregate result for the whole corpus.
#[derive(Debug)]
pub struct CorpusReport {
    pub covered: usize,
    pub skipped: usize,
    pub failures: Vec<CaseFailure>,
}

impl CorpusReport {
    pub fn assert_ok(self) {
        eprintln!(
            "IR differential: covered {}, skipped {}",
            self.covered, self.skipped
        );
        assert_eq!(
            self.skipped, 0,
            "IR differential silently skipped {} fixture(s); a fixture with a golden must load, check and run",
            self.skipped
        );
        if self.failures.is_empty() {
            return;
        }
        let mut message = String::new();
        writeln!(
            message,
            "IR differential failed for {} of {} fixtures:",
            self.failures.len(),
            self.covered
        )
        .expect("write report");
        for failure in &self.failures {
            writeln!(message, "\nfixture {}", failure.id).expect("write report");
            if let Some(note) = &failure.note {
                writeln!(message, "  {note}").expect("write report");
            }
            if let Some(first) = failure.differences.first() {
                writeln!(message, "  first difference: {}", first.field).expect("write report");
                writeln!(message, "    expected: {}", first.expected).expect("write report");
                writeln!(message, "    actual:   {}", first.actual).expect("write report");
                for difference in &failure.differences[1..] {
                    writeln!(message, "  difference: {}", difference.field).expect("write report");
                    writeln!(message, "    expected: {}", difference.expected)
                        .expect("write report");
                    writeln!(message, "    actual:   {}", difference.actual).expect("write report");
                }
            }
            if let Some(dump) = &failure.ir_dump {
                writeln!(message, "\n  IR dump:\n{dump}").expect("write report");
            }
        }
        panic!("{message}");
    }
}

/// Run the full corpus against `expected`.
pub fn run_all(expected: &BTreeMap<String, SnapshotRecord>) -> CorpusReport {
    let cases = snapshot_cases::all_cases();
    let mut covered = 0;
    let skipped = 0;
    let mut failures = Vec::new();

    for case in cases {
        let Some(expected) = expected.get(case.id()) else {
            panic!(
                "missing snapshot for {}; run:\n  cargo test -p martite-conformance --test snapshots -- --ignored update_snapshots",
                case.id()
            );
        };
        if case.checked_program().is_none() {
            failures.push(load_failure(&case));
            continue;
        }
        covered += 1;

        let ir = case.observe();
        let mut differences = observable_differences(expected, &ir);
        match case.verify_ir_thresholds(expected) {
            Ok(()) => {}
            Err(mismatches) => {
                differences.extend(mismatches.into_iter().map(|mismatch| FieldDifference {
                    field: mismatch.field.into(),
                    expected: mismatch.expected,
                    actual: mismatch.actual,
                }));
            }
        }
        if !differences.is_empty() {
            failures.push(CaseFailure {
                id: case.id().to_owned(),
                note: None,
                differences,
                ir_dump: Some(ir_dump(&case, ProgramStage::Lowered)),
            });
        }
    }

    CorpusReport {
        covered,
        skipped,
        failures,
    }
}

/// Run passes off versus on for every executable fixture.
///
/// The lowered observation is the oracle for the optimized observation: under
/// `Preserve`, every observable field, exact fuel and all three thresholds
/// must match. The checked-in snapshot is also checked against the optimized
/// record so a pass cannot quietly replace the oracle.
pub fn run_optimized(expected: &BTreeMap<String, SnapshotRecord>) -> CorpusReport {
    let cases = snapshot_cases::all_cases();
    let mut covered = 0;
    let skipped = 0;
    let mut failures = Vec::new();

    for case in cases {
        let Some(expected) = expected.get(case.id()) else {
            panic!(
                "missing snapshot for {}; run:\n  cargo test -p martite-conformance --test snapshots -- --ignored update_snapshots",
                case.id()
            );
        };
        if case.checked_program().is_none() {
            failures.push(load_failure(&case));
            continue;
        }
        covered += 1;

        let lowered = case.observe_with(ProgramStage::Lowered);
        let optimized = case.observe_with(ProgramStage::Optimized);
        let mut differences = observable_differences(&lowered, &optimized);
        differences.extend(observable_differences(expected, &optimized));
        match case.verify_ir_thresholds_with(expected, ProgramStage::Optimized) {
            Ok(()) => {}
            Err(mismatches) => {
                differences.extend(mismatches.into_iter().map(|mismatch| FieldDifference {
                    field: mismatch.field.into(),
                    expected: mismatch.expected,
                    actual: mismatch.actual,
                }));
            }
        }
        if !differences.is_empty() {
            failures.push(CaseFailure {
                id: case.id().to_owned(),
                note: Some("optimizer differential".into()),
                differences,
                ir_dump: Some(ir_dump(&case, ProgramStage::Optimized)),
            });
        }
    }

    CorpusReport {
        covered,
        skipped,
        failures,
    }
}

fn load_failure(case: &SnapshotCase) -> CaseFailure {
    CaseFailure {
        id: case.id().to_owned(),
        note: Some("fixture has a golden but does not load or check".into()),
        differences: vec![FieldDifference {
            field: "fixture".into(),
            expected: "loads and checks".into(),
            actual: "load or check failed".into(),
        }],
        ir_dump: None,
    }
}

fn ir_dump(case: &SnapshotCase, stage: ProgramStage) -> String {
    let Some((program, sources)) = case.checked_program() else {
        return "<not executable>".into();
    };
    match lower(&program, &sources) {
        Ok(mut ir) => {
            if stage == ProgramStage::Optimized
                && let Err(error) = optimize(&mut ir, OptimizeOptions::default())
            {
                return format!("<IR optimization failed: {error}>");
            }
            match verify(&ir) {
                Ok(()) => ir.print(),
                Err(error) => format!("<IR verifier rejected the fixture: {error}>"),
            }
        }
        Err(error) => format!("<IR lowering failed: {error}>"),
    }
}

/// Compare the full observable record.
///
/// The numeric threshold levels are checked behaviorally by
/// [`SnapshotCase::verify_ir_thresholds`], but the note is part of the record and
/// stays in the comparison. Dropping the whole `thresholds` field would let a
/// forged note or a limit-trap opt-out pass unnoticed.
pub fn observable_differences(
    expected: &SnapshotRecord,
    actual: &SnapshotRecord,
) -> Vec<FieldDifference> {
    let mut expected = serde_json::to_value(expected).expect("serialize expected snapshot");
    let mut actual = serde_json::to_value(actual).expect("serialize actual snapshot");
    if let Some(object) = expected.as_object_mut() {
        remove_threshold_levels(object);
    }
    if let Some(object) = actual.as_object_mut() {
        remove_threshold_levels(object);
    }
    let mut differences = Vec::new();
    compare_values("", &expected, &actual, &mut differences);
    differences
}

fn remove_threshold_levels(object: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(serde_json::Value::Object(thresholds)) = object.get_mut("thresholds") {
        thresholds.remove("fuel");
        thresholds.remove("memory");
        thresholds.remove("depth");
    }
}

fn compare_values(
    path: &str,
    expected: &serde_json::Value,
    actual: &serde_json::Value,
    differences: &mut Vec<FieldDifference>,
) {
    use serde_json::Value;

    if expected == actual {
        return;
    }
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, expected) in expected {
                let child = join_path(path, key);
                match actual.get(key) {
                    Some(actual) => compare_values(&child, expected, actual, differences),
                    None => differences.push(FieldDifference {
                        field: child,
                        expected: render_value(expected),
                        actual: "<missing>".into(),
                    }),
                }
            }
            for (key, actual) in actual {
                if !expected.contains_key(key) {
                    differences.push(FieldDifference {
                        field: join_path(path, key),
                        expected: "<missing>".into(),
                        actual: render_value(actual),
                    });
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                differences.push(FieldDifference {
                    field: format!("{path}.len"),
                    expected: expected.len().to_string(),
                    actual: actual.len().to_string(),
                });
            }
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                compare_values(&format!("{path}[{index}]"), expected, actual, differences);
            }
        }
        _ => differences.push(FieldDifference {
            field: path.into(),
            expected: render_value(expected),
            actual: render_value(actual),
        }),
    }
}

fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.into()
    } else {
        format!("{path}.{key}")
    }
}

fn render_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => format!("{text:?}"),
        _ => value.to_string(),
    }
}
