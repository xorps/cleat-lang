use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crate::ast::{BinOp, Expr, ExprKind, Label, UnOp, is_positional};

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// Produced after a reported error; compatible with everything to avoid cascades.
    Error,
    Never,
    Int,
    Float,
    Bool,
    Str,
    Nominal(String),
    Enum(String),
    Variant(String, String),
    /// Tuples are records with positional labels.
    Record { fields: BTreeMap<Label, Ty>, open: bool },
    Union(Vec<Ty>),
    /// Predicate over `self`.
    Refine(Box<Ty>, Expr),
    Fn(Vec<Ty>, Box<Ty>),
}

/// `Erased` ignores refinement predicates (base typing; predicates become VCs).
/// `Strict` requires predicates to be implied syntactically (used for
/// normalization, narrowing and exhaustiveness, where we must not guess).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Erased,
    Strict,
}

impl Ty {
    pub fn unit() -> Ty {
        Ty::Record { fields: BTreeMap::new(), open: false }
    }

    pub fn tuple(items: Vec<Ty>) -> Ty {
        Ty::Record { fields: items.into_iter().enumerate().map(|(i, t)| (Label::Pos(i), t)).collect(), open: false }
    }

    /// The type with outer refinements removed.
    pub fn strip(&self) -> &Ty {
        match self {
            Ty::Refine(b, _) => b.strip(),
            t => t,
        }
    }

    pub fn contains_fn(&self) -> bool {
        match self {
            Ty::Fn(..) => true,
            Ty::Record { fields, .. } => fields.values().any(Ty::contains_fn),
            Ty::Union(ms) => ms.iter().any(Ty::contains_fn),
            Ty::Refine(b, _) => b.contains_fn(),
            _ => false,
        }
    }

    /// Substitute variables in every predicate (used to instantiate dependent signatures).
    pub fn subst_many(&self, map: &[(String, Expr)]) -> Ty {
        match self {
            Ty::Refine(b, p) => Ty::Refine(Box::new(b.subst_many(map)), p.subst_many(map)),
            Ty::Record { fields, open } => Ty::Record {
                fields: fields.iter().map(|(l, t)| (l.clone(), t.subst_many(map))).collect(),
                open: *open,
            },
            Ty::Union(ms) => Ty::Union(ms.iter().map(|m| m.subst_many(map)).collect()),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|p| p.subst_many(map)).collect(), Box::new(r.subst_many(map))),
            t => t.clone(),
        }
    }

    fn fmt_nested(&self, f: &mut fmt::Formatter<'_>, nested: bool) -> fmt::Result {
        let paren = nested && matches!(self, Ty::Union(_) | Ty::Refine(..) | Ty::Fn(..));
        if paren {
            write!(f, "(")?;
        }
        match self {
            Ty::Error => write!(f, "<error>")?,
            Ty::Never => write!(f, "Never")?,
            Ty::Int => write!(f, "Int")?,
            Ty::Float => write!(f, "Float")?,
            Ty::Bool => write!(f, "Bool")?,
            Ty::Str => write!(f, "Str")?,
            Ty::Nominal(n) | Ty::Enum(n) => write!(f, "{n}")?,
            Ty::Variant(e, v) => write!(f, "{e}.{v}")?,
            Ty::Record { fields, open } => {
                if !open && is_positional(fields.keys()) {
                    write!(f, "(")?;
                    for (i, t) in fields.values().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        t.fmt_nested(f, false)?;
                    }
                    if fields.len() == 1 {
                        write!(f, ",")?;
                    }
                    write!(f, ")")?;
                } else {
                    write!(f, "{{ ")?;
                    for (i, (l, t)) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{l}: ")?;
                        t.fmt_nested(f, false)?;
                    }
                    if *open {
                        write!(f, "{}..", if fields.is_empty() { "" } else { ", " })?;
                    }
                    write!(f, " }}")?;
                }
            }
            Ty::Union(ms) => {
                for (i, m) in ms.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    m.fmt_nested(f, true)?;
                }
            }
            Ty::Refine(b, p) => {
                b.fmt_nested(f, true)?;
                write!(f, " where {p}")?;
            }
            Ty::Fn(ps, r) => {
                write!(f, "fn(")?;
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    p.fmt_nested(f, false)?;
                }
                write!(f, ") -> ")?;
                r.fmt_nested(f, false)?;
            }
        }
        if paren {
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_nested(f, false)
    }
}

/// Every conjunct of `q` appears in `p`. A syntactic stand-in for SMT.
pub fn implies(p: &Expr, q: &Expr) -> bool {
    let ps = p.conjuncts();
    q.conjuncts().iter().all(|c| ps.contains(c))
}

/// Declarations the type algebra needs: enum variants and nominal underlyings.
#[derive(Default)]
pub struct TypeCtx {
    /// Enum name -> variants in declaration order, each with its payload record type.
    pub enums: HashMap<String, Vec<(String, Ty)>>,
    pub nominals: HashMap<String, Ty>,
}

