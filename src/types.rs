use std::collections::HashMap;
use std::fmt;

use crate::ast::{BinOp, Expr, Lit, TypeExpr, TypeKind, fmt_key};
use crate::diag::Diag;
use crate::eval::{Value, eval_closed};

#[derive(Debug, Clone, PartialEq)]
pub enum Presence {
    Required,
    /// `f?: T`: may be absent; reads as `T | null`.
    Optional,
    /// `f: T = e` (or a single-literal type): filled in when absent.
    Default(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub presence: Presence,
}

impl Field {
    /// Guaranteed to be present on a value that has been checked against the record.
    pub fn present(&self) -> bool {
        !matches!(self.presence, Presence::Optional)
    }

    /// The type of reading `x.f`.
    pub fn read_ty(&self) -> Ty {
        match self.presence {
            Presence::Optional => Ty::Union(vec![self.ty.clone(), Ty::null()]),
            _ => self.ty.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rec {
    pub fields: Vec<Field>,
    pub open: bool,
}

impl Rec {
    pub fn get(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// Produced after a reported error; compatible with everything to avoid cascades.
    Error,
    Never,
    Json,
    Bool,
    Int,
    Float,
    Str,
    Lit(Lit),
    List(Box<Ty>),
    Tuple(Vec<Ty>),
    Map(Box<Ty>),
    Record(Rec),
    Union(Vec<Ty>),
    /// Predicate over `self`.
    Refine(Box<Ty>, Expr),
    /// A named type alias, unfolded on demand (keeps names in messages).
    Alias(String),
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
    pub fn null() -> Ty {
        Ty::Lit(Lit::Null)
    }

    /// The type with outer refinements removed.
    pub fn strip(&self) -> &Ty {
        match self {
            Ty::Refine(b, _) => b.strip(),
            t => t,
        }
    }

    fn json_members() -> Vec<Ty> {
        vec![
            Ty::null(),
            Ty::Lit(Lit::Bool(true)),
            Ty::Lit(Lit::Bool(false)),
            Ty::Float,
            Ty::Str,
            Ty::List(Box::new(Ty::Json)),
            Ty::Map(Box::new(Ty::Json)),
        ]
    }

    pub fn subst_many(&self, map: &[(String, Expr)]) -> Ty {
        let go = |t: &Ty| t.subst_many(map);
        match self {
            Ty::Refine(b, p) => Ty::Refine(Box::new(go(b)), p.subst_many(map)),
            Ty::Record(r) => Ty::Record(Rec {
                fields: r.fields.iter().map(|f| Field { ty: go(&f.ty), ..f.clone() }).collect(),
                open: r.open,
            }),
            Ty::List(t) => Ty::List(Box::new(go(t))),
            Ty::Map(t) => Ty::Map(Box::new(go(t))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(go).collect()),
            Ty::Union(ms) => Ty::Union(ms.iter().map(go).collect()),
            t => t.clone(),
        }
    }

    fn fmt_nested(&self, f: &mut fmt::Formatter<'_>, nested: bool) -> fmt::Result {
        let paren = nested && matches!(self, Ty::Union(_) | Ty::Refine(..));
        if paren {
            write!(f, "(")?;
        }
        match self {
            Ty::Error => write!(f, "<error>")?,
            Ty::Never => write!(f, "Never")?,
            Ty::Json => write!(f, "Json")?,
            Ty::Bool => write!(f, "Bool")?,
            Ty::Int => write!(f, "Int")?,
            Ty::Float => write!(f, "Float")?,
            Ty::Str => write!(f, "Str")?,
            Ty::Lit(l) => write!(f, "{l}")?,
            Ty::Alias(n) => write!(f, "{n}")?,
            Ty::List(t) => {
                write!(f, "[")?;
                t.fmt_nested(f, false)?;
                write!(f, "]")?;
            }
            Ty::Map(t) => {
                write!(f, "{{ [Str]: ")?;
                t.fmt_nested(f, false)?;
                write!(f, " }}")?;
            }
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    t.fmt_nested(f, false)?;
                }
                if ts.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")?;
            }
            Ty::Record(r) => {
                if r.fields.is_empty() && !r.open {
                    write!(f, "{{}}")?;
                } else {
                    write!(f, "{{ ")?;
                    for (i, fl) in r.fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        let q = if fl.presence == Presence::Optional { "?" } else { "" };
                        write!(f, "{}{q}: ", fmt_key(&fl.name))?;
                        fl.ty.fmt_nested(f, false)?;
                        if let Presence::Default(e) = &fl.presence {
                            if !matches!(fl.ty, Ty::Lit(_)) {
                                write!(f, " = {e}")?;
                            }
                        }
                    }
                    if r.open {
                        write!(f, "{}..", if r.fields.is_empty() { "" } else { ", " })?;
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

/// Every conjunct of `q` appears in `p`. A syntactic stand-in for a solver.
pub fn implies(p: &Expr, q: &Expr) -> bool {
    let ps = p.conjuncts();
    q.conjuncts().iter().all(|c| ps.contains(c))
}

/// Type aliases and the type algebra over them.
#[derive(Debug, Default, Clone)]
pub struct TypeCtx {
    pub alias_src: HashMap<String, TypeExpr>,
    pub aliases: HashMap<String, Ty>,
    resolving: Vec<String>,
    /// Diagnostics from resolution; the checker drains these.
    pub diags: Vec<Diag>,
}

pub const BUILTIN_TYPES: [&str; 6] = ["Never", "Json", "Bool", "Int", "Float", "Str"];

impl TypeCtx {
    // ---------- resolution ----------

    pub fn resolve(&mut self, te: &TypeExpr) -> Ty {
        let t = self.resolve_raw(te);
        self.norm(&t)
    }

    fn resolve_raw(&mut self, te: &TypeExpr) -> Ty {
        match &te.kind {
            TypeKind::Named(n) => match n.as_str() {
                "Never" => Ty::Never,
                "Json" => Ty::Json,
                "Bool" => Ty::Bool,
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Str" => Ty::Str,
                _ if self.alias_src.contains_key(n) => {
                    self.resolve_alias(n, te);
                    Ty::Alias(n.clone())
                }
                _ => {
                    self.diags.push(Diag::new(te.span, format!("unknown type `{n}`")));
                    Ty::Error
                }
            },
            TypeKind::Lit(l) => Ty::Lit(l.clone()),
            TypeKind::List(t) => Ty::List(Box::new(self.resolve_raw(t))),
            TypeKind::Map(t) => Ty::Map(Box::new(self.resolve_raw(t))),
            TypeKind::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve_raw(t)).collect()),
            TypeKind::Union(ms) => Ty::Union(ms.iter().map(|m| self.resolve_raw(m)).collect()),
            TypeKind::Refine(b, p) => Ty::Refine(Box::new(self.resolve(b)), p.clone()),
            TypeKind::Record { fields, open } => {
                let mut out: Vec<Field> = Vec::new();
                for fd in fields {
                    if out.iter().any(|f| f.name == fd.name) {
                        self.diags.push(Diag::new(fd.span, format!("duplicate field `{}`", fd.name)));
                        continue;
                    }
                    let ty = self.resolve(&fd.ty);
                    let presence = match (&fd.default, fd.optional) {
                        (Some(_), true) => {
                            self.diags.push(Diag::new(
                                fd.span,
                                format!("field `{}` has a default, so it is already optional; remove the `?`", fd.name),
                            ));
                            Presence::Optional
                        }
                        (Some(e), false) => Presence::Default(e.clone()),
                        (None, true) => Presence::Optional,
                        // A field whose type has exactly one value fills itself in.
                        (None, false) => match self.unfold(&ty) {
                            Ty::Lit(l) => Presence::Default(Expr::lit(l, fd.span)),
                            _ => Presence::Required,
                        },
                    };
                    out.push(Field { name: fd.name.clone(), ty, presence });
                }
                Ty::Record(Rec { fields: out, open: *open })
            }
        }
    }

    fn resolve_alias(&mut self, n: &str, at: &TypeExpr) {
        if self.aliases.contains_key(n) {
            return;
        }
        if self.resolving.iter().any(|r| r == n) {
            self.diags.push(Diag::new(
                at.span,
                format!("type alias `{n}` is recursive; use `Json` for recursive data"),
            ));
            self.aliases.insert(n.to_string(), Ty::Error);
            return;
        }
        let src = self.alias_src[n].clone();
        self.resolving.push(n.to_string());
        let t = self.resolve(&src);
        self.resolving.pop();
        self.aliases.entry(n.to_string()).or_insert(t);
    }

    /// Expand an alias (repeatedly) to its definition.
    pub fn unfold(&self, t: &Ty) -> Ty {
        match t {
            Ty::Alias(n) => self.aliases.get(n).map_or(Ty::Error, |t| self.unfold(t)),
            t => t.clone(),
        }
    }

    /// Unfold aliases and strip refinements: the shape of a value.
    pub fn shape(&self, t: &Ty) -> Ty {
        match self.unfold(&self.norm(t)) {
            Ty::Refine(b, _) => self.shape(&b),
            t => t,
        }
    }

    // ---------- normalization (SPEC §3.2) ----------

    pub fn union(&self, ts: Vec<Ty>) -> Ty {
        self.norm(&Ty::Union(ts))
    }

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
                let t = Ty::Lit(Lit::Bool(true));
                let f = Ty::Lit(Lit::Bool(false));
                if flat.contains(&t) && flat.contains(&f) {
                    let pos = flat.iter().position(|m| *m == t || *m == f).unwrap();
                    flat.retain(|m| *m != t && *m != f);
                    flat.insert(pos.min(flat.len()), Ty::Bool);
                }
                let mut kept = Vec::new();
                for (i, m) in flat.iter().enumerate() {
                    // Drop `m` if another member contains it; among equivalent members keep the first.
                    let subsumed = flat.iter().enumerate().any(|(j, o)| {
                        j != i && self.sub_n(m, o, Mode::Strict) && (j < i || !self.sub_n(o, m, Mode::Strict))
                    });
                    if !subsumed {
                        kept.push(m.clone());
                    }
                }
                match kept.len() {
                    0 => Ty::Never,
                    1 => kept.pop().unwrap(),
                    _ => Ty::Union(kept),
                }
            }
            Ty::Record(r) => {
                let mut fields = Vec::new();
                for f in &r.fields {
                    let ty = self.norm(&f.ty);
                    if ty == Ty::Never && f.presence == Presence::Required {
                        return Ty::Never;
                    }
                    fields.push(Field { ty, ..f.clone() });
                }
                Ty::Record(Rec { fields, open: r.open })
            }
            Ty::Tuple(ts) => {
                let ts: Vec<Ty> = ts.iter().map(|t| self.norm(t)).collect();
                if ts.contains(&Ty::Never) { Ty::Never } else { Ty::Tuple(ts) }
            }
            Ty::List(t) => Ty::List(Box::new(self.norm(t))),
            Ty::Map(t) => Ty::Map(Box::new(self.norm(t))),
            Ty::Refine(b, p) => {
                let nb = self.norm(b);
                match eval_closed(p) {
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
            Ty::Alias(_) if self.unfold(t) == Ty::Never => Ty::Never,
            t => t.clone(),
        }
    }

    /// The union members of `t`, with aliases unfolded and `Bool`/`Json` expanded.
    pub fn members(&self, t: &Ty) -> Vec<Ty> {
        match self.unfold(&self.norm(t)) {
            Ty::Never => vec![],
            Ty::Union(ms) => ms.iter().flat_map(|m| self.members(m)).collect(),
            Ty::Bool => vec![Ty::Lit(Lit::Bool(true)), Ty::Lit(Lit::Bool(false))],
            Ty::Json => Ty::json_members(),
            m => vec![m],
        }
    }

    // ---------- subtyping (SPEC §3.1) ----------

    pub fn sub(&self, a: &Ty, b: &Ty, mode: Mode) -> bool {
        self.sub_n(&self.norm(a), &self.norm(b), mode)
    }

    fn sub_n(&self, a: &Ty, b: &Ty, mode: Mode) -> bool {
        use Ty::*;
        if a == b {
            return true;
        }
        match (a, b) {
            (Error, _) | (_, Error) | (Never, _) | (_, Json) => true,
            (Alias(_), _) => self.sub_n(&self.unfold(a), b, mode),
            (_, Alias(_)) => self.sub_n(a, &self.unfold(b), mode),
            (Union(ms), _) => ms.iter().all(|m| self.sub_n(m, b, mode)),
            (Json, _) => Ty::json_members().iter().all(|m| self.sub_n(m, b, mode)),
            (Bool, _) => {
                self.sub_n(&Lit(crate::ast::Lit::Bool(true)), b, mode)
                    && self.sub_n(&Lit(crate::ast::Lit::Bool(false)), b, mode)
            }
            (_, Union(ms)) => {
                ms.iter().any(|m| self.sub_n(a, m, mode)) || matches!(a, Refine(ab, _) if self.sub_n(ab, b, mode))
            }
            (Refine(ab, p), Refine(bb, q)) => self.sub_n(ab, bb, mode) && (mode == Mode::Erased || implies(p, q)),
            (Refine(ab, _), _) => self.sub_n(ab, b, mode),
            (_, Refine(bb, _)) => mode == Mode::Erased && self.sub_n(a, bb, mode),
            (Lit(l), Bool) => matches!(l, crate::ast::Lit::Bool(_)),
            (Lit(l), Int) => matches!(l, crate::ast::Lit::Int(_)),
            (Lit(l), Float) => matches!(l, crate::ast::Lit::Int(_) | crate::ast::Lit::Float(_)),
            (Lit(l), Str) => matches!(l, crate::ast::Lit::Str(_)),
            (Int, Float) => true,
            (List(x), List(y)) => self.sub_n(x, y, mode),
            (Tuple(xs), Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.sub_n(x, y, mode)),
            (Tuple(xs), List(y)) => xs.iter().all(|x| self.sub_n(x, y, mode)),
            (Map(x), Map(y)) => self.sub_n(x, y, mode),
            (Record(r), Map(y)) => {
                r.fields.iter().all(|f| self.sub_n(&f.ty, y, mode)) && (!r.open || self.sub_n(&Json, y, mode))
            }
            (Map(x), Record(r)) => {
                r.open && r.fields.iter().all(|f| f.presence != Presence::Required && self.sub_n(x, &f.ty, mode))
            }
            (Record(ra), Record(rb)) => {
                if ra.open && !rb.open {
                    return false;
                }
                let targets_ok = rb.fields.iter().all(|bf| match ra.get(&bf.name) {
                    None => bf.presence != Presence::Required,
                    Some(af) => {
                        !(bf.presence == Presence::Required && !af.present()) && self.sub_n(&af.ty, &bf.ty, mode)
                    }
                });
                targets_ok && (rb.open || ra.fields.iter().all(|af| rb.get(&af.name).is_some()))
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
