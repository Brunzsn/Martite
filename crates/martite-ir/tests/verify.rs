#![allow(clippy::let_and_return)]

use martite_ir::{
    AccessMode, BuiltinFnId, CaptureInfo, Charge, ChildRange, ConstId, ConstValue, Effects,
    Entries, EntryKind, FieldDef, FnId, FnKind, HostFnInfo, IrFn, IrId, IrProgram, MatchArm,
    MatchSet, MatchSetId, Node, NodeKind, Pattern, PatternId, Slot, SlotInfo, SourceId, Span,
    StructLayout, StructLayoutId, Ty, TyId, TypeDef, TypeDefKind, TypeTable, VerifyErrorKind,
};

fn types() -> (TypeTable, TyId, TyId, TyId, TyId) {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let bool_ty = types.intern_ty(Ty::Bool);
    let str_ty = types.intern_ty(Ty::Str);
    (types, unit, int, bool_ty, str_ty)
}

fn node(kind: NodeKind, ty: TyId, charge: Charge, effects: Effects) -> Node {
    Node::new(kind, ty, charge, effects)
}

fn valid_program() -> IrProgram {
    let (types, unit, int, _bool_ty, _str_ty) = types();
    let nodes = vec![
        // 0: constant 1
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        // 1: let value: int = 1
        node(
            NodeKind::Let {
                pattern: PatternId::new(0),
                value: IrId::new(0),
            },
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL | Effects::SAFEPOINT,
        ),
        // 2: value
        node(
            NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            },
            int,
            Charge::expression(),
            Effects::READS_LOCAL,
        ),
        // 3: constant 1
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        // 4: value + 1
        node(
            NodeKind::Add {
                lhs: IrId::new(2),
                rhs: IrId::new(3),
            },
            int,
            Charge::expression(),
            Effects::MAY_TRAP | Effects::READS_LOCAL,
        ),
        // 5: place value
        node(
            NodeKind::SlotPlace(Slot::new(0)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        // 6: value = value + 1
        node(
            NodeKind::Assign {
                place: IrId::new(5),
                value: IrId::new(4),
            },
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL
                | Effects::WRITES_HEAP
                | Effects::ALLOCATES
                | Effects::DYN_COST
                | Effects::MAY_TRAP
                | Effects::READS_LOCAL
                | Effects::SAFEPOINT,
        ),
        // 7: value (for print)
        node(
            NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            },
            int,
            Charge::expression(),
            Effects::READS_LOCAL,
        ),
        // 8: print(value)
        node(
            NodeKind::CallBuiltinFn {
                builtin: BuiltinFnId::PRINT,
                first_arg: 0,
            },
            unit,
            Charge::expression(),
            Effects::LOGS | Effects::DYN_COST | Effects::ALLOCATES | Effects::READS_LOCAL,
        ),
        // 9: expression statement
        node(
            NodeKind::ExprStmt(IrId::new(8)),
            unit,
            Charge::ZERO,
            Effects::LOGS
                | Effects::DYN_COST
                | Effects::ALLOCATES
                | Effects::READS_LOCAL
                | Effects::SAFEPOINT,
        ),
        // 10: return
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        // 11: body block
        node(
            NodeKind::Block(martite_ir::ChildRange::new(1, 4)),
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL
                | Effects::READS_LOCAL
                | Effects::WRITES_HEAP
                | Effects::ALLOCATES
                | Effects::DYN_COST
                | Effects::MAY_TRAP
                | Effects::LOGS
                | Effects::NONLOCAL_EXIT
                | Effects::SAFEPOINT,
        ),
    ];

    let mut safepoints = vec![false; nodes.len()];
    for id in [1, 6, 9, 10, 11] {
        safepoints[id] = true;
    }

    let function = IrFn {
        id: FnId::new(0),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: int,
            mutable: true,
            name: Some("value".into()),
        }],
        captures: Vec::new(),
        patterns: vec![Pattern::Bind {
            slot: Slot::new(0),
            ty: int,
        }],
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 12],
        synthetic: vec![None; 12],
        safepoints,
        children: vec![
            IrId::new(7),  // print argument
            IrId::new(1),  // let
            IrId::new(6),  // assign
            IrId::new(9),  // print statement
            IrId::new(10), // return
        ],
        body: Some(IrId::new(11)),
    };

    IrProgram {
        fns: vec![function],
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["test.mt".into()],
    }
}

#[test]
fn valid_program_verifies() {
    valid_program().verify().expect("valid program");
}

fn error_kind(program: IrProgram) -> VerifyErrorKind {
    program.verify().expect_err("program must not verify").kind
}

fn type_id(program: &IrProgram, ty: Ty) -> TyId {
    program
        .types
        .tys
        .iter()
        .position(|candidate| *candidate == ty)
        .map(|index| TyId::new(index as u32))
        .expect("type is interned")
}

#[test]
fn rejects_out_of_range_function_id() {
    let mut program = valid_program();
    program.fns[0].id = FnId::new(1);
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::FunctionIdMismatch { .. }
    ));
}

#[test]
fn rejects_out_of_range_source() {
    let mut program = valid_program();
    program.fns[0].source = SourceId(1);
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::SourceOutOfRange { .. }
    ));
}

#[test]
fn rejects_missing_side_vectors() {
    let mut program = valid_program();
    program.fns[0].spans.pop();
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::SideVectorLength { .. }
    ));
}

#[test]
fn rejects_out_of_range_body() {
    let mut program = valid_program();
    program.fns[0].body = Some(IrId::new(99));
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::OutOfRange { .. }
    ));
}

