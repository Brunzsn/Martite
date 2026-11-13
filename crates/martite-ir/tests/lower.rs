use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load};
use martite_ir::{IrProgram, LowerError, LowerErrorKind, lower, verify};

fn lower_source_result(source: &str) -> Result<IrProgram, LowerError> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
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

fn lower_source(source: &str) -> IrProgram {
    let ir = lower_source_result(source).unwrap_or_else(|error| panic!("{error}"));
    verify(&ir).unwrap_or_else(|error| panic!("{error}"));
    ir
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
    let call = lower_source_result(
        r#"
fn f() -> int {
    f()
}
"#,
    )
    .expect_err("call is #104");
    assert!(matches!(
        call.kind,
        LowerErrorKind::Unsupported { construct: "call" }
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
