//! The only mutation path for optimizer passes.
//!
//! Passes describe replacements; this module validates the cost policy, sets
//! provenance, rebuilds the function arena, and preserves the tree property.
//! Effects are recomputed by [`martite_ir::recompute_effects`] immediately
//! after a pass commits.

use std::collections::BTreeMap;

use martite_ir::{
    Charge, ChildRange, Effects, IrFn, IrId, IrProgram, MatchSet, MatchSetId, Node, NodeKind, Span,
    StrPart, StructLayoutId, Ty, TyId, children_of,
};

use crate::error::{RewriteError, RewriteErrorKind};
use crate::pass::{Changed, CostPolicy, RewriteStats};

/// A value-level rewrite for one node.
#[derive(Debug, Clone)]
struct ValueEdit {
    kind: NodeKind,
    ty: TyId,
    /// `None` replaces the node with the leaf directly. `Some(units)` wraps the
    /// leaf in an `ExprBlock` with an explicit `Charge`.
    wrapper_units: Option<u32>,
    safepoint: bool,
}

#[derive(Debug, Clone)]
struct RemoveEdit {
    units: u32,
}

#[derive(Debug, Clone)]
enum Edit {
    Value(ValueEdit),
    Remove(RemoveEdit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Unvisited,
    Building,
    Done,
}

/// Accumulated rewrites for one function.
pub struct Rewriter<'p> {
    program: &'p IrProgram,
    policy: CostPolicy,
    edits: BTreeMap<IrId, Edit>,
    remap: Vec<Option<IrId>>,
    states: Vec<VisitState>,
}

impl<'p> Rewriter<'p> {
    pub fn new(program: &'p IrProgram, policy: CostPolicy) -> Self {
        Self {
            program,
            policy,
            edits: BTreeMap::new(),
            remap: Vec::new(),
            states: Vec::new(),
        }
    }

