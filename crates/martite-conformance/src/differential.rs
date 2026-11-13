//! AST-vs-IR differential harness for the conformance corpus (#112).
//!
//! The harness enumerates every snapshot case, compares the full observable
//! record and checks the IR at the AST oracle's exact fuel, memory and depth
//! thresholds. The same comparison runs against recorded snapshots only, which
//! is the oracle that survives the AST interpreter (#116).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use martite_ir::{lower, verify};

use crate::snapshot::{SnapshotCase, SnapshotRecord};
use crate::snapshot_cases;

/// Where the expected record comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oracle {
    /// Run the AST interpreter and compare it with the recorded oracle first.
    LiveAst,
    /// Compare the IR directly with the recorded snapshots.
    Snapshots,
}

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
    pub oracle: Oracle,
    pub covered: usize,
    pub skipped: usize,
    pub failures: Vec<CaseFailure>,
}

impl CorpusReport {
    pub fn assert_ok(self) {
        eprintln!(
            "IR differential ({:?}): covered {}, skipped {}",
            self.oracle, self.covered, self.skipped
        );
        if self.failures.is_empty() {
            return;
        }
        let mut message = String::new();
        writeln!(
            message,
            "IR differential ({:?}) failed for {} of {} fixtures:",
            self.oracle,
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
pub fn run_all(oracle: Oracle, expected: &BTreeMap<String, SnapshotRecord>) -> CorpusReport {
    let cases = snapshot_cases::all_cases();
    let mut covered = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for case in cases {
        let Some(expected) = expected.get(case.id()) else {
            panic!(
                "missing snapshot for {}; run:\n  cargo test -p martite-conformance --test snapshots -- --ignored update_snapshots",
                case.id()
            );
        };
        if case.checked_program().is_none() {
            skipped += 1;
            continue;
        }
        covered += 1;

        if oracle == Oracle::LiveAst {
            let ast = case.observe();
            let differences = observable_differences(expected, &ast);
            if !differences.is_empty() {
                failures.push(CaseFailure {
                    id: case.id().to_owned(),
                    note: Some("live AST did not reproduce the recorded oracle".into()),
                    differences,
                    ir_dump: Some(ir_dump(&case)),
                });
                continue;
            }
        }

        let ir = case.observe_ir();
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
                ir_dump: Some(ir_dump(&case)),
            });
        }
    }

    CorpusReport {
        oracle,
        covered,
        skipped,
        failures,
    }
}

fn ir_dump(case: &SnapshotCase) -> String {
    let Some((program, sources)) = case.checked_program() else {
        return "<not executable>".into();
    };
    match lower(&program, &sources) {
        Ok(ir) => match verify(&ir) {
            Ok(()) => ir.print(),
            Err(error) => format!("<IR verifier rejected the fixture: {error}>"),
        },
        Err(error) => format!("<IR lowering failed: {error}>"),
    }
}

/// Compare the full observable record, ignoring the threshold fields because
/// they are checked behaviorally by [`SnapshotCase::verify_ir_thresholds`].
pub fn observable_differences(
    expected: &SnapshotRecord,
    actual: &SnapshotRecord,
) -> Vec<FieldDifference> {
    let mut expected = serde_json::to_value(expected).expect("serialize expected snapshot");
    let mut actual = serde_json::to_value(actual).expect("serialize actual snapshot");
    if let Some(object) = expected.as_object_mut() {
        object.remove("thresholds");
    }
    if let Some(object) = actual.as_object_mut() {
        object.remove("thresholds");
    }
    let mut differences = Vec::new();
    compare_values("", &expected, &actual, &mut differences);
    differences
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
