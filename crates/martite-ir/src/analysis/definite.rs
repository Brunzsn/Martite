//! Definite assignment: every slot read is dominated by a write.
//!
//! The IR is a structured tree, so this is a forward must-analysis: joins
//! intersect, a loop header is seeded from the pre-state (body writes never
//! become definite before the first iteration), and a `Take` read unassigns its
//! slot. Unreachable code is vacuously assigned, matching the usual dominance
//! definition: no path from entry reaches the read.

use crate::analysis::slots::{SlotSet, pattern_binding_slots};
use crate::id::{IrId, MatchSetId, PatternId, Slot};
use crate::node::{AccessMode, Node, NodeKind};
use crate::pattern::Pattern;
use crate::program::{IrFn, IrProgram};
use crate::verify::children_of;

/// One slot read that is not definitely assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnboundRead {
    pub node: IrId,
    pub slot: Slot,
    pub context: &'static str,
}

/// Per-node definite-assignment facts for one function.
///
/// `None` means "not reachable"; a node can be reachable but not fall through
/// (for example `return`), in which case `assigned_after` is `None`.
#[derive(Debug, Clone)]
pub struct DefiniteAssignment {
    before: Vec<Option<SlotSet>>,
    after: Vec<Option<SlotSet>>,
}

impl DefiniteAssignment {
    /// Compute the analysis for `function`.
    ///
    /// The function must already have passed structural, type and tree
    /// verification. The first unbound read in evaluation order is returned.
    pub fn compute(program: &IrProgram, function: &IrFn) -> Result<Self, UnboundRead> {
        let mut analyzer = Analyzer {
            program,
            function,
            before: vec![None; function.nodes.len()],
            after: vec![None; function.nodes.len()],
            loops: Vec::new(),
        };
        let result = analyzer.run();
        let analysis = Self {
            before: analyzer.before,
            after: analyzer.after,
        };
        result.map(|()| analysis)
    }

    /// Whether control flow reaches `node`.
    pub fn reachable(&self, node: IrId) -> bool {
        self.before.get(node.index()).is_some_and(Option::is_some)
    }

    /// Slots definitely assigned on entry to `node`, or `None` when the node is
    /// not reachable.
    pub fn assigned_before(&self, node: IrId) -> Option<&SlotSet> {
        self.before.get(node.index())?.as_ref()
    }

    /// Slots definitely assigned when `node` completes, or `None` when the node
    /// does not fall through or is not reachable.
    pub fn assigned_after(&self, node: IrId) -> Option<&SlotSet> {
        self.after.get(node.index())?.as_ref()
    }

    /// Convenience query for one slot at one node.
    pub fn is_assigned_before(&self, node: IrId, slot: Slot) -> bool {
        self.assigned_before(node)
            .is_some_and(|assigned| assigned.contains(slot))
    }
}

struct Analyzer<'a> {
    program: &'a IrProgram,
    function: &'a IrFn,
    before: Vec<Option<SlotSet>>,
    after: Vec<Option<SlotSet>>,
    loops: Vec<LoopCtx>,
}

#[derive(Default)]
struct LoopCtx {
    breaks: Option<SlotSet>,
    continues: Option<SlotSet>,
}

impl<'a> Analyzer<'a> {
    fn run(&mut self) -> Result<(), UnboundRead> {
        let Some(body) = self.function.body else {
            return Ok(());
        };
        let entry = self.entry_state();
        self.analyze(body, &entry)?;
        Ok(())
    }

    fn entry_state(&self) -> SlotSet {
        let mut assigned = SlotSet::new(self.function.locals);
        for param in &self.function.params {
            assigned.insert(param.slot);
            if let Some(pattern) = param.pattern
                && let Some(pattern) = self.function.pattern(pattern)
            {
                let mut slots = Vec::new();
                pattern_binding_slots(pattern, &mut slots);
                for slot in slots {
                    assigned.insert(slot);
                }
            }
        }
        assigned
    }

    fn node(&self, id: IrId) -> Node {
        *self
            .function
            .node(id)
            .expect("definite assignment runs after tree verification")
    }

