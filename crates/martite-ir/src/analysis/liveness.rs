//! Slot use-liveness for passes.
//!
//! This is a backward may-analysis: joins union, loop headers are a least
//! fixed point. It answers "is this slot read again before being written?"
//! Whole-slot writes kill; field and index writes read the root without
//! killing it. `take` is a read.
//!
//! This is not the memory census: under rule M2 every slot in a live frame is
//! counted, whether or not it is use-live.

use crate::analysis::slots::{SlotSet, pattern_binding_slots};
use crate::id::{IrId, MatchSetId, PatternId, Slot};
use crate::node::{Node, NodeKind};
use crate::program::{IrFn, IrProgram};
use crate::verify::children_of;

/// Per-node liveness facts for one function.
#[derive(Debug, Clone)]
pub struct Liveness {
    before: Vec<SlotSet>,
    after: Vec<SlotSet>,
}

impl Liveness {
    /// Compute liveness for `function`.
    ///
    /// The function must already have passed structural, type and tree
    /// verification.
    pub fn compute(program: &IrProgram, function: &IrFn) -> Self {
        let mut analyzer = Analyzer {
            program,
            function,
            before: vec![SlotSet::new(function.locals); function.nodes.len()],
            after: vec![SlotSet::new(function.locals); function.nodes.len()],
            loops: Vec::new(),
        };
        analyzer.run();
        Self {
            before: analyzer.before,
            after: analyzer.after,
        }
    }

    pub fn live_before(&self, node: IrId) -> &SlotSet {
        &self.before[node.index()]
    }

    pub fn live_after(&self, node: IrId) -> &SlotSet {
        &self.after[node.index()]
    }

    pub fn is_live_after(&self, node: IrId, slot: Slot) -> bool {
        self.after[node.index()].contains(slot)
    }
}

struct Analyzer<'a> {
    program: &'a IrProgram,
    function: &'a IrFn,
    before: Vec<SlotSet>,
    after: Vec<SlotSet>,
    loops: Vec<LiveLoop>,
}

struct LiveLoop {
    exit: SlotSet,
    header: SlotSet,
}

impl<'a> Analyzer<'a> {
    fn run(&mut self) {
        let Some(body) = self.function.body else {
            return;
        };
        let end = SlotSet::new(self.function.locals);
        self.live(body, &end);
    }

    fn node(&self, id: IrId) -> Node {
        *self
            .function
            .node(id)
            .expect("liveness runs after tree verification")
    }

