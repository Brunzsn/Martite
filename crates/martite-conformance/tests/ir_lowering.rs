//! Lowering gate for the conformance corpus.
//!
//! Every snapshot case that checks must lower and verify. Errors are never an
//! expected outcome: a lowering or verifier failure is a bug in lowering or in
//! the corpus. The benchmark corpus separately records lowering and
//! verification cost so verification stays cheap relative to lowering.

use std::hint::black_box;
use std::time::{Duration, Instant};

use martite_conformance::snapshot_cases;
use martite_ir::{
    BuiltinFnId, BuiltinItemId, Charge, Effects, EntryKind, IrId, IrProgram, LowerOptions,
    NodeKind, lower, lower_with, verify,
};

#[test]
fn every_checkable_fixture_lowers_and_verifies() {
    let mut checked = 0;
    let mut lowered = 0;
    let mut skipped = 0;
    for case in snapshot_cases::all_cases() {
        let Some((program, sources)) = case.checked_program() else {
            skipped += 1;
            continue;
        };
        checked += 1;
        let ir: IrProgram =
            lower(&program, &sources).unwrap_or_else(|error| panic!("{}: {error}", case.id()));
        verify(&ir)
            .unwrap_or_else(|error| panic!("{} lowered but did not verify: {error}", case.id()));
        lowered += 1;
    }
    assert!(checked > 0, "no conformance fixture checks");
    assert_eq!(
        lowered, checked,
        "every checked fixture must lower (lowered {lowered}, checked {checked}, skipped {skipped})"
    );
    eprintln!("ir lowering gate: lowered {lowered}, skipped {skipped}");
}

#[test]
fn every_fixture_uses_the_lowered_schedule() {
    let mut checked = 0;
    for case in snapshot_cases::all_cases() {
        let Some((program, sources)) = case.checked_program() else {
            continue;
        };
        checked += 1;
        let ir = lower(&program, &sources).unwrap_or_else(|error| panic!("{}: {error}", case.id()));
        verify(&ir)
            .unwrap_or_else(|error| panic!("{} lowered but did not verify: {error}", case.id()));

        for function in &ir.fns {
            let mut tail_exprs = vec![false; function.nodes.len()];
            for node in &function.nodes {
                let (NodeKind::Block(range) | NodeKind::ExprBlock(range)) = node.kind else {
                    continue;
                };
                if range.len == 0 {
                    continue;
                }
                let last_index = range.start as usize + range.len as usize - 1;
                let Some(last) = function.children.get(last_index).copied() else {
                    continue;
                };
                if matches!(
                    function.node(last).map(|node| node.kind),
                    Some(NodeKind::ExprStmt(_))
                ) && let Some(slot) = tail_exprs.get_mut(last.index())
                {
                    *slot = true;
                }
            }
            for (index, node) in function.nodes.iter().enumerate() {
                let id = IrId::new(index as u32);
                let context = format!("{}: {} node #{index}", case.id(), function.name);
                assert!(
                    !matches!(node.kind, NodeKind::Charge(_)),
                    "{context}: lowering must not insert Charge compensation nodes"
                );
                match node.kind {
                    NodeKind::While { .. } | NodeKind::For { .. } => {
                        assert_eq!(node.charge, Charge::new(0, 0, 1), "{context}: loop charge");
                    }
                    NodeKind::Map(_) => {
                        assert_eq!(node.charge.post, 0, "{context}: map post is dynamic");
                        assert!(
                            node.effects.contains(Effects::DYN_COST),
                            "{context}: dynamic map post must set DYN_COST"
                        );
                    }
                    NodeKind::PreludeVariant { .. } => {
                        assert_eq!(
                            node.charge,
                            Charge::expression(),
                            "{context}: prelude charge"
                        );
                    }
                    NodeKind::Break | NodeKind::Continue => {
                        assert!(
                            node.effects.contains(Effects::NONLOCAL_EXIT),
                            "{context}: break/continue carry NONLOCAL_EXIT"
                        );
                    }
                    NodeKind::MutArg(_) => {
                        assert_eq!(node.charge, Charge::ZERO, "{context}: mut argument charge");
                    }
                    kind if kind.is_call() => {
                        assert_eq!(node.charge, Charge::expression(), "{context}: call charge");
                    }
                    _ => {}
                }

                let required = id == function.body.expect("body")
                    || (matches!(node.kind, NodeKind::ExprStmt(_)) && !tail_exprs[index])
                    || matches!(
                        node.kind,
                        NodeKind::Let { .. }
                            | NodeKind::Assign { .. }
                            | NodeKind::AddAssign { .. }
                            | NodeKind::SubAssign { .. }
                            | NodeKind::MulAssign { .. }
                            | NodeKind::DivAssign { .. }
                            | NodeKind::RemAssign { .. }
                            | NodeKind::Return(_)
                            | NodeKind::CallHost { .. }
                    )
                    || matches!(
                        node.kind,
                        NodeKind::CallBuiltinFn { builtin, .. } if builtin == BuiltinFnId::PRINT
                    )
                    || matches!(
                        node.kind,
                        NodeKind::CallBuiltinMethod { method, .. }
                            if method.def().is_some_and(|def| matches!(
                                def.name,
                                "sorted" | "sorted_by" | "sort"
                            ))
                    )
                    || matches!(
                        node.kind,
                        NodeKind::CallBuiltinModule { item, .. } if item == BuiltinItemId::JSON_PARSE
                    );
                assert_eq!(
                    function.is_safepoint(id),
                    required,
                    "{context}: safepoint placement"
                );
            }
        }
    }
    assert!(checked > 0, "no conformance fixture checks");
}

