#![allow(clippy::let_and_return)]

use martite_ir::analysis::{DefiniteAssignment, Liveness};
use martite_ir::{
    AccessMode, BuiltinFnId, Charge, ChildRange, ConstId, ConstValue, Effects, Entries, FnId,
    FnKind, IrFn, IrId, IrProgram, MatchArm, MatchSet, Node, NodeKind, ParamInfo, Pattern,
    PatternId, PreludeVariant, Slot, SlotInfo, SourceId, Span, Ty, TyId, TypeTable,
};

struct Types {
    table: TypeTable,
    unit: TyId,
    int: TyId,
    bool_ty: TyId,
    option_int: TyId,
}

fn types() -> Types {
    let mut table = TypeTable::default();
    let unit = table.intern_ty(Ty::Unit);
    let int = table.intern_ty(Ty::Int);
    let bool_ty = table.intern_ty(Ty::Bool);
    let option_int = table.intern_ty(Ty::Option(int));
    Types {
        table,
        unit,
        int,
        bool_ty,
        option_int,
    }
}

fn slot_info(ty: TyId, mutable: bool) -> SlotInfo {
    SlotInfo {
        ty,
        mutable,
        name: None,
    }
}

struct FnBuilder {
    nodes: Vec<Node>,
    spans: Vec<Span>,
    safepoints: Vec<bool>,
    children: Vec<IrId>,
    slot_info: Vec<SlotInfo>,
    params: Vec<ParamInfo>,
    patterns: Vec<Pattern>,
    matches: Vec<MatchSet>,
    ret: TyId,
    body: Option<IrId>,
}

impl FnBuilder {
    fn new(ret: TyId, slots: Vec<SlotInfo>) -> Self {
        Self {
            nodes: Vec::new(),
            spans: Vec::new(),
            safepoints: Vec::new(),
            children: Vec::new(),
            slot_info: slots,
            params: Vec::new(),
            patterns: Vec::new(),
            matches: Vec::new(),
            ret,
            body: None,
        }
    }

    fn node(&mut self, kind: NodeKind, ty: TyId, charge: Charge) -> IrId {
        let id = IrId::new(self.nodes.len() as u32);
        self.nodes.push(Node::new(kind, ty, charge, Effects::ALL));
        self.spans.push(Span::empty(0));
        self.safepoints.push(false);
        id
    }

    fn statement(&mut self, kind: NodeKind, ty: TyId) -> IrId {
        let id = self.node(kind, ty, Charge::ZERO);
        self.safepoint(id);
        id
    }

    fn safepoint(&mut self, id: IrId) {
        self.safepoints[id.index()] = true;
    }

    fn param(&mut self, slot: Slot, ty: TyId, mutable: bool) {
        self.params.push(ParamInfo {
            slot,
            name: "p".into(),
            ty,
            mutable,
            is_self: false,
            pattern: None,
        });
    }

    fn pattern(&mut self, pattern: Pattern) -> PatternId {
        let id = PatternId::new(self.patterns.len() as u32);
        self.patterns.push(pattern);
        id
    }

    fn match_set(&mut self, arms: Vec<MatchArm>) -> martite_ir::MatchSetId {
        let id = martite_ir::MatchSetId::new(self.matches.len() as u32);
        self.matches.push(MatchSet { arms });
        id
    }

    fn child(&mut self, id: IrId) -> u32 {
        let index = self.children.len() as u32;
        self.children.push(id);
        index
    }

    fn range(&mut self, ids: &[IrId]) -> ChildRange {
        let start = self.children.len() as u32;
        self.children.extend_from_slice(ids);
        ChildRange::new(start, ids.len() as u32)
    }

    fn body(&mut self, body: IrId) {
        self.body = Some(body);
    }

    fn build(mut self, name: &str, kind: FnKind) -> IrFn {
        // A trailing expression statement is the block's value and is not a
        // safepoint in the interpreter.
        for node in &self.nodes {
            let (NodeKind::Block(range) | NodeKind::ExprBlock(range)) = node.kind else {
                continue;
            };
            if range.len == 0 {
                continue;
            }
            let last = range.start as usize + range.len as usize - 1;
            let Some(last) = self.children.get(last).copied() else {
                continue;
            };
            if matches!(
                self.nodes.get(last.index()).map(|node| node.kind),
                Some(NodeKind::ExprStmt(_))
            ) {
                self.safepoints[last.index()] = false;
            }
        }
        let synthetic = vec![None; self.nodes.len()];
        IrFn {
            id: FnId::new(0),
            name: name.into(),
            source: SourceId(0),
            kind,
            params: self.params,
            ret: self.ret,
            locals: self.slot_info.len() as u32,
            slot_info: self.slot_info,
            captures: Vec::new(),
            patterns: self.patterns,
            matches: self.matches,
            struct_layouts: Vec::new(),
            str_parts: Vec::new(),
            nodes: self.nodes,
            spans: self.spans,
            synthetic,
            safepoints: self.safepoints,
            children: self.children,
            body: self.body,
        }
    }
}

