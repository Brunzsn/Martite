use martite_ir::{
    Charge, ChildRange, ConstId, ConstValue, Effects, Entries, EntryKind, FnId, FnKind, HookSig,
    HostFnInfo, IrFn, IrId, MatchArm, MatchSet, Node, NodeKind, ParamInfo, Pattern, PatternId,
    Slot, SlotInfo, SourceId, Span, StrId, StrPart, StructLayout, TyId, TypeId, TypeTable,
};

fn span(start: u32, end: u32) -> Span {
    Span::new(start, end)
}

fn node(kind: NodeKind, ty: TyId, charge: Charge, effects: Effects) -> Node {
    Node::new(kind, ty, charge, effects)
}

#[test]
fn function_side_vectors_and_arenas_are_indexed_by_id() {
    let ty = TyId::new(0);
    let unit = TyId::new(1);
    let nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            ty,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Int(ConstId::new(1)),
            ty,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Add {
                lhs: IrId::new(0),
                rhs: IrId::new(1),
            },
            ty,
            Charge::expression(),
            Effects::MAY_TRAP,
        ),
        node(
            NodeKind::List(ChildRange::new(0, 2)),
            ty,
            Charge::new(1, 2, 0),
            Effects::ALLOCATES,
        ),
        node(
            NodeKind::Return(Some(IrId::new(3))),
            unit,
            Charge::ZERO,
            Effects::NONE,
        ),
    ];
    let spans = vec![span(0, 1), span(2, 3), span(0, 3), span(4, 10), span(4, 10)];
    let mut synthetic = vec![None; nodes.len()];
    synthetic[3] = Some(IrId::new(2));

    let function = IrFn {
        id: FnId::new(0),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: vec![ParamInfo {
            slot: Slot::new(0),
            name: "n".into(),
            ty,
            mutable: false,
            is_self: false,
            pattern: None,
        }],
        locals: 1,
        slot_info: vec![SlotInfo {
            ty,
            mutable: false,
            name: Some("n".into()),
        }],
        captures: Vec::new(),
        patterns: vec![
            Pattern::Bind {
                slot: Slot::new(0),
                ty,
            },
            Pattern::Wild,
        ],
        matches: vec![MatchSet {
            arms: vec![MatchArm {
                pattern: PatternId::new(0),
                guard: Some(IrId::new(0)),
                body: IrId::new(2),
            }],
        }],
        struct_layouts: vec![StructLayout { fields: vec![0, 2] }],
        str_parts: vec![StrPart::Text(StrId::new(0)), StrPart::Expr(IrId::new(2))],
        nodes,
        spans,
        synthetic,
        children: vec![IrId::new(0), IrId::new(1)],
        body: Some(IrId::new(4)),
    };

    assert_eq!(
        function.node(IrId::new(0)).unwrap().kind,
        NodeKind::Int(ConstId::new(0))
    );
    assert_eq!(function.span(IrId::new(1)), Some(span(2, 3)));
    assert_eq!(function.synthetic_origin(IrId::new(3)), Some(IrId::new(2)));
    assert!(!function.is_synthetic(IrId::new(4)));
    assert_eq!(
        function.child_slice(ChildRange::new(0, 2)),
        &[IrId::new(0), IrId::new(1)]
    );
    assert!(function.child_slice(ChildRange::new(1, 5)).is_empty());
    assert_eq!(
        function.str_part_slice(ChildRange::new(0, 2)),
        &[StrPart::Text(StrId::new(0)), StrPart::Expr(IrId::new(2))]
    );
    assert_eq!(function.pattern(PatternId::new(1)), Some(&Pattern::Wild));
    assert_eq!(
        function
            .match_set(martite_ir::MatchSetId::new(0))
            .unwrap()
            .arms
            .len(),
        1
    );
    assert_eq!(
        function
            .struct_layout(martite_ir::StructLayoutId::new(0))
            .unwrap()
            .fields,
        vec![0, 2]
    );
    assert_eq!(
        function.body_node().unwrap().kind,
        NodeKind::Return(Some(IrId::new(3)))
    );
}

#[test]
fn program_owns_pools_hosts_grants_and_entries() {
    let host = HostFnInfo {
        name: "log".into(),
        module: "std.io".into(),
        params: vec![TyId::new(0)],
        ret: TyId::new(1),
        opaque_return: None,
    };
    let hook = HookSig {
        name: "on_start".into(),
        module: "std.plugin".into(),
        params: Vec::new(),
        ret: TyId::new(1),
        source: SourceId(0),
        span: span(0, 1),
    };
    let program = martite_ir::IrProgram {
        fns: Vec::new(),
        consts: vec![ConstValue::Int(7), ConstValue::Bool(true)],
        strs: vec!["hello".into()],
        types: TypeTable,
        hosts: vec![host],
        grants: vec!["std.io".into()],
        entries: Entries {
            main: Some(FnId::new(0)),
            tests: vec![FnId::new(1)],
            handlers: vec![("on_start".into(), FnId::new(2))],
            hooks: vec![("on_start".into(), hook)],
            init: None,
            shutdown: None,
            state_type: Some(TyId::new(0)),
            json_type: Some(TypeId::new(0)),
            entry_kind: EntryKind::Plugin,
        },
    };

    assert_eq!(program.constant(ConstId::new(0)), Some(&ConstValue::Int(7)));
    assert_eq!(program.string(StrId::new(0)), Some("hello"));
    assert_eq!(
        program
            .host(martite_ir::HostId::new(0))
            .unwrap()
            .name
            .as_ref(),
        "log"
    );
    assert!(program.has_grant("std.io"));
    assert!(!program.has_grant("std.env"));
    assert_eq!(program.entries.main, Some(FnId::new(0)));
    assert_eq!(program.entries.entry_kind, EntryKind::Plugin);
    assert_eq!(program.entries.hooks[0].1.params.len(), 0);
}

#[test]
fn optional_dimensions_are_explicit() {
    let function = IrFn {
        id: FnId::new(3),
        name: "<lambda>".into(),
        source: SourceId(2),
        kind: FnKind::Lambda,
        params: Vec::new(),
        locals: 0,
        slot_info: Vec::new(),
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: Vec::new(),
        spans: Vec::new(),
        synthetic: Vec::new(),
        children: Vec::new(),
        body: None,
    };
    assert!(function.body_node().is_none());
    assert_eq!(function.span(IrId::new(0)), None);
    assert_eq!(function.synthetic_origin(IrId::new(0)), None);
}