    fn require(
        &self,
        node: IrId,
        slot: Slot,
        state: &SlotSet,
        context: &'static str,
    ) -> Result<(), UnboundRead> {
        if state.contains(slot) {
            Ok(())
        } else {
            Err(UnboundRead {
                node,
                slot,
                context,
            })
        }
    }

    fn analyze(&mut self, id: IrId, state: &SlotSet) -> Result<Option<SlotSet>, UnboundRead> {
        let node = self.node(id);
        self.before[id.index()] = Some(state.clone());

        let after = match node.kind {
            NodeKind::Block(_) | NodeKind::ExprBlock(_) => self.analyze_children(id, state)?,
            NodeKind::If(_) => self.analyze_if(id, state)?,
            NodeKind::While { cond, body } => self.analyze_while(cond, body, state)?,
            NodeKind::For {
                pattern,
                first_child,
            } => {
                let children = children_of(self.program, self.function, node);
                let iterable = children
                    .first()
                    .copied()
                    .or_else(|| self.function.children.get(first_child as usize).copied());
                let body = children.get(1).copied();
                match (iterable, body) {
                    (Some(iterable), Some(body)) => {
                        self.analyze_for(pattern, iterable, body, state)?
                    }
                    _ => None,
                }
            }
            NodeKind::Match {
                match_set,
                scrutinee,
            } => self.analyze_match(match_set, scrutinee, state)?,
            NodeKind::And { lhs, rhs }
            | NodeKind::Or { lhs, rhs }
            | NodeKind::Coalesce { lhs, rhs } => self.analyze_short_circuit(lhs, rhs, state)?,
            NodeKind::Try(inner) => self.analyze(inner, state)?,
            NodeKind::Return(value) => {
                if let Some(value) = value {
                    self.analyze(value, state)?;
                }
                None
            }
            NodeKind::Break => {
                if let Some(context) = self.loops.last_mut() {
                    join_into(&mut context.breaks, state);
                }
                None
            }
            NodeKind::Continue => {
                if let Some(context) = self.loops.last_mut() {
                    join_into(&mut context.continues, state);
                }
                None
            }
            NodeKind::SlotRead { slot, mode } => {
                self.require(id, slot, state, "slot read")?;
                let mut out = state.clone();
                if mode == AccessMode::Take {
                    out.remove(slot);
                }
                Some(out)
            }
            NodeKind::SlotPlace(slot) => {
                self.require(id, slot, state, "place read")?;
                Some(state.clone())
            }
            NodeKind::SlotWrite { slot, value } | NodeKind::SlotInit { slot, value } => {
                match self.analyze(value, state)? {
                    Some(mut out) => {
                        out.insert(slot);
                        Some(out)
                    }
                    None => None,
                }
            }
            NodeKind::Let { pattern, value } => match self.analyze(value, state)? {
                Some(mut out) => {
                    if let Some(pattern) = self.function.pattern(pattern) {
                        let mut slots = Vec::new();
                        pattern_binding_slots(pattern, &mut slots);
                        for slot in slots {
                            out.insert(slot);
                        }
                    }
                    Some(out)
                }
                None => None,
            },
            NodeKind::Assign { place, value } => self.analyze_assign(place, value, state)?,
            NodeKind::AddAssign { place, value }
            | NodeKind::SubAssign { place, value }
            | NodeKind::MulAssign { place, value }
            | NodeKind::DivAssign { place, value }
            | NodeKind::RemAssign { place, value } => {
                let place_out = self.analyze_place(place, state, true)?;
                let Some(place_out) = place_out else {
                    self.after[id.index()] = None;
                    return Ok(None);
                };
                match self.analyze(value, &place_out)? {
                    Some(mut out) => {
                        if let Some(root) = self.place_root_slot(place) {
                            out.insert(root);
                        }
                        Some(out)
                    }
                    None => None,
                }
            }
            NodeKind::ExprStmt(inner) => self.analyze(inner, state)?,
            NodeKind::MutArg(place) => self.analyze_place(place, state, true)?,
            NodeKind::MakeClosure(target) => {
                if let Some(target) = self.program.function(target) {
                    for capture in &target.captures {
                        self.require(id, capture.outer, state, "capture read")?;
                    }
                }
                Some(state.clone())
            }
            NodeKind::Field { .. } | NodeKind::TupleField { .. } | NodeKind::Index { .. } => {
                self.analyze_children(id, state)?
            }
            _ => self.analyze_children(id, state)?,
        };

        self.after[id.index()] = after.clone();
        Ok(after)
    }

