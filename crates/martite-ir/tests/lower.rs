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

fn lower_files(files: &[(&str, &str)]) -> IrProgram {
    let mut provider = MemoryProvider::new();
    for (path, text) in files {
        provider.insert(*path, *text);
    }
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
            ..LoadOptions::default()
        },
    );
    assert!(
        !loaded.has_errors(),
        "fixture must check: {:?}",
        loaded.diagnostics
    );
    let program = loaded.program.expect("checked program");
    let ir = lower(&program, &loaded.sources).expect("lower");
    verify(&ir).expect("verify");
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
fn static_strings_are_pooled_per_literal_site() {
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
    assert_ne!(
        strings[0], strings[1],
        "identical literals at different sites must not share the first-use cache"
    );
    assert_eq!(ir.string(strings[0]), Some("same"));
    assert_eq!(ir.string(strings[1]), Some("same"));
}

#[test]
fn zero_payload_variant_value_and_constructor_are_distinct_nodes() {
    let ir = lower_source(
        r#"
enum Shape { Empty, Dot(int) }

fn make() {
    let value = Shape.Empty
    let constructed = Shape.Empty()
    let _ = (value, constructed)
}
"#,
    );
    let function = named_function(&ir, "make");
    let value = function
        .nodes
        .iter()
        .find_map(|node| match node.kind {
            martite_ir::NodeKind::EnumValue { variant } => Some(variant),
            _ => None,
        })
        .expect("value-form variant");
    let constructed = function
        .nodes
        .iter()
        .find_map(|node| match node.kind {
            martite_ir::NodeKind::EnumVariant {
                variant,
                first_payload,
            } => Some((variant, first_payload)),
            _ => None,
        })
        .expect("constructor-form variant");
    assert_eq!(value, 0);
    assert_eq!(constructed.0, 0);
    assert_eq!(constructed.1, 0);
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
    assert!(ir.grants.iter().any(|grant| grant.as_ref() == "app.bench"));
}

#[test]
fn host_opaque_returns_follow_the_runtime_handle_contract() {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", "fn compute() -> int {\n    1\n}\n");
    let interface = martite_resolve::InterfaceSource {
        path: "app.api".into(),
        text: "\
struct Doc
fn direct() -> Doc
fn optional() -> Doc?
fn ok_result() -> Result[Doc, str]
fn err_result() -> Result[str, Doc]
fn listed() -> list[Doc]
fn tuple() -> (Doc, int)
"
        .into(),
    };
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
            interfaces: vec![interface],
            grants: vec!["app.api".into()],
            ..LoadOptions::default()
        },
    );
    assert!(!loaded.has_errors(), "{:?}", loaded.diagnostics);
    let program = loaded.program.expect("checked program");
    let ir = lower(&program, &loaded.sources).expect("lower");
    verify(&ir).expect("verify");
    let has_handle = |name: &str| {
        ir.hosts
            .iter()
            .find(|info| info.name.as_ref() == name)
            .unwrap_or_else(|| panic!("missing host {name}"))
            .opaque_return
            .is_some()
    };
    assert!(has_handle("direct"));
    assert!(has_handle("optional"));
    assert!(has_handle("ok_result"));
    assert!(!has_handle("err_result"));
    assert!(!has_handle("listed"));
    assert!(!has_handle("tuple"));
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
}

#[test]
fn lowers_lambdas_with_direct_captures() {
    let ir = lower_source(
        r#"
fn main() {
    let value = 1
    let read = fn() => value
    let _ = read()
}
"#,
    );
    let lambda = ir
        .fns
        .iter()
        .find(|function| matches!(function.kind, martite_ir::FnKind::Lambda))
        .expect("lambda function");
    assert_eq!(lambda.name.as_ref(), "<lambda>");
    assert_eq!(ir.source_name(lambda.source), Some("main.mt"));
    assert_eq!(lambda.captures.len(), 1);
    assert_eq!(
        lambda.captures[0].source,
        martite_ir::CaptureSource::Slot(martite_ir::Slot::new(0))
    );
    assert!(lambda.params.is_empty());
    assert!(
        lambda
            .nodes
            .iter()
            .any(|node| matches!(node.kind, martite_ir::NodeKind::CaptureRead(_))),
        "capture read must not lower to a slot read"
    );
    let main = named_function(&ir, "main");
    let (_, closure) = find_node(main, |kind| {
        matches!(kind, martite_ir::NodeKind::MakeClosure(_))
    });
    assert_eq!(closure.charge, martite_ir::Charge::expression());
}