impl TypeCtx {
    pub fn variants(&self, e: &str) -> Vec<Ty> {
        self.enums
            .get(e)
            .map(|vs| vs.iter().map(|(v, _)| Ty::Variant(e.to_string(), v.clone())).collect())
            .unwrap_or_default()
    }

    pub fn payload(&self, e: &str, v: &str) -> Option<&Ty> {
        self.enums.get(e)?.iter().find(|(n, _)| n == v).map(|(_, t)| t)
    }

    pub fn union(&self, ts: Vec<Ty>) -> Ty {
        self.norm(&Ty::Union(ts))
    }

    /// SPEC §4: flatten unions, drop `Never`, drop subsumed members,
    /// collapse uninhabited products/refinements to `Never`.
    pub fn norm(&self, t: &Ty) -> Ty {
        match t {
            Ty::Union(ms) => {
                let mut flat = Vec::new();
                for m in ms {
                    match self.norm(m) {
                        Ty::Union(inner) => flat.extend(inner),
                        Ty::Never => {}
                        Ty::Error => return Ty::Error,
                        n => flat.push(n),
                    }
                }
                let mut kept = Vec::new();
                for (i, m) in flat.iter().enumerate() {
                    // Drop `m` if another member strictly contains it; among
                    // equivalent members keep the first.
                    let subsumed = flat.iter().enumerate().any(|(j, o)| {
                        j != i && self.sub_n(m, o, Mode::Strict) && (j < i || !self.sub_n(o, m, Mode::Strict))
                    });
                    if !subsumed {
                        kept.push(m.clone());
                    }
                }
                let mut kept = self.collapse_enums(kept);
                match kept.len() {
                    0 => Ty::Never,
                    1 => kept.pop().unwrap(),
                    _ => Ty::Union(kept),
                }
            }
            Ty::Record { fields, open } => {
                let mut nf = BTreeMap::new();
                for (l, ft) in fields {
                    match self.norm(ft) {
                        Ty::Never => return Ty::Never,
                        n => {
                            nf.insert(l.clone(), n);
                        }
                    }
                }
                Ty::Record { fields: nf, open: *open }
            }
            Ty::Refine(b, p) => {
                let nb = self.norm(b);
                match const_eval(p) {
                    Some(Value::Bool(true)) => return nb,
                    Some(Value::Bool(false)) => return Ty::Never,
                    _ => {}
                }
                match nb {
                    Ty::Never => Ty::Never,
                    Ty::Error => Ty::Error,
                    Ty::Refine(bb, q) => Ty::Refine(bb, Expr::binary(BinOp::And, q, p.clone())),
                    other => Ty::Refine(Box::new(other), p.clone()),
                }
            }
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|p| self.norm(p)).collect(), Box::new(self.norm(r))),
            Ty::Enum(e) if self.enums.get(e).is_some_and(|v| v.is_empty()) => Ty::Never,
            t => t.clone(),
        }
    }

    /// Replace a complete set of `E.V` members with `E`.
    fn collapse_enums(&self, mut ms: Vec<Ty>) -> Vec<Ty> {
        let enum_names: Vec<String> = ms
            .iter()
            .filter_map(|m| if let Ty::Variant(e, _) = m { Some(e.clone()) } else { None })
            .collect();
        for e in enum_names {
            let all = self.variants(&e);
            if !all.is_empty() && all.iter().all(|v| ms.contains(v)) {
                let pos = ms.iter().position(|m| all.contains(m)).unwrap();
                ms.retain(|m| !all.contains(m));
                ms.insert(pos.min(ms.len()), Ty::Enum(e));
            }
        }
        ms
    }

    /// The union members of `t`, with enums expanded into their variants.
    pub fn members(&self, t: &Ty) -> Vec<Ty> {
        let expand = |m: Ty| match m {
            Ty::Enum(e) => self.variants(&e),
            m => vec![m],
        };
        match self.norm(t) {
            Ty::Never => vec![],
            Ty::Union(ms) => ms.into_iter().flat_map(expand).collect(),
            m => expand(m),
        }
    }

    pub fn sub(&self, a: &Ty, b: &Ty, mode: Mode) -> bool {
        self.sub_n(&self.norm(a), &self.norm(b), mode)
    }

    /// Subtyping on normalized types (SPEC §3).
    fn sub_n(&self, a: &Ty, b: &Ty, mode: Mode) -> bool {
        use Ty::*;
        if a == b {
            return true;
        }
        match (a, b) {
            (Error, _) | (_, Error) | (Never, _) => true,
            (Union(ms), _) => ms.iter().all(|m| self.sub_n(m, b, mode)),
            (Enum(e), _) => self.variants(e).iter().all(|v| self.sub_n(v, b, mode)),
            (_, Union(ms)) => {
                ms.iter().any(|m| self.sub_n(a, m, mode)) || matches!(a, Refine(ab, _) if self.sub_n(ab, b, mode))
            }
            (Refine(ab, p), Refine(bb, q)) => {
                self.sub_n(ab, bb, mode) && (mode == Mode::Erased || implies(p, q))
            }
            (Refine(ab, _), _) => self.sub_n(ab, b, mode),
            (_, Refine(bb, _)) => mode == Mode::Erased && self.sub_n(a, bb, mode),
            (Variant(e, _), Enum(e2)) => e == e2,
            (Record { fields: af, open: ao }, Record { fields: bf, open: bo }) => {
                if !*bo && (*ao || af.len() != bf.len()) {
                    return false;
                }
                bf.iter().all(|(l, bt)| af.get(l).is_some_and(|at| self.sub_n(at, bt, mode)))
            }
            (Fn(ap, ar), Fn(bp, br)) => {
                ap.len() == bp.len()
                    && ap.iter().zip(bp).all(|(x, y)| self.sub_n(y, x, mode))
                    && self.sub_n(ar, br, mode)
            }
            _ => false,
        }
    }

    /// The part of `t` that also has type `by` (for `x: T` patterns).
    pub fn narrow(&self, t: &Ty, by: &Ty) -> Ty {
        let by_n = self.norm(by);
        let by_ms = self.members(&by_n);
        let mut out = Vec::new();
        for m in self.members(t) {
            if self.sub_n(&m, &by_n, Mode::Strict) {
                out.push(m);
            } else {
                out.extend(by_ms.iter().filter(|b| self.sub_n(b, &m, Mode::Strict)).cloned());
            }
        }
        self.union(out)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// Evaluate a closed expression. Short-circuits `&&`/`||` even when one side is unknown.
pub fn const_eval(e: &Expr) -> Option<Value> {
    use Value as V;
    match &e.kind {
        ExprKind::Int(i) => Some(V::Int(*i)),
        ExprKind::Float(x) => Some(V::Float(*x)),
        ExprKind::Str(s) => Some(V::Str(s.clone())),
        ExprKind::Bool(b) => Some(V::Bool(*b)),
        ExprKind::Unary(UnOp::Neg, x) => match const_eval(x)? {
            V::Int(i) => i.checked_neg().map(V::Int),
            V::Float(f) => Some(V::Float(-f)),
            _ => None,
        },
        ExprKind::Unary(UnOp::Not, x) => match const_eval(x)? {
            V::Bool(b) => Some(V::Bool(!b)),
            _ => None,
        },
        ExprKind::Binary(op @ (BinOp::And | BinOp::Or), l, r) => {
            let short = *op == BinOp::Or; // value that decides the result
            match (const_eval(l), const_eval(r)) {
                (Some(V::Bool(x)), _) | (_, Some(V::Bool(x))) if x == short => Some(V::Bool(short)),
                (Some(V::Bool(_)), Some(V::Bool(_))) => Some(V::Bool(!short)),
                _ => None,
            }
        }
        ExprKind::Binary(op, l, r) => eval_bin(*op, const_eval(l)?, const_eval(r)?),
        _ => None,
    }
}

fn eval_bin(op: BinOp, a: Value, b: Value) -> Option<Value> {
    use BinOp::*;
    use Value as V;
    Some(match (a, b) {
        (V::Int(x), V::Int(y)) => match op {
            Add => V::Int(x.checked_add(y)?),
            Sub => V::Int(x.checked_sub(y)?),
            Mul => V::Int(x.checked_mul(y)?),
            Div => V::Int(x.checked_div(y)?),
            Rem => V::Int(x.checked_rem(y)?),
            Eq => V::Bool(x == y),
            Ne => V::Bool(x != y),
            Lt => V::Bool(x < y),
            Le => V::Bool(x <= y),
            Gt => V::Bool(x > y),
            Ge => V::Bool(x >= y),
            And | Or => return None,
        },
        (V::Float(x), V::Float(y)) => match op {
            Add => V::Float(x + y),
            Sub => V::Float(x - y),
            Mul => V::Float(x * y),
            Div => V::Float(x / y),
            Rem => V::Float(x % y),
            Eq => V::Bool(x == y),
            Ne => V::Bool(x != y),
            Lt => V::Bool(x < y),
            Le => V::Bool(x <= y),
            Gt => V::Bool(x > y),
            Ge => V::Bool(x >= y),
            And | Or => return None,
        },
        (V::Str(x), V::Str(y)) => match op {
            Add => V::Str(x + &y),
            Eq => V::Bool(x == y),
            Ne => V::Bool(x != y),
            Lt => V::Bool(x < y),
            Le => V::Bool(x <= y),
            Gt => V::Bool(x > y),
            Ge => V::Bool(x >= y),
            _ => return None,
        },
        (V::Bool(x), V::Bool(y)) => match op {
            Eq => V::Bool(x == y),
            Ne => V::Bool(x != y),
            _ => return None,
        },
        _ => return None,
    })
}