fn program(types: Types, functions: Vec<IrFn>) -> IrProgram {
    IrProgram {
        fns: functions,
        consts: vec![ConstValue::Int(1)],
        strs: Vec::new(),
        types: types.table,
        hosts: Vec::new(),
        grants: Vec::new(),
        entries: Entries::default(),
        source_names: vec!["analysis.mt".into()],
    }
}

fn int_const(builder: &mut FnBuilder, ty: TyId) -> IrId {
    builder.node(NodeKind::Int(ConstId::new(0)), ty, Charge::expression())
}

fn print_call(builder: &mut FnBuilder, unit: TyId, argument: IrId) -> IrId {
    let first_arg = builder.child(argument);
    let id = builder.node(
        NodeKind::CallBuiltinFn {
            builtin: BuiltinFnId::PRINT,
            first_arg,
        },
        unit,
        Charge::expression(),
    );
    builder.safepoint(id);
    id
}

#[test]
fn definite_assignment_and_liveness_queries() {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.unit, vec![slot_info(types.int, false)]);
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );
    let read = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let print = print_call(&mut builder, types.unit, read);
    let statement = builder.statement(NodeKind::ExprStmt(print), types.unit);
    let ret = builder.statement(NodeKind::Return(None), types.unit);
    let range = builder.range(&[let_node, statement, ret]);
    let body = builder.node(NodeKind::Block(range), types.unit, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);
    program.verify().expect("valid program");

    let definite = DefiniteAssignment::compute(&program, &program.fns[0]).expect("analysis");
    assert!(!definite.is_assigned_before(let_node, slot));
    assert!(definite.is_assigned_before(read, slot));
    assert!(definite.is_assigned_before(print, slot));
    assert!(definite.reachable(read));
    assert!(
        definite
            .assigned_after(read)
            .is_some_and(|set| set.contains(slot))
    );

    let live = Liveness::compute(&program, &program.fns[0]);
    assert!(live.is_live_after(let_node, slot));
    assert!(!live.is_live_after(read, slot));
    assert!(!live.is_live_after(statement, slot));
}

#[test]
fn take_unassigns_the_slot() {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.unit, vec![slot_info(types.int, true)]);
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );
    let take = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Take,
        },
        types.int,
        Charge::expression(),
    );
    let take_stmt = builder.statement(NodeKind::ExprStmt(take), types.unit);
    let read = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let read_stmt = builder.statement(NodeKind::ExprStmt(read), types.unit);
    let ret = builder.statement(NodeKind::Return(None), types.unit);
    let range = builder.range(&[let_node, take_stmt, read_stmt, ret]);
    let body = builder.node(NodeKind::Block(range), types.unit, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);

    let error = program.verify().expect_err("read after take");
    assert_eq!(error.node, Some(read));
    assert!(matches!(
        error.kind,
        martite_ir::VerifyErrorKind::UnboundSlot { slot: bound, .. } if bound == slot
    ));
}

fn loop_program(read_before_init: bool, read_after_loop: bool) -> (IrProgram, IrId, Option<IrId>) {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.unit, vec![slot_info(types.int, true)]);
    let cond = builder.node(NodeKind::Bool(false), types.bool_ty, Charge::expression());
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );
    let read = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let read_stmt = builder.statement(NodeKind::ExprStmt(read), types.unit);
    let unit_value = builder.node(NodeKind::Unit, types.unit, Charge::expression());
    let unit_stmt = builder.statement(NodeKind::ExprStmt(unit_value), types.unit);
    let body_range = if read_before_init {
        builder.range(&[read_stmt, let_node, unit_stmt])
    } else {
        builder.range(&[let_node, read_stmt, unit_stmt])
    };
    let body = builder.node(NodeKind::Block(body_range), types.unit, Charge::ZERO);
    let while_node = builder.node(
        NodeKind::While { cond, body },
        types.unit,
        Charge::iteration(),
    );

    let mut after_read = None;
    let body_range = if read_after_loop {
        let after = builder.node(
            NodeKind::SlotRead {
                slot,
                mode: AccessMode::Share,
            },
            types.int,
            Charge::expression(),
        );
        after_read = Some(after);
        let print = print_call(&mut builder, types.unit, after);
        let stmt = builder.statement(NodeKind::ExprStmt(print), types.unit);
        builder.range(&[while_node, stmt])
    } else {
        builder.range(&[while_node])
    };
    let body = builder.node(NodeKind::Block(body_range), types.unit, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);
    (program, read, after_read)
}