#[test]
fn lowers_nested_lambdas_with_transitive_captures() {
    let ir = lower_source(
        r#"
fn main() {
    let base = 10
    let add = fn(a: int) => fn(b: int) => base + a + b
    let _ = add(5)
}
"#,
    );
    let lambdas: Vec<_> = ir
        .fns
        .iter()
        .filter(|function| matches!(function.kind, martite_ir::FnKind::Lambda))
        .collect();
    assert_eq!(lambdas.len(), 2);
    let outer = lambdas
        .iter()
        .find(|function| function.captures.len() == 1)
        .expect("outer lambda");
    let inner = lambdas
        .iter()
        .find(|function| function.captures.len() == 2)
        .expect("inner lambda");
    assert_eq!(
        outer.captures[0].source,
        martite_ir::CaptureSource::Slot(martite_ir::Slot::new(0))
    );
    assert_eq!(outer.locals, 1, "outer lambda has only parameter `a`");
    let sources: Vec<_> = inner
        .captures
        .iter()
        .map(|capture| (capture.name.as_deref(), capture.source))
        .collect();
    assert!(sources.contains(&(
        Some("base"),
        martite_ir::CaptureSource::Capture(martite_ir::CaptureIndex::new(0))
    )));
    assert!(sources.contains(&(
        Some("a"),
        martite_ir::CaptureSource::Slot(martite_ir::Slot::new(0))
    )));
    assert_eq!(
        inner
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, martite_ir::NodeKind::CaptureRead(_)))
            .count(),
        2
    );
}

#[test]
fn lambda_source_is_the_defining_file_not_the_caller() {
    let ir = lower_files(&[
        (
            "main.mt",
            r#"
use util.make
fn main() {
    let read = make()
    let _ = read()
}
"#,
        ),
        (
            "util.mt",
            r#"
pub fn make() -> fn() -> int {
    let value = 7
    fn() => value
}
"#,
        ),
    ]);
    let lambda = ir
        .fns
        .iter()
        .find(|function| matches!(function.kind, martite_ir::FnKind::Lambda))
        .expect("lambda function");
    assert_eq!(ir.source_name(lambda.source), Some("util.mt"));
}

#[test]
fn lowers_expression_and_block_lambda_bodies() {
    let ir = lower_source(
        r#"
fn main() {
    let expression = fn(value: int) => value + 1
    let block = fn(value: int) {
        let doubled = value * 2
        doubled
    }
    let _ = (expression(1), block(2))
}
"#,
    );
    let lambdas: Vec<_> = ir
        .fns
        .iter()
        .filter(|function| matches!(function.kind, martite_ir::FnKind::Lambda))
        .collect();
    assert_eq!(lambdas.len(), 2);
    for lambda in lambdas {
        assert!(lambda.body.is_some());
        assert!(lambda.is_safepoint(lambda.body.expect("body")));
    }
}

#[test]
fn lowers_lambdas_inside_methods_handlers_and_tests() {
    let ir = lower_source(
        r#"
struct Counter { value: int }

impl Counter {
    fn read(self) -> int {
        let read = fn() => self.value
        read()
    }
}

test "lambda in test" {
    let value = 1
    let read = fn() => value
    assert(read() == 1, "value")
}
"#,
    );
    let lambdas: Vec<_> = ir
        .fns
        .iter()
        .filter(|function| matches!(function.kind, martite_ir::FnKind::Lambda))
        .collect();
    assert_eq!(lambdas.len(), 2);
    assert_eq!(ir.entries.tests.len(), 1);
}

#[test]
fn lowers_destructured_lambda_parameters() {
    let ir = lower_source(
        r#"
fn main() {
    let pair = (1, 2)
    let sum = fn((a, b): (int, int)) => a + b
    let _ = sum(pair)
}
"#,
    );
    let lambda = ir
        .fns
        .iter()
        .find(|function| matches!(function.kind, martite_ir::FnKind::Lambda))
        .expect("lambda function");
    assert_eq!(lambda.params.len(), 1);
    assert!(lambda.params[0].pattern.is_some());
    assert_eq!(lambda.locals, 3, "one parameter slot plus two bindings");
    assert!(
        lambda
            .patterns
            .iter()
            .any(|pattern| matches!(pattern, martite_ir::Pattern::Tuple(_)))
    );
}