#[test]
fn rejects_shared_subtree() {
    let mut program = valid_program();
    program.fns[0].nodes[1].kind = NodeKind::Let {
        pattern: PatternId::new(0),
        value: IrId::new(7),
    };
    assert!(matches!(error_kind(program), VerifyErrorKind::Tree { .. }));
}

#[test]
fn rejects_cycle() {
    let mut program = valid_program();
    program.fns[0].nodes[4].kind = NodeKind::Add {
        lhs: IrId::new(4),
        rhs: IrId::new(3),
    };
    assert!(matches!(error_kind(program), VerifyErrorKind::Tree { .. }));
}

#[test]
fn rejects_unreachable_node() {
    let mut program = valid_program();
    program.fns[0].nodes[11].kind = NodeKind::Block(ChildRange::new(1, 3));
    assert!(matches!(error_kind(program), VerifyErrorKind::Tree { .. }));
}

#[test]
fn rejects_wrong_child_role() {
    let mut program = valid_program();
    program.fns[0].children[1] = IrId::new(0);
    program.fns[0].nodes[0].charge = Charge::ZERO;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::WrongRole { .. }
    ));
}

#[test]
fn rejects_wrong_charge() {
    let mut program = valid_program();
    program.fns[0].nodes[2].charge = Charge::ZERO;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Charge { .. }
    ));
}

#[test]
fn rejects_missing_safepoint() {
    let mut program = valid_program();
    program.fns[0].safepoints[1] = false;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Safepoint { .. }
    ));
}

#[test]
fn rejects_unexpected_safepoint() {
    let mut program = valid_program();
    program.fns[0].safepoints[2] = true;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Safepoint { .. }
    ));
}

#[test]
fn rejects_missing_effect() {
    let mut program = valid_program();
    program.fns[0].nodes[2].effects = Effects::NONE;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Effects { .. }
    ));
}

#[test]
fn rejects_missing_descendant_safepoint_effect() {
    let mut program = valid_program();
    let effects = program.fns[0].nodes[11].effects;
    program.fns[0].nodes[11].effects =
        Effects::from_bits_truncate(effects.bits() & !Effects::SAFEPOINT.bits());
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Effects { .. }
    ));
}

#[test]
fn rejects_wrong_literal_type() {
    let mut program = valid_program();
    let bool_ty = type_id(&program, Ty::Bool);
    program.fns[0].nodes[0].ty = bool_ty;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::TypeMismatch { .. }
    ));
}

#[test]
fn rejects_wrong_function_return_type() {
    let mut program = valid_program();
    let int = type_id(&program, Ty::Int);
    program.fns[0].ret = int;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::TypeMismatch { .. } | VerifyErrorKind::NodeType { .. }
    ));
}

#[test]
fn rejects_break_outside_loop() {
    let mut program = valid_program();
    program.fns[0].nodes[10].kind = NodeKind::Break;
    program.fns[0].safepoints[10] = false;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::ControlFlow { .. }
    ));
}

#[test]
fn rejects_continue_outside_loop() {
    let mut program = valid_program();
    program.fns[0].nodes[10].kind = NodeKind::Continue;
    program.fns[0].safepoints[10] = false;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::ControlFlow { .. }
    ));
}

fn valid_call_and_mut_program() -> IrProgram {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);

    let inc_nodes = vec![
        node(
            NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            },
            int,
            Charge::expression(),
            Effects::READS_LOCAL,
        ),
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Add {
                lhs: IrId::new(0),
                rhs: IrId::new(1),
            },
            int,
            Charge::expression(),
            Effects::MAY_TRAP | Effects::READS_LOCAL,
        ),
        node(
            NodeKind::SlotWrite {
                slot: Slot::new(0),
                value: IrId::new(2),
            },
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL | Effects::MAY_TRAP | Effects::READS_LOCAL,
        ),
        node(
            NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            },
            int,
            Charge::expression(),
            Effects::READS_LOCAL,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(4)),
            unit,
            Charge::ZERO,
            Effects::READS_LOCAL | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(0, 2)),
            int,
            Charge::ZERO,
            Effects::WRITES_LOCAL | Effects::MAY_TRAP | Effects::READS_LOCAL | Effects::SAFEPOINT,
        ),
    ];
    let mut inc_safepoints = vec![false; inc_nodes.len()];
    inc_safepoints[5] = true;
    inc_safepoints[6] = true;
    let inc = IrFn {
        id: FnId::new(0),
        name: "inc".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: vec![martite_ir::ParamInfo {
            slot: Slot::new(0),
            name: "value".into(),
            ty: int,
            mutable: true,
            is_self: false,
            pattern: None,
        }],
        ret: int,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: int,
            mutable: true,
            name: Some("value".into()),
        }],
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: inc_nodes,
        spans: vec![Span::empty(0); 7],
        synthetic: vec![None; 7],
        safepoints: inc_safepoints,
        children: vec![IrId::new(3), IrId::new(5)],
        body: Some(IrId::new(6)),
    };

    let main_nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Let {
                pattern: PatternId::new(0),
                value: IrId::new(0),
            },
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::SlotPlace(Slot::new(0)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::MutArg(IrId::new(2)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::CallDirect {
                target: FnId::new(0),
                first_arg: 0,
            },
            int,
            Charge::expression(),
            Effects::CALLS | Effects::MAY_TRAP | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(4)),
            unit,
            Charge::ZERO,
            Effects::CALLS | Effects::MAY_TRAP | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(1, 3)),
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL
                | Effects::CALLS
                | Effects::MAY_TRAP
                | Effects::SAFEPOINT
                | Effects::NONLOCAL_EXIT,
        ),
    ];
    let mut main_safepoints = vec![false; main_nodes.len()];
    for id in [1, 5, 6, 7] {
        main_safepoints[id] = true;
    }
    let main = IrFn {
        id: FnId::new(1),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: int,
            mutable: true,
            name: Some("value".into()),
        }],
        captures: Vec::new(),
        patterns: vec![Pattern::Bind {
            slot: Slot::new(0),
            ty: int,
        }],
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: main_nodes,
        spans: vec![Span::empty(0); 8],
        synthetic: vec![None; 8],
        safepoints: main_safepoints,
        children: vec![IrId::new(3), IrId::new(1), IrId::new(5), IrId::new(6)],
        body: Some(IrId::new(7)),
    };

    let program = IrProgram {
        fns: vec![inc, main],
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(1)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["call.mt".into()],
    };
    program
}

