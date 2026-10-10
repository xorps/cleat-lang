//! Base type checking (SPEC §7 steps 3–4) plus verification-condition
//! generation for refinements (step 5, minus the solver).

use std::collections::{BTreeMap, HashMap};

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::types::{Mode, Ty, TypeCtx};
use crate::vc::{Vc, VcStatus, triage};

#[derive(Debug, Default)]
pub struct Outcome {
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
    pub vcs: Vec<Vc>,
}

#[derive(Clone)]
struct FnSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
}

/// A pattern with its type annotations resolved.
enum RPat {
    Wild,
    Bind(String, Span),
    Typed(Option<String>, Ty, Span),
    Lit(Expr),
    Variant { e: String, v: String, payload: Option<(Vec<(Label, RPat)>, bool)>, span: Span },
    Record(Vec<(Label, RPat)>, bool, Span),
}

const BUILTIN_TYPES: [&str; 5] = ["Never", "Int", "Float", "Bool", "Str"];

pub fn check_program(decls: &[Decl]) -> Outcome {
    let mut c = Checker {
        ctx: TypeCtx::default(),
        aliases: HashMap::new(),
        alias_cache: HashMap::new(),
        resolving: Vec::new(),
        fns: HashMap::new(),
        scope: Vec::new(),
        hyps: Vec::new(),
        cur_fn: String::new(),
        out: Outcome::default(),
    };
    c.run(decls);
    let mut out = c.out;
    out.errors.sort_by_key(|d| (d.span.line, d.span.col));
    out.warnings.sort_by_key(|d| (d.span.line, d.span.col));
    out
}

/// Prefer the field expression of a record literal over `e.l`, so VCs read naturally.
fn project(subject: &Expr, l: &Label) -> Expr {
    if let ExprKind::Record(fs) = &subject.kind {
        if let Some((_, e)) = fs.iter().find(|(fl, _)| fl == l) {
            return e.clone();
        }
    }
    Expr::field(subject.clone(), l.clone())
}

fn is_num_lit(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) => true,
        ExprKind::Unary(UnOp::Neg, x) => is_num_lit(x),
        _ => false,
    }
}

/// SPEC §5.1: the decidable predicate fragment.
fn pred_fragment(e: &Expr) -> Result<(), Diag> {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Var(_) => Ok(()),
        ExprKind::Field(b, _) => pred_fragment(b),
        ExprKind::Unary(_, x) => pred_fragment(x),
        ExprKind::Binary(op, l, r) => {
            pred_fragment(l)?;
            pred_fragment(r)?;
            let linear = match op {
                BinOp::Mul => is_num_lit(l) || is_num_lit(r),
                BinOp::Div | BinOp::Rem => is_num_lit(r),
                _ => true,
            };
            if linear {
                Ok(())
            } else {
                Err(Diag::new(e.span, format!("non-linear arithmetic is not allowed in refinement predicates: `{e}`")))
            }
        }
        _ => Err(Diag::new(
            e.span,
            format!(
                "`{e}` is not allowed in a refinement predicate (only literals, variables, fields, \
                 linear arithmetic, comparisons and boolean logic)"
            ),
        )),
    }
}

struct Checker<'a> {
    ctx: TypeCtx,
    aliases: HashMap<String, &'a TypeExpr>,
    alias_cache: HashMap<String, Ty>,
    resolving: Vec<String>,
    fns: HashMap<String, FnSig>,
    /// Variables in scope, innermost last.
    scope: Vec<(String, Ty)>,
    /// Path conditions and facts known at the current program point.
    hyps: Vec<Expr>,
    cur_fn: String,
    out: Outcome,
}