    fn analyze_children(
        &mut self,
        id: IrId,
        state: &SlotSet,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let node = self.node(id);
        let mut current = Some(state.clone());
        for child in children_of(self.program, self.function, node) {
            match current {
                Some(state) => current = self.analyze(child, &state)?,
                None => break,
            }
        }
        Ok(current)
    }

    fn analyze_if(&mut self, id: IrId, state: &SlotSet) -> Result<Option<SlotSet>, UnboundRead> {
        let node = self.node(id);
        let children = children_of(self.program, self.function, node);
        let (Some(cond), Some(then_branch)) = (children.first().copied(), children.get(1).copied())
        else {
            return Ok(None);
        };
        let else_branch = children.get(2).copied();

        let Some(cond_out) = self.analyze(cond, state)? else {
            return Ok(None);
        };
        let then_out = self.analyze(then_branch, &cond_out)?;
        let else_out = match else_branch {
            Some(else_branch) => self.analyze(else_branch, &cond_out)?,
            None => Some(cond_out),
        };
        Ok(intersect_options(then_out, else_out))
    }

    fn analyze_short_circuit(
        &mut self,
        lhs: IrId,
        rhs: IrId,
        state: &SlotSet,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let Some(lhs_out) = self.analyze(lhs, state)? else {
            return Ok(None);
        };
        let rhs_out = self.analyze(rhs, &lhs_out)?;
        Ok(intersect_options(Some(lhs_out), rhs_out))
    }

    fn analyze_while(
        &mut self,
        cond: IrId,
        body: IrId,
        state: &SlotSet,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let pre = state.clone();
        let mut header = pre.clone();

        let (final_cond, final_breaks) = loop {
            let cond_out = self.analyze(cond, &header)?;
            self.loops.push(LoopCtx::default());
            let body_out = match &cond_out {
                Some(cond_out) => self.analyze(body, cond_out)?,
                None => None,
            };
            let context = self.loops.pop().expect("loop context");
            let back = intersect_options(body_out, context.continues);
            let next = match &back {
                Some(back) => pre.intersection(back),
                None => pre.clone(),
            };
            if next == header {
                break (cond_out, context.breaks);
            }
            header = next;
        };

        Ok(intersect_options(final_cond, final_breaks))
    }

    fn analyze_for(
        &mut self,
        pattern: PatternId,
        iterable: IrId,
        body: IrId,
        state: &SlotSet,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let Some(iter_out) = self.analyze(iterable, state)? else {
            return Ok(None);
        };
        let mut bindings = Vec::new();
        if let Some(pattern) = self.function.pattern(pattern) {
            pattern_binding_slots(pattern, &mut bindings);
        }

        let mut header = iter_out.clone();
        let final_breaks = loop {
            let mut body_entry = header.clone();
            for slot in &bindings {
                body_entry.insert(*slot);
            }
            self.loops.push(LoopCtx::default());
            let body_out = self.analyze(body, &body_entry)?;
            let context = self.loops.pop().expect("loop context");
            let back = intersect_options(body_out, context.continues);
            let next = match &back {
                Some(back) => iter_out.intersection(back),
                None => iter_out.clone(),
            };
            if next == header {
                break context.breaks;
            }
            header = next;
        };

        Ok(intersect_options(Some(iter_out), final_breaks))
    }