#[test]
fn scripts_tests_and_plugins_have_entries() {
    let find = |id: &str| {
        snapshot_cases::all_cases()
            .into_iter()
            .find(|case| case.id() == id)
            .unwrap_or_else(|| panic!("missing case {id}"))
    };

    let (program, sources) = find("e2e/hello_world")
        .checked_program()
        .expect("hello_world checks");
    let ir = lower(&program, &sources).expect("lower hello_world");
    assert_eq!(ir.entries.entry_kind, EntryKind::Script);
    assert!(ir.entries.main.is_some());

    let (program, sources) = find("plugins/init_state_and_handler_copy_out_work")
        .checked_program()
        .expect("plugin checks");
    let ir = lower(&program, &sources).expect("lower plugin");
    assert_eq!(ir.entries.entry_kind, EntryKind::Plugin);
    assert!(ir.entries.main.is_none());
    assert!(ir.entries.init.is_some());
    assert!(ir.entries.shutdown.is_some());
    assert!(ir.entries.state_type.is_some());
    assert!(!ir.entries.hooks.is_empty());
    assert!(
        ir.entries
            .handlers
            .iter()
            .any(|(name, _)| name.as_ref() == "init")
    );
    assert!(
        ir.entries
            .handlers
            .iter()
            .any(|(name, _)| name.as_ref() == "saved")
    );
    assert!(
        !ir.entries
            .handlers
            .iter()
            .any(|(name, _)| name.as_ref() == "shutdown"),
        "shutdown has its own entry and is not in the handler map"
    );
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
        .filter(|case| case.id().starts_with("bench/"))
        .filter_map(|case| case.checked_program())
        .collect();
    assert!(!cases.is_empty(), "no benchmark fixtures check");

    let programs: Vec<_> = cases
        .iter()
        .map(|(program, sources)| {
            lower_with(program, sources, LowerOptions { verify: false })
                .expect("benchmark fixture lowers")
        })
        .collect();

    const REPS: u32 = 10;
    let start = Instant::now();
    for _ in 0..REPS {
        for (program, sources) in &cases {
            black_box(
                lower_with(program, sources, LowerOptions { verify: false })
                    .expect("benchmark fixture lowers"),
            );
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
        "ir benchmark-corpus cost: lower={lower_time:?}, verify={verify_time:?}, ratio={:.3}",
        verify_time.as_secs_f64() / lower_time.as_secs_f64().max(f64::EPSILON)
    );
    assert!(
        verify_time <= lower_time.saturating_mul(8) + Duration::from_millis(25),
        "verification cost is not small relative to lowering: lower={lower_time:?}, verify={verify_time:?}"
    );
}