#[test]
fn valid_call_and_mut_argument_verifies() {
    valid_call_and_mut_program().verify().expect("call program");
}

fn valid_builtin_method_program() -> IrProgram {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let list = types.intern_ty(Ty::List(int));
    let nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Int(ConstId::new(1)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::List(ChildRange::new(0, 2)),
            list,
            Charge::new(1, 2, 0),
            Effects::ALLOCATES,
        ),
        node(
            NodeKind::Let {
                pattern: PatternId::new(0),
                value: IrId::new(2),
            },
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL | Effects::ALLOCATES | Effects::SHARES | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::SlotPlace(Slot::new(0)),
            list,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::MutArg(IrId::new(4)),
            list,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::Int(ConstId::new(1)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::CallBuiltinMethod {
                method: martite_ir::BuiltinMethodId::lookup(
                    martite_ir::BuiltinReceiver::List,
                    "push",
                )
                .expect("push"),
                first_child: 2,
            },
            unit,
            Charge::expression(),
            Effects::DYN_COST | Effects::READS_HEAP | Effects::WRITES_HEAP | Effects::ALLOCATES,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(7)),
            unit,
            Charge::ZERO,
            Effects::DYN_COST
                | Effects::READS_HEAP
                | Effects::WRITES_HEAP
                | Effects::ALLOCATES
                | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(4, 3)),
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL
                | Effects::ALLOCATES
                | Effects::SHARES
                | Effects::DYN_COST
                | Effects::READS_HEAP
                | Effects::WRITES_HEAP
                | Effects::SAFEPOINT
                | Effects::NONLOCAL_EXIT,
        ),
    ];
    let mut safepoints = vec![false; nodes.len()];
    for id in [3, 8, 9, 10] {
        safepoints[id] = true;
    }
    let function = IrFn {
        id: FnId::new(0),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: list,
            mutable: true,
            name: Some("items".into()),
        }],
        captures: Vec::new(),
        patterns: vec![Pattern::Bind {
            slot: Slot::new(0),
            ty: list,
        }],
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 11],
        synthetic: vec![None; 11],
        safepoints,
        children: vec![
            IrId::new(0),
            IrId::new(1),
            IrId::new(5),
            IrId::new(6),
            IrId::new(3),
            IrId::new(8),
            IrId::new(9),
        ],
        body: Some(IrId::new(10)),
    };
    let program = IrProgram {
        fns: vec![function],
        consts: vec![ConstValue::Int(1), ConstValue::Int(2)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["method.mt".into()],
    };
    program
}

#[test]
fn valid_builtin_method_verifies() {
    valid_builtin_method_program()
        .verify()
        .expect("builtin method program");
}

fn valid_closure_program() -> IrProgram {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let fn_ty = types.intern_ty(Ty::Fn(Vec::new(), int));

    let lambda = IrFn {
        id: FnId::new(1),
        name: "<lambda>".into(),
        source: SourceId(0),
        kind: FnKind::Lambda,
        params: Vec::new(),
        ret: int,
        locals: 0,
        slot_info: Vec::new(),
        captures: vec![CaptureInfo {
            source: martite_ir::CaptureSource::Slot(Slot::new(0)),
            ty: int,
            name: Some("value".into()),
        }],
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: vec![node(
            NodeKind::CaptureRead(martite_ir::CaptureIndex::new(0)),
            int,
            Charge::expression(),
            Effects::READS_LOCAL | Effects::SAFEPOINT,
        )],
        spans: vec![Span::empty(0)],
        synthetic: vec![None],
        safepoints: vec![true],
        children: Vec::new(),
        body: Some(IrId::new(0)),
    };

    let nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Let {
                pattern: PatternId::new(0),
                value: IrId::new(0),
            },
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::MakeClosure(FnId::new(1)),
            fn_ty,
            Charge::expression(),
            Effects::ALLOCATES | Effects::READS_LOCAL,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(2)),
            unit,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::READS_LOCAL | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(0, 3)),
            unit,
            Charge::ZERO,
            Effects::WRITES_LOCAL
                | Effects::ALLOCATES
                | Effects::READS_LOCAL
                | Effects::SAFEPOINT
                | Effects::NONLOCAL_EXIT,
        ),
    ];
    let main = IrFn {
        id: FnId::new(0),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: int,
            mutable: true,
            name: Some("value".into()),
        }],
        captures: Vec::new(),
        patterns: vec![Pattern::Bind {
            slot: Slot::new(0),
            ty: int,
        }],
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 6],
        synthetic: vec![None; 6],
        safepoints: vec![false, true, false, true, true, true],
        children: vec![IrId::new(1), IrId::new(3), IrId::new(4)],
        body: Some(IrId::new(5)),
    };
    let program = IrProgram {
        fns: vec![main, lambda],
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["closure.mt".into()],
    };
    program
}

