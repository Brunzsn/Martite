use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load};
use martite_ir::{
    IrFn, IrId, IrProgram, LowerError, LowerErrorKind, Node, NodeKind, lower, verify,
};
use martite_resolve::InterfaceSource;

fn lower_source_result_with(
    source: &str,
    interfaces: Vec<InterfaceSource>,
    grants: Vec<String>,
) -> Result<IrProgram, LowerError> {
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
    lower(&program, &loaded.sources)
}

fn lower_source_result(source: &str) -> Result<IrProgram, LowerError> {
    lower_source_result_with(source, Vec::new(), Vec::new())
}

fn lower_source(source: &str) -> IrProgram {
    let ir = lower_source_result(source).unwrap_or_else(|error| panic!("{error}"));
    verify(&ir).unwrap_or_else(|error| panic!("{error}"));
    ir
}

fn lower_source_with(
    source: &str,
    interfaces: Vec<InterfaceSource>,
    grants: Vec<String>,
) -> IrProgram {
    let ir = lower_source_result_with(source, interfaces, grants)
        .unwrap_or_else(|error| panic!("{error}"));
    verify(&ir).unwrap_or_else(|error| panic!("{error}"));
    ir
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

fn node_kinds(ir: &IrProgram, function: martite_ir::FnId) -> Vec<martite_ir::NodeKind> {
    ir.function(function)
        .expect("function")
        .nodes
        .iter()
        .map(|node| node.kind)
        .collect()
}

fn named_function<'a>(ir: &'a IrProgram, name: &str) -> &'a martite_ir::IrFn {
    ir.fns
        .iter()
        .find(|function| function.name.as_ref() == name)
        .unwrap_or_else(|| panic!("function `{name}`"))
}

#[test]
fn lowers_values_operators_and_aggregates() {
    let ir = lower_source(
        r#"
struct Point { x: int, y: int }
enum Shape { Dot, Line(int, int) }
const LIMIT: int = 3

fn compute(flag: bool, n: int) -> int {
    let p = Point { y: 2, x: 1 }
    let q = Point { ..p, x: p.x + 1 }
    let s = Shape.Line(q.x, q.y)
    let values = [1, 2, 3]
    let table = { "a": 1, "b": 2 }
    let t = (1, "two", flag)
    let r = 1..5
    let maybe: int? = None
    let chosen = maybe ?? n
    let text = "hello {n} and {flag}"
    let as_float = float(n)
    let as_int = int(as_float)
    let as_text = str(t)
    let _ = (q, s, values, table, t, r, chosen, text, as_int, as_text)
    if flag and n > LIMIT { n } else { 0 }
}
"#,
    );
    let function = ir.function(martite_ir::FnId::new(0)).expect("compute");
    assert_eq!(function.name.as_ref(), "compute");
    assert_eq!(function.locals, 15);
}

#[test]
fn lowers_control_flow_and_try() {
    let ir = lower_source(
        r#"
fn loops() -> int {
    var total = 0
    for i in 0..10 {
        if i == 3 { continue }
        if i == 8 { break }
        total += i
    }
    var i = 0
    while i < 3 {
        total = total + i
        i += 1
    }
    if total > 0 {
        total
    } else {
        let inner = 1
        inner + 2
    }
}

fn result() -> Result[int, str] {
    let value: Result[int, str] = Ok(1)
    let inner = value?
    Ok(inner)
}
"#,
    );
    assert_eq!(ir.fns.len(), 2);
}

#[test]
fn lowers_assignments_and_places() {
    let ir = lower_source(
        r#"
struct Point { x: int, y: int }

fn mutate() -> int {
    var p = Point { x: 1, y: 2 }
    p.x = 3
    p.x += 2
    var values = [1, 2, 3]
    values[0] = 4
    values[1] += 5
    p.x + values[0]
}
"#,
    );
    let function = ir.function(martite_ir::FnId::new(0)).expect("mutate");
    assert!(function.node(function.body.unwrap()).is_some());
}

#[test]
fn compound_assignment_and_written_out_addition_differ() {
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
    let kinds = node_kinds(&ir, martite_ir::FnId::new(0));
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, martite_ir::NodeKind::AddAssign { .. }))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, martite_ir::NodeKind::Add { .. }))
    );
    let function = ir.function(martite_ir::FnId::new(0)).expect("update");
    let add_assign = function
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::AddAssign { .. }))
        .expect("compound assignment");
    assert_eq!(add_assign.charge, martite_ir::Charge::ZERO);
    let add = function
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::Add { .. }))
        .expect("binary addition");
    assert_eq!(add.charge, martite_ir::Charge::expression());
}

