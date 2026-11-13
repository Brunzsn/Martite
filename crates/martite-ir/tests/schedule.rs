//! Charge, effect and safepoint audit for lowering (#107).
//!
//! These tests assert the schedule directly instead of relying on the
//! verifier: lowering and the verifier share the leaf-effect rules, so a wrong
//! rule would otherwise agree with itself.

use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load};
use martite_ir::{
    BuiltinFnId, BuiltinItemId, Charge, Effects, IrFn, IrId, IrProgram, Node, NodeKind, Printer,
    Ty, lower, verify,
};
use martite_resolve::InterfaceSource;

fn lower_source_result(
    source: &str,
    interfaces: Vec<InterfaceSource>,
    grants: Vec<String>,
) -> IrProgram {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
            interfaces,
            grants,
            limits: LoadLimits::default(),
            ..LoadOptions::default()
        },
    );
    assert!(
        !loaded.has_errors(),
        "fixture must check: {:?}",
        loaded.diagnostics
    );
    let program = loaded.program.expect("checked program");
    let ir = lower(&program, &loaded.sources).unwrap_or_else(|error| panic!("{error}"));
    verify(&ir).unwrap_or_else(|error| panic!("{error}"));
    ir
}

fn lower_source(source: &str) -> IrProgram {
    lower_source_result(source, Vec::new(), Vec::new())
}

fn named_function<'a>(ir: &'a IrProgram, name: &str) -> &'a IrFn {
    ir.fns
        .iter()
        .find(|function| function.name.as_ref() == name)
        .unwrap_or_else(|| panic!("function `{name}`"))
}

fn find_node(function: &IrFn, predicate: impl Fn(&NodeKind) -> bool) -> (IrId, &Node) {
    function
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| predicate(&node.kind))
        .map(|(index, node)| (IrId::new(index as u32), node))
        .unwrap_or_else(|| panic!("no matching node in `{}`", function.name))
}

fn assert_charge(node: &Node, pre: u32, post: u32, per_iter: u32) {
    assert_eq!(
        node.charge,
        Charge::new(pre, post, per_iter),
        "charge for {:?}",
        node.kind
    );
}

fn assert_has(effects: Effects, required: Effects, context: &str) {
    assert!(
        effects.contains(required),
        "{context}: expected {required:?}, found {effects:?}"
    );
}

fn assert_lacks(effects: Effects, forbidden: Effects, context: &str) {
    assert!(
        !effects.intersects(forbidden),
        "{context}: did not expect {forbidden:?}, found {effects:?}"
    );
}

#[test]
fn compound_assignment_has_no_binary_entry_and_no_read() {
    let ir = lower_source(
        r#"
fn update() -> int {
    var x = 0
    x += 1
    x = x + 1
    x
}
"#,
    );
    let function = named_function(&ir, "update");
    let (assign_id, add_assign) =
        find_node(function, |kind| matches!(kind, NodeKind::AddAssign { .. }));
    let (_, add) = find_node(function, |kind| matches!(kind, NodeKind::Add { .. }));

    assert_charge(add_assign, 0, 0, 0);
    assert_charge(add, 1, 0, 0);
    assert_has(
        add_assign.effects,
        Effects::READS_LOCAL | Effects::WRITES_LOCAL | Effects::MAY_TRAP | Effects::PANICS,
        "compound assignment",
    );
    assert_lacks(
        add_assign.effects,
        Effects::DYN_COST | Effects::ALLOCATES,
        "int compound assignment",
    );

    let NodeKind::AddAssign { place, value } = add_assign.kind else {
        unreachable!()
    };
    assert!(
        !function
            .nodes
            .iter()
            .skip(place.index() + 1)
            .take(value.index().saturating_sub(place.index() + 1))
            .any(|node| matches!(node.kind, NodeKind::SlotRead { .. })),
        "the current place value is not a lowered read"
    );
    assert_eq!(
        function.node(assign_id).expect("assign").charge,
        Charge::ZERO
    );
}