#[test]
fn valid_closure_verifies() {
    valid_closure_program().verify().expect("closure program");
}

fn valid_match_program() -> IrProgram {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let nodes = vec![
        node(
            NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            },
            int,
            Charge::expression(),
            Effects::READS_LOCAL,
        ),
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Int(ConstId::new(1)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Match {
                match_set: MatchSetId::new(0),
                scrutinee: IrId::new(0),
            },
            int,
            Charge::expression(),
            Effects::MAY_TRAP | Effects::READS_HEAP | Effects::WRITES_LOCAL | Effects::READS_LOCAL,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(3)),
            unit,
            Charge::ZERO,
            Effects::MAY_TRAP
                | Effects::READS_HEAP
                | Effects::WRITES_LOCAL
                | Effects::READS_LOCAL
                | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(0, 1)),
            int,
            Charge::ZERO,
            Effects::MAY_TRAP
                | Effects::READS_HEAP
                | Effects::WRITES_LOCAL
                | Effects::READS_LOCAL
                | Effects::SAFEPOINT,
        ),
    ];
    let mut safepoints = vec![false; nodes.len()];
    safepoints[4] = true;
    safepoints[5] = true;
    let function = IrFn {
        id: FnId::new(0),
        name: "pick".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: vec![martite_ir::ParamInfo {
            slot: Slot::new(0),
            name: "value".into(),
            ty: int,
            mutable: false,
            is_self: false,
            pattern: None,
        }],
        ret: int,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: int,
            mutable: false,
            name: Some("value".into()),
        }],
        captures: Vec::new(),
        patterns: vec![Pattern::Int(1), Pattern::Wild],
        matches: vec![MatchSet {
            arms: vec![
                MatchArm {
                    pattern: PatternId::new(0),
                    guard: None,
                    body: IrId::new(1),
                },
                MatchArm {
                    pattern: PatternId::new(1),
                    guard: None,
                    body: IrId::new(2),
                },
            ],
        }],
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 6],
        synthetic: vec![None; 6],
        safepoints,
        children: vec![IrId::new(4)],
        body: Some(IrId::new(5)),
    };
    let program = IrProgram {
        fns: vec![function],
        consts: vec![ConstValue::Int(1), ConstValue::Int(2)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["match.mt".into()],
    };
    program
}

#[test]
fn valid_match_verifies() {
    valid_match_program().verify().expect("match program");
}

fn host_program(grants: Vec<Box<str>>) -> IrProgram {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::CallHost {
                host: martite_ir::HostId::new(0),
                first_arg: 0,
            },
            unit,
            Charge::expression(),
            Effects::HOST | Effects::DYN_COST,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(1)),
            unit,
            Charge::ZERO,
            Effects::HOST | Effects::DYN_COST | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(1, 2)),
            unit,
            Charge::ZERO,
            Effects::HOST | Effects::DYN_COST | Effects::SAFEPOINT | Effects::NONLOCAL_EXIT,
        ),
    ];
    let mut safepoints = vec![false; nodes.len()];
    for id in [2, 3, 4] {
        safepoints[id] = true;
    }
    let function = IrFn {
        id: FnId::new(0),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 0,
        slot_info: Vec::new(),
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 5],
        synthetic: vec![None; 5],
        safepoints,
        children: vec![IrId::new(0), IrId::new(2), IrId::new(3)],
        body: Some(IrId::new(4)),
    };
    IrProgram {
        fns: vec![function],
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types,
        hosts: vec![HostFnInfo {
            name: "log".into(),
            module: "std.io".into(),
            params: vec![int],
            ret: unit,
            opaque_return: None,
        }],
        grants,
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["host.mt".into()],
    }
}

#[test]
fn valid_host_call_verifies() {
    host_program(vec!["std.io".into()])
        .verify()
        .expect("host program");
}

#[test]
fn rejects_host_call_without_grant() {
    let program = host_program(Vec::new());
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::HostGrant { .. }
    ));
}

#[test]
fn valid_struct_literal_verifies() {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let type_id = types.add_def(TypeDef {
        name: "Point".into(),
        kind: TypeDefKind::Struct {
            fields: vec![
                FieldDef {
                    name: "x".into(),
                    ty: int,
                },
                FieldDef {
                    name: "y".into(),
                    ty: int,
                },
            ],
        },
        params: 0,
        is_interface: false,
        is_builtin: false,
    });
    let point = types.intern_ty(Ty::Named(type_id, Vec::new()));
    let nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Int(ConstId::new(1)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::Struct {
                type_id,
                layout: StructLayoutId::new(0),
            },
            point,
            Charge::new(1, 2, 0),
            Effects::ALLOCATES,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(2)),
            unit,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(2, 1)),
            point,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::SAFEPOINT,
        ),
    ];
    let mut safepoints = vec![false; nodes.len()];
    safepoints[3] = true;
    safepoints[4] = true;
    let function = IrFn {
        id: FnId::new(0),
        name: "make".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: point,
        locals: 0,
        slot_info: Vec::new(),
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: vec![StructLayout {
            first_child: 0,
            fields: vec![0, 1],
        }],
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 5],
        synthetic: vec![None; 5],
        safepoints,
        children: vec![IrId::new(0), IrId::new(1), IrId::new(3)],
        body: Some(IrId::new(4)),
    };
    let program = IrProgram {
        fns: vec![function],
        consts: vec![ConstValue::Int(1), ConstValue::Int(2)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["struct.mt".into()],
    };
    program.verify().expect("struct program");
}