#[test]
fn lowers_assert_and_panic() {
    let ir = lower_source(
        r#"
fn check(x: int) -> int {
    assert(x > 0, "positive")
    if x == 0 { panic("zero") }
    x
}
"#,
    );
    let kinds = node_kinds(&ir, martite_ir::FnId::new(0));
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, martite_ir::NodeKind::Assert(_)))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, martite_ir::NodeKind::Panic(_)))
    );
}

#[test]
fn lowers_every_call_form() {
    let ir = lower_source(
        r#"
use std.math

struct Counter { n: int }

impl Counter {
    fn bump(mut self, mut by: int) -> int {
        self.n += by
        by += 1
        self.n + by
    }
}

fn add(a: int, b: int) -> int { a + b }

fn main() {
    var counter = Counter { n: 0 }
    var by = 1
    let f = add
    let first = add(1, 2)
    let second = f(3, 4)
    let third = counter.bump(mut by)
    var values = [1, 2, 3]
    values.push(4)
    let smallest = math.min(first, second)
    print("{smallest + third + values.len()}")
}
"#,
    );
    let main = named_function(&ir, "main");

    let (_, direct) = find_node(main, |kind| matches!(kind, NodeKind::CallDirect { .. }));
    let NodeKind::CallDirect { target, .. } = direct.kind else {
        unreachable!()
    };
    assert_eq!(ir.function(target).expect("target").name.as_ref(), "add");

    let (_, value_call) = find_node(main, |kind| matches!(kind, NodeKind::CallValue { .. }));
    let NodeKind::CallValue { first_child, argc } = value_call.kind else {
        unreachable!()
    };
    assert_eq!(argc, 2);
    let callee = main.children[first_child as usize];
    assert!(matches!(
        main.node(callee).expect("callee").kind,
        NodeKind::SlotRead { .. }
    ));

    let (_, method) = find_node(main, |kind| matches!(kind, NodeKind::CallMethod { .. }));
    let NodeKind::CallMethod {
        target,
        first_child,
    } = method.kind
    else {
        unreachable!()
    };
    assert_eq!(ir.function(target).expect("target").name.as_ref(), "bump");
    let receiver = main.children[first_child as usize];
    assert!(matches!(
        main.node(receiver).expect("receiver").kind,
        NodeKind::MutArg(_)
    ));

    let (_, builtin_method) = find_node(main, |kind| {
        matches!(kind, NodeKind::CallBuiltinMethod { .. })
    });
    let NodeKind::CallBuiltinMethod {
        method,
        first_child,
    } = builtin_method.kind
    else {
        unreachable!()
    };
    assert_eq!(method.def().expect("method").name, "push");
    let receiver = main.children[first_child as usize];
    assert!(matches!(
        main.node(receiver).expect("receiver").kind,
        NodeKind::MutArg(_)
    ));

    let (_, builtin_module) = find_node(main, |kind| {
        matches!(kind, NodeKind::CallBuiltinModule { .. })
    });
    let NodeKind::CallBuiltinModule { item, .. } = builtin_module.kind else {
        unreachable!()
    };
    assert_eq!(item.item().expect("item").name, "min");

    let (_, builtin_fn) = find_node(main, |kind| matches!(kind, NodeKind::CallBuiltinFn { .. }));
    let NodeKind::CallBuiltinFn { builtin, .. } = builtin_fn.kind else {
        unreachable!()
    };
    assert_eq!(builtin, martite_ir::BuiltinFnId::PRINT);
}