#[test]
fn while_and_for_schedules_are_per_iteration() {
    let ir = lower_source(
        r#"
fn loops(xs: list[int]) {
    var i = 0
    while i < 3 { i += 1 }
    for value in xs { }
    let values = [1, 2]
    let _ = values
}
"#,
    );
    let function = named_function(&ir, "loops");
    let (_, while_node) = find_node(function, |kind| matches!(kind, NodeKind::While { .. }));
    let (_, for_node) = find_node(function, |kind| matches!(kind, NodeKind::For { .. }));
    let (_, list) = find_node(function, |kind| matches!(kind, NodeKind::List(_)));

    assert_charge(while_node, 0, 0, 1);
    assert_charge(for_node, 0, 0, 1);
    assert_charge(list, 1, 2, 0);
    assert_lacks(
        for_node.effects,
        Effects::ALLOCATES,
        "for over a list of ints with an empty body",
    );
}

#[test]
fn expression_blocks_charge_once_and_body_blocks_do_not() {
    let ir = lower_source(
        r#"
fn main() {
    let value = if true { 1 } else { 2 }
    let _ = value
}
"#,
    );
    let function = named_function(&ir, "main");
    let (_, if_node) = find_node(function, |kind| matches!(kind, NodeKind::If(_)));
    let (_, expr_block) = find_node(function, |kind| matches!(kind, NodeKind::ExprBlock(_)));
    let (_, body_block) = find_node(function, |kind| matches!(kind, NodeKind::Block(_)));

    assert_charge(if_node, 1, 0, 0);
    assert_charge(expr_block, 1, 0, 0);
    assert_charge(body_block, 0, 0, 0);
    assert!(
        function.is_safepoint(function.body.expect("body")),
        "the function body is the entry/fallthrough safepoint"
    );
    assert!(
        !function.is_safepoint(
            function
                .nodes
                .iter()
                .position(|node| matches!(node.kind, NodeKind::ExprBlock(_)))
                .map(|index| IrId::new(index as u32))
                .expect("expression block")
        ),
        "an expression block is not a body safepoint"
    );
}

#[test]
fn struct_and_map_literals_keep_source_order_and_dynamic_post() {
    let ir = lower_source(
        r#"
struct Point { x: int, y: int }

fn make() {
    let p = Point { y: 2, x: 1 }
    let m = { "b": 2, "a": 1, "b": 3 }
}
"#,
    );
    let function = named_function(&ir, "make");
    let (_, struct_node) = find_node(function, |kind| matches!(kind, NodeKind::Struct { .. }));
    let (_, map_node) = find_node(function, |kind| matches!(kind, NodeKind::Map(_)));

    assert_charge(struct_node, 1, 2, 0);
    assert_has(struct_node.effects, Effects::ALLOCATES, "struct literal");
    assert_lacks(
        struct_node.effects,
        Effects::DYN_COST,
        "struct literal post is static",
    );

    assert_charge(map_node, 1, 0, 0);
    assert_has(
        map_node.effects,
        Effects::ALLOCATES | Effects::DYN_COST,
        "map literal",
    );

    let NodeKind::Map(range) = map_node.kind else {
        unreachable!()
    };
    let children = function.child_slice(range);
    assert_eq!(children.len(), 6);
    let kinds: Vec<_> = children
        .iter()
        .map(|child| function.node(*child).expect("child").kind)
        .collect();
    assert!(matches!(kinds[0], NodeKind::Str(_)));
    assert!(matches!(kinds[1], NodeKind::Int(_)));
    assert!(matches!(kinds[2], NodeKind::Str(_)));
    assert!(matches!(kinds[3], NodeKind::Int(_)));
    assert!(matches!(kinds[4], NodeKind::Str(_)));
    assert!(matches!(kinds[5], NodeKind::Int(_)));
}

#[test]
fn prelude_constructors_have_no_post_charge() {
    let ir = lower_source(
        r#"
fn make() {
    let a: int? = Some(1)
    let b: Result[int, str] = Ok(2)
}
"#,
    );
    let function = named_function(&ir, "make");
    let preludes: Vec<_> = function
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::PreludeVariant { .. }))
        .collect();
    assert_eq!(preludes.len(), 2);
    for node in preludes {
        assert_eq!(node.charge, Charge::expression(), "{:?}", node.kind);
        assert_has(node.effects, Effects::ALLOCATES, "prelude constructor");
    }
}