fn duplicate_mut_program() -> IrProgram {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);

    let pair_nodes = vec![
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(0, 1)),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
    ];
    let pair = IrFn {
        id: FnId::new(0),
        name: "pair".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: vec![
            martite_ir::ParamInfo {
                slot: Slot::new(0),
                name: "a".into(),
                ty: int,
                mutable: true,
                is_self: false,
                pattern: None,
            },
            martite_ir::ParamInfo {
                slot: Slot::new(1),
                name: "b".into(),
                ty: int,
                mutable: true,
                is_self: false,
                pattern: None,
            },
        ],
        ret: unit,
        locals: 2,
        slot_info: vec![
            SlotInfo {
                ty: int,
                mutable: true,
                name: Some("a".into()),
            },
            SlotInfo {
                ty: int,
                mutable: true,
                name: Some("b".into()),
            },
        ],
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: pair_nodes,
        spans: vec![Span::empty(0); 2],
        synthetic: vec![None; 2],
        safepoints: vec![true, true],
        children: vec![IrId::new(0)],
        body: Some(IrId::new(1)),
    };

    let main_nodes = vec![
        node(
            NodeKind::SlotPlace(Slot::new(0)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::MutArg(IrId::new(0)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::SlotPlace(Slot::new(0)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::MutArg(IrId::new(2)),
            int,
            Charge::ZERO,
            Effects::NONE,
        ),
        node(
            NodeKind::CallDirect {
                target: FnId::new(0),
                first_arg: 0,
            },
            unit,
            Charge::expression(),
            Effects::CALLS | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(4)),
            unit,
            Charge::ZERO,
            Effects::CALLS | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Return(None),
            unit,
            Charge::ZERO,
            Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(2, 2)),
            unit,
            Charge::ZERO,
            Effects::CALLS | Effects::SAFEPOINT | Effects::NONLOCAL_EXIT,
        ),
    ];
    let mut safepoints = vec![false; main_nodes.len()];
    for id in [5, 6, 7] {
        safepoints[id] = true;
    }
    let main = IrFn {
        id: FnId::new(1),
        name: "main".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 1,
        slot_info: vec![SlotInfo {
            ty: int,
            mutable: true,
            name: Some("value".into()),
        }],
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: main_nodes,
        spans: vec![Span::empty(0); 8],
        synthetic: vec![None; 8],
        safepoints,
        children: vec![IrId::new(1), IrId::new(3), IrId::new(5), IrId::new(6)],
        body: Some(IrId::new(7)),
    };

    IrProgram {
        fns: vec![pair, main],
        consts: Vec::new(),
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(1)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["dup.mt".into()],
    }
}

#[test]
fn rejects_duplicate_mut_roots() {
    let error = error_kind(duplicate_mut_program());
    println!("duplicate mut error: {error:?}");
    assert!(matches!(error, VerifyErrorKind::DuplicateMutRoot { .. }));
}

#[test]
fn rejects_out_of_range_const() {
    let mut program = valid_program();
    program.fns[0].nodes[0].kind = NodeKind::Int(ConstId::new(99));
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::OutOfRange { .. }
    ));
}

#[test]
fn rejects_out_of_range_string() {
    let mut program = valid_program();
    program.fns[0].nodes[0].kind = NodeKind::Str(martite_ir::StrId::new(99));
    assert!(matches!(error_kind(program), VerifyErrorKind::Str { .. }));
}

#[test]
fn rejects_out_of_range_slot() {
    let mut program = valid_program();
    program.fns[0].nodes[2].kind = NodeKind::SlotRead {
        slot: Slot::new(99),
        mode: AccessMode::Share,
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::SlotInfo { .. }
    ));
}

#[test]
fn rejects_out_of_range_builtin_function() {
    let mut program = valid_program();
    program.fns[0].nodes[8].kind = NodeKind::CallBuiltinFn {
        builtin: BuiltinFnId::new(99),
        first_arg: 0,
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Builtin { .. }
    ));
}

#[test]
fn rejects_out_of_range_host() {
    let mut program = host_program(vec!["std.io".into()]);
    program.fns[0].nodes[1].kind = NodeKind::CallHost {
        host: martite_ir::HostId::new(99),
        first_arg: 0,
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::HostOutOfRange { .. }
    ));
}

#[test]
fn rejects_out_of_range_call_target() {
    let mut program = valid_call_and_mut_program();
    program.fns[1].nodes[4].kind = NodeKind::CallDirect {
        target: FnId::new(99),
        first_arg: 0,
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::OutOfRange { .. }
    ));
}

#[test]
fn rejects_binary_operand_type_mismatch() {
    let mut program = valid_program();
    let bool_ty = type_id(&program, Ty::Bool);
    program.fns[0].nodes[3] = node(
        NodeKind::Bool(true),
        bool_ty,
        Charge::expression(),
        Effects::NONE,
    );
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::TypeMismatch { .. }
    ));
}