#[test]
fn mutable_receiver_and_arguments_follow_parameter_order() {
    let ir = lower_source(
        r#"
struct Counter { n: int }

impl Counter {
    fn apply(mut self, mut a: int, mut b: int) { self.n += a + b }
}

fn pair(mut a: int, mut b: int) {
    a += 1
    b += 1
}

fn main() {
    var counter = Counter { n: 0 }
    var a = 1
    var b = 2
    counter.apply(mut a, mut b)
    pair(mut a, mut b)
}
"#,
    );
    let main = named_function(&ir, "main");

    let (_, method) = find_node(main, |kind| matches!(kind, NodeKind::CallMethod { .. }));
    let NodeKind::CallMethod { first_child, .. } = method.kind else {
        unreachable!()
    };
    let children: Vec<_> = (0..3)
        .map(|offset| main.children[first_child as usize + offset])
        .collect();
    let slots: Vec<_> = children
        .iter()
        .map(|child| match main.node(*child).expect("child").kind {
            NodeKind::MutArg(place) => match main.node(place).expect("place").kind {
                NodeKind::SlotPlace(slot) => slot.index(),
                other => panic!("expected slot place, found {other:?}"),
            },
            other => panic!("expected mut argument, found {other:?}"),
        })
        .collect();
    assert_eq!(slots, vec![0, 1, 2], "receiver first, then arguments");

    let (_, direct) = find_node(main, |kind| matches!(kind, NodeKind::CallDirect { .. }));
    let NodeKind::CallDirect { first_arg, .. } = direct.kind else {
        unreachable!()
    };
    let children: Vec<_> = (0..2)
        .map(|offset| main.children[first_arg as usize + offset])
        .collect();
    assert!(
        children
            .iter()
            .all(|child| matches!(main.node(*child).expect("child").kind, NodeKind::MutArg(_)))
    );
    assert!(
        !main.nodes.iter().any(|node| matches!(
            node.kind,
            NodeKind::Assign { .. }
                | NodeKind::AddAssign { .. }
                | NodeKind::SubAssign { .. }
                | NodeKind::MulAssign { .. }
                | NodeKind::DivAssign { .. }
                | NodeKind::RemAssign { .. }
        )),
        "writeback is part of call semantics, not lowered nodes"
    );
}

#[test]
fn mutable_argument_places_charge_only_index_expressions() {
    let ir = lower_source(
        r#"
fn bump(mut value: int) {}

fn main() {
    var values = [1, 2, 3]
    var index = 0
    bump(mut values[index])
}
"#,
    );
    let main = named_function(&ir, "main");
    let (_, call) = find_node(main, |kind| matches!(kind, NodeKind::CallDirect { .. }));
    assert_eq!(call.charge, martite_ir::Charge::expression());

    let (_, mut_arg) = find_node(main, |kind| matches!(kind, NodeKind::MutArg(_)));
    assert_eq!(mut_arg.charge, martite_ir::Charge::ZERO);
    let NodeKind::MutArg(place) = mut_arg.kind else {
        unreachable!()
    };
    let place = main.node(place).expect("place");
    assert_eq!(place.charge, martite_ir::Charge::ZERO);
    let NodeKind::Index { base, index } = place.kind else {
        panic!("expected an index place, found {:?}", place.kind);
    };
    let base = main.node(base).expect("base");
    assert!(matches!(base.kind, NodeKind::SlotPlace(_)));
    assert_eq!(base.charge, martite_ir::Charge::ZERO);
    let index = main.node(index).expect("index");
    assert!(matches!(index.kind, NodeKind::SlotRead { .. }));
    assert_eq!(index.charge, martite_ir::Charge::expression());

    let reads: Vec<_> = main
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::SlotRead { slot, .. } => Some(slot.index()),
            _ => None,
        })
        .collect();
    assert_eq!(reads, vec![1], "the place root is not read as a value");
}

#[test]
fn lowers_handler_calls_with_state_as_the_receiver() {
    let ir = lower_source_with(
        r#"
struct State { n: int }

on init() -> State { State { n: 0 } }
on tick(mut self) { self.n += 1 }

test "handler call" {
    var state = State { n: 0 }
    tick(mut state)
    assert(state.n == 1, "state")
}
"#,
        vec![InterfaceSource {
            path: "app.hooks".into(),
            text: "on tick()\n".into(),
        }],
        vec!["app.hooks".into()],
    );
    let test = ir
        .fns
        .iter()
        .find(|function| matches!(function.kind, martite_ir::FnKind::Test))
        .expect("test function");
    let (_, call) = find_node(test, |kind| matches!(kind, NodeKind::CallMethod { .. }));
    let NodeKind::CallMethod {
        target,
        first_child,
    } = call.kind
    else {
        unreachable!()
    };
    assert_eq!(ir.function(target).expect("target").name.as_ref(), "tick");
    let receiver = test.children[first_child as usize];
    let receiver = test.node(receiver).expect("receiver");
    assert!(matches!(receiver.kind, NodeKind::MutArg(_)));
}

