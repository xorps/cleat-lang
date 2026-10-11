//! Verification conditions produced by refinement checking (SPEC §6).
//!
//! `triage` decides what it can by evaluating closed goals and matching known
//! facts; everything else is checked at runtime during evaluation.

use std::fmt;

use crate::ast::{BinOp, Expr, ExprKind};
use crate::diag::Span;
use crate::eval::{Value, eval_closed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcStatus {
    /// Proved statically.
    Proved,
    /// Definitely false: a compile error.
    Refuted,
    /// Left to the runtime check at the annotated boundary.
    Runtime,
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
            VcStatus::Proved => "proved",
            VcStatus::Refuted => "REFUTED",
            VcStatus::Runtime => "runtime",
        };
        writeln!(
            f,
            "[{tag}] in `{}` at {}: required by {}",
            self.func, self.span, self.reason
        )?;
        for h in &self.hyps {
            writeln!(f, "      {h}")?;
        }
        write!(f, "    ⊢ {}", self.goal)
    }
}

pub fn triage(hyps: &[Expr], goal: &Expr) -> VcStatus {
    let known: Vec<&Expr> = hyps.iter().flat_map(|h| h.conjuncts()).collect();
    // Contradictory hypotheses: the program point is unreachable.
    if known.iter().any(|h| eval_closed(h) == Some(Value::Bool(false))) {
        return VcStatus::Proved;
    }
    // Propagate `x == <constant>` facts into the goal.
    let eqs: Vec<(String, Expr)> = known
        .iter()
        .filter_map(|h| match &h.kind {
            ExprKind::Binary(BinOp::Eq, l, r) => match (&l.kind, eval_closed(r)) {
                (ExprKind::Var(n), Some(_)) => Some((n.clone(), (**r).clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let g = goal.subst_many(&eqs);
    match eval_closed(&g) {
        Some(Value::Bool(true)) => return VcStatus::Proved,
        Some(Value::Bool(false)) => return VcStatus::Refuted,
        _ => {}
    }
    let holds = |c: &&Expr| known.contains(c) || eval_closed(c) == Some(Value::Bool(true));
    if g.conjuncts().iter().all(holds) || goal.conjuncts().iter().all(holds) {
        VcStatus::Proved
    } else {
        VcStatus::Runtime
    }
}