#[test]
fn rejects_try_when_function_return_type_disagrees() {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let option_int = types.intern_ty(Ty::Option(int));
    let nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::PreludeVariant {
                variant: martite_ir::PreludeVariant::Some,
                first_payload: 0,
            },
            option_int,
            Charge::new(1, 1, 0),
            Effects::ALLOCATES,
        ),
        node(
            NodeKind::Try(IrId::new(1)),
            int,
            Charge::expression(),
            Effects::MAY_TRAP | Effects::ALLOCATES | Effects::NONLOCAL_EXIT,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(2)),
            unit,
            Charge::ZERO,
            Effects::MAY_TRAP | Effects::ALLOCATES | Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(1, 1)),
            int,
            Charge::ZERO,
            Effects::MAY_TRAP | Effects::ALLOCATES | Effects::NONLOCAL_EXIT | Effects::SAFEPOINT,
        ),
    ];
    let function = IrFn {
        id: FnId::new(0),
        name: "bad".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: unit,
        locals: 0,
        slot_info: Vec::new(),
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes,
        spans: vec![Span::empty(0); 5],
        synthetic: vec![None; 5],
        safepoints: vec![false, false, false, true, true],
        children: vec![IrId::new(0), IrId::new(3)],
        body: Some(IrId::new(4)),
    };
    let program = IrProgram {
        fns: vec![function],
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["try.mt".into()],
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::TypeMismatch { .. }
    ));
}

#[test]
fn rejects_call_argument_type_mismatch() {
    let mut program = duplicate_mut_program();
    let str_ty = program.types.intern_ty(Ty::Str);
    program.fns[0].params[0].ty = str_ty;
    program.fns[0].slot_info[0].ty = str_ty;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::TypeMismatch { .. }
    ));
}

#[test]
fn rejects_call_argument_range() {
    let mut program = duplicate_mut_program();
    program.fns[1].nodes[4].kind = NodeKind::CallDirect {
        target: FnId::new(0),
        first_arg: 3,
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::ChildRange { .. }
    ));
}

#[test]
fn rejects_mut_arguments_on_value_call() {
    let mut program = duplicate_mut_program();
    let int = type_id(&program, Ty::Int);
    let unit = type_id(&program, Ty::Unit);
    let fn_ty = program.types.intern_ty(Ty::Fn(vec![int, int], unit));
    let fn_ref = IrId::new(program.fns[1].nodes.len() as u32);
    program.fns[1].nodes.push(node(
        NodeKind::FnRef(FnId::new(0)),
        fn_ty,
        Charge::expression(),
        Effects::NONE,
    ));
    program.fns[1].spans.push(Span::empty(0));
    program.fns[1].synthetic.push(None);
    program.fns[1].safepoints.push(false);
    program.fns[1].nodes[4].kind = NodeKind::CallValue {
        first_child: 0,
        argc: 2,
    };
    program.fns[1].children = vec![
        fn_ref,
        IrId::new(1),
        IrId::new(3),
        IrId::new(5),
        IrId::new(6),
    ];
    program.fns[1].nodes[7].kind = NodeKind::Block(ChildRange::new(3, 2));
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::MutArg {
            expected: false,
            ..
        }
    ));
}

#[test]
fn rejects_value_call_arity_mismatch() {
    let mut program = duplicate_mut_program();
    let int = type_id(&program, Ty::Int);
    let unit = type_id(&program, Ty::Unit);
    let fn_ty = program.types.intern_ty(Ty::Fn(vec![int, int], unit));
    let fn_ref = IrId::new(program.fns[1].nodes.len() as u32);
    program.fns[1].nodes.push(node(
        NodeKind::FnRef(FnId::new(0)),
        fn_ty,
        Charge::expression(),
        Effects::NONE,
    ));
    program.fns[1].spans.push(Span::empty(0));
    program.fns[1].synthetic.push(None);
    program.fns[1].safepoints.push(false);
    program.fns[1].nodes[4].kind = NodeKind::CallValue {
        first_child: 0,
        argc: 1,
    };
    program.fns[1].children = vec![
        fn_ref,
        IrId::new(1),
        IrId::new(3),
        IrId::new(5),
        IrId::new(6),
    ];
    program.fns[1].nodes[7].kind = NodeKind::Block(ChildRange::new(3, 2));
    assert!(matches!(error_kind(program), VerifyErrorKind::Arity { .. }));
}

#[test]
fn rejects_builtin_method_receiver_mismatch() {
    let mut program = valid_builtin_method_program();
    program.fns[0].nodes[5].kind = NodeKind::SlotRead {
        slot: Slot::new(0),
        mode: AccessMode::Share,
    };
    program.fns[0].nodes[5].charge = Charge::expression();
    program.fns[0].nodes[5].effects = Effects::READS_LOCAL | Effects::SHARES;
    program.fns[0].nodes[7].kind = NodeKind::CallBuiltinMethod {
        method: martite_ir::BuiltinMethodId::lookup(martite_ir::BuiltinReceiver::Str, "len")
            .expect("str.len"),
        first_child: 2,
    };
    let error = error_kind(program);
    println!("receiver error: {error:?}");
    assert!(matches!(error, VerifyErrorKind::BuiltinReceiver { .. }));
}

#[test]
fn rejects_pattern_type_mismatch() {
    let mut program = valid_match_program();
    program.fns[0].patterns[0] = Pattern::Bool(true);
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::PatternShape { .. }
    ));
}

#[test]
fn rejects_bad_type_table_reference() {
    let mut program = valid_program();
    let int = type_id(&program, Ty::Int);
    program.types.tys.push(Ty::List(TyId::new(99)));
    let broken = TyId::new((program.types.tys.len() - 1) as u32);
    program.fns[0].nodes[0].ty = broken;
    let _ = int;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::TypeTable { .. } | VerifyErrorKind::OutOfRange { .. }
    ));
}

