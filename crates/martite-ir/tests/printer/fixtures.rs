//! Hand-built IR for the printer goldens.
//!
//! The corpus is built directly from the data model so each fixture exercises
//! one area the printer must make reviewable: literals, calls, closures, match,
//! loops, mutable arguments and synthetic provenance. #114 adds dumps of real
//! sources alongside these.

use martite_ir::{
    Charge, ChildRange, ConstId, ConstValue, Effects, Entries, EntryKind, FnId, FnKind, HostFnInfo,
    IrFn, IrId, MatchArm, MatchSet, Node, NodeKind, ParamInfo, Pattern, PatternId, Slot, SlotInfo,
    SourceId, Span, StrId, TyId, TypeId, TypeTable,
};

const SOURCE: SourceId = SourceId(0);
const INT: TyId = TyId(0);
const UNIT: TyId = TyId(1);

pub struct FnBuilder {
    function: IrFn,
    next_span: u32,
}

impl FnBuilder {
    pub fn new(id: u32, name: &str, kind: FnKind, locals: u32) -> Self {
        Self {
            function: IrFn {
                id: FnId::new(id),
                name: name.into(),
                source: SOURCE,
                kind,
                params: Vec::new(),
                ret: TyId::new(0),
                locals,
                slot_info: Vec::new(),
                captures: Vec::new(),
                patterns: Vec::new(),
                matches: Vec::new(),
                struct_layouts: Vec::new(),
                str_parts: Vec::new(),
                nodes: Vec::new(),
                spans: Vec::new(),
                synthetic: Vec::new(),
                safepoints: Vec::new(),
                children: Vec::new(),
                body: None,
            },
            next_span: 0,
        }
    }

    pub fn ret(mut self, ty: TyId) -> Self {
        self.function.ret = ty;
        self
    }

    pub fn param(mut self, slot: u32, name: &str, ty: TyId, mutable: bool) -> Self {
        self.function.params.push(ParamInfo {
            slot: Slot::new(slot),
            name: name.into(),
            ty,
            mutable,
            is_self: false,
            pattern: None,
        });
        self
    }

    pub fn slot(mut self, ty: TyId, mutable: bool, name: Option<&str>) -> Self {
        self.function.slot_info.push(SlotInfo {
            ty,
            mutable,
            name: name.map(Into::into),
        });
        self
    }

    pub fn capture(mut self, outer: u32, ty: TyId, name: &str) -> Self {
        self.function.captures.push(martite_ir::CaptureInfo {
            source: martite_ir::CaptureSource::Slot(Slot::new(outer)),
            ty,
            name: Some(name.into()),
        });
        self
    }

    pub fn capture_from(mut self, source: martite_ir::CaptureSource, ty: TyId, name: &str) -> Self {
        self.function.captures.push(martite_ir::CaptureInfo {
            source,
            ty,
            name: Some(name.into()),
        });
        self
    }

    pub fn pattern(&mut self, pattern: Pattern) -> PatternId {
        let id = PatternId::new(self.function.patterns.len() as u32);
        self.function.patterns.push(pattern);
        id
    }

    pub fn match_set(&mut self, arms: Vec<MatchArm>) -> martite_ir::MatchSetId {
        let id = martite_ir::MatchSetId::new(self.function.matches.len() as u32);
        self.function.matches.push(MatchSet { arms });
        id
    }

    pub fn node(&mut self, kind: NodeKind, ty: TyId, charge: Charge, effects: Effects) -> IrId {
        let span = Span::new(self.next_span, self.next_span + 1);
        self.next_span += 1;
        self.node_at(kind, ty, charge, effects, span)
    }

    pub fn synthetic(
        &mut self,
        kind: NodeKind,
        ty: TyId,
        charge: Charge,
        effects: Effects,
        origin: IrId,
    ) -> IrId {
        let id = self.node(kind, ty, charge, effects);
        self.function.synthetic[id.index()] = Some(origin);
        id
    }

