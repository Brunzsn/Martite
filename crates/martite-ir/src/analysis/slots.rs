//! Dense slot sets and the one-initialization-site check.
//!
//! [`SlotSet`] is the domain of both analyses. [`check_slot_initialization`]
//! enforces rule M2's static half: lowering assigns one slot per binding and
//! no pass may reuse it. The dynamic half, "no pass may elide a heap-valued
//! slot write under `Preserve`", belongs to the rewriter in #123.

use std::fmt;

use crate::id::{IrId, PatternId, Slot};
use crate::node::NodeKind;
use crate::pattern::Pattern;
use crate::program::{IrFn, IrProgram};
use crate::verify::children_of;

/// A set of frame slots, indexed densely over `IrFn::locals`.
#[derive(Clone, PartialEq, Eq)]
pub struct SlotSet {
    slots: u32,
    words: Vec<u64>,
}

impl SlotSet {
    pub fn new(slots: u32) -> Self {
        Self {
            slots,
            words: vec![0; word_count(slots)],
        }
    }

    /// Number of slots this set can hold.
    pub const fn slot_count(&self) -> u32 {
        self.slots
    }

    pub fn contains(&self, slot: Slot) -> bool {
        let index = slot.index();
        index < self.slots as usize && self.words[index / 64] & (1u64 << (index % 64)) != 0
    }

    pub fn insert(&mut self, slot: Slot) {
        let index = slot.index();
        if index < self.slots as usize {
            self.words[index / 64] |= 1u64 << (index % 64);
        }
    }

    pub fn remove(&mut self, slot: Slot) {
        let index = slot.index();
        if index < self.slots as usize {
            self.words[index / 64] &= !(1u64 << (index % 64));
        }
    }

    pub fn union_with(&mut self, other: &Self) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word |= *other;
        }
    }

    pub fn intersect_with(&mut self, other: &Self) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word &= *other;
        }
    }

    pub fn subtract_with(&mut self, other: &Self) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word &= !*other;
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.union_with(other);
        result
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.intersect_with(other);
        result
    }

    pub fn difference(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.subtract_with(other);
        result
    }

    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    pub fn iter(&self) -> impl Iterator<Item = Slot> + '_ {
        let slots = self.slots;
        self.words
            .iter()
            .enumerate()
            .flat_map(move |(word_index, word)| {
                let word = *word;
                (0..64).filter_map(move |bit| {
                    let index = word_index * 64 + bit;
                    (index < slots as usize && word & (1u64 << bit) != 0)
                        .then_some(Slot::new(index as u32))
                })
            })
    }
}

impl fmt::Debug for SlotSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

fn word_count(slots: u32) -> usize {
    slots.div_ceil(64) as usize
}

/// One place that initializes a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitSite {
    Parameter(u32),
    ParameterPattern(u32),
    Let(IrId),
    ForLoop(IrId),
    MatchArm(IrId),
    SlotInit(IrId),
}

impl InitSite {
    fn node(self) -> Option<IrId> {
        match self {
            Self::Parameter(_) | Self::ParameterPattern(_) => None,
            Self::Let(node) | Self::ForLoop(node) | Self::MatchArm(node) | Self::SlotInit(node) => {
                Some(node)
            }
        }
    }
}

/// A slot that is initialized more than once or never initialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotBindingError {
    pub slot: Slot,
    pub node: Option<IrId>,
    pub context: &'static str,
}

/// Enforce one initialization site per slot.
///
/// Sites are parameters, destructured parameter patterns, `Let`/`For`/match-arm
/// pattern bindings, and `SlotInit` nodes. Or-pattern alternatives share one
/// site; the verifier rejects alternatives that bind different slots.
pub fn check_slot_initialization(
    program: &IrProgram,
    function: &IrFn,
) -> Result<(), SlotBindingError> {
    let mut sites = vec![None; function.locals as usize];

    for (index, param) in function.params.iter().enumerate() {
        insert_site(&mut sites, param.slot, InitSite::Parameter(index as u32))?;
        if let Some(pattern) = param.pattern {
            add_pattern_sites(
                function,
                &mut sites,
                pattern,
                InitSite::ParameterPattern(index as u32),
            )?;
        }
    }

    if let Some(body) = function.body {
        collect_node_sites(program, function, &mut sites, body)?;
    }

    for (index, site) in sites.iter().enumerate() {
        if site.is_none() {
            return Err(SlotBindingError {
                slot: Slot::new(index as u32),
                node: None,
                context: "slot has no initialization site",
            });
        }
    }
    Ok(())
}

fn collect_node_sites(
    program: &IrProgram,
    function: &IrFn,
    sites: &mut [Option<InitSite>],
    id: IrId,
) -> Result<(), SlotBindingError> {
    stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
        collect_node_sites_inner(program, function, sites, id)
    })
}

