//! Verification conditions produced by refinement checking (SPEC §5.2).
//!
//! This prototype has no SMT backend yet: `triage` discharges what it can by
//! constant folding and syntactic matching, and leaves the rest `Pending`.

use std::fmt;

use crate::ast::{BinOp, Expr, ExprKind};
use crate::diag::Span;
use crate::types::{Value, const_eval};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcStatus {
    /// Discharged without a solver.
    Trivial,
    /// The goal is definitely false.
    Refuted,
    /// Needs the SMT backend.
    Pending,
}

#[derive(Debug, Clone)]
pub struct Vc {
    pub func: String,
    pub span: Span,
    pub hyps: Vec<Expr>,
    pub goal: Expr,
    /// The refined type that demanded this VC.
    pub reason: String,
    pub status: VcStatus,
}

impl fmt::Display for Vc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.status {
            VcStatus::Trivial => "proved",
            VcStatus::Refuted => "REFUTED",
            VcStatus::Pending => "pending",
        };
        writeln!(f, "[{tag}] in `{}` at {}: required by {}", self.func, self.span, self.reason)?;
        for h in &self.hyps {
            writeln!(f, "      {h}")?;
        }
        write!(f, "    ⊢ {}", self.goal)
    }
}

pub fn triage(hyps: &[Expr], goal: &Expr) -> VcStatus {
    let known: Vec<&Expr> = hyps.iter().flat_map(|h| h.conjuncts()).collect();
    // Contradictory hypotheses: the program point is unreachable.
    if known.iter().any(|h| const_eval(h) == Some(Value::Bool(false))) {
        return VcStatus::Trivial;
    }
    // Propagate `x == <constant>` facts into the goal.
    let eqs: Vec<(String, Expr)> = known
        .iter()
        .filter_map(|h| match &h.kind {
            ExprKind::Binary(BinOp::Eq, l, r) => match (&l.kind, const_eval(r)) {
                (ExprKind::Var(n), Some(_)) => Some((n.clone(), (**r).clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let g = goal.subst_many(&eqs);
    match const_eval(&g) {
        Some(Value::Bool(true)) => return VcStatus::Trivial,
        Some(Value::Bool(false)) => return VcStatus::Refuted,
        _ => {}
    }
    let holds = |c: &&Expr| known.contains(c) || const_eval(c) == Some(Value::Bool(true));
    if g.conjuncts().iter().all(holds) || goal.conjuncts().iter().all(holds) {
        VcStatus::Trivial
    } else {
        VcStatus::Pending
    }
}