#[test]
fn accepts_read_after_loop_body_initialization() {
    let (program, _, _) = loop_program(false, false);
    program.verify().expect("read follows the init in the body");
}

#[test]
fn rejects_read_before_loop_body_initialization() {
    let (program, read, _) = loop_program(true, false);
    let error = program.verify().expect_err("first iteration is unbound");
    assert_eq!(error.node, Some(read));
}

#[test]
fn rejects_read_after_loop_that_only_the_body_initializes() {
    let (program, _, after_read) = loop_program(false, true);
    let error = program.verify().expect_err("loop may run zero times");
    assert_eq!(error.node, after_read);
}

fn if_program() -> (IrProgram, IrId) {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.unit, vec![slot_info(types.int, false)]);
    let cond = builder.node(NodeKind::Bool(true), types.bool_ty, Charge::expression());
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );
    let then_range = builder.range(&[let_node]);
    let then_block = builder.node(NodeKind::Block(then_range), types.unit, Charge::ZERO);
    let if_range = builder.range(&[cond, then_block]);
    let if_node = builder.node(NodeKind::If(if_range), types.unit, Charge::expression());
    let if_stmt = builder.statement(NodeKind::ExprStmt(if_node), types.unit);
    let read = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let print = print_call(&mut builder, types.unit, read);
    let stmt = builder.statement(NodeKind::ExprStmt(print), types.unit);
    let range = builder.range(&[if_stmt, stmt]);
    let body = builder.node(NodeKind::Block(range), types.unit, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);
    (program, read)
}

#[test]
fn rejects_read_after_a_conditional_initialization() {
    let (program, read) = if_program();
    let error = program.verify().expect_err("the else path does not bind");
    assert_eq!(error.node, Some(read));
}

fn match_program() -> (IrProgram, IrId) {
    let types = types();
    let mut builder = FnBuilder::new(
        types.int,
        vec![
            slot_info(types.option_int, false),
            slot_info(types.int, false),
        ],
    );
    builder.param(Slot::new(0), types.option_int, false);

    let scrutinee = builder.node(
        NodeKind::SlotRead {
            slot: Slot::new(0),
            mode: AccessMode::Share,
        },
        types.option_int,
        Charge::expression(),
    );
    let guard_read = builder.node(
        NodeKind::SlotRead {
            slot: Slot::new(1),
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let one = int_const(&mut builder, types.int);
    let guard = builder.node(
        NodeKind::Eq {
            lhs: guard_read,
            rhs: one,
        },
        types.bool_ty,
        Charge::expression(),
    );
    let arm0_body = int_const(&mut builder, types.int);
    let arm1_body = builder.node(
        NodeKind::SlotRead {
            slot: Slot::new(1),
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );

    let pattern0 = builder.pattern(Pattern::Prelude {
        variant: PreludeVariant::Some,
        args: vec![Pattern::Bind {
            slot: Slot::new(1),
            ty: types.int,
        }],
    });
    let pattern1 = builder.pattern(Pattern::Wild);
    let match_set = builder.match_set(vec![
        MatchArm {
            pattern: pattern0,
            guard: Some(guard),
            body: arm0_body,
        },
        MatchArm {
            pattern: pattern1,
            guard: None,
            body: arm1_body,
        },
    ]);
    let match_node = builder.node(
        NodeKind::Match {
            match_set,
            scrutinee,
        },
        types.int,
        Charge::expression(),
    );
    let stmt = builder.statement(NodeKind::ExprStmt(match_node), types.unit);
    let range = builder.range(&[stmt]);
    let body = builder.node(NodeKind::Block(range), types.int, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("pick", FnKind::Free)]);
    (program, arm1_body)
}

#[test]
fn rejects_read_after_a_guard_failed_arm() {
    let (program, read) = match_program();
    let error = program.verify().expect_err("arm bindings are cleared");
    assert_eq!(error.node, Some(read));
    assert!(matches!(
        error.kind,
        martite_ir::VerifyErrorKind::UnboundSlot { slot, .. } if slot == Slot::new(1)
    ));
}

#[test]
fn liveness_reports_a_loop_carried_read() {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.unit, vec![slot_info(types.int, false)]);
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );
    let cond = builder.node(NodeKind::Bool(false), types.bool_ty, Charge::expression());
    let read = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let read_stmt = builder.statement(NodeKind::ExprStmt(read), types.unit);
    let unit_value = builder.node(NodeKind::Unit, types.unit, Charge::expression());
    let unit_stmt = builder.statement(NodeKind::ExprStmt(unit_value), types.unit);
    let body_range = builder.range(&[read_stmt, unit_stmt]);
    let loop_body = builder.node(NodeKind::Block(body_range), types.unit, Charge::ZERO);
    let while_node = builder.node(
        NodeKind::While {
            cond,
            body: loop_body,
        },
        types.unit,
        Charge::iteration(),
    );
    let range = builder.range(&[let_node, while_node]);
    let body = builder.node(NodeKind::Block(range), types.unit, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);
    program.verify().expect("valid program");

    let live = Liveness::compute(&program, &program.fns[0]);
    assert!(!live.is_live_after(while_node, slot));
    assert!(live.live_before(while_node).contains(slot));
    assert!(live.is_live_after(let_node, slot));
    assert!(live.live_before(read).contains(slot));
}