#[test]
fn rejects_bad_entry_function() {
    let mut program = valid_program();
    program.entries.main = Some(FnId::new(99));
    assert!(matches!(error_kind(program), VerifyErrorKind::Entry { .. }));
}

#[test]
fn rejects_out_of_range_capture() {
    let mut program = valid_closure_program();
    program.fns[1].captures[0].source = martite_ir::CaptureSource::Slot(Slot::new(99));
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Capture { .. }
    ));
}

#[test]
fn rejects_out_of_range_capture_source() {
    let mut program = valid_closure_program();
    program.fns[1].captures[0].source =
        martite_ir::CaptureSource::Capture(martite_ir::CaptureIndex::new(0));
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Capture { .. }
    ));
}

#[test]
fn rejects_captures_on_non_lambda_functions() {
    let mut program = valid_closure_program();
    program.fns[0].captures = program.fns[1].captures.clone();
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Capture { .. }
    ));
}

#[test]
fn rejects_missing_callee_effect() {
    let mut program = valid_call_and_mut_program();
    let effects = program.fns[1].nodes[4].effects;
    program.fns[1].nodes[4].effects =
        Effects::from_bits_truncate(effects.bits() & !Effects::MAY_TRAP.bits());
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Effects { .. }
    ));
}

#[test]
fn valid_enum_and_prelude_variants_verify() {
    let mut types = TypeTable::default();
    let unit = types.intern_ty(Ty::Unit);
    let int = types.intern_ty(Ty::Int);
    let option_int = types.intern_ty(Ty::Option(int));
    let choice_id = types.add_def(TypeDef {
        name: "Choice".into(),
        kind: TypeDefKind::Enum {
            variants: vec![
                martite_ir::VariantDef {
                    name: "A".into(),
                    payload: Vec::new(),
                },
                martite_ir::VariantDef {
                    name: "B".into(),
                    payload: vec![int],
                },
            ],
        },
        params: 0,
        is_interface: false,
        is_builtin: false,
    });
    let choice = types.intern_ty(Ty::Named(choice_id, Vec::new()));

    let variant_nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::EnumVariant {
                variant: 1,
                first_payload: 0,
            },
            choice,
            Charge::new(1, 1, 0),
            Effects::ALLOCATES,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(1)),
            unit,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(1, 1)),
            choice,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::SAFEPOINT,
        ),
    ];
    let variant_fn = IrFn {
        id: FnId::new(0),
        name: "choice".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: choice,
        locals: 0,
        slot_info: Vec::new(),
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: variant_nodes,
        spans: vec![Span::empty(0); 4],
        synthetic: vec![None; 4],
        safepoints: vec![false, false, true, true],
        children: vec![IrId::new(0), IrId::new(2)],
        body: Some(IrId::new(3)),
    };

    let prelude_nodes = vec![
        node(
            NodeKind::Int(ConstId::new(0)),
            int,
            Charge::expression(),
            Effects::NONE,
        ),
        node(
            NodeKind::PreludeVariant {
                variant: martite_ir::PreludeVariant::Some,
                first_payload: 0,
            },
            option_int,
            Charge::new(1, 1, 0),
            Effects::ALLOCATES,
        ),
        node(
            NodeKind::ExprStmt(IrId::new(1)),
            unit,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::SAFEPOINT,
        ),
        node(
            NodeKind::Block(ChildRange::new(1, 1)),
            option_int,
            Charge::ZERO,
            Effects::ALLOCATES | Effects::SAFEPOINT,
        ),
    ];
    let prelude_fn = IrFn {
        id: FnId::new(1),
        name: "some".into(),
        source: SourceId(0),
        kind: FnKind::Free,
        params: Vec::new(),
        ret: option_int,
        locals: 0,
        slot_info: Vec::new(),
        captures: Vec::new(),
        patterns: Vec::new(),
        matches: Vec::new(),
        struct_layouts: Vec::new(),
        str_parts: Vec::new(),
        nodes: prelude_nodes,
        spans: vec![Span::empty(0); 4],
        synthetic: vec![None; 4],
        safepoints: vec![false, false, true, true],
        children: vec![IrId::new(0), IrId::new(2)],
        body: Some(IrId::new(3)),
    };

    let program = IrProgram {
        fns: vec![variant_fn, prelude_fn],
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries {
            main: Some(FnId::new(0)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        },
        source_names: vec!["variants.mt".into()],
    };
    program.verify().expect("variant program");
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "IR verification failed after pass")]
fn verify_after_pass_panics_on_invalid_ir() {
    let mut program = valid_program();
    program.fns[0].spans.pop();
    martite_ir::verify_after_pass(&program);
}

#[test]
fn rejects_uncallable_builtin_item() {
    let mut program = valid_program();
    program.fns[0].nodes[8].kind = NodeKind::CallBuiltinModule {
        item: martite_ir::BuiltinItemId::MATH_PI,
        first_arg: 0,
    };
    let error = error_kind(program);
    assert!(matches!(
        error,
        VerifyErrorKind::Builtin { .. } | VerifyErrorKind::Arity { .. }
    ));
}

#[test]
fn rejects_read_before_let_binding() {
    let mut program = valid_program();
    // Move the print after the let but before it: [ExprStmt, Let, Assign, Return].
    program.fns[0].children[1] = IrId::new(9);
    program.fns[0].children[2] = IrId::new(1);
    program.fns[0].children[3] = IrId::new(6);
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::UnboundSlot { slot, .. } if slot == Slot::new(0)
    ));
}