#[test]
fn lowers_script_test_and_plugin_entries() {
    let script = lower_source(
        r#"
fn main() {}
test "first" { assert(true, "ok") }
test "second" { assert(true, "ok") }
"#,
    );
    assert_eq!(script.entries.entry_kind, martite_ir::EntryKind::Script);
    assert!(script.entries.main.is_some());
    let test_names: Vec<_> = script
        .entries
        .tests
        .iter()
        .map(|id| script.function(*id).expect("test function").name.as_ref())
        .collect();
    assert_eq!(test_names, vec!["first", "second"]);

    let library = lower_source("fn helper() -> int { 1 }");
    assert_eq!(library.entries.entry_kind, martite_ir::EntryKind::Library);
    assert!(library.entries.main.is_none());

    let plugin = lower_source_with(
        r#"
use app.hooks
on init() -> int { 1 }
on saved() {}
on shutdown() {}
"#,
        vec![InterfaceSource {
            path: "app.hooks".into(),
            text: "on saved()\n".into(),
        }],
        vec!["app.hooks".into()],
    );
    assert_eq!(plugin.entries.entry_kind, martite_ir::EntryKind::Plugin);
    assert!(plugin.entries.main.is_none());
    assert!(plugin.entries.init.is_some());
    assert!(plugin.entries.shutdown.is_some());
    assert!(plugin.entries.state_type.is_some());
    assert_eq!(plugin.entries.hooks.len(), 1);
    assert_eq!(plugin.entries.hooks[0].0.as_ref(), "saved");
    assert_eq!(plugin.entries.hooks[0].1.module.as_ref(), "app.hooks");
    let handler_names: Vec<_> = plugin
        .entries
        .handlers
        .iter()
        .map(|(name, _)| name.as_ref())
        .collect();
    assert_eq!(handler_names, vec!["init", "saved"]);
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

#[test]
fn lowers_variant_or_patterns_and_guards() {
    let ir = lower_source(
        r#"
enum Value { A(int, int), B(int) }

fn pick(value: Value) -> int {
    match value {
        A(x, 1) | A(x, 2) if x > 0 => x,
        A(x, _) => x + 10,
        B(x) => x,
    }
}
"#,
    );
    let function = named_function(&ir, "pick");
    let match_node = function
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::Match { .. }))
        .expect("match node");
    let martite_ir::NodeKind::Match { match_set, .. } = match_node.kind else {
        unreachable!()
    };
    assert_eq!(match_node.charge, martite_ir::Charge::expression());
    let set = function.match_set(match_set).expect("match set");
    assert_eq!(set.arms.len(), 3);
    assert!(set.arms[0].guard.is_some());

    let martite_ir::Pattern::Or(alternatives) =
        function.pattern(set.arms[0].pattern).expect("or pattern")
    else {
        panic!("first arm must keep its or-pattern");
    };
    assert_eq!(alternatives.len(), 2);
    let mut bound_slot = None;
    for (index, alternative) in alternatives.iter().enumerate() {
        let martite_ir::Pattern::Variant {
            type_id,
            variant,
            args,
        } = alternative
        else {
            panic!("alternative must be a variant pattern");
        };
        assert_eq!(*variant, 0);
        assert_eq!(args.len(), 2);
        let martite_ir::Pattern::Bind { slot, .. } = &args[0] else {
            panic!("payload must bind");
        };
        if let Some(previous) = bound_slot {
            assert_eq!(previous, *slot);
        }
        bound_slot = Some(*slot);
        assert_eq!(args[1], martite_ir::Pattern::Int(index as i64 + 1));
        assert!(ir.types.get(*type_id).is_some());
    }

    let guard = set.arms[0].guard.expect("guard");
    assert!(matches!(
        function.node(guard).expect("guard node").kind,
        martite_ir::NodeKind::Gt { .. }
    ));
}

#[test]
fn lowers_nested_struct_patterns_in_declaration_order() {
    let ir = lower_source(
        r#"
struct Point { x: int, y: int, z: int }
struct Segment { start: Point, end: Point }

fn read(segment: Segment) -> int {
    match segment {
        Segment { end: Point { y: end_y, .. }, .. } => end_y,
    }
}
"#,
    );
    let function = named_function(&ir, "read");
    let match_node = function
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::Match { .. }))
        .expect("match node");
    let martite_ir::NodeKind::Match { match_set, .. } = match_node.kind else {
        unreachable!()
    };
    let set = function.match_set(match_set).expect("match set");
    let martite_ir::Pattern::Struct { fields, .. } = function
        .pattern(set.arms[0].pattern)
        .expect("segment pattern")
    else {
        panic!("top-level pattern must be a struct");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].0, 1, "end is the second declaration field");
    let martite_ir::Pattern::Struct {
        fields: point_fields,
        ..
    } = &fields[0].1
    else {
        panic!("nested point pattern");
    };
    assert_eq!(point_fields.len(), 1);
    assert_eq!(point_fields[0].0, 1, "y is the second declaration field");
    assert!(matches!(
        point_fields[0].1,
        martite_ir::Pattern::Bind { .. }
    ));
}

