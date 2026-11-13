//! Lowering gate for the conformance corpus.
//!
//! #103 only lowers the first-half constructs. This test walks every snapshot
//! case, verifies every program that lowers, and fails if lowering reports an
//! internal error or an unexpected unsupported construct. When #104–#106 land,
//! the allowed gap set shrinks to empty and the corpus-wide gate moves to #106.

use std::hint::black_box;
use std::time::{Duration, Instant};

use martite_conformance::snapshot_cases;
use martite_ir::{LowerErrorKind, lower, verify};

const KNOWN_GAPS: &[&str] = &[
    "call",
    "pattern",
    "match",
    "lambda",
    "method value",
    "variant value",
    "mut argument",
    "builtin item value",
    "function value kind",
];

#[test]
fn checkable_fixtures_lower_or_report_a_known_gap() {
    let mut lowered = 0;
    let mut skipped = 0;
    for case in snapshot_cases::all_cases() {
        let Some((program, sources)) = case.checked_program() else {
            skipped += 1;
            continue;
        };
        match lower(&program, &sources) {
            Ok(ir) => {
                verify(&ir).unwrap_or_else(|error| {
                    panic!("{} lowered but did not verify: {error}", case.id())
                });
                lowered += 1;
            }
            Err(error) => match error.kind {
                LowerErrorKind::Unsupported { construct } => {
                    assert!(
                        KNOWN_GAPS.contains(&construct),
                        "{}: unexpected unsupported construct `{construct}`",
                        case.id()
                    );
                }
                other => panic!("{}: unexpected lowering error {other:?}", case.id()),
            },
        }
    }
    assert!(lowered > 0, "no conformance fixture lowered");
    eprintln!("ir lowering gate: lowered {lowered}, skipped {skipped}");
}

#[test]
fn verification_cost_is_small_relative_to_lowering() {
    let cases: Vec<_> = snapshot_cases::all_cases()
        .into_iter()
        .filter_map(|case| case.checked_program())
        .collect();
    let programs: Vec<_> = cases
        .iter()
        .filter_map(|(program, sources)| lower(program, sources).ok())
        .collect();
    assert!(!programs.is_empty(), "no programs to measure");

    const REPS: u32 = 10;
    let start = Instant::now();
    for _ in 0..REPS {
        for (program, sources) in &cases {
            black_box(lower(program, sources).ok());
        }
    }
    let lower_time = start.elapsed();

    let start = Instant::now();
    for _ in 0..REPS {
        for program in &programs {
            verify(black_box(program)).expect("lowered program verifies");
        }
    }
    let verify_time = start.elapsed();

    eprintln!(
        "ir verification cost: lower={lower_time:?}, verify={verify_time:?}, ratio={:.3}",
        verify_time.as_secs_f64() / lower_time.as_secs_f64().max(f64::EPSILON)
    );
    assert!(
        verify_time <= lower_time.saturating_mul(8) + Duration::from_millis(25),
        "verification cost is not small relative to lowering: lower={lower_time:?}, verify={verify_time:?}"
    );
}