#[test]
fn break_and_continue_are_handled() {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.unit, vec![slot_info(types.int, true)]);
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );

    let while_cond = builder.node(NodeKind::Bool(false), types.bool_ty, Charge::expression());

    let continue_cond = builder.node(NodeKind::Bool(true), types.bool_ty, Charge::expression());
    let continue_node = builder.node(NodeKind::Continue, types.unit, Charge::ZERO);
    let continue_range = builder.range(&[continue_node]);
    let continue_block = builder.node(NodeKind::Block(continue_range), types.unit, Charge::ZERO);
    let continue_if_range = builder.range(&[continue_cond, continue_block]);
    let continue_if = builder.node(
        NodeKind::If(continue_if_range),
        types.unit,
        Charge::expression(),
    );
    let continue_stmt = builder.statement(NodeKind::ExprStmt(continue_if), types.unit);

    let break_cond = builder.node(NodeKind::Bool(true), types.bool_ty, Charge::expression());
    let break_node = builder.node(NodeKind::Break, types.unit, Charge::ZERO);
    let break_range = builder.range(&[break_node]);
    let break_block = builder.node(NodeKind::Block(break_range), types.unit, Charge::ZERO);
    let break_if_range = builder.range(&[break_cond, break_block]);
    let break_if = builder.node(
        NodeKind::If(break_if_range),
        types.unit,
        Charge::expression(),
    );
    let break_stmt = builder.statement(NodeKind::ExprStmt(break_if), types.unit);

    let unit_value = builder.node(NodeKind::Unit, types.unit, Charge::expression());
    let unit_stmt = builder.statement(NodeKind::ExprStmt(unit_value), types.unit);

    let loop_range = builder.range(&[continue_stmt, break_stmt, unit_stmt]);
    let loop_body = builder.node(NodeKind::Block(loop_range), types.unit, Charge::ZERO);
    let while_node = builder.node(
        NodeKind::While {
            cond: while_cond,
            body: loop_body,
        },
        types.unit,
        Charge::iteration(),
    );

    let range = builder.range(&[let_node, while_node]);
    let body = builder.node(NodeKind::Block(range), types.unit, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);
    program.verify().expect("break and continue are accepted");
}

#[test]
fn try_keeps_the_normal_path_definite() {
    let types = types();
    let slot = Slot::new(0);
    let mut builder = FnBuilder::new(types.option_int, vec![slot_info(types.int, false)]);
    let one = int_const(&mut builder, types.int);
    let pattern = builder.pattern(Pattern::Bind {
        slot,
        ty: types.int,
    });
    let let_node = builder.statement(
        NodeKind::Let {
            pattern,
            value: one,
        },
        types.unit,
    );
    let read = builder.node(
        NodeKind::SlotRead {
            slot,
            mode: AccessMode::Share,
        },
        types.int,
        Charge::expression(),
    );
    let payload = builder.child(read);
    let some = builder.node(
        NodeKind::PreludeVariant {
            variant: PreludeVariant::Some,
            first_payload: payload,
        },
        types.option_int,
        Charge::expression(),
    );
    let try_node = builder.node(NodeKind::Try(some), types.int, Charge::expression());
    let payload = builder.child(try_node);
    let returned = builder.node(
        NodeKind::PreludeVariant {
            variant: PreludeVariant::Some,
            first_payload: payload,
        },
        types.option_int,
        Charge::expression(),
    );
    let expr_stmt = builder.statement(NodeKind::ExprStmt(returned), types.unit);
    let range = builder.range(&[let_node, expr_stmt]);
    let body = builder.node(NodeKind::Block(range), types.option_int, Charge::ZERO);
    builder.safepoint(body);
    builder.body(body);
    let program = program(types, vec![builder.build("main", FnKind::Free)]);
    program.verify().expect("try normal path");
}