#[test]
fn calls_and_mutable_writeback_carry_caller_effects() {
    let ir = lower_source(
        r#"
fn add(a: int, b: int) -> int { a + b }
fn bump(mut value: int) {}

fn main() {
    var value = 1
    var values = [1, 2]
    let f = add
    let _ = add(value, 2)
    let g = f(value, 2)
    bump(mut value)
    bump(mut values[0])
    let _ = g
}
"#,
    );
    let function = named_function(&ir, "main");

    let (_, direct) = find_node(function, |kind| matches!(kind, NodeKind::CallDirect { .. }));
    assert_charge(direct, 1, 0, 0);
    let (_, value_call) = find_node(function, |kind| matches!(kind, NodeKind::CallValue { .. }));
    assert_charge(value_call, 1, 0, 0);
    let NodeKind::CallValue { first_child, .. } = value_call.kind else {
        unreachable!()
    };
    let callee = function
        .node(function.children[first_child as usize])
        .expect("callee");
    assert!(
        matches!(callee.kind, NodeKind::SlotRead { .. }),
        "indirect callee is evaluated as a value"
    );
    assert_charge(callee, 1, 0, 0);

    let mut_args: Vec<_> = function
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::MutArg(_)))
        .collect();
    assert_eq!(mut_args.len(), 2);
    let root = mut_args
        .iter()
        .find(|node| {
            let NodeKind::MutArg(place) = node.kind else {
                return false;
            };
            matches!(
                function.node(place).expect("place").kind,
                NodeKind::SlotPlace(_)
            )
        })
        .expect("root mut argument");
    assert_charge(root, 0, 0, 0);
    assert_has(
        root.effects,
        Effects::READS_LOCAL | Effects::WRITES_LOCAL,
        "root mut argument",
    );
    assert_lacks(
        root.effects,
        Effects::ALLOCATES | Effects::DYN_COST,
        "root mut argument on an int",
    );

    let nested = mut_args
        .iter()
        .find(|node| {
            let NodeKind::MutArg(place) = node.kind else {
                return false;
            };
            matches!(
                function.node(place).expect("place").kind,
                NodeKind::Index { .. }
            )
        })
        .expect("nested mut argument");
    assert_has(
        nested.effects,
        Effects::READS_LOCAL
            | Effects::WRITES_LOCAL
            | Effects::READS_HEAP
            | Effects::WRITES_HEAP
            | Effects::ALLOCATES
            | Effects::DYN_COST
            | Effects::SHARES
            | Effects::MAY_TRAP
            | Effects::PANICS,
        "nested mut argument",
    );
}

#[test]
fn direct_check_calls_are_safepoints() {
    let ir = lower_source(
        r#"
use std.json

fn main() {
    print("hello")
    let xs = [3, 1].sorted()
    let _ = json.parse("\{\}")
    let _ = xs
}
"#,
    );
    let function = named_function(&ir, "main");

    let (print_id, _) = find_node(
        function,
        |kind| matches!(kind, NodeKind::CallBuiltinFn { builtin, .. } if *builtin == BuiltinFnId::PRINT),
    );
    let (sorted_id, _) = find_node(function, |kind| {
        matches!(
            kind,
            NodeKind::CallBuiltinMethod { method, .. }
                if method.def().is_some_and(|def| def.name == "sorted")
        )
    });
    let (parse_id, _) = find_node(function, |kind| {
        matches!(
            kind,
            NodeKind::CallBuiltinModule { item, .. } if *item == BuiltinItemId::JSON_PARSE
        )
    });

    for (id, context) in [
        (print_id, "print"),
        (sorted_id, "sorted"),
        (parse_id, "json.parse"),
    ] {
        assert!(
            function.is_safepoint(id),
            "{context} must carry a safepoint flag"
        );
        assert_has(
            function.node(id).expect("node").effects,
            Effects::SAFEPOINT,
            context,
        );
    }
}