    pub fn child(&mut self, id: IrId) -> u32 {
        let index = self.function.children.len() as u32;
        self.function.children.push(id);
        index
    }

    pub fn children(&mut self, ids: &[IrId]) -> ChildRange {
        let start = self.function.children.len() as u32;
        self.function.children.extend_from_slice(ids);
        ChildRange::new(start, ids.len() as u32)
    }

    pub fn body(mut self, body: IrId) -> IrFn {
        self.function.body = Some(body);
        self.function
    }

    fn node_at(
        &mut self,
        kind: NodeKind,
        ty: TyId,
        charge: Charge,
        effects: Effects,
        span: Span,
    ) -> IrId {
        let id = IrId::new(self.function.nodes.len() as u32);
        self.function
            .nodes
            .push(Node::new(kind, ty, charge, effects));
        self.function.spans.push(span);
        self.function.synthetic.push(None);
        self.function.safepoints.push(false);
        id
    }

    pub fn safepoint(&mut self, id: IrId) -> IrId {
        if let Some(flag) = self.function.safepoints.get_mut(id.index()) {
            *flag = true;
        }
        id
    }
}

pub struct ProgramBuilder {
    program: martite_ir::IrProgram,
}

impl ProgramBuilder {
    pub fn new(source: &str) -> Self {
        Self {
            program: martite_ir::IrProgram {
                fns: Vec::new(),
                consts: Vec::new(),
                strs: Vec::new(),
                types: TypeTable::default(),
                hosts: Vec::new(),
                grants: Vec::new(),
                entries: Entries {
                    entry_kind: EntryKind::Script,
                    ..Entries::default()
                },
                source_names: vec![source.into()],
            },
        }
    }

    pub fn function(mut self, function: IrFn) -> Self {
        self.program.fns.push(function);
        self
    }

    pub fn consts(mut self, consts: Vec<ConstValue>) -> Self {
        self.program.consts = consts;
        self
    }

    pub fn strs(mut self, strs: &[&str]) -> Self {
        self.program.strs = strs.iter().map(|text| (*text).into()).collect();
        self
    }

    pub fn host(mut self, host: HostFnInfo) -> Self {
        self.program.hosts.push(host);
        self
    }

    pub fn grant(mut self, grant: &str) -> Self {
        self.program.grants.push(grant.into());
        self
    }

    pub fn entries(mut self, entries: Entries) -> Self {
        self.program.entries = entries;
        self
    }

    pub fn build(mut self) -> martite_ir::IrProgram {
        if self.program.entries.main.is_none() {
            self.program.entries.main = Some(FnId::new(0));
        }
        self.program
    }
}

pub fn all() -> Vec<(&'static str, martite_ir::IrProgram)> {
    vec![
        ("literals", literals()),
        ("calls", calls()),
        ("closures", closures()),
        ("match", match_program()),
        ("loops", loops()),
        ("mut_params", mut_params()),
        ("synthetic", synthetic()),
    ]
}