impl<'a> Checker<'a> {
    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.out.errors.push(Diag::new(span, msg));
    }

    fn warn(&mut self, span: Span, msg: impl Into<String>) {
        self.out.warnings.push(Diag::new(span, msg));
    }

    fn norm(&self, t: &Ty) -> Ty {
        self.ctx.norm(t)
    }

    /// Normalized, refinements stripped: the shape used for operators.
    fn prim(&self, t: &Ty) -> Ty {
        self.ctx.norm(t).strip().clone()
    }

    // ---------- declarations ----------

    fn run(&mut self, decls: &'a [Decl]) {
        // 1. Register type names so declarations can refer to each other in any order.
        let mut owner: HashMap<String, Span> = HashMap::new();
        for d in decls {
            let (name, span) = match d {
                Decl::Type { name, span, .. } | Decl::Nominal { name, span, .. } | Decl::Enum { name, span, .. } => {
                    (name, *span)
                }
                Decl::Fn(_) => continue,
            };
            if BUILTIN_TYPES.contains(&name.as_str()) || owner.contains_key(name) {
                self.error(span, format!("type `{name}` is already defined"));
                continue;
            }
            owner.insert(name.clone(), span);
            match d {
                Decl::Type { ty, .. } => {
                    self.aliases.insert(name.clone(), ty);
                }
                Decl::Nominal { .. } => {
                    self.ctx.nominals.insert(name.clone(), Ty::Error);
                }
                Decl::Enum { variants, .. } => {
                    let mut vs: Vec<(String, Ty)> = Vec::new();
                    for v in variants {
                        if vs.iter().any(|(n, _)| *n == v.name) {
                            self.error(v.span, format!("duplicate variant `{name}.{}`", v.name));
                        } else {
                            vs.push((v.name.clone(), Ty::unit()));
                        }
                    }
                    self.ctx.enums.insert(name.clone(), vs);
                }
                Decl::Fn(_) => unreachable!(),
            }
        }

        // 2. Resolve type declaration bodies.
        for d in decls {
            match d {
                Decl::Type { name, span, .. } if owner.get(name) == Some(span) => {
                    self.resolve_alias(name, *span);
                }
                Decl::Nominal { name, ty, span } if owner.get(name) == Some(span) => {
                    let t = self.resolve(ty);
                    self.ctx.nominals.insert(name.clone(), t);
                }
                Decl::Enum { name, variants, span } if owner.get(name) == Some(span) => {
                    for v in variants {
                        let t = match &v.payload {
                            Some(te) => self.resolve(te),
                            None => Ty::unit(),
                        };
                        if let Some(slot) = self.ctx.enums.get_mut(name).and_then(|vs| vs.iter_mut().find(|(n, _)| *n == v.name)) {
                            slot.1 = t;
                        }
                    }
                }
                _ => {}
            }
        }

        // 3. Function signatures. Parameter refinements may mention earlier parameters.
        let mut accepted: Vec<&'a FnDecl> = Vec::new();
        for d in decls {
            let Decl::Fn(f) = d else { continue };
            if self.fns.contains_key(&f.name) || self.ctx.nominals.contains_key(&f.name) || f.name == "panic" {
                self.error(f.span, format!("`{}` is already defined", f.name));
                continue;
            }
            let mut params = Vec::new();
            for (p, te, ps) in &f.params {
                let t = self.resolve(te);
                self.bind(p, t.clone(), *ps);
                params.push((p.clone(), t));
            }
            let ret = match &f.ret {
                Some(te) => self.resolve(te),
                None => Ty::unit(),
            };
            self.scope.clear();
            self.fns.insert(f.name.clone(), FnSig { params, ret });
            accepted.push(f);
        }

        // 4. Function bodies.
        for f in accepted {
            self.cur_fn = f.name.clone();
            let sig = self.fns[&f.name].clone();
            for (p, t) in &sig.params {
                self.scope.push((p.clone(), t.clone()));
                let facts = self.facts(t, &Expr::var(p, f.span), 0);
                self.hyps.extend(facts);
            }
            self.check(&f.body, &sig.ret);
            self.scope.clear();
            self.hyps.clear();
        }
    }

    // ---------- type resolution ----------

    fn resolve(&mut self, te: &TypeExpr) -> Ty {
        let t = self.resolve_raw(te);
        self.norm(&t)
    }

    fn resolve_raw(&mut self, te: &TypeExpr) -> Ty {
        match &te.kind {
            TypeKind::Named(n) => match n.as_str() {
                "Never" => Ty::Never,
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Bool" => Ty::Bool,
                "Str" => Ty::Str,
                _ if self.ctx.nominals.contains_key(n) => Ty::Nominal(n.clone()),
                _ if self.ctx.enums.contains_key(n) => Ty::Enum(n.clone()),
                _ if self.aliases.contains_key(n) => self.resolve_alias(n, te.span),
                _ => {
                    self.error(te.span, format!("unknown type `{n}`"));
                    Ty::Error
                }
            },
            TypeKind::Path(e, v) => {
                if self.ctx.payload(e, v).is_some() {
                    Ty::Variant(e.clone(), v.clone())
                } else {
                    self.error(te.span, format!("unknown enum variant `{e}.{v}`"));
                    Ty::Error
                }
            }
            TypeKind::Record { fields, open } => {
                let mut map = BTreeMap::new();
                for (l, ft) in fields {
                    let t = self.resolve_raw(ft);
                    if map.insert(l.clone(), t).is_some() {
                        self.error(ft.span, format!("duplicate field `{l}`"));
                    }
                }
                Ty::Record { fields: map, open: *open }
            }
            TypeKind::Union(ms) => Ty::Union(ms.iter().map(|m| self.resolve_raw(m)).collect()),
            TypeKind::Refine(b, pred) => {
                let base = self.resolve(b);
                self.check_pred(pred, &base);
                Ty::Refine(Box::new(base), pred.clone())
            }
            TypeKind::Fn(ps, r) => {
                Ty::Fn(ps.iter().map(|p| self.resolve_raw(p)).collect(), Box::new(self.resolve_raw(r)))
            }
        }
    }

    fn resolve_alias(&mut self, n: &str, span: Span) -> Ty {
        if let Some(t) = self.alias_cache.get(n) {
            return t.clone();
        }
        if self.resolving.iter().any(|r| r == n) {
            self.error(span, format!("type alias `{n}` is recursive; use `enum` or `nominal` for recursive types"));
            return Ty::Error;
        }
        let te: &'a TypeExpr = self.aliases[n];
        self.resolving.push(n.to_string());
        // Alias predicates may only mention `self`, never local variables.
        let saved = std::mem::take(&mut self.scope);
        let t = self.resolve(te);
        self.scope = saved;
        self.resolving.pop();
        self.alias_cache.insert(n.to_string(), t.clone());
        t
    }

    fn check_pred(&mut self, pred: &Expr, base: &Ty) {
        if let Err(d) = pred_fragment(pred) {
            self.out.errors.push(d);
            return;
        }
        self.scope.push(("self".to_string(), base.clone()));
        let t = self.synth(pred);
        self.scope.pop();
        if !matches!(self.prim(&t), Ty::Bool | Ty::Error) {
            self.error(pred.span, format!("refinement predicate must be `Bool`, found `{t}`"));
        }
    }

    // ---------- environment ----------

    fn lookup(&self, name: &str) -> Option<&Ty> {
        self.scope.iter().rev().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    fn bind(&mut self, name: &str, t: Ty, span: Span) {
        if name == "self" {
            self.error(span, "`self` is reserved for refinement predicates");
        } else if self.scope.iter().any(|(n, _)| n == name) {
            // Refinement facts mention variables by name, so shadowing would make them lie.
            self.error(span, format!("shadowing is not supported yet: `{name}` is already bound"));
        }
        self.scope.push((name.to_string(), t));
    }

    /// Bind a variable and record what its type tells us about it.
    fn bind_var(&mut self, name: &str, t: Ty, span: Span) {
        let facts = self.facts(&t, &Expr::var(name, span), 0);
        self.bind(name, t, span);
        self.hyps.extend(facts);
    }

    /// Predicates that hold of `subject` because it has type `ty`.
    fn facts(&self, ty: &Ty, subject: &Expr, depth: u32) -> Vec<Expr> {
        if depth > 4 {
            return vec![];
        }
        match ty {
            Ty::Refine(b, p) => {
                let mut v = vec![p.subst("self", subject)];
                v.extend(self.facts(b, subject, depth + 1));
                v
            }
            Ty::Record { fields, .. } => {
                fields.iter().flat_map(|(l, ft)| self.facts(ft, &project(subject, l), depth + 1)).collect()
            }
            Ty::Nominal(n) => match self.ctx.nominals.get(n) {
                Some(u) => self.facts(u, &Expr::field(subject.clone(), Label::Pos(0)), depth + 1),
                None => vec![],
            },
            _ => vec![],
        }
    }

    fn assume(&mut self, cond: &Expr, positive: bool) {
        if cond.is_pure() {
            self.hyps.push(if positive { cond.clone() } else { cond.negate() });
        }
    }

    // ---------- refinement obligations ----------

    fn push_vc(&mut self, hyps: Vec<Expr>, goal: Expr, span: Span, reason: String) {
        let status = triage(&hyps, &goal);
        if status == VcStatus::Refuted {
            self.error(span, format!("refinement violated: `{goal}` does not hold (required by {reason})"));
        }
        self.out.vcs.push(Vc { func: self.cur_fn.clone(), span, hyps, goal, reason, status });
    }

    /// `subject : actual` flows into `expected`: emit a VC for every refinement
    /// in `expected` that `actual` doesn't already carry.
    fn gen_vcs(&mut self, actual: &Ty, expected: &Ty, subject: &Expr) {
        let expected = self.norm(expected);
        let actual = self.norm(actual);
        if self.ctx.sub(&actual, &expected, Mode::Strict) {
            return;
        }
        match &expected {
            Ty::Refine(base, pred) => {
                let goal = pred.subst("self", subject);
                let mut hyps = self.hyps.clone();
                hyps.extend(self.facts(&actual, subject, 0));
                self.push_vc(hyps, goal, subject.span, format!("`{expected}`"));
                self.gen_vcs(&actual, base, subject);
            }
            Ty::Record { fields, .. } => {
                if let Ty::Record { fields: af, .. } = actual.strip() {
                    for (l, ft) in fields {
                        if let Some(at) = af.get(l) {
                            self.gen_vcs(at, ft, &project(subject, l));
                        }
                    }
                }
            }
            Ty::Union(ms) => {
                // Route each member of `actual` to the expected member it lands in.
                // TODO: VCs for one member of a union-typed subject should be
                // guarded by a runtime-tag hypothesis.
                for am in self.ctx.members(&actual) {
                    let pick = ms
                        .iter()
                        .find(|m| self.ctx.sub(&am, m, Mode::Strict))
                        .or_else(|| ms.iter().find(|m| self.ctx.sub(&am, m, Mode::Erased)))
                        .cloned();
                    if let Some(m) = pick {
                        self.gen_vcs(&am, &m, subject);
                    }
                }
            }
            _ => {}
        }
    }

    fn subsume(&mut self, actual: &Ty, expected: &Ty, subject: &Expr) {
        if !self.ctx.sub(actual, expected, Mode::Erased) {
            let msg = format!("type mismatch: expected `{}`, found `{}`", self.norm(expected), self.norm(actual));
            self.error(subject.span, msg);
            return;
        }
        self.gen_vcs(actual, expected, subject);
    }

    // ---------- expressions ----------

    fn check(&mut self, e: &Expr, expected: &Ty) {
        match &e.kind {
            ExprKind::If(c, t, Some(el)) => {
                self.check(c, &Ty::Bool);
                let m = self.hyps.len();
                self.assume(c, true);
                self.check(t, expected);
                self.hyps.truncate(m);
                self.assume(c, false);
                self.check(el, expected);
                self.hyps.truncate(m);
            }
            ExprKind::Block(stmts, tail) => {
                self.block(stmts, tail.as_deref(), Some(expected));
            }
            ExprKind::Match(s, arms) => {
                self.do_match(s, arms, Some(expected), e.span);
            }
            _ => {
                let actual = self.synth(e);
                self.subsume(&actual, expected, e);
            }
        }
    }

    fn synth(&mut self, e: &Expr) -> Ty {
        match &e.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Var(n) => self.var_ty(n, e.span),
            ExprKind::Record(fs) => {
                let mut fields = BTreeMap::new();
                for (l, fe) in fs {
                    let t = self.synth(fe);
                    if fields.insert(l.clone(), t).is_some() {
                        self.error(fe.span, format!("duplicate field `{l}`"));
                    }
                }
                self.norm(&Ty::Record { fields, open: false })
            }
            ExprKind::Field(b, l) => {
                let bt = self.synth(b);
                match self.try_field(&bt, l) {
                    Some(t) => t,
                    None => {
                        let what = if matches!(self.norm(&bt).strip(), Ty::Union(_) | Ty::Enum(_)) {
                            format!("not every member of `{}` has a field `{l}`", self.norm(&bt))
                        } else {
                            format!("type `{}` has no field `{l}`", self.norm(&bt))
                        };
                        self.error(e.span, what);
                        Ty::Error
                    }
                }
            }
            ExprKind::Call(f, args) => self.synth_call(f, args, e.span),
            ExprKind::Variant { enum_name, variant, payload } => {
                let Some(pt) = self.ctx.payload(enum_name, variant).cloned() else {
                    if self.ctx.enums.contains_key(enum_name) {
                        self.error(e.span, format!("enum `{enum_name}` has no variant `{variant}`"));
                    } else {
                        self.error(e.span, format!("unknown enum `{enum_name}`"));
                    }
                    if let Some(p) = payload {
                        self.synth(p);
                    }
                    return Ty::Error;
                };
                match payload {
                    Some(p) => self.check(p, &pt),
                    None if pt != Ty::unit() => {
                        self.error(e.span, format!("variant `{enum_name}.{variant}` requires a payload of type `{pt}`"))
                    }
                    None => {}
                }
                Ty::Variant(enum_name.clone(), variant.clone())
            }
            ExprKind::Unary(op, x) => {
                let t = self.synth(x);
                match (op, self.prim(&t)) {
                    (_, Ty::Error) => Ty::Error,
                    (UnOp::Neg, Ty::Int) => Ty::Int,
                    (UnOp::Neg, Ty::Float) => Ty::Float,
                    (UnOp::Not, Ty::Bool) => Ty::Bool,
                    _ => {
                        let sym = if *op == UnOp::Neg { "-" } else { "!" };
                        self.error(e.span, format!("operator `{sym}` cannot be applied to `{t}`"));
                        Ty::Error
                    }
                }
            }
            ExprKind::Binary(op, l, r) => self.synth_binary(*op, l, r, e.span),
            ExprKind::If(c, t, el) => {
                self.check(c, &Ty::Bool);
                let m = self.hyps.len();
                self.assume(c, true);
                let tt = self.synth(t);
                self.hyps.truncate(m);
                match el {
                    Some(el) => {
                        self.assume(c, false);
                        let et = self.synth(el);
                        self.hyps.truncate(m);
                        self.ctx.union(vec![tt, et])
                    }
                    None => {
                        if !self.ctx.sub(&tt, &Ty::unit(), Mode::Erased) {
                            self.error(t.span, format!("`if` without `else` must have type `()`, found `{tt}`"));
                        }
                        Ty::unit()
                    }
                }
            }
            ExprKind::Match(s, arms) => self.do_match(s, arms, None, e.span),
            ExprKind::Block(stmts, tail) => self.block(stmts, tail.as_deref(), None),
        }
    }

    fn var_ty(&mut self, n: &str, span: Span) -> Ty {
        if let Some(t) = self.lookup(n) {
            return t.clone();
        }
        if let Some(sig) = self.fns.get(n) {
            return Ty::Fn(sig.params.iter().map(|(_, t)| t.clone()).collect(), Box::new(sig.ret.clone()));
        }
        if n == "panic" {
            return Ty::Fn(vec![Ty::Str], Box::new(Ty::Never));
        }
        if self.ctx.nominals.contains_key(n) {
            self.error(span, format!("nominal type `{n}` is not a value; construct one with `{n}(..)`"));
        } else {
            self.error(span, format!("unknown variable `{n}`"));
        }
        Ty::Error
    }

    fn synth_call(&mut self, f: &Expr, args: &[Expr], span: Span) -> Ty {
        if let ExprKind::Var(name) = &f.kind {
            if self.lookup(name).is_none() {
                // Nominal construction: the argument must satisfy the underlying type,
                // including its refinement (which then becomes an invariant).
                if let Some(under) = self.ctx.nominals.get(name).cloned() {
                    if args.len() != 1 {
                        self.error(span, format!("`{name}(..)` takes exactly one argument"));
                        for a in args {
                            self.synth(a);
                        }
                    } else {
                        self.check(&args[0], &under);
                    }
                    return Ty::Nominal(name.clone());
                }
                // Direct call to a declared function: instantiate dependent refinements.
                if let Some(sig) = self.fns.get(name).cloned() {
                    if args.len() != sig.params.len() {
                        self.error(span, format!("`{name}` takes {} argument(s), got {}", sig.params.len(), args.len()));
                        for a in args {
                            self.synth(a);
                        }
                        return sig.ret;
                    }
                    let mut map: Vec<(String, Expr)> = Vec::new();
                    for ((p, pt), a) in sig.params.iter().zip(args) {
                        self.check(a, &pt.subst_many(&map));
                        map.push((p.clone(), a.clone()));
                    }
                    return sig.ret.subst_many(&map);
                }
            }
        }
        let ft = self.synth(f);
        match self.prim(&ft) {
            Ty::Fn(ps, r) => {
                if ps.len() != args.len() {
                    self.error(span, format!("expected {} argument(s), got {}", ps.len(), args.len()));
                    for a in args {
                        self.synth(a);
                    }
                } else {
                    for (p, a) in ps.iter().zip(args) {
                        self.check(a, p);
                    }
                }
                *r
            }
            Ty::Error => Ty::Error,
            _ => {
                self.error(f.span, format!("`{ft}` is not callable"));
                Ty::Error
            }
        }
    }

    fn synth_binary(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span) -> Ty {
        use BinOp::*;
        if matches!(op, And | Or) {
            self.check(l, &Ty::Bool);
            // Short-circuit: the right side runs only when the left decided nothing.
            let m = self.hyps.len();
            self.assume(l, op == And);
            self.check(r, &Ty::Bool);
            self.hyps.truncate(m);
            return Ty::Bool;
        }
        let lt = self.synth(l);
        let rt = self.synth(r);
        let (a, b) = (self.prim(&lt), self.prim(&rt));
        let arith = matches!(op, Add | Sub | Mul | Div | Rem);
        if a == Ty::Error || b == Ty::Error {
            return if arith { Ty::Error } else { Ty::Bool };
        }
        match op {
            Add | Sub | Mul | Div | Rem => match (&a, &b) {
                (Ty::Int, Ty::Int) => Ty::Int,
                (Ty::Float, Ty::Float) => Ty::Float,
                (Ty::Str, Ty::Str) if op == Add => Ty::Str,
                _ => {
                    self.error(span, format!("operator `{}` cannot be applied to `{lt}` and `{rt}`", op.symbol()));
                    Ty::Error
                }
            },
            Lt | Le | Gt | Ge => {
                if !matches!((&a, &b), (Ty::Int, Ty::Int) | (Ty::Float, Ty::Float) | (Ty::Str, Ty::Str)) {
                    self.error(span, format!("operator `{}` cannot be applied to `{lt}` and `{rt}`", op.symbol()));
                }
                Ty::Bool
            }
            Eq | Ne => {
                if !(self.ctx.sub(&lt, &rt, Mode::Erased) || self.ctx.sub(&rt, &lt, Mode::Erased)) {
                    self.error(span, format!("cannot compare `{lt}` with `{rt}`"));
                }
                Ty::Bool
            }
            And | Or => unreachable!(),
        }
    }

    fn block(&mut self, stmts: &[Stmt], tail: Option<&Expr>, expected: Option<&Ty>) -> Ty {
        let (scope_mark, hyp_mark) = (self.scope.len(), self.hyps.len());
        let mut diverges = false;
        for s in stmts {
            match s {
                Stmt::Let { name, ty, value, span } => {
                    let t = match ty {
                        Some(te) => {
                            let t = self.resolve(te);
                            self.check(value, &t);
                            t
                        }
                        None => self.synth(value),
                    };
                    diverges |= self.norm(&t) == Ty::Never;
                    if name == "_" {
                        continue;
                    }
                    self.bind_var(name, t.clone(), *span);
                    if value.is_pure() && matches!(self.prim(&t), Ty::Int | Ty::Float | Ty::Bool | Ty::Str) {
                        self.hyps.push(Expr::binary(BinOp::Eq, Expr::var(name, *span), value.clone()));
                    }
                }
                Stmt::Expr(e) => {
                    let t = self.synth(e);
                    diverges |= self.norm(&t) == Ty::Never;
                }
            }
        }
        let result = match (tail, expected) {
            (Some(e), Some(x)) => {
                self.check(e, x);
                x.clone()
            }
            (Some(e), None) => self.synth(e),
            (None, _) => {
                let t = if diverges { Ty::Never } else { Ty::unit() };
                if let Some(x) = expected {
                    if !self.ctx.sub(&t, x, Mode::Erased) {
                        let span = stmts.last().map_or(Span::default(), |s| match s {
                            Stmt::Let { span, .. } => *span,
                            Stmt::Expr(e) => e.span,
                        });
                        self.error(span, format!("expected `{x}`, but this block has no tail expression and evaluates to `()`"));
                    }
                }
                t
            }
        };
        self.scope.truncate(scope_mark);
        self.hyps.truncate(hyp_mark);
        result
    }

    // ---------- pattern matching ----------

    fn do_match(&mut self, scrut: &Expr, arms: &[Arm], expected: Option<&Ty>, span: Span) -> Ty {
        let st = self.synth(scrut);
        let mut remaining = self.norm(&st);
        let mut arm_tys = Vec::new();
        for arm in arms {
            let rp = self.resolve_pat(&arm.pat);
            if remaining == Ty::Never && st != Ty::Never {
                self.warn(arm.pat.span, "unreachable match arm: earlier arms cover every case");
            }
            let target = if remaining == Ty::Never { st.clone() } else { remaining.clone() };
            let (scope_mark, hyp_mark) = (self.scope.len(), self.hyps.len());
            self.bind_pat(&rp, &target, Some(scrut));
            match expected {
                Some(x) => self.check(&arm.body, x),
                None => {
                    let t = self.synth(&arm.body);
                    arm_tys.push(t);
                }
            }
            self.scope.truncate(scope_mark);
            self.hyps.truncate(hyp_mark);
            let left: Vec<Ty> = self.ctx.members(&remaining).into_iter().filter(|m| !self.covers(&rp, m)).collect();
            remaining = self.ctx.union(left);
        }
        if remaining != Ty::Never && remaining != Ty::Error {
            self.error(span, format!("non-exhaustive match: values of type `{remaining}` are not covered"));
        }
        match expected {
            Some(x) => x.clone(),
            None => self.ctx.union(arm_tys),
        }
    }

    fn resolve_pat(&mut self, p: &Pat) -> RPat {
        match &p.kind {
            PatKind::Wild => RPat::Wild,
            PatKind::Bind(n) => RPat::Bind(n.clone(), p.span),
            PatKind::Typed(n, te) => {
                let t = self.resolve(te);
                if t.contains_fn() {
                    self.error(te.span, "function types cannot be tested at runtime");
                }
                RPat::Typed(n.clone(), t, p.span)
            }
            PatKind::Lit(e) => RPat::Lit(e.clone()),
            PatKind::Variant { enum_name, variant, payload } => {
                let payload = match payload {
                    Some((fs, open)) => {
                        let mut out = Vec::new();
                        for (l, fp) in fs {
                            out.push((l.clone(), self.resolve_pat(fp)));
                        }
                        Some((out, *open))
                    }
                    None => None,
                };
                RPat::Variant { e: enum_name.clone(), v: variant.clone(), payload, span: p.span }
            }
            PatKind::Record(fs, open) => {
                let mut out = Vec::new();
                for (l, fp) in fs {
                    out.push((l.clone(), self.resolve_pat(fp)));
                }
                RPat::Record(out, *open, p.span)
            }
        }
    }

    /// Bind the variables of `p` matched against a value of type `t`, narrowing as we go.
    fn bind_pat(&mut self, p: &RPat, t: &Ty, subject: Option<&Expr>) {
        match p {
            RPat::Wild => {}
            RPat::Bind(x, span) => self.bind_var(x, t.clone(), *span),
            RPat::Typed(name, ty, span) => {
                let n = self.ctx.narrow(t, ty);
                let bound = if n == Ty::Never {
                    self.error(*span, format!("a value of type `{t}` can never have type `{ty}`"));
                    ty.clone()
                } else {
                    n
                };
                if let Some(x) = name {
                    self.bind_var(x, bound, *span);
                }
            }
            RPat::Lit(lit) => {
                let lt = self.synth(lit);
                if !self.ctx.members(t).iter().any(|m| self.ctx.sub(&lt, m, Mode::Erased)) {
                    self.error(lit.span, format!("literal of type `{lt}` can never match a value of type `{t}`"));
                }
                if let Some(s) = subject.filter(|s| s.is_pure()) {
                    self.hyps.push(Expr::binary(BinOp::Eq, s.clone(), lit.clone()));
                }
            }
            RPat::Variant { e, v, payload, span } => {
                let Some(pt) = self.ctx.payload(e, v).cloned() else {
                    self.error(*span, format!("unknown enum variant `{e}.{v}`"));
                    return;
                };
                let vt = Ty::Variant(e.clone(), v.clone());
                if self.ctx.narrow(t, &vt) == Ty::Never {
                    self.error(*span, format!("variant `{e}.{v}` can never match a value of type `{t}`"));
                }
                if let Some((fps, open)) = payload {
                    self.bind_fields(fps, *open, &pt, *span, &format!("variant `{e}.{v}`"));
                }
            }
            RPat::Record(fps, open, span) => {
                let ms: Vec<Ty> =
                    self.ctx.members(t).into_iter().filter(|m| self.record_compatible(m, fps, *open)).collect();
                if ms.is_empty() {
                    self.error(*span, format!("this record pattern can never match a value of type `{t}`"));
                }
                let rt = self.ctx.union(ms);
                for (l, fp) in fps {
                    let ft = self.try_field(&rt, l).unwrap_or(Ty::Error);
                    let sub = subject.map(|s| project(s, l));
                    self.bind_pat(fp, &ft, sub.as_ref());
                }
            }
        }
    }

    fn bind_fields(&mut self, fps: &[(Label, RPat)], open: bool, rec: &Ty, span: Span, what: &str) {
        let Ty::Record { fields, .. } = self.prim(rec) else { return };
        for (l, fp) in fps {
            let ft = match fields.get(l) {
                Some(ft) => ft.clone(),
                None => {
                    self.error(span, format!("{what} has no field `{l}`"));
                    Ty::Error
                }
            };
            self.bind_pat(fp, &ft, None);
        }
        if !open {
            let missing: Vec<String> =
                fields.keys().filter(|k| !fps.iter().any(|(l, _)| l == *k)).map(|k| format!("`{k}`")).collect();
            if !missing.is_empty() {
                self.error(
                    span,
                    format!("pattern for {what} is missing field(s) {}; add `..` to ignore them", missing.join(", ")),
                );
            }
        }
    }

    fn record_compatible(&self, m: &Ty, fps: &[(Label, RPat)], open: bool) -> bool {
        match m.strip() {
            Ty::Error => true,
            Ty::Record { fields, .. } => {
                fps.iter().all(|(l, _)| fields.contains_key(l)) && (open || fields.len() == fps.len())
            }
            _ => false,
        }
    }

    /// Does `p` match every value of the single (non-union) type `m`?
    fn covers(&self, p: &RPat, m: &Ty) -> bool {
        match p {
            RPat::Wild | RPat::Bind(..) => true,
            RPat::Typed(_, ty, _) => self.ctx.sub(m, ty, Mode::Strict),
            RPat::Lit(_) => false,
            RPat::Variant { e, v, payload, .. } => {
                let Ty::Variant(me, mv) = m.strip() else { return false };
                if me != e || mv != v {
                    return false;
                }
                let Some((fps, _)) = payload else { return true };
                let Some(pt) = self.ctx.payload(e, v) else { return false };
                fps.iter().all(|(l, fp)| self.try_field(pt, l).is_some_and(|ft| self.covers_ty(fp, &ft)))
            }
            RPat::Record(fps, open, _) => {
                // An exact pattern can't cover an open record: the value may have more fields.
                let member_open = matches!(m.strip(), Ty::Record { open: true, .. });
                self.record_compatible(m, fps, *open)
                    && (*open || !member_open)
                    && fps.iter().all(|(l, fp)| self.try_field(m, l).is_some_and(|ft| self.covers_ty(fp, &ft)))
            }
        }
    }

    fn covers_ty(&self, p: &RPat, t: &Ty) -> bool {
        self.ctx.members(t).iter().all(|m| self.covers(p, m))
    }

    /// The type of `t.l`, if every member of `t` has field `l`.
    fn try_field(&self, t: &Ty, l: &Label) -> Option<Ty> {
        let n = self.norm(t);
        match n.strip() {
            Ty::Error => Some(Ty::Error),
            Ty::Never => Some(Ty::Never),
            Ty::Record { fields, .. } => fields.get(l).cloned(),
            Ty::Nominal(name) if *l == Label::Pos(0) => self.ctx.nominals.get(name).cloned(),
            Ty::Variant(e, v) => self.ctx.payload(e, v).and_then(|p| self.try_field(p, l)),
            s @ (Ty::Union(_) | Ty::Enum(_)) => {
                let mut out = Vec::new();
                for m in self.ctx.members(s) {
                    out.push(self.try_field(&m, l)?);
                }
                Some(self.ctx.union(out))
            }
            _ => None,
        }
    }
}