#[test]
fn host_calls_are_safepoints_and_can_trap() {
    let ir = lower_source_result(
        r#"
use app.bench

fn main() {
    let result = bench.mix(1)
    let _ = result
}
"#,
        vec![InterfaceSource {
            path: "app.bench".into(),
            text: "fn mix(value: int) -> int\n".into(),
        }],
        vec!["app.bench".into()],
    );
    let function = named_function(&ir, "main");
    let (id, node) = find_node(function, |kind| matches!(kind, NodeKind::CallHost { .. }));
    assert!(function.is_safepoint(id));
    assert_has(
        node.effects,
        Effects::HOST | Effects::DYN_COST | Effects::MAY_TRAP | Effects::SAFEPOINT,
        "host call",
    );
}

#[test]
fn safepoint_vector_matches_the_contract() {
    let ir = lower_source(
        r#"
fn helper() -> int {
    var x = 0
    x = 1
    x += 1
    print("{x}")
    let f = fn() { x }
    let g = fn() => x
    let _ = (f, g)
    x
}

fn early(flag: bool) -> int {
    if flag { return 1 }
    2
}

fn main() {
    let _ = (helper(), early(true))
}
"#,
    );
    for function in &ir.fns {
        for (index, node) in function.nodes.iter().enumerate() {
            let id = IrId::new(index as u32);
            let expected = id == function.body.expect("body")
                || matches!(
                    node.kind,
                    NodeKind::Let { .. }
                        | NodeKind::Assign { .. }
                        | NodeKind::AddAssign { .. }
                        | NodeKind::SubAssign { .. }
                        | NodeKind::MulAssign { .. }
                        | NodeKind::DivAssign { .. }
                        | NodeKind::RemAssign { .. }
                        | NodeKind::ExprStmt(_)
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
                        if method
                            .def()
                            .is_some_and(|def| matches!(def.name, "sorted" | "sorted_by" | "sort"))
                )
                || matches!(
                    node.kind,
                    NodeKind::CallBuiltinModule { item, .. } if item == BuiltinItemId::JSON_PARSE
                );
            assert_eq!(
                function.is_safepoint(id),
                expected,
                "safepoint flag for {} node #{index} {:?}",
                function.name,
                node.kind
            );
        }
    }
}

#[test]
fn dynamic_cost_flags_follow_the_interpreter() {
    let ir = lower_source(
        r#"
fn main() {
    let a = "ab" + "cd"
    let b = "{a}"
    let c = a == b
    let xs = [3, 1].sorted()
    let part = xs[0..1]
    let k = 1 + 2
    print("{c}")
    let _ = (b, part, k)
}
"#,
    );
    let function = named_function(&ir, "main");

    let (_, string_add) = function
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            matches!(node.kind, NodeKind::Add { .. })
                && ir
                    .types
                    .get_ty(node.ty)
                    .is_some_and(|ty| matches!(ty, Ty::Str))
        })
        .map(|(index, node)| (IrId::new(index as u32), node))
        .expect("string add");
    assert_has(
        string_add.effects,
        Effects::DYN_COST | Effects::ALLOCATES,
        "string concatenation",
    );
    let (_, int_add) = function
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            matches!(node.kind, NodeKind::Add { .. })
                && ir
                    .types
                    .get_ty(node.ty)
                    .is_some_and(|ty| matches!(ty, Ty::Int))
        })
        .map(|(index, node)| (IrId::new(index as u32), node))
        .expect("int add");
    assert_has(int_add.effects, Effects::PANICS, "int addition");
    assert_lacks(
        int_add.effects,
        Effects::DYN_COST | Effects::ALLOCATES,
        "int addition",
    );

    let (_, interp) = find_node(function, |kind| matches!(kind, NodeKind::StrInterp(_)));
    assert_has(interp.effects, Effects::DYN_COST, "interpolation");
    let (_, eq) = find_node(function, |kind| matches!(kind, NodeKind::Eq { .. }));
    assert_has(eq.effects, Effects::DYN_COST, "comparison");
    let (_, sorted) = find_node(function, |kind| {
        matches!(
            kind,
            NodeKind::CallBuiltinMethod { method, .. }
                if method.def().is_some_and(|def| def.name == "sorted")
        )
    });
    assert_has(
        sorted.effects,
        Effects::DYN_COST | Effects::ALLOCATES | Effects::SHARES | Effects::SAFEPOINT,
        "sorted",
    );
    let (_, print) = find_node(
        function,
        |kind| matches!(kind, NodeKind::CallBuiltinFn { builtin, .. } if *builtin == BuiltinFnId::PRINT),
    );
    assert_has(
        print.effects,
        Effects::LOGS | Effects::DYN_COST | Effects::SAFEPOINT,
        "print",
    );
    let (_, slice) = find_node(function, |kind| matches!(kind, NodeKind::Index { .. }));
    assert_has(
        slice.effects,
        Effects::DYN_COST | Effects::ALLOCATES | Effects::PANICS,
        "list slice",
    );
}

