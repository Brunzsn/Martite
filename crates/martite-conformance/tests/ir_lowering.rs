//! Lowering gate for the conformance corpus.
//!
//! Calls are lowered by #104. The remaining gap set belongs to #105 (patterns
//! and match) and #106 (lambdas and capture-dependent values). This test walks
//! every snapshot case, verifies every program that lowers, and fails if
//! lowering reports an internal error or an unexpected unsupported construct.

use std::hint::black_box;
use std::time::{Duration, Instant};

use martite_conformance::snapshot_cases;
use martite_ir::{Charge, IrProgram, LowerErrorKind, NodeKind, lower, verify};

const KNOWN_GAPS: &[&str] = &[
    "lambda",
    "method value",
    "variant value",
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
fn call_fixtures_use_exact_call_and_place_charges() {
    for id in [
        "bench/runtime/mut_params.mt",
        "bench/runtime/field_index_ops.mt",
        "plugins/settings_are_delivered_to_init_as_json",
    ] {
        let case = snapshot_cases::all_cases()
            .into_iter()
            .find(|case| case.id() == id)
            .unwrap_or_else(|| panic!("missing case {id}"));
        let (program, sources) = case
            .checked_program()
            .unwrap_or_else(|| panic!("{id} must check"));
        let ir: IrProgram =
            lower(&program, &sources).unwrap_or_else(|error| panic!("{id}: {error}"));
        verify(&ir).unwrap_or_else(|error| panic!("{id}: {error}"));
        for function in &ir.fns {
            for node in &function.nodes {
                if node.kind.is_call() {
                    assert_eq!(
                        node.charge,
                        Charge::expression(),
                        "{}: call charge",
                        function.name
                    );
                }
                match node.kind {
                    NodeKind::SlotPlace(_) => {
                        assert_eq!(node.charge, Charge::ZERO, "{}: place charge", function.name)
                    }
                    NodeKind::MutArg(place) => {
                        assert_eq!(
                            node.charge,
                            Charge::ZERO,
                            "{}: mut argument wrapper charge",
                            function.name
                        );
                        assert_place_charges(function, place);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn assert_place_charges(function: &martite_ir::IrFn, id: martite_ir::IrId) {
    let node = function.node(id).expect("place node");
    assert_eq!(node.charge, Charge::ZERO, "place chain charge");
    match node.kind {
        NodeKind::SlotPlace(_) => {}
        NodeKind::Field { base, .. } | NodeKind::TupleField { base, .. } => {
            assert_place_charges(function, base);
        }
        NodeKind::Index { base, .. } => assert_place_charges(function, base),
        other => panic!("expected a place node, found {other:?}"),
    }
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