#[test]
fn rejects_second_slot_initialization() {
    let mut program = valid_program();
    let int = program.fns[0].slot_info[0].ty;
    let value = IrId::new(program.fns[0].nodes.len() as u32);
    program.fns[0].nodes.push(node(
        NodeKind::Int(ConstId::new(0)),
        int,
        Charge::expression(),
        Effects::NONE,
    ));
    program.fns[0].spans.push(Span::empty(0));
    program.fns[0].synthetic.push(None);
    program.fns[0].safepoints.push(false);

    let pattern = PatternId::new(program.fns[0].patterns.len() as u32);
    program.fns[0].patterns.push(Pattern::Bind {
        slot: Slot::new(0),
        ty: int,
    });
    let unit = program.fns[0].ret;
    let second = IrId::new(program.fns[0].nodes.len() as u32);
    program.fns[0].nodes.push(node(
        NodeKind::Let { pattern, value },
        unit,
        Charge::ZERO,
        Effects::WRITES_LOCAL,
    ));
    program.fns[0].spans.push(Span::empty(0));
    program.fns[0].synthetic.push(None);
    program.fns[0].safepoints.push(true);
    program.fns[0].children.push(second);
    if let NodeKind::Block(range) = &mut program.fns[0].nodes[11].kind {
        range.len += 1;
    }

    let error = program.verify().expect_err("second init");
    assert_eq!(error.node, Some(second));
    assert!(
        matches!(error.kind, VerifyErrorKind::SlotBinding { slot, .. } if slot == Slot::new(0)),
        "{error:?}"
    );
}

#[test]
fn rejects_missing_slot_initialization() {
    let mut program = valid_program();
    let unit = program.fns[0].ret;
    program.fns[0].nodes[1] = node(
        NodeKind::SlotWrite {
            slot: Slot::new(0),
            value: IrId::new(0),
        },
        unit,
        Charge::ZERO,
        Effects::WRITES_LOCAL,
    );
    program.fns[0].safepoints[1] = false;
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::SlotBinding { slot, .. } if slot == Slot::new(0)
    ));
}

#[test]
fn rejects_closure_created_before_capture_is_initialized() {
    let mut program = valid_closure_program();
    program.fns[0].children = vec![IrId::new(3), IrId::new(1), IrId::new(4)];
    let error = program.verify().expect_err("capture read before init");
    assert_eq!(error.node, Some(IrId::new(2)));
    assert!(matches!(
        error.kind,
        VerifyErrorKind::UnboundSlot { slot, .. } if slot == Slot::new(0)
    ));
}

#[test]
fn rejects_capture_read_with_empty_capture_list() {
    let mut program = valid_closure_program();
    program.fns[1].captures.clear();
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::Capture { .. }
    ));
}

#[test]
fn accepts_parameter_read_at_entry() {
    let program = valid_call_and_mut_program();
    program.verify().expect("parameters are written at entry");
}

#[test]
fn unbound_slot_error_names_slot_function_and_span() {
    let mut program = valid_program();
    program.fns[0].children[1] = IrId::new(9);
    program.fns[0].children[2] = IrId::new(1);
    program.fns[0].children[3] = IrId::new(6);
    program.fns[0].spans[7] = Span::new(5, 9);
    let error = program.verify().expect_err("read before let");
    assert_eq!(error.node, Some(IrId::new(7)));
    assert!(matches!(
        error.kind,
        VerifyErrorKind::UnboundSlot { slot, .. } if slot == Slot::new(0)
    ));
    let text = error.to_string();
    assert!(text.contains("fn #0"), "{text}");
    assert!(text.contains("slot #0"), "{text}");
    assert!(text.contains("at 5..9"), "{text}");
}

#[test]
fn rejects_or_pattern_alternatives_that_bind_different_slots() {
    let mut program = valid_program();
    let int = program.fns[0].slot_info[0].ty;
    program.fns[0].locals = 2;
    program.fns[0].slot_info.push(SlotInfo {
        ty: int,
        mutable: false,
        name: None,
    });
    program.fns[0].patterns[0] = Pattern::Or(vec![
        Pattern::Bind {
            slot: Slot::new(0),
            ty: int,
        },
        Pattern::Bind {
            slot: Slot::new(1),
            ty: int,
        },
    ]);
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::PatternShape { .. }
    ));
}

#[test]
fn rejects_refutable_let_pattern() {
    let mut program = valid_program();
    program.fns[0].patterns[0] = Pattern::Int(1);
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::PatternShape {
            context: "`let` pattern is refutable",
            ..
        }
    ));
}

#[test]
fn rejects_refutable_parameter_pattern() {
    let mut program = valid_match_program();
    program.fns[0].params[0].pattern = Some(PatternId::new(0));
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::PatternShape {
            context: "parameter pattern is refutable",
            ..
        }
    ));
}

#[test]
fn rejects_match_arm_binding_shared_with_another_arm() {
    let mut program = valid_match_program();
    let int = program.fns[0].slot_info[0].ty;
    program.fns[0].locals = 2;
    program.fns[0].slot_info.push(SlotInfo {
        ty: int,
        mutable: false,
        name: None,
    });
    program.fns[0].patterns[0] = Pattern::Bind {
        slot: Slot::new(1),
        ty: int,
    };
    program.fns[0].patterns[1] = Pattern::Bind {
        slot: Slot::new(1),
        ty: int,
    };
    assert!(matches!(
        error_kind(program),
        VerifyErrorKind::PatternShape {
            context: "match arm binding slot is shared with another arm",
            ..
        }
    ));
}
