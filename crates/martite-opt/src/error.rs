//! Structured optimizer failures.

use std::fmt;

use martite_ir::{Charge, Effects, FnId, IrId, NodeKind, SourceId, Span, TyId, VerifyError};

use crate::CostPolicy;

/// One rewrite refused by the rewriter or rejected by an invariant check.
#[derive(Debug, Clone, PartialEq)]
pub struct RewriteError {
    pub function: FnId,
    pub source: Option<SourceId>,
    pub node: Option<IrId>,
    pub span: Option<Span>,
    pub kind: RewriteErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RewriteErrorKind {
    /// A value replacement was requested for a statement or block node.
    NotAValue { found: NodeKind },
    /// A statement replacement was requested for a non-statement node.
    NotAStatement { found: NodeKind },
    /// The replacement leaf is not a childless scalar value.
    NotScalarLeaf { found: NodeKind },
    /// The replacement's type does not match the removed node's type.
    TypeMismatch { expected: TyId, found: TyId },
    /// The removed node is or contains a safepoint.
    Safepoint { node: IrId },
    /// The removed node is or contains a shared heap read.
    Shares { node: IrId },
    /// The removed subtree has a runtime-dependent charge.
    DynamicCharge { node: IrId },
    /// The removed subtree contains a loop, whose total charge is unknown.
    ControlFlowCharge { node: IrId },
    /// The removed subtree contains a charge call that carries multiple fuel
    /// units, so a `Charge(n)` loop cannot preserve the tick cadence.
    MultipleChargeUnits { node: IrId, charge: Charge },
    /// The removed subtree has an observable effect a `Preserve` pass may not
    /// drop.
    UnsafeEffects { node: IrId, effects: Effects },
    /// The replacement would cost more fuel than the removed subtree.
    ChargeOverflow {
        node: IrId,
        removed: u32,
        replacement: u32,
    },
    /// The same node was reached more than once while rebuilding the tree.
    TreeReuse { node: IrId },
    /// A non-block parent lost one of its children to a statement removal.
    ChildRemoval { node: IrId },
    /// A synthetic-origin anchor could not be represented.
    Provenance { node: IrId, origin: IrId },
    /// The program's type table has no `unit` entry.
    MissingUnitType,
    /// A function pass ran without a program view.
    MissingProgram,
}

impl RewriteError {
    pub(crate) fn new(
        function: FnId,
        source: Option<SourceId>,
        span: Option<Span>,
        node: Option<IrId>,
        kind: RewriteErrorKind,
    ) -> Self {
        Self {
            function,
            source,
            node,
            span,
            kind,
        }
    }
}

impl fmt::Display for RewriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn #{}", self.function.0)?;
        if let Some(span) = self.span {
            write!(f, " at {}..{}", span.start, span.end)?;
        }
        write!(f, ": ")?;
        match &self.kind {
            RewriteErrorKind::NotAValue { found } => {
                write!(f, "cannot replace statement node {found:?} with a value")
            }
            RewriteErrorKind::NotAStatement { found } => {
                write!(f, "cannot remove value node {found:?} as a statement")
            }
            RewriteErrorKind::NotScalarLeaf { found } => write!(
                f,
                "replacement leaf {found:?} is not a childless scalar value"
            ),
            RewriteErrorKind::TypeMismatch { expected, found } => write!(
                f,
                "replacement type ty#{} does not match removed type ty#{}",
                found.0, expected.0
            ),
            RewriteErrorKind::Safepoint { node } => write!(
                f,
                "Preserve policy refuses to remove or merge safepoint node #{}",
                node.0
            ),
            RewriteErrorKind::Shares { node } => write!(
                f,
                "Preserve policy refuses to remove shared heap read node #{}",
                node.0
            ),
            RewriteErrorKind::DynamicCharge { node } => write!(
                f,
                "node #{} has a runtime-dependent charge that cannot be carried",
                node.0
            ),
            RewriteErrorKind::ControlFlowCharge { node } => write!(
                f,
                "node #{} contains control flow whose executed charge is not statically known",
                node.0
            ),
            RewriteErrorKind::MultipleChargeUnits { node, charge } => write!(
                f,
                "node #{} charges {}/{} units per call; the carried charge preserves tick cadence only for single-unit calls",
                node.0, charge.pre, charge.post
            ),
            RewriteErrorKind::UnsafeEffects { node, effects } => write!(
                f,
                "Preserve policy refuses to drop effects {effects:?} from node #{}",
                node.0
            ),
            RewriteErrorKind::ChargeOverflow {
                node,
                removed,
                replacement,
            } => write!(
                f,
                "replacement for node #{} costs {replacement} fuel units but only {removed} may be carried",
                node.0
            ),
            RewriteErrorKind::TreeReuse { node } => {
                write!(f, "node #{} was reached more than once", node.0)
            }
            RewriteErrorKind::ChildRemoval { node } => write!(
                f,
                "node #{} was removed but its parent cannot omit a child",
                node.0
            ),
            RewriteErrorKind::Provenance { node, origin } => write!(
                f,
                "node #{} cannot use node #{} as its synthetic origin",
                node.0, origin.0
            ),
            RewriteErrorKind::MissingUnitType => {
                f.write_str("program type table has no `unit` entry")
            }
            RewriteErrorKind::MissingProgram => f.write_str("function pass has no program view"),
        }
    }
}

impl std::error::Error for RewriteError {}

/// One optimization run failure.
#[derive(Debug)]
#[non_exhaustive]
pub enum OptimizeError {
    Rewrite {
        pass: &'static str,
        error: RewriteError,
    },
    Policy {
        pass: &'static str,
        policy: CostPolicy,
    },
    Verify {
        pass: &'static str,
        error: Box<VerifyError>,
    },
    FixpointLimit {
        limit: u32,
    },
}

impl fmt::Display for OptimizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rewrite { pass, error } => write!(f, "pass `{pass}` failed: {error}"),
            Self::Policy { pass, policy } => write!(
                f,
                "pass `{pass}` declares {policy:?} but Reduce passes are not enabled"
            ),
            Self::Verify { pass, error } => {
                write!(f, "pass `{pass}` produced invalid IR: {error}")
            }
            Self::FixpointLimit { limit } => {
                write!(
                    f,
                    "pass manager did not reach a fixpoint in {limit} iterations"
                )
            }
        }
    }
}

impl std::error::Error for OptimizeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rewrite { error, .. } => Some(error),
            Self::Verify { error, .. } => Some(error),
            Self::Policy { .. } | Self::FixpointLimit { .. } => None,
        }
    }
}