fn literals() -> martite_ir::IrProgram {
    let mut function = FnBuilder::new(0, "main", FnKind::Free, 1)
        .ret(INT)
        .slot(INT, false, None);
    let int = function.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let float = function.node(
        NodeKind::Float(ConstId::new(1)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let boolean = function.node(
        NodeKind::Bool(true),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let text = function.node(
        NodeKind::Str(StrId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let items = function.children(&[int, float, boolean, text]);
    let tuple = function.node(
        NodeKind::Tuple(items),
        INT,
        Charge::new(1, 4, 0),
        Effects::ALLOCATES,
    );
    let body = function.node(
        NodeKind::Return(Some(tuple)),
        UNIT,
        Charge::ZERO,
        Effects::NONE,
    );
    function.safepoint(body);
    ProgramBuilder::new("literals.mt")
        .consts(vec![
            ConstValue::Int(7),
            ConstValue::Float(2.5),
            ConstValue::Bool(true),
        ])
        .strs(&["hello"])
        .function(function.body(body))
        .build()
}

fn calls() -> martite_ir::IrProgram {
    let host = HostFnInfo {
        name: "log".into(),
        module: "std.io".into(),
        params: vec![INT],
        ret: UNIT,
        opaque_return: None,
    };

    let mut fib = FnBuilder::new(0, "fib", FnKind::Free, 1)
        .ret(INT)
        .param(0, "n", INT, false)
        .slot(INT, false, Some("n"));
    let read_n = fib.node(
        NodeKind::SlotRead {
            slot: Slot::new(0),
            mode: martite_ir::AccessMode::Share,
        },
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::SHARES,
    );
    let one = fib.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let less = fib.node(
        NodeKind::Lt {
            lhs: read_n,
            rhs: one,
        },
        INT,
        Charge::expression(),
        Effects::MAY_TRAP,
    );
    let base = fib.node(
        NodeKind::Return(Some(read_n)),
        UNIT,
        Charge::ZERO,
        Effects::READS_LOCAL,
    );
    let n1 = fib.node(
        NodeKind::SlotRead {
            slot: Slot::new(0),
            mode: martite_ir::AccessMode::Share,
        },
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::SHARES,
    );
    let one_again = fib.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let n_minus_one = fib.node(
        NodeKind::Sub {
            lhs: n1,
            rhs: one_again,
        },
        INT,
        Charge::expression(),
        Effects::MAY_TRAP,
    );
    let arg_one = fib.child(n_minus_one);
    let first_call = fib.node(
        NodeKind::CallDirect {
            target: FnId::new(0),
            first_arg: arg_one,
        },
        INT,
        Charge::expression(),
        Effects::CALLS | Effects::MAY_TRAP | Effects::DYN_COST,
    );
    let n2 = fib.node(
        NodeKind::SlotRead {
            slot: Slot::new(0),
            mode: martite_ir::AccessMode::Share,
        },
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::SHARES,
    );
    let two = fib.node(
        NodeKind::Int(ConstId::new(1)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let n_minus_two = fib.node(
        NodeKind::Sub { lhs: n2, rhs: two },
        INT,
        Charge::expression(),
        Effects::MAY_TRAP,
    );
    let arg_two = fib.child(n_minus_two);
    let second_call = fib.node(
        NodeKind::CallDirect {
            target: FnId::new(0),
            first_arg: arg_two,
        },
        INT,
        Charge::expression(),
        Effects::CALLS | Effects::MAY_TRAP | Effects::DYN_COST,
    );
    let sum = fib.node(
        NodeKind::Add {
            lhs: first_call,
            rhs: second_call,
        },
        INT,
        Charge::expression(),
        Effects::MAY_TRAP | Effects::CALLS | Effects::DYN_COST,
    );
    let recursive = fib.node(
        NodeKind::Return(Some(sum)),
        UNIT,
        Charge::ZERO,
        Effects::CALLS | Effects::MAY_TRAP | Effects::DYN_COST,
    );
    let then_range = fib.children(&[base]);
    let then_block = fib.node(
        NodeKind::Block(then_range),
        INT,
        Charge::ZERO,
        Effects::READS_LOCAL,
    );
    let else_range = fib.children(&[recursive]);
    let else_block = fib.node(
        NodeKind::Block(else_range),
        INT,
        Charge::ZERO,
        Effects::CALLS | Effects::MAY_TRAP | Effects::DYN_COST,
    );
    let if_children = fib.children(&[less, then_block, else_block]);
    let body = fib.node(
        NodeKind::If(if_children),
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::CALLS | Effects::MAY_TRAP | Effects::DYN_COST,
    );

    ProgramBuilder::new("calls.mt")
        .consts(vec![ConstValue::Int(1), ConstValue::Int(2)])
        .host(host)
        .grant("std.io")
        .entries(Entries {
            main: Some(FnId::new(0)),
            tests: vec![FnId::new(0)],
            handlers: Vec::new(),
            hooks: Vec::new(),
            init: None,
            shutdown: None,
            state_type: Some(INT),
            json_type: Some(TypeId::new(0)),
            entry_kind: EntryKind::Script,
        })
        .function(fib.body(body))
        .build()
}

fn closures() -> martite_ir::IrProgram {
    let mut lambda = FnBuilder::new(1, "<lambda>", FnKind::Lambda, 0)
        .ret(INT)
        .capture(0, INT, "x")
        .capture_from(
            martite_ir::CaptureSource::Capture(martite_ir::CaptureIndex::new(0)),
            INT,
            "y",
        );
    let captured = lambda.node(
        NodeKind::CaptureRead(martite_ir::CaptureIndex::new(0)),
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::SHARES,
    );

    let mut main = FnBuilder::new(0, "main", FnKind::Free, 1)
        .ret(UNIT)
        .slot(INT, false, Some("x"));
    let closure = main.node(
        NodeKind::MakeClosure(FnId::new(1)),
        INT,
        Charge::expression(),
        Effects::ALLOCATES,
    );
    let statement = main.node(
        NodeKind::ExprStmt(closure),
        UNIT,
        Charge::ZERO,
        Effects::ALLOCATES,
    );

    ProgramBuilder::new("closures.mt")
        .function(main.body(statement))
        .function(lambda.body(captured))
        .build()
}

fn match_program() -> martite_ir::IrProgram {
    let mut function = FnBuilder::new(0, "classify", FnKind::Free, 1)
        .ret(INT)
        .param(0, "value", INT, false)
        .slot(INT, false, Some("value"));
    let one_pattern = function.pattern(Pattern::Int(1));
    let wildcard = function.pattern(Pattern::Wild);

    let scrutinee = function.node(
        NodeKind::SlotRead {
            slot: Slot::new(0),
            mode: martite_ir::AccessMode::Share,
        },
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::SHARES,
    );
    let one = function.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let guard = function.node(
        NodeKind::Eq {
            lhs: scrutinee,
            rhs: one,
        },
        INT,
        Charge::expression(),
        Effects::MAY_TRAP,
    );
    let first_body = function.node(
        NodeKind::Return(Some(scrutinee)),
        UNIT,
        Charge::ZERO,
        Effects::READS_LOCAL,
    );
    let fallback = function.node(NodeKind::Return(None), UNIT, Charge::ZERO, Effects::NONE);
    let arms = function.match_set(vec![
        MatchArm {
            pattern: one_pattern,
            guard: None,
            body: first_body,
        },
        MatchArm {
            pattern: wildcard,
            guard: Some(guard),
            body: fallback,
        },
    ]);
    let body = function.node(
        NodeKind::Match {
            match_set: arms,
            scrutinee,
        },
        INT,
        Charge::expression(),
        Effects::READS_LOCAL | Effects::MAY_TRAP,
    );

    ProgramBuilder::new("match.mt")
        .consts(vec![ConstValue::Int(1)])
        .function(function.body(body))
        .build()
}

fn loops() -> martite_ir::IrProgram {
    let mut function = FnBuilder::new(0, "main", FnKind::Free, 1)
        .ret(UNIT)
        .slot(INT, false, None);
    let condition = function.node(
        NodeKind::Bool(true),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let break_node = function.node(NodeKind::Break, UNIT, Charge::ZERO, Effects::NONLOCAL_EXIT);
    let while_range = function.children(&[break_node]);
    let while_body = function.node(
        NodeKind::Block(while_range),
        UNIT,
        Charge::ZERO,
        Effects::NONLOCAL_EXIT,
    );
    let while_node = function.node(
        NodeKind::While {
            cond: condition,
            body: while_body,
        },
        UNIT,
        Charge::new(0, 0, 1),
        Effects::NONLOCAL_EXIT,
    );

    let first = function.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let second = function.node(
        NodeKind::Int(ConstId::new(1)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let items = function.children(&[first, second]);
    let iterable = function.node(
        NodeKind::List(items),
        INT,
        Charge::new(1, 2, 0),
        Effects::ALLOCATES,
    );
    let continue_node = function.node(
        NodeKind::Continue,
        UNIT,
        Charge::ZERO,
        Effects::NONLOCAL_EXIT,
    );
    let for_range = function.children(&[continue_node]);
    let for_body = function.node(
        NodeKind::Block(for_range),
        UNIT,
        Charge::ZERO,
        Effects::NONLOCAL_EXIT,
    );
    let iter = function.child(iterable);
    let body_child = function.child(for_body);
    let bind_pattern = function.pattern(Pattern::Bind {
        slot: Slot::new(0),
        ty: INT,
    });
    let for_node = function.node(
        NodeKind::For {
            pattern: bind_pattern,
            first_child: iter,
        },
        UNIT,
        Charge::new(0, 0, 1),
        Effects::NONLOCAL_EXIT,
    );
    debug_assert_eq!(body_child, iter + 1);
    let body_range = function.children(&[while_node, for_node]);
    let body_block = function.node(
        NodeKind::Block(body_range),
        UNIT,
        Charge::ZERO,
        Effects::NONLOCAL_EXIT,
    );

    ProgramBuilder::new("loops.mt")
        .consts(vec![ConstValue::Int(0), ConstValue::Int(1)])
        .function(function.body(body_block))
        .build()
}

fn mut_params() -> martite_ir::IrProgram {
    let mut bump = FnBuilder::new(0, "bump", FnKind::Free, 1)
        .ret(UNIT)
        .param(0, "value", INT, true)
        .slot(INT, true, Some("value"));
    let one = bump.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let write = bump.node(
        NodeKind::SlotWrite {
            slot: Slot::new(0),
            value: one,
        },
        INT,
        Charge::ZERO,
        Effects::WRITES_LOCAL,
    );

    let mut main =
        FnBuilder::new(1, "main", FnKind::Free, 1)
            .ret(UNIT)
            .slot(INT, true, Some("value"));
    let place = main.node(
        NodeKind::SlotPlace(Slot::new(0)),
        INT,
        Charge::ZERO,
        Effects::NONE,
    );
    let argument = main.node(NodeKind::MutArg(place), INT, Charge::ZERO, Effects::NONE);
    let first_arg = main.child(argument);
    let call = main.node(
        NodeKind::CallDirect {
            target: FnId::new(0),
            first_arg,
        },
        INT,
        Charge::expression(),
        Effects::CALLS | Effects::MAY_TRAP,
    );
    let statement = main.node(
        NodeKind::ExprStmt(call),
        UNIT,
        Charge::ZERO,
        Effects::CALLS | Effects::MAY_TRAP,
    );

    ProgramBuilder::new("mut_params.mt")
        .consts(vec![ConstValue::Int(1)])
        .entries(Entries {
            main: Some(FnId::new(1)),
            entry_kind: EntryKind::Script,
            ..Entries::default()
        })
        .function(bump.body(write))
        .function(main.body(statement))
        .build()
}

fn synthetic() -> martite_ir::IrProgram {
    let mut function = FnBuilder::new(0, "main", FnKind::Free, 1)
        .ret(INT)
        .slot(INT, false, None);
    let zero = function.node(
        NodeKind::Int(ConstId::new(0)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let one = function.node(
        NodeKind::Int(ConstId::new(1)),
        INT,
        Charge::expression(),
        Effects::NONE,
    );
    let sum = function.synthetic(
        NodeKind::Add {
            lhs: zero,
            rhs: one,
        },
        INT,
        Charge::expression(),
        Effects::MAY_TRAP,
        zero,
    );
    let carried = function.synthetic(NodeKind::Charge(2), INT, Charge::ZERO, Effects::NONE, sum);
    let body = function.node(
        NodeKind::Return(Some(carried)),
        UNIT,
        Charge::ZERO,
        Effects::NONE,
    );

    ProgramBuilder::new("synthetic.mt")
        .consts(vec![ConstValue::Int(1), ConstValue::Int(2)])
        .function(function.body(body))
        .build()
}