#[test]
fn const_refs_do_not_claim_a_dynamic_cost() {
    let ir = lower_source(
        r#"
const VALUES: list[int] = [1, 2]

fn main() {
    let values = VALUES
    let _ = values
}
"#,
    );
    let function = named_function(&ir, "main");
    let (_, const_ref) = find_node(function, |kind| matches!(kind, NodeKind::ConstRef(_)));
    assert_has(
        const_ref.effects,
        Effects::ALLOCATES | Effects::SHARES,
        "heap constant",
    );
    assert_lacks(
        const_ref.effects,
        Effects::DYN_COST,
        "materializing a constant charges no dynamic fuel",
    );
}

#[test]
fn break_continue_map_iteration_and_panics_are_marked() {
    let ir = lower_source(
        r#"
fn main() {
    while true { break }
    for value in [1] { continue }
    let table = { "a": [1] }
    for (key, value) in table { let _ = (key, value) }
}
"#,
    );
    let function = named_function(&ir, "main");
    let (_, break_node) = find_node(function, |kind| matches!(kind, NodeKind::Break));
    let (_, continue_node) = find_node(function, |kind| matches!(kind, NodeKind::Continue));
    assert_has(break_node.effects, Effects::NONLOCAL_EXIT, "break");
    assert_has(continue_node.effects, Effects::NONLOCAL_EXIT, "continue");
    assert_has(
        function
            .node(function.body.expect("body"))
            .expect("body")
            .effects,
        Effects::NONLOCAL_EXIT,
        "function body",
    );

    let map_for = function
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::For { .. }))
        .find(|node| {
            let NodeKind::For { first_child, .. } = node.kind else {
                return false;
            };
            function
                .node(function.children[first_child as usize])
                .is_some_and(|iterable| {
                    ir.types
                        .get_ty(iterable.ty)
                        .is_some_and(|ty| matches!(ty, Ty::Map(_, _)))
                })
        })
        .expect("for over a map");
    assert_has(
        map_for.effects,
        Effects::ALLOCATES | Effects::SHARES,
        "for over a map",
    );
}

#[test]
fn printer_shows_charge_effects_and_safepoint_for_every_node() {
    let ir = lower_source(
        r#"
fn main() {
    var value = [1, 2]
    value[0] = 3
}
"#,
    );
    let printer = Printer::new(&ir);
    for function in &ir.fns {
        let dump = printer.function(function);
        for (index, node) in function.nodes.iter().enumerate() {
            let needle = format!("#{} ", index);
            let line = dump
                .lines()
                .find(|line| line.trim_start().starts_with(&needle) && line.contains(" span="))
                .unwrap_or_else(|| panic!("missing node #{index} in dump:\n{dump}"));
            assert!(
                line.contains(&format!(
                    "charge={}/{}/{}",
                    node.charge.pre, node.charge.post, node.charge.per_iter
                )),
                "missing charge for node #{index}: {line}"
            );
            assert!(
                line.contains("effects="),
                "missing effects for node #{index}: {line}"
            );
            assert!(
                line.contains("safepoint="),
                "missing safepoint for node #{index}: {line}"
            );
        }
    }
}