#[test]
fn lowers_host_calls() {
    let ir = lower_source_with(
        r#"
use app.bench

fn compute(seed: int) -> int {
    let token = bench.mint(seed)
    bench.mix(token, seed)
}
"#,
        vec![InterfaceSource {
            path: "app.bench".into(),
            text: "struct Token\nfn mint(seed: int) -> Token\nfn mix(token: Token, value: int) -> int\n"
                .into(),
        }],
        vec!["app.bench".into()],
    );
    let function = named_function(&ir, "compute");
    let hosts: Vec<_> = function
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::CallHost { host, .. } => Some(host),
            _ => None,
        })
        .collect();
    assert_eq!(hosts.len(), 2);
    let names: Vec<_> = hosts
        .iter()
        .map(|host| ir.host(*host).expect("host").name.as_ref())
        .collect();
    assert_eq!(names, vec!["mint", "mix"]);
}

#[test]
fn lowers_references_to_functions_builtins_and_constants() {
    let ir = lower_source(
        r#"
const LIMIT: int = 3

fn add(a: int, b: int) -> int { a + b }

fn compute() -> int {
    let f = add
    let p = print
    let a = assert
    let l = LIMIT
    let _ = (f, p, a)
    l
}
"#,
    );
    let function = named_function(&ir, "compute");
    let kinds = node_kinds(&ir, function.id);
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, martite_ir::NodeKind::ConstRef(_)))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, martite_ir::NodeKind::FnRef(_)))
    );
    assert!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, martite_ir::NodeKind::BuiltinFnRef(_)))
            .count()
            >= 2
    );
}

#[test]
fn struct_literal_children_follow_source_order_and_map_to_declaration_slots() {
    let ir = lower_source(
        r#"
struct Point { x: int, y: int }

fn make() -> Point {
    Point { y: 2, x: 1 }
}
"#,
    );
    let function = named_function(&ir, "make");
    let struct_node = function
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::Struct { .. }))
        .expect("struct node");
    let martite_ir::NodeKind::Struct { layout, .. } = struct_node.kind else {
        unreachable!()
    };
    let layout = function.struct_layout(layout).expect("layout");
    assert_eq!(layout.fields, vec![1, 0]);
    let children = function.child_slice(martite_ir::ChildRange::new(
        layout.first_child,
        layout.fields.len() as u32,
    ));
    assert_eq!(children[0], martite_ir::IrId::new(0));
    assert_eq!(children[1], martite_ir::IrId::new(1));
    let first = function.node(children[0]).expect("first child");
    let second = function.node(children[1]).expect("second child");
    assert_eq!(
        first.kind,
        martite_ir::NodeKind::Int(martite_ir::ConstId::new(0))
    );
    assert_eq!(
        second.kind,
        martite_ir::NodeKind::Int(martite_ir::ConstId::new(1))
    );
    assert_eq!(
        ir.constant(martite_ir::ConstId::new(0)),
        Some(&martite_ir::ConstValue::Int(2))
    );
    assert_eq!(
        ir.constant(martite_ir::ConstId::new(1)),
        Some(&martite_ir::ConstValue::Int(1))
    );
}

#[test]
fn static_strings_are_interned() {
    let ir = lower_source(
        r#"
fn names() -> int {
    let a = "same"
    let b = "same"
    let _ = (a, b)
    1
}
"#,
    );
    let function = named_function(&ir, "names");
    let strings: Vec<_> = function
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            martite_ir::NodeKind::Str(id) => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(strings.len(), 2);
    assert_eq!(strings[0], strings[1]);
}

#[test]
fn host_functions_and_opaque_returns_are_recorded() {
    let mut provider = MemoryProvider::new();
    provider.insert(
        "main.mt",
        r#"
fn compute() -> int {
    1
}
"#,
    );
    let interface = martite_resolve::InterfaceSource {
        path: "app.bench".into(),
        text: "struct Token\nfn mint(seed: int) -> Token\n".into(),
    };
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
            interfaces: vec![interface],
            grants: vec!["app.bench".into()],
            ..LoadOptions::default()
        },
    );
    assert!(!loaded.has_errors(), "{:?}", loaded.diagnostics);
    let program = loaded.program.expect("checked program");
    let ir = lower(&program, &loaded.sources).expect("lower");
    verify(&ir).expect("verify");
    let host = ir
        .fns
        .iter()
        .find(|function| matches!(function.kind, martite_ir::FnKind::Host(_)))
        .expect("host function");
    assert_eq!(host.name.as_ref(), "mint");
    assert_eq!(host.locals, 1);
    assert!(host.body.is_none());
    let info = ir
        .hosts
        .iter()
        .find(|info| info.name.as_ref() == "mint")
        .expect("host info");
    assert_eq!(info.module.as_ref(), "app.bench");
    assert!(info.opaque_return.is_some());
}