    /// Compute liveness before `id` given the live set after it. Returns the
    /// live-before set and stores both states on the node.
    fn live(&mut self, id: IrId, after: &SlotSet) -> SlotSet {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || self.live_inner(id, after))
    }

    fn live_inner(&mut self, id: IrId, after: &SlotSet) -> SlotSet {
        let node = self.node(id);
        let (before, effective_after) = match node.kind {
            NodeKind::Block(_) | NodeKind::ExprBlock(_) => {
                (self.live_children(id, after), after.clone())
            }
            NodeKind::If(_) => (self.live_if(id, after), after.clone()),
            NodeKind::While { cond, body } => (self.live_while(cond, body, after), after.clone()),
            NodeKind::For {
                pattern,
                first_child,
            } => {
                let node_children = children_of(self.program, self.function, node);
                let iterable = node_children
                    .first()
                    .copied()
                    .or_else(|| self.function.children.get(first_child as usize).copied());
                let body = node_children.get(1).copied();
                match (iterable, body) {
                    (Some(iterable), Some(body)) => {
                        (self.live_for(pattern, iterable, body, after), after.clone())
                    }
                    _ => (after.clone(), after.clone()),
                }
            }
            NodeKind::Match {
                match_set,
                scrutinee,
            } => (self.live_match(match_set, scrutinee, after), after.clone()),
            NodeKind::And { lhs, rhs }
            | NodeKind::Or { lhs, rhs }
            | NodeKind::Coalesce { lhs, rhs } => {
                let rhs_live = self.live(rhs, after);
                let lhs_after = rhs_live.union(after);
                (self.live(lhs, &lhs_after), after.clone())
            }
            NodeKind::Try(inner) => (self.live(inner, after), after.clone()),
            NodeKind::Return(value) => {
                let empty = SlotSet::new(self.function.locals);
                let before = match value {
                    Some(value) => self.live(value, &empty),
                    None => empty.clone(),
                };
                (before, empty)
            }
            NodeKind::Break => {
                let target = self
                    .loops
                    .last()
                    .map(|context| context.exit.clone())
                    .unwrap_or_else(|| SlotSet::new(self.function.locals));
                (target.clone(), target)
            }
            NodeKind::Continue => {
                let target = self
                    .loops
                    .last()
                    .map(|context| context.header.clone())
                    .unwrap_or_else(|| SlotSet::new(self.function.locals));
                (target.clone(), target)
            }
            NodeKind::SlotRead { slot, .. } => {
                let mut before = after.clone();
                before.insert(slot);
                (before, after.clone())
            }
            NodeKind::SlotPlace(slot) => {
                let mut before = after.clone();
                before.insert(slot);
                (before, after.clone())
            }
            NodeKind::SlotWrite { slot, value } | NodeKind::SlotInit { slot, value } => {
                let mut value_after = after.clone();
                value_after.remove(slot);
                (self.live(value, &value_after), after.clone())
            }
            NodeKind::Let { pattern, value } => {
                let mut value_after = after.clone();
                if let Some(pattern) = self.function.pattern(pattern) {
                    let mut slots = Vec::new();
                    pattern_binding_slots(pattern, &mut slots);
                    for slot in slots {
                        value_after.remove(slot);
                    }
                }
                (self.live(value, &value_after), after.clone())
            }
            NodeKind::Assign { place, value } => {
                (self.live_assign(place, value, after), after.clone())
            }
            NodeKind::AddAssign { place, value }
            | NodeKind::SubAssign { place, value }
            | NodeKind::MulAssign { place, value }
            | NodeKind::DivAssign { place, value }
            | NodeKind::RemAssign { place, value } => {
                (self.live_compound(place, value, after), after.clone())
            }
            NodeKind::ExprStmt(inner) => (self.live(inner, after), after.clone()),
            NodeKind::MutArg(place) => (self.live_place(place, after, true), after.clone()),
            NodeKind::MakeClosure(target) => {
                let mut before = after.clone();
                if let Some(target) = self.program.function(target) {
                    for capture in &target.captures {
                        if let crate::program::CaptureSource::Slot(slot) = capture.source {
                            before.insert(slot);
                        }
                    }
                }
                (before, after.clone())
            }
            _ => (self.live_children(id, after), after.clone()),
        };

        self.before[id.index()] = before.clone();
        self.after[id.index()] = effective_after;
        before
    }

    fn live_children(&mut self, id: IrId, after: &SlotSet) -> SlotSet {
        let node = self.node(id);
        let mut current = after.clone();
        for child in children_of(self.program, self.function, node)
            .into_iter()
            .rev()
        {
            current = self.live(child, &current);
        }
        current
    }

    fn live_if(&mut self, id: IrId, after: &SlotSet) -> SlotSet {
        let node = self.node(id);
        let children = children_of(self.program, self.function, node);
        let (Some(cond), Some(then_branch)) = (children.first().copied(), children.get(1).copied())
        else {
            return after.clone();
        };
        let then_live = self.live(then_branch, after);
        let else_live = match children.get(2).copied() {
            Some(else_branch) => self.live(else_branch, after),
            None => after.clone(),
        };
        let cond_after = then_live.union(&else_live);
        self.live(cond, &cond_after)
    }

    fn live_while(&mut self, cond: IrId, body: IrId, after: &SlotSet) -> SlotSet {
        let exit = after.clone();
        let mut header = exit.clone();

        loop {
            self.loops.push(LiveLoop {
                exit: exit.clone(),
                header: header.clone(),
            });
            let body_live = self.live(body, &header);
            self.loops.pop();

            let cond_after = body_live.union(&exit);
            let cond_live = self.live(cond, &cond_after);
            if cond_live == header {
                return cond_live;
            }
            header = cond_live;
        }
    }

    fn live_for(
        &mut self,
        pattern: PatternId,
        iterable: IrId,
        body: IrId,
        after: &SlotSet,
    ) -> SlotSet {
        let exit = after.clone();
        let mut bindings = Vec::new();
        if let Some(pattern) = self.function.pattern(pattern) {
            pattern_binding_slots(pattern, &mut bindings);
        }

        let mut header = exit.clone();
        loop {
            let mut body_after = header.union(&exit);
            for slot in &bindings {
                body_after.remove(*slot);
            }
            self.loops.push(LiveLoop {
                exit: exit.clone(),
                header: header.clone(),
            });
            let body_live = self.live(body, &body_after);
            self.loops.pop();

            let mut next = body_live;
            for slot in &bindings {
                next.remove(*slot);
            }
            next.union_with(&exit);
            if next == header {
                break;
            }
            header = next;
        }

        self.live(iterable, &header)
    }

    fn live_match(&mut self, match_set: MatchSetId, scrutinee: IrId, after: &SlotSet) -> SlotSet {
        let arms = self
            .function
            .match_set(match_set)
            .map(|set| set.arms.clone())
            .unwrap_or_default();

        let mut next_live = after.clone();
        for arm in arms.iter().rev() {
            let mut arm_live = self.live(arm.body, after);
            if let Some(guard) = arm.guard {
                let guard_after = arm_live.union(&next_live);
                let guard_live = self.live(guard, &guard_after);
                arm_live.union_with(&guard_live);
            }
            if let Some(pattern) = self.function.pattern(arm.pattern) {
                let mut slots = Vec::new();
                pattern_binding_slots(pattern, &mut slots);
                for slot in slots {
                    arm_live.remove(slot);
                }
            }
            next_live.union_with(&arm_live);
        }

        self.live(scrutinee, &next_live)
    }

    fn live_assign(&mut self, place: IrId, value: IrId, after: &SlotSet) -> SlotSet {
        let place_node = self.node(place);
        let bare = matches!(place_node.kind, NodeKind::SlotPlace(_));
        let mut value_after = after.clone();
        if let NodeKind::SlotPlace(slot) = place_node.kind
            && bare
        {
            value_after.remove(slot);
        }
        let value_live = self.live(value, &value_after);
        self.live_place(place, &value_live, !bare)
    }

    fn live_compound(&mut self, place: IrId, value: IrId, after: &SlotSet) -> SlotSet {
        let place_node = self.node(place);
        let bare = matches!(place_node.kind, NodeKind::SlotPlace(_));
        let mut value_after = after.clone();
        if let NodeKind::SlotPlace(slot) = place_node.kind
            && bare
        {
            value_after.remove(slot);
        }
        let value_live = self.live(value, &value_after);
        self.live_place(place, &value_live, true)
    }

    fn live_place(&mut self, id: IrId, after: &SlotSet, read: bool) -> SlotSet {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.live_place_inner(id, after, read)
        })
    }

    fn live_place_inner(&mut self, id: IrId, after: &SlotSet, read: bool) -> SlotSet {
        let node = self.node(id);
        let before = match node.kind {
            NodeKind::SlotPlace(slot) => {
                let mut before = after.clone();
                if read {
                    before.insert(slot);
                }
                before
            }
            NodeKind::Field { base, .. } | NodeKind::TupleField { base, .. } => {
                self.live_place(base, after, true)
            }
            NodeKind::Index { base, index } => {
                let index_live = self.live(index, after);
                self.live_place(base, &index_live, true)
            }
            _ => self.live(id, after),
        };
        self.before[id.index()] = before.clone();
        self.after[id.index()] = after.clone();
        before
    }
}