fn collect_node_sites_inner(
    program: &IrProgram,
    function: &IrFn,
    sites: &mut [Option<InitSite>],
    id: IrId,
) -> Result<(), SlotBindingError> {
    let Some(node) = function.node(id).copied() else {
        return Ok(());
    };

    match node.kind {
        NodeKind::Let { pattern, .. } => {
            add_pattern_sites(function, sites, pattern, InitSite::Let(id))?;
        }
        NodeKind::For { pattern, .. } => {
            add_pattern_sites(function, sites, pattern, InitSite::ForLoop(id))?;
        }
        NodeKind::Match { match_set, .. } => {
            if let Some(set) = function.match_set(match_set) {
                for arm in &set.arms {
                    add_pattern_sites(function, sites, arm.pattern, InitSite::MatchArm(id))?;
                }
            }
        }
        NodeKind::SlotInit { slot, .. } => {
            insert_site(sites, slot, InitSite::SlotInit(id))?;
        }
        _ => {}
    }

    for child in children_of(program, function, node) {
        collect_node_sites(program, function, sites, child)?;
    }
    Ok(())
}

fn add_pattern_sites(
    function: &IrFn,
    sites: &mut [Option<InitSite>],
    pattern: PatternId,
    site: InitSite,
) -> Result<(), SlotBindingError> {
    let Some(pattern) = function.pattern(pattern) else {
        return Ok(());
    };
    let mut slots = Vec::new();
    pattern_binding_slots(pattern, &mut slots);
    for slot in slots {
        insert_site(sites, slot, site)?;
    }
    Ok(())
}

fn insert_site(
    sites: &mut [Option<InitSite>],
    slot: Slot,
    site: InitSite,
) -> Result<(), SlotBindingError> {
    let Some(entry) = sites.get_mut(slot.index()) else {
        return Err(SlotBindingError {
            slot,
            node: site.node(),
            context: "slot is out of range",
        });
    };
    if entry.is_some() {
        return Err(SlotBindingError {
            slot,
            node: site.node(),
            context: "slot has more than one initialization site",
        });
    }
    *entry = Some(site);
    Ok(())
}

/// Collect every slot a pattern binds, without duplicates.
///
/// Or-pattern alternatives share one binding slot set (the verifier checks it),
/// so deduplicating is enough.
pub(crate) fn pattern_binding_slots(pattern: &Pattern, out: &mut Vec<Slot>) {
    match pattern {
        Pattern::Wild | Pattern::Int(_) | Pattern::Bool(_) | Pattern::Str(_) => {}
        Pattern::Bind { slot, .. } => {
            if !out.contains(slot) {
                out.push(*slot);
            }
        }
        Pattern::Tuple(items) | Pattern::Or(items) => {
            for item in items {
                pattern_binding_slots(item, out);
            }
        }
        Pattern::Variant { args, .. } | Pattern::Prelude { args, .. } => {
            for item in args {
                pattern_binding_slots(item, out);
            }
        }
        Pattern::Struct { fields, .. } => {
            for (_, item) in fields {
                pattern_binding_slots(item, out);
            }
        }
    }
}

/// The sorted, deduplicated slot set of a pattern.
pub(crate) fn pattern_binding_set(pattern: &Pattern) -> Vec<Slot> {
    let mut slots = Vec::new();
    pattern_binding_slots(pattern, &mut slots);
    slots.sort_unstable();
    slots
}

#[cfg(test)]
mod tests {
    use super::{SlotSet, pattern_binding_set};
    use crate::{Pattern, Slot};

    #[test]
    fn slot_sets_union_intersect_and_iterate() {
        let mut left = SlotSet::new(5);
        left.insert(Slot::new(0));
        left.insert(Slot::new(4));
        let mut right = SlotSet::new(5);
        right.insert(Slot::new(0));
        right.insert(Slot::new(2));

        assert_eq!(
            left.iter().collect::<Vec<_>>(),
            vec![Slot::new(0), Slot::new(4)]
        );
        assert!(left.contains(Slot::new(4)));
        assert!(!left.contains(Slot::new(2)));

        assert_eq!(
            left.union(&right).iter().collect::<Vec<_>>(),
            vec![Slot::new(0), Slot::new(2), Slot::new(4)]
        );
        assert_eq!(
            left.intersection(&right).iter().collect::<Vec<_>>(),
            vec![Slot::new(0)]
        );
        assert_eq!(
            left.difference(&right).iter().collect::<Vec<_>>(),
            vec![Slot::new(4)]
        );
    }

    #[test]
    fn or_pattern_slots_are_deduplicated() {
        let pattern = Pattern::Or(vec![
            Pattern::Tuple(vec![
                Pattern::Bind {
                    slot: Slot::new(1),
                    ty: crate::TyId::new(0),
                },
                Pattern::Bind {
                    slot: Slot::new(2),
                    ty: crate::TyId::new(0),
                },
            ]),
            Pattern::Bind {
                slot: Slot::new(1),
                ty: crate::TyId::new(0),
            },
        ]);
        assert_eq!(
            pattern_binding_set(&pattern),
            vec![Slot::new(1), Slot::new(2)]
        );
    }
}