#[test]
fn slots_are_not_reused_across_scopes() {
    let ir = lower_source(
        r#"
fn scoped(flag: bool) -> int {
    let a = 1
    if flag {
        let b = 2
        b
    } else {
        let c = 3
        c
    }
}
"#,
    );
    let function = named_function(&ir, "scoped");
    assert_eq!(function.locals, 4);
    let names: Vec<_> = function
        .slot_info
        .iter()
        .map(|slot| slot.name.as_deref().unwrap_or("-"))
        .collect();
    assert_eq!(names, vec!["flag", "a", "b", "c"]);
}

#[test]
fn spans_and_sources_are_present_on_every_node() {
    let ir = lower_source(
        r#"
fn compute(n: int) -> int {
    let x = n + 1
    x
}
"#,
    );
    for function in &ir.fns {
        assert!(ir.source_name(function.source).is_some());
        assert_eq!(function.nodes.len(), function.spans.len());
        assert_eq!(function.nodes.len(), function.synthetic.len());
        assert_eq!(function.nodes.len(), function.safepoints.len());
        for (node, span) in function.nodes.iter().zip(&function.spans) {
            assert!(span.start <= span.end, "invalid span for {node:?}");
        }
        assert!(function.is_safepoint(function.body.expect("body")));
    }
}

#[test]
fn unsupported_constructs_are_reported() {
    let method = lower_source_result(
        r#"
struct P { n: int }

impl P {
    fn get(self) -> int { self.n }
}

fn f(p: P) -> int {
    let m = p.get
    let _ = m
    0
}
"#,
    )
    .expect_err("method value is not lowered yet");
    assert!(matches!(
        method.kind,
        LowerErrorKind::Unsupported {
            construct: "method value"
        }
    ));

    let pattern = lower_source_result(
        r#"
fn f() -> int {
    let (a, b) = (1, 2)
    a + b
}
"#,
    )
    .expect_err("pattern is #105");
    assert!(matches!(
        pattern.kind,
        LowerErrorKind::Unsupported {
            construct: "pattern"
        }
    ));

    let lambda = lower_source_result(
        r#"
fn f() -> int {
    let g = fn(x: int) => x
    g(1)
}
"#,
    )
    .expect_err("lambda is #106");
    assert!(matches!(
        lambda.kind,
        LowerErrorKind::Unsupported {
            construct: "lambda"
        }
    ));
}

#[test]
fn deep_but_legal_trees_lower_and_verify() {
    let mut expression = String::from("1");
    for _ in 0..900 {
        expression.push_str(" + 1");
    }
    let source = format!("fn deep() -> int {{ {expression} }}");
    let ir = lower_source(&source);
    assert_eq!(ir.fns.len(), 1);
}

#[test]
fn excessively_deep_trees_hit_the_depth_limit() {
    let mut expression = String::from("1");
    for _ in 0..(martite_ir::MAX_NODE_DEPTH as usize + 10) {
        expression.push_str(" + 1");
    }
    let source = format!("fn deep() -> int {{ {expression} }}");
    let error = lower_source_result(&source).expect_err("depth limit");
    assert!(matches!(error.kind, LowerErrorKind::DepthLimit { .. }));
}

#[test]
fn lowers_a_function_with_local_slots() {
    let ir = lower_source(
        r#"
fn compute(a: int) -> int {
    let x = a + 1
    let y = x * 2
    y
}
"#,
    );
    let function = ir.function(martite_ir::FnId::new(0)).expect("compute");
    assert_eq!(function.name.as_ref(), "compute");
    assert_eq!(function.locals, 3);
    assert_eq!(function.slot_info.len(), 3);
    assert_eq!(function.slot_info[0].name.as_deref(), Some("a"));
    assert_eq!(function.slot_info[1].name.as_deref(), Some("x"));
    assert_eq!(function.slot_info[2].name.as_deref(), Some("y"));
    assert!(function.body.is_some());
}