    pub fn policy(&self) -> CostPolicy {
        self.policy
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// The exact static fuel charge of `node`'s subtree.
    ///
    /// This also enforces the `Preserve` policy: dynamic charges, control
    /// flow, multi-unit charge calls, safepoints below the root, shared heap
    /// reads and observable effects are rejected with a structured error.
    pub fn carry_charge(&self, function: &IrFn, node: IrId) -> Result<u32, RewriteError> {
        self.static_charge(function, node, true)
    }

    /// Replace one value node with a childless scalar leaf, carrying the
    /// removed subtree's charge.
    pub fn replace_value(
        &mut self,
        function: &IrFn,
        node: IrId,
        kind: NodeKind,
        ty: TyId,
    ) -> Result<(), RewriteError> {
        let old = self.node(function, node)?;
        if !is_value_kind(old.kind) {
            return Err(self.error(
                function,
                Some(node),
                RewriteErrorKind::NotAValue { found: old.kind },
            ));
        }
        if !is_scalar_leaf(kind) {
            return Err(self.error(
                function,
                Some(node),
                RewriteErrorKind::NotScalarLeaf { found: kind },
            ));
        }
        if old.ty != ty {
            return Err(self.error(
                function,
                Some(node),
                RewriteErrorKind::TypeMismatch {
                    expected: old.ty,
                    found: ty,
                },
            ));
        }
        let removed = self.static_charge(function, node, true)?;
        if removed == 0 {
            return Err(self.error(
                function,
                Some(node),
                RewriteErrorKind::ChargeOverflow {
                    node,
                    removed,
                    replacement: 1,
                },
            ));
        }
        let wrapper_units = if removed == 1 {
            None
        } else {
            Some(removed - 2)
        };
        self.edits.insert(
            node,
            Edit::Value(ValueEdit {
                kind,
                ty,
                wrapper_units,
                safepoint: function.is_safepoint(node),
            }),
        );
        Ok(())
    }

    /// Remove one statement, replacing it in place with an explicit charge.
    pub fn remove_statement(&mut self, function: &IrFn, node: IrId) -> Result<(), RewriteError> {
        let old = self.node(function, node)?;
        if is_value_kind(old.kind) {
            return Err(self.error(
                function,
                Some(node),
                RewriteErrorKind::NotAStatement { found: old.kind },
            ));
        }
        let units = self.static_charge(function, node, false)?;
        self.edits.insert(node, Edit::Remove(RemoveEdit { units }));
        Ok(())
    }

    /// Apply every recorded edit. Returns `Changed::NO` and leaves the
    /// function untouched when nothing was recorded.
    pub fn finish(mut self, function: &mut IrFn) -> Result<(Changed, RewriteStats), RewriteError> {
        if self.edits.is_empty() {
            return Ok((Changed::NO, RewriteStats::default()));
        }
        let old = function.clone();
        let Some(old_body) = old.body else {
            return Ok((Changed::NO, RewriteStats::default()));
        };
        let unit_ty = self
            .program
            .types
            .tys
            .iter()
            .position(|ty| matches!(ty, Ty::Unit))
            .map(|index| TyId::new(index as u32))
            .ok_or_else(|| self.error(&old, old.body, RewriteErrorKind::MissingUnitType))?;
        self.remap = vec![None; old.nodes.len()];
        self.states = vec![VisitState::Unvisited; old.nodes.len()];

        function.nodes.clear();
        function.spans.clear();
        function.synthetic.clear();
        function.safepoints.clear();
        function.children.clear();
        function.matches.clear();
        function.struct_layouts.clear();
        function.str_parts.clear();
        function.body = None;

        let Some(new_body) = self.build(function, &old, old_body, None, false, unit_ty)? else {
            return Err(self.error(
                &old,
                Some(old_body),
                RewriteErrorKind::ChildRemoval { node: old_body },
            ));
        };
        function.body = Some(new_body);

        let stats = RewriteStats {
            rewrites: self.edits.len(),
            nodes_added: function.nodes.len().saturating_sub(old.nodes.len()),
            nodes_removed: old.nodes.len().saturating_sub(function.nodes.len()),
        };
        Ok((Changed::YES, stats))
    }

    fn build(
        &mut self,
        out: &mut IrFn,
        old: &IrFn,
        id: IrId,
        anchor: Option<IrId>,
        allow_remove: bool,
        unit_ty: TyId,
    ) -> Result<Option<IrId>, RewriteError> {
        let index = id.index();
        let Some(state) = self.states.get(index).copied() else {
            return Err(self.error(old, Some(id), RewriteErrorKind::ChildRemoval { node: id }));
        };
        match state {
            VisitState::Building | VisitState::Done => {
                return Err(self.error(old, Some(id), RewriteErrorKind::TreeReuse { node: id }));
            }
            VisitState::Unvisited => {}
        }
        self.states[index] = VisitState::Building;

        if let Some(edit) = self.edits.remove(&id) {
            let new_id = match edit {
                Edit::Value(edit) => self.build_value_edit(out, old, id, edit, anchor, unit_ty)?,
                Edit::Remove(edit) => {
                    if !allow_remove {
                        return Err(self.error(
                            old,
                            Some(id),
                            RewriteErrorKind::ChildRemoval { node: id },
                        ));
                    }
                    let span = old.span(id).unwrap_or(Span::empty(0));
                    self.alloc(
                        out,
                        NodeKind::Charge(edit.units),
                        unit_ty,
                        Charge::ZERO,
                        span,
                        anchor,
                        false,
                        unit_ty,
                    )
                }
            };
            self.remap[index] = Some(new_id);
            self.states[index] = VisitState::Done;
            return Ok(Some(new_id));
        }

        let node = self.node(old, id)?;
        let span = old.span(id).unwrap_or(Span::empty(0));
        let new_id = self.alloc(
            out,
            NodeKind::Unit,
            unit_ty,
            Charge::ZERO,
            span,
            anchor,
            old.is_safepoint(id),
            unit_ty,
        );
        self.remap[index] = Some(new_id);

        let kind = match node.kind {
            NodeKind::Match {
                match_set,
                scrutinee,
            } => {
                let Some(set) = old.match_set(match_set).cloned() else {
                    return Err(self.error(
                        old,
                        Some(id),
                        RewriteErrorKind::ChildRemoval { node: id },
                    ));
                };
                let scrutinee = self
                    .build(out, old, scrutinee, Some(new_id), false, unit_ty)?
                    .ok_or_else(|| {
                        self.error(
                            old,
                            Some(scrutinee),
                            RewriteErrorKind::ChildRemoval { node: scrutinee },
                        )
                    })?;
                let mut new_set = MatchSet::default();
                for arm in set.arms {
                    let guard = match arm.guard {
                        Some(guard) => Some(
                            self.build(out, old, guard, Some(new_id), false, unit_ty)?
                                .ok_or_else(|| {
                                    self.error(
                                        old,
                                        Some(guard),
                                        RewriteErrorKind::ChildRemoval { node: guard },
                                    )
                                })?,
                        ),
                        None => None,
                    };
                    let body = self
                        .build(out, old, arm.body, Some(new_id), false, unit_ty)?
                        .ok_or_else(|| {
                            self.error(
                                old,
                                Some(arm.body),
                                RewriteErrorKind::ChildRemoval { node: arm.body },
                            )
                        })?;
                    new_set.arms.push(martite_ir::MatchArm {
                        pattern: arm.pattern,
                        guard,
                        body,
                    });
                }
                let match_set = MatchSetId::new(out.matches.len() as u32);
                out.matches.push(new_set);
                NodeKind::Match {
                    match_set,
                    scrutinee,
                }
            }
            NodeKind::StrInterp(range) => {
                let parts = old.str_part_slice(range).to_vec();
                let start = out.str_parts.len() as u32;
                for part in parts {
                    let part = match part {
                        StrPart::Text(id) => StrPart::Text(id),
                        StrPart::Expr(expr) => {
                            let expr = self
                                .build(out, old, expr, Some(new_id), false, unit_ty)?
                                .ok_or_else(|| {
                                    self.error(
                                        old,
                                        Some(expr),
                                        RewriteErrorKind::ChildRemoval { node: expr },
                                    )
                                })?;
                            StrPart::Expr(expr)
                        }
                    };
                    out.str_parts.push(part);
                }
                NodeKind::StrInterp(ChildRange::new(start, out.str_parts.len() as u32 - start))
            }
            kind => {
                let kids = children_of(self.program, old, node);
                let allow_child_removal =
                    matches!(kind, NodeKind::Block(_) | NodeKind::ExprBlock(_));
                let mut new_kids = Vec::with_capacity(kids.len());
                for child in kids {
                    match self.build(out, old, child, Some(new_id), allow_child_removal, unit_ty)? {
                        Some(child) => new_kids.push(child),
                        None if allow_child_removal => {}
                        None => {
                            return Err(self.error(
                                old,
                                Some(child),
                                RewriteErrorKind::ChildRemoval { node: child },
                            ));
                        }
                    }
                }
                self.remap_kind(out, old, kind, new_kids)?
            }
        };

        out.nodes[new_id.index()] = Node::new(kind, node.ty, node.charge, node.effects);
        out.synthetic[new_id.index()] = old
            .synthetic_origin(id)
            .and_then(|origin| {
                self.remap
                    .get(origin.index())
                    .copied()
                    .flatten()
                    .filter(|mapped| mapped.0 != new_id.0)
            })
            .or_else(|| anchor.filter(|anchor| anchor.0 != new_id.0));
        self.states[index] = VisitState::Done;
        Ok(Some(new_id))
    }

    #[allow(clippy::too_many_arguments)]
    fn alloc(
        &self,
        out: &mut IrFn,
        kind: NodeKind,
        ty: TyId,
        charge: Charge,
        span: Span,
        synthetic: Option<IrId>,
        safepoint: bool,
        _unit_ty: TyId,
    ) -> IrId {
        let id = IrId::new(out.nodes.len() as u32);
        out.nodes.push(Node::new(kind, ty, charge, Effects::NONE));
        out.spans.push(span);
        out.synthetic.push(synthetic);
        out.safepoints.push(safepoint);
        id
    }

    fn build_value_edit(
        &self,
        out: &mut IrFn,
        old: &IrFn,
        id: IrId,
        edit: ValueEdit,
        anchor: Option<IrId>,
        unit_ty: TyId,
    ) -> Result<IrId, RewriteError> {
        let span = old.span(id).unwrap_or(Span::empty(0));
        if edit.wrapper_units.is_none() {
            return Ok(self.alloc(
                out,
                edit.kind,
                edit.ty,
                Charge::expression(),
                span,
                anchor,
                edit.safepoint,
                unit_ty,
            ));
        }
        let root = self.alloc(
            out,
            NodeKind::Unit,
            edit.ty,
            Charge::expression(),
            span,
            anchor,
            edit.safepoint,
            unit_ty,
        );
        let child_anchor = Some(root);
        let leaf = self.alloc(
            out,
            edit.kind,
            edit.ty,
            Charge::expression(),
            span,
            child_anchor,
            false,
            unit_ty,
        );
        let units = edit.wrapper_units.unwrap_or_default();
        let charge = self.alloc(
            out,
            NodeKind::Charge(units),
            unit_ty,
            Charge::ZERO,
            span,
            child_anchor,
            false,
            unit_ty,
        );
        let statement = self.alloc(
            out,
            NodeKind::ExprStmt(leaf),
            unit_ty,
            Charge::ZERO,
            span,
            child_anchor,
            false,
            unit_ty,
        );
        let start = out.children.len() as u32;
        out.children.push(charge);
        out.children.push(statement);
        out.nodes[root.index()] = Node::new(
            NodeKind::ExprBlock(ChildRange::new(start, 2)),
            edit.ty,
            Charge::expression(),
            Effects::NONE,
        );
        Ok(root)
    }

    fn remap_kind(
        &self,
        out: &mut IrFn,
        old: &IrFn,
        kind: NodeKind,
        kids: Vec<IrId>,
    ) -> Result<NodeKind, RewriteError> {
        let append = |out: &mut IrFn, kids: Vec<IrId>| -> ChildRange {
            let start = out.children.len() as u32;
            let len = kids.len() as u32;
            out.children.extend(kids);
            ChildRange::new(start, len)
        };
        Ok(match kind {
            NodeKind::Unit
            | NodeKind::Int(_)
            | NodeKind::Float(_)
            | NodeKind::Bool(_)
            | NodeKind::Str(_)
            | NodeKind::ConstRef(_)
            | NodeKind::FnRef(_)
            | NodeKind::BuiltinFnRef(_)
            | NodeKind::BuiltinModuleRef(_)
            | NodeKind::SlotRead { .. }
            | NodeKind::CaptureRead(_)
            | NodeKind::SlotPlace(_)
            | NodeKind::EnumValue { .. }
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::MakeClosure(_)
            | NodeKind::Charge(_) => kind,
            NodeKind::Field { base: _, index } => NodeKind::Field {
                base: kids[0],
                index,
            },
            NodeKind::TupleField { base: _, index } => NodeKind::TupleField {
                base: kids[0],
                index,
            },
            NodeKind::Index { .. } => NodeKind::Index {
                base: kids[0],
                index: kids[1],
            },
            NodeKind::Neg(_) => NodeKind::Neg(kids[0]),
            NodeKind::Not(_) => NodeKind::Not(kids[0]),
            NodeKind::Add { .. } => NodeKind::Add {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Sub { .. } => NodeKind::Sub {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Mul { .. } => NodeKind::Mul {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Div { .. } => NodeKind::Div {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Rem { .. } => NodeKind::Rem {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Eq { .. } => NodeKind::Eq {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Ne { .. } => NodeKind::Ne {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Lt { .. } => NodeKind::Lt {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Le { .. } => NodeKind::Le {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Gt { .. } => NodeKind::Gt {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Ge { .. } => NodeKind::Ge {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::And { .. } => NodeKind::And {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Or { .. } => NodeKind::Or {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::Coalesce { .. } => NodeKind::Coalesce {
                lhs: kids[0],
                rhs: kids[1],
            },
            NodeKind::CallDirect { target, .. } => NodeKind::CallDirect {
                target,
                first_arg: append(out, kids).start,
            },
            NodeKind::CallMethod { target, .. } => NodeKind::CallMethod {
                target,
                first_child: append(out, kids).start,
            },
            NodeKind::CallValue { .. } => {
                let argc = kids.len().saturating_sub(1) as u32;
                NodeKind::CallValue {
                    first_child: append(out, kids).start,
                    argc,
                }
            }
            NodeKind::CallBuiltinFn { builtin, .. } => NodeKind::CallBuiltinFn {
                builtin,
                first_arg: append(out, kids).start,
            },
            NodeKind::CallBuiltinMethod { method, .. } => NodeKind::CallBuiltinMethod {
                method,
                first_child: append(out, kids).start,
            },
            NodeKind::CallBuiltinModule { item, .. } => NodeKind::CallBuiltinModule {
                item,
                first_arg: append(out, kids).start,
            },
            NodeKind::CallHost { host, .. } => NodeKind::CallHost {
                host,
                first_arg: append(out, kids).start,
            },
            NodeKind::Struct { type_id, layout } => {
                let mut layout = old.struct_layout(layout).cloned().unwrap_or_default();
                let range = append(out, kids);
                layout.first_child = range.start;
                let layout_id = StructLayoutId::new(out.struct_layouts.len() as u32);
                out.struct_layouts.push(layout);
                NodeKind::Struct {
                    type_id,
                    layout: layout_id,
                }
            }
            NodeKind::StructUpdate { type_id, layout } => {
                let mut layout = old.struct_layout(layout).cloned().unwrap_or_default();
                let range = append(out, kids);
                layout.first_child = range.start;
                let layout_id = StructLayoutId::new(out.struct_layouts.len() as u32);
                out.struct_layouts.push(layout);
                NodeKind::StructUpdate {
                    type_id,
                    layout: layout_id,
                }
            }
            NodeKind::EnumVariant { variant, .. } => NodeKind::EnumVariant {
                variant,
                first_payload: append(out, kids).start,
            },
            NodeKind::PreludeVariant { variant, .. } => NodeKind::PreludeVariant {
                variant,
                first_payload: append(out, kids).start,
            },
            NodeKind::Tuple(_) => NodeKind::Tuple(append(out, kids)),
            NodeKind::List(_) => NodeKind::List(append(out, kids)),
            NodeKind::Map(_) => NodeKind::Map(append(out, kids)),
            NodeKind::Range { .. } => NodeKind::Range {
                start: kids[0],
                end: kids[1],
            },
            NodeKind::InclusiveRange { .. } => NodeKind::InclusiveRange {
                start: kids[0],
                end: kids[1],
            },
            NodeKind::Block(_) => NodeKind::Block(append(out, kids)),
            NodeKind::ExprBlock(_) => NodeKind::ExprBlock(append(out, kids)),
            NodeKind::If(_) => NodeKind::If(append(out, kids)),
            NodeKind::While { .. } => NodeKind::While {
                cond: kids[0],
                body: kids[1],
            },
            NodeKind::For { pattern, .. } => NodeKind::For {
                pattern,
                first_child: append(out, kids).start,
            },
            NodeKind::Return(_) => NodeKind::Return(kids.first().copied()),
            NodeKind::Try(_) => NodeKind::Try(kids[0]),
            NodeKind::Let { pattern, .. } => NodeKind::Let {
                pattern,
                value: kids[0],
            },
            NodeKind::Assign { .. } => NodeKind::Assign {
                place: kids[0],
                value: kids[1],
            },
            NodeKind::AddAssign { .. } => NodeKind::AddAssign {
                place: kids[0],
                value: kids[1],
            },
            NodeKind::SubAssign { .. } => NodeKind::SubAssign {
                place: kids[0],
                value: kids[1],
            },
            NodeKind::MulAssign { .. } => NodeKind::MulAssign {
                place: kids[0],
                value: kids[1],
            },
            NodeKind::DivAssign { .. } => NodeKind::DivAssign {
                place: kids[0],
                value: kids[1],
            },
            NodeKind::RemAssign { .. } => NodeKind::RemAssign {
                place: kids[0],
                value: kids[1],
            },
            NodeKind::ExprStmt(_) => NodeKind::ExprStmt(kids[0]),
            NodeKind::ConvertInt(_) => NodeKind::ConvertInt(kids[0]),
            NodeKind::ConvertFloat(_) => NodeKind::ConvertFloat(kids[0]),
            NodeKind::ConvertStr(_) => NodeKind::ConvertStr(kids[0]),
            NodeKind::Assert(_) => NodeKind::Assert(append(out, kids)),
            NodeKind::Panic(_) => NodeKind::Panic(append(out, kids)),
            NodeKind::MutArg(_) => NodeKind::MutArg(kids[0]),
            NodeKind::SlotWrite { slot, .. } => NodeKind::SlotWrite {
                slot,
                value: kids[0],
            },
            NodeKind::SlotInit { slot, .. } => NodeKind::SlotInit {
                slot,
                value: kids[0],
            },
            NodeKind::Match { .. } | NodeKind::StrInterp(_) => {
                unreachable!("match and string interpolation are rebuilt explicitly")
            }
        })
    }

    fn static_charge(&self, function: &IrFn, id: IrId, is_root: bool) -> Result<u32, RewriteError> {
        let node = self.node(function, id)?;
        self.validate_removed(function, id, node, is_root)?;
        let mut total = node.charge.pre.saturating_add(node.charge.post);
        if let NodeKind::Charge(units) = node.kind {
            total = total.saturating_add(units);
        }
        for child in children_of(self.program, function, node) {
            total = total.saturating_add(self.static_charge(function, child, false)?);
        }
        Ok(total)
    }

    fn validate_removed(
        &self,
        function: &IrFn,
        id: IrId,
        node: Node,
        is_root: bool,
    ) -> Result<(), RewriteError> {
        if node.effects.contains(Effects::SHARES) {
            return Err(self.error(function, Some(id), RewriteErrorKind::Shares { node: id }));
        }
        if node.effects.contains(Effects::SAFEPOINT) && !(is_root && function.is_safepoint(id)) {
            return Err(self.error(function, Some(id), RewriteErrorKind::Safepoint { node: id }));
        }
        if function.is_safepoint(id) && !is_root {
            return Err(self.error(function, Some(id), RewriteErrorKind::Safepoint { node: id }));
        }
        if node.effects.contains(Effects::DYN_COST) {
            return Err(self.error(
                function,
                Some(id),
                RewriteErrorKind::DynamicCharge { node: id },
            ));
        }
        let unsafe_effects = Effects::READS_LOCAL
            | Effects::WRITES_LOCAL
            | Effects::READS_HEAP
            | Effects::WRITES_HEAP
            | Effects::ALLOCATES
            | Effects::CALLS
            | Effects::HOST
            | Effects::LOGS
            | Effects::NONLOCAL_EXIT;
        if node.effects.intersects(unsafe_effects) {
            return Err(self.error(
                function,
                Some(id),
                RewriteErrorKind::UnsafeEffects {
                    node: id,
                    effects: Effects::from_bits_truncate(
                        node.effects.bits() & unsafe_effects.bits(),
                    ),
                },
            ));
        }
        if node.charge.per_iter != 0
            || matches!(
                node.kind,
                NodeKind::If(_)
                    | NodeKind::While { .. }
                    | NodeKind::For { .. }
                    | NodeKind::Match { .. }
                    | NodeKind::Try(_)
                    | NodeKind::And { .. }
                    | NodeKind::Or { .. }
                    | NodeKind::Coalesce { .. }
                    | NodeKind::Block(_)
            )
            || (matches!(node.kind, NodeKind::ExprBlock(_))
                && !self.is_carrier_wrapper(function, id, node))
        {
            return Err(self.error(
                function,
                Some(id),
                RewriteErrorKind::ControlFlowCharge { node: id },
            ));
        }
        if node.charge.pre > 1 || node.charge.post > 1 {
            return Err(self.error(
                function,
                Some(id),
                RewriteErrorKind::MultipleChargeUnits {
                    node: id,
                    charge: node.charge,
                },
            ));
        }
        Ok(())
    }

    fn is_carrier_wrapper(&self, function: &IrFn, id: IrId, node: Node) -> bool {
        let NodeKind::ExprBlock(range) = node.kind else {
            return false;
        };
        let children = function.child_slice(range);
        children.len() == 2
            && matches!(
                function.node(children[0]).map(|node| node.kind),
                Some(NodeKind::Charge(_))
            )
            && matches!(
                function.node(children[1]).map(|node| node.kind),
                Some(NodeKind::ExprStmt(_))
            )
            && id.index() < function.nodes.len()
    }

    fn node(&self, function: &IrFn, id: IrId) -> Result<Node, RewriteError> {
        function.node(id).copied().ok_or_else(|| {
            self.error(
                function,
                Some(id),
                RewriteErrorKind::ChildRemoval { node: id },
            )
        })
    }

    fn error(&self, function: &IrFn, node: Option<IrId>, kind: RewriteErrorKind) -> RewriteError {
        RewriteError::new(
            function.id,
            Some(function.source),
            node.and_then(|node| function.span(node)),
            node,
            kind,
        )
    }
}

fn is_scalar_leaf(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Unit | NodeKind::Int(_) | NodeKind::Float(_) | NodeKind::Bool(_)
    )
}

fn is_value_kind(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::Block(_)
            | NodeKind::ExprStmt(_)
            | NodeKind::Let { .. }
            | NodeKind::Assign { .. }
            | NodeKind::AddAssign { .. }
            | NodeKind::SubAssign { .. }
            | NodeKind::MulAssign { .. }
            | NodeKind::DivAssign { .. }
            | NodeKind::RemAssign { .. }
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::Return(_)
            | NodeKind::SlotWrite { .. }
            | NodeKind::SlotInit { .. }
            | NodeKind::Charge(_)
            | NodeKind::MutArg(_)
    )
}