#[test]
fn lowers_prelude_and_literal_patterns() {
    let ir = lower_source(
        r#"
fn option_pick(value: int?) -> int {
    match value {
        Some(0) => 1,
        Some(x) => x,
        None => 0,
    }
}

fn bool_pick(value: bool) -> int {
    match value {
        true => 1,
        false => 0,
    }
}

fn str_pick(value: str) -> int {
    match value {
        "a" => 1,
        _ => 0,
    }
}
"#,
    );

    let option = named_function(&ir, "option_pick");
    let match_node = option
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::Match { .. }))
        .expect("match node");
    let martite_ir::NodeKind::Match { match_set, .. } = match_node.kind else {
        unreachable!()
    };
    let set = option.match_set(match_set).expect("match set");
    assert!(matches!(
        option.pattern(set.arms[0].pattern),
        Some(martite_ir::Pattern::Prelude {
            variant: martite_ir::PreludeVariant::Some,
            args,
        }) if args.len() == 1
    ));
    assert!(matches!(
        option.pattern(set.arms[2].pattern),
        Some(martite_ir::Pattern::Prelude {
            variant: martite_ir::PreludeVariant::None,
            args,
        }) if args.is_empty()
    ));

    let boolean = named_function(&ir, "bool_pick");
    assert!(
        boolean
            .patterns
            .iter()
            .any(|pattern| matches!(pattern, martite_ir::Pattern::Bool(true)))
    );

    let string = named_function(&ir, "str_pick");
    let string_pattern = string
        .patterns
        .iter()
        .find_map(|pattern| match pattern {
            martite_ir::Pattern::Str(id) => Some(*id),
            _ => None,
        })
        .expect("string pattern");
    assert_eq!(ir.string(string_pattern), Some("a"));
}

#[test]
fn lowers_irrefutable_destructuring_and_first_or_alternative() {
    let ir = lower_source(
        r#"
struct Point { x: int, y: int, z: int }
enum Only { A(int) }

fn destructure(pair: (int, int), point: Point, only: Only) -> int {
    let (a, b) = pair
    let Point { y, .. } = point
    let A(c) = only
    let (d | d) = 1
    a + b + y + c + d
}
"#,
    );
    let function = named_function(&ir, "destructure");
    assert!(
        !function
            .patterns
            .iter()
            .any(|pattern| matches!(pattern, martite_ir::Pattern::Or(_))),
        "binding positions select the first or-pattern alternative"
    );
    assert!(function.patterns.iter().any(|pattern| matches!(
        pattern,
        martite_ir::Pattern::Struct { fields, .. } if fields.len() == 1
    )));
    assert!(function.patterns.iter().any(|pattern| matches!(
        pattern,
        martite_ir::Pattern::Variant { variant: 0, args, .. } if args.len() == 1
    )));
}

#[test]
fn match_block_bodies_lower_as_bodies() {
    let ir = lower_source(
        r#"
fn block_body(value: int) -> int {
    match value {
        x => {
            let y = x + 1
            y
        }
    }
}
"#,
    );
    let function = named_function(&ir, "block_body");
    let match_node = function
        .nodes
        .iter()
        .find(|node| matches!(node.kind, martite_ir::NodeKind::Match { .. }))
        .expect("match node");
    let martite_ir::NodeKind::Match { match_set, .. } = match_node.kind else {
        unreachable!()
    };
    let set = function.match_set(match_set).expect("match set");
    let body = function.node(set.arms[0].body).expect("arm body");
    assert!(matches!(body.kind, martite_ir::NodeKind::Block(_)));
    assert_eq!(body.charge, martite_ir::Charge::ZERO);
    assert_eq!(body.ty, function.ret);
}

#[test]
fn verifier_rejects_refutable_for_pattern() {
    let mut ir = lower_source(
        r#"
fn loops() -> int {
    for x in [1, 2] {
        let _ = x
    }
    0
}
"#,
    );
    let function = &mut ir.fns[0];
    let for_node = function
        .nodes
        .iter()
        .position(|node| matches!(node.kind, martite_ir::NodeKind::For { .. }))
        .expect("for node");
    let pattern = martite_ir::PatternId::new(function.patterns.len() as u32);
    function.patterns.push(martite_ir::Pattern::Int(1));
    let martite_ir::NodeKind::For {
        pattern: slot_pattern,
        ..
    } = &mut function.nodes[for_node].kind
    else {
        unreachable!()
    };
    *slot_pattern = pattern;

    let error = verify(&ir).expect_err("refutable for pattern");
    assert!(matches!(
        error.kind,
        martite_ir::VerifyErrorKind::PatternShape {
            context: "`for` pattern is refutable",
            ..
        }
    ));
}