    fn analyze_match(
        &mut self,
        match_set: MatchSetId,
        scrutinee: IrId,
        state: &SlotSet,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let Some(scrutinee_out) = self.analyze(scrutinee, state)? else {
            return Ok(None);
        };
        let arms = self
            .function
            .match_set(match_set)
            .map(|set| set.arms.clone())
            .unwrap_or_default();

        let mut arm_in = scrutinee_out;
        let mut exits = None;
        let mut stopped = false;

        for arm in &arms {
            if stopped {
                continue;
            }
            let mut bindings = Vec::new();
            if let Some(pattern) = self.function.pattern(arm.pattern) {
                pattern_binding_slots(pattern, &mut bindings);
            }
            let mut entry = arm_in.clone();
            for slot in &bindings {
                entry.insert(*slot);
            }
            let irrefutable = self.pattern_is_irrefutable(arm.pattern);

            if let Some(guard) = arm.guard {
                let Some(guard_out) = self.analyze(guard, &entry)? else {
                    if irrefutable {
                        stopped = true;
                    }
                    continue;
                };
                let body_out = self.analyze(arm.body, &guard_out)?;
                exits = intersect_options(exits, body_out);

                let mut failed = guard_out;
                for slot in &bindings {
                    failed.remove(*slot);
                }
                arm_in = if irrefutable {
                    failed
                } else {
                    arm_in.intersection(&failed)
                };
            } else {
                let body_out = self.analyze(arm.body, &entry)?;
                exits = intersect_options(exits, body_out);
                if irrefutable {
                    stopped = true;
                }
            }
        }

        if !stopped {
            exits = intersect_options(exits, Some(arm_in));
        }
        Ok(exits)
    }

    fn pattern_is_irrefutable(&self, pattern: PatternId) -> bool {
        self.function
            .pattern(pattern)
            .is_some_and(pattern_is_irrefutable)
    }

    fn analyze_assign(
        &mut self,
        place: IrId,
        value: IrId,
        state: &SlotSet,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let place_node = self.node(place);
        let bare = matches!(place_node.kind, NodeKind::SlotPlace(_));
        let Some(place_out) = self.analyze_place(place, state, !bare)? else {
            return Ok(None);
        };
        let Some(mut out) = self.analyze(value, &place_out)? else {
            return Ok(None);
        };
        if let Some(root) = self.place_root_slot(place) {
            out.insert(root);
        }
        Ok(Some(out))
    }

    fn place_root_slot(&self, id: IrId) -> Option<Slot> {
        match self.node(id).kind {
            NodeKind::SlotPlace(slot) => Some(slot),
            NodeKind::Field { base, .. } | NodeKind::TupleField { base, .. } => {
                self.place_root_slot(base)
            }
            NodeKind::Index { base, .. } => self.place_root_slot(base),
            _ => None,
        }
    }

    fn analyze_place(
        &mut self,
        id: IrId,
        state: &SlotSet,
        read: bool,
    ) -> Result<Option<SlotSet>, UnboundRead> {
        let node = self.node(id);
        self.before[id.index()] = Some(state.clone());

        let after = match node.kind {
            NodeKind::SlotPlace(slot) => {
                if read {
                    self.require(id, slot, state, "place read")?;
                }
                Some(state.clone())
            }
            NodeKind::Field { base, .. } | NodeKind::TupleField { base, .. } => {
                self.analyze_place(base, state, true)?
            }
            NodeKind::Index { base, index } => match self.analyze_place(base, state, true)? {
                Some(base_out) => self.analyze(index, &base_out)?,
                None => None,
            },
            _ => self.analyze(id, state)?,
        };

        self.after[id.index()] = after.clone();
        Ok(after)
    }
}

fn pattern_is_irrefutable(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Wild | Pattern::Bind { .. } => true,
        Pattern::Tuple(items) => items.iter().all(pattern_is_irrefutable),
        Pattern::Struct { fields, .. } => fields
            .iter()
            .all(|(_, pattern)| pattern_is_irrefutable(pattern)),
        Pattern::Or(items) => items.iter().any(pattern_is_irrefutable),
        Pattern::Int(_)
        | Pattern::Bool(_)
        | Pattern::Str(_)
        | Pattern::Variant { .. }
        | Pattern::Prelude { .. } => false,
    }
}

fn join_into(target: &mut Option<SlotSet>, state: &SlotSet) {
    *target = intersect_options(target.take(), Some(state.clone()));
}

/// Intersect two optional states. `None` means "no reachable path"; the join of
/// one reachable path and no path is that path.
fn intersect_options(left: Option<SlotSet>, right: Option<SlotSet>) -> Option<SlotSet> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.intersection(&right)),
        (Some(state), None) | (None, Some(state)) => Some(state),
        (None, None) => None,
    }
}
