use std::fmt;

use crate::diag::Span;

/// Field selector: `x.name` or tuple index `x.0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Label {
    Pos(usize),
    Name(String),
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Label::Pos(i) => write!(f, "{i}"),
            Label::Name(s) => write!(f, "{s}"),
        }
    }
}

/// A literal value; also a literal (singleton) type.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl fmt::Display for Lit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lit::Null => write!(f, "null"),
            Lit::Bool(b) => write!(f, "{b}"),
            Lit::Int(i) => write!(f, "{i}"),
            Lit::Float(x) => write!(f, "{x:?}"),
            Lit::Str(s) => write!(f, "{s:?}"),
        }
    }
}

/// Render an object key, quoting it unless it is a plain identifier.
pub fn fmt_key(k: &str) -> String {
    let plain = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain { k.to_string() } else { format!("{k:?}") }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Coalesce => "??",
        }
    }

    pub fn prec(self) -> u8 {
        match self {
            BinOp::Coalesce => 1,
            BinOp::Or => 2,
            BinOp::And => 3,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 4,
            BinOp::Add | BinOp::Sub => 5,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 6,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// Equality ignores spans, so predicates can be compared syntactically.
impl PartialEq for Expr {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListItem {
    Elem(Expr),
    Spread(Expr),
    If(Expr, Box<ListItem>),
    /// `for a, b in e => item`: one variable binds list elements; two bind
    /// (index, element) for lists and (key, value) for objects.
    For(String, Option<String>, Expr, Box<ListItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Member {
    /// `key: e`. Dotted keys are desugared into nested objects by the parser.
    Field(String, Expr, Span),
    /// `[e]: e`
    Computed(Expr, Expr),
    Spread(Expr),
    If(Expr, Vec<Member>, Option<Vec<Member>>),
    For(String, Option<String>, Expr, Vec<Member>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Lit(Lit),
    Interp(Vec<InterpPart>),
    Var(String),
    List(Vec<ListItem>),
    Tuple(Vec<Expr>),
    Object(Vec<Member>),
    /// `e.f` or, when the flag is set, `e?.f`.
    Field(Box<Expr>, Label, bool),
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Arg>),
    /// `T { ... }`
    Typed(String, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Let(String, Option<Box<TypeExpr>>, Box<Expr>, Box<Expr>),
    Match(Box<Expr>, Vec<Arm>),
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
    }

    pub fn lit(l: Lit, span: Span) -> Self {
        Expr::new(ExprKind::Lit(l), span)
    }

    pub fn var(name: &str, span: Span) -> Self {
        Expr::new(ExprKind::Var(name.to_string()), span)
    }

    pub fn field(base: Expr, label: Label) -> Self {
        let span = base.span;
        Expr::new(ExprKind::Field(Box::new(base), label, false), span)
    }

    pub fn binary(op: BinOp, l: Expr, r: Expr) -> Self {
        let span = l.span;
        Expr::new(ExprKind::Binary(op, Box::new(l), Box::new(r)), span)
    }

    /// Logical negation, flipping comparisons so `!(b == 0)` becomes `b != 0`.
    pub fn negate(&self) -> Expr {
        use BinOp::*;
        if let ExprKind::Binary(op, l, r) = &self.kind {
            let flipped = match op {
                Eq => Some(Ne),
                Ne => Some(Eq),
                Lt => Some(Ge),
                Le => Some(Gt),
                Gt => Some(Le),
                Ge => Some(Lt),
                _ => None,
            };
            if let Some(f) = flipped {
                return Expr::new(ExprKind::Binary(f, l.clone(), r.clone()), self.span);
            }
        }
        if let ExprKind::Unary(UnOp::Not, inner) = &self.kind {
            return (**inner).clone();
        }
        Expr::new(ExprKind::Unary(UnOp::Not, Box::new(self.clone())), self.span)
    }

    /// Simple enough to record as a fact about the program state.
    pub fn is_pure(&self) -> bool {
        match &self.kind {
            ExprKind::Lit(_) | ExprKind::Var(_) => true,
            ExprKind::Field(b, _, _) => b.is_pure(),
            ExprKind::Unary(_, x) => x.is_pure(),
            ExprKind::Binary(_, l, r) => l.is_pure() && r.is_pure(),
            ExprKind::Call(_, args) => args.iter().all(|a| a.value.is_pure()),
            _ => false,
        }
    }

    pub fn conjuncts(&self) -> Vec<&Expr> {
        match &self.kind {
            ExprKind::Binary(BinOp::And, l, r) => {
                let mut v = l.conjuncts();
                v.extend(r.conjuncts());
                v
            }
            _ => vec![self],
        }
    }

    pub fn subst(&self, name: &str, with: &Expr) -> Expr {
        self.subst_many(&[(name.to_string(), with.clone())])
    }

    /// Simultaneous substitution of free variables. Used on predicates, which
    /// may contain binders (`let`, `for`, `match`); bound names are respected.
    pub fn subst_many(&self, map: &[(String, Expr)]) -> Expr {
        if map.is_empty() {
            return self.clone();
        }
        let without = |names: &[&str]| -> Vec<(String, Expr)> {
            map.iter().filter(|(n, _)| !names.contains(&n.as_str())).cloned().collect()
        };
        let go = |e: &Expr| e.subst_many(map);
        let kind = match &self.kind {
            ExprKind::Var(n) => {
                if let Some((_, e)) = map.iter().find(|(m, _)| m == n) {
                    return e.clone();
                }
                ExprKind::Var(n.clone())
            }
            ExprKind::Lit(l) => ExprKind::Lit(l.clone()),
            ExprKind::Interp(ps) => ExprKind::Interp(
                ps.iter()
                    .map(|p| match p {
                        InterpPart::Lit(s) => InterpPart::Lit(s.clone()),
                        InterpPart::Expr(e) => InterpPart::Expr(go(e)),
                    })
                    .collect(),
            ),
            ExprKind::List(items) => ExprKind::List(items.iter().map(|i| subst_item(i, map)).collect()),
            ExprKind::Tuple(es) => ExprKind::Tuple(es.iter().map(go).collect()),
            ExprKind::Object(ms) => ExprKind::Object(subst_members(ms, map)),
            ExprKind::Field(b, l, q) => ExprKind::Field(Box::new(go(b)), l.clone(), *q),
            ExprKind::Index(b, i) => ExprKind::Index(Box::new(go(b)), Box::new(go(i))),
            ExprKind::Call(f, args) => ExprKind::Call(
                f.clone(),
                args.iter().map(|a| Arg { name: a.name.clone(), value: go(&a.value) }).collect(),
            ),
            ExprKind::Typed(t, e) => ExprKind::Typed(t.clone(), Box::new(go(e))),
            ExprKind::Unary(op, x) => ExprKind::Unary(*op, Box::new(go(x))),
            ExprKind::Binary(op, l, r) => ExprKind::Binary(*op, Box::new(go(l)), Box::new(go(r))),
            ExprKind::If(c, t, e) => ExprKind::If(Box::new(go(c)), Box::new(go(t)), Box::new(go(e))),
            ExprKind::Let(n, t, v, b) => {
                ExprKind::Let(n.clone(), t.clone(), Box::new(go(v)), Box::new(b.subst_many(&without(&[n]))))
            }
            ExprKind::Match(s, arms) => ExprKind::Match(
                Box::new(go(s)),
                arms.iter()
                    .map(|a| {
                        let bound = a.pat.binders();
                        let names: Vec<&str> = bound.iter().map(String::as_str).collect();
                        Arm { pat: a.pat.clone(), body: a.body.subst_many(&without(&names)) }
                    })
                    .collect(),
            ),
        };
        Expr::new(kind, self.span)
    }

    fn fmt_prec(&self, f: &mut fmt::Formatter<'_>, prec: u8) -> fmt::Result {
        match &self.kind {
            ExprKind::Lit(l) => write!(f, "{l}"),
            ExprKind::Interp(ps) => {
                write!(f, "\"")?;
                for p in ps {
                    match p {
                        InterpPart::Lit(s) => write!(f, "{}", s.escape_debug())?,
                        InterpPart::Expr(e) => write!(f, "\\({e})")?,
                    }
                }
                write!(f, "\"")
            }
            ExprKind::Var(n) => write!(f, "{n}"),
            ExprKind::List(items) => write!(f, "[{}]", if items.is_empty() { "" } else { "..." }),
            ExprKind::Tuple(es) => {
                write!(f, "(")?;
                for (i, e) in es.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    e.fmt_prec(f, 0)?;
                }
                write!(f, ")")
            }
            ExprKind::Object(ms) => write!(f, "{}", if ms.is_empty() { "{}" } else { "{ ... }" }),
            ExprKind::Field(b, l, q) => {
                b.fmt_prec(f, 9)?;
                write!(f, "{}{}", if *q { "?." } else { "." }, l)
            }
            ExprKind::Index(b, i) => {
                b.fmt_prec(f, 9)?;
                write!(f, "[{i}]")
            }
            ExprKind::Call(name, args) => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Some(n) = &a.name {
                        write!(f, "{n}: ")?;
                    }
                    a.value.fmt_prec(f, 0)?;
                }
                write!(f, ")")
            }
            ExprKind::Typed(t, e) => write!(f, "{t} {e}"),
            ExprKind::Unary(op, x) => {
                write!(f, "{}", if *op == UnOp::Neg { "-" } else { "!" })?;
                x.fmt_prec(f, 8)
            }
            ExprKind::Binary(op, l, r) => {
                let p = op.prec();
                if p < prec {
                    write!(f, "(")?;
                }
                l.fmt_prec(f, p)?;
                write!(f, " {} ", op.symbol())?;
                r.fmt_prec(f, p + 1)?;
                if p < prec {
                    write!(f, ")")?;
                }
                Ok(())
            }
            ExprKind::If(c, t, e) => {
                if prec > 0 {
                    write!(f, "(")?;
                }
                write!(f, "if {c} then {t} else {e}")?;
                if prec > 0 {
                    write!(f, ")")?;
                }
                Ok(())
            }
            ExprKind::Let(n, _, v, b) => write!(f, "(let {n} = {v}; {b})"),
            ExprKind::Match(s, _) => write!(f, "match {s} {{ ... }}"),
        }
    }
}

fn subst_item(item: &ListItem, map: &[(String, Expr)]) -> ListItem {
    match item {
        ListItem::Elem(e) => ListItem::Elem(e.subst_many(map)),
        ListItem::Spread(e) => ListItem::Spread(e.subst_many(map)),
        ListItem::If(c, i) => ListItem::If(c.subst_many(map), Box::new(subst_item(i, map))),
        ListItem::For(a, b, e, i) => {
            let inner: Vec<(String, Expr)> =
                map.iter().filter(|(n, _)| n != a && Some(n) != b.as_ref()).cloned().collect();
            ListItem::For(a.clone(), b.clone(), e.subst_many(map), Box::new(subst_item(i, &inner)))
        }
    }
}

fn subst_members(ms: &[Member], map: &[(String, Expr)]) -> Vec<Member> {
    ms.iter()
        .map(|m| match m {
            Member::Field(k, e, s) => Member::Field(k.clone(), e.subst_many(map), *s),
            Member::Computed(k, v) => Member::Computed(k.subst_many(map), v.subst_many(map)),
            Member::Spread(e) => Member::Spread(e.subst_many(map)),
            Member::If(c, t, e) => {
                Member::If(c.subst_many(map), subst_members(t, map), e.as_ref().map(|e| subst_members(e, map)))
            }
            Member::For(a, b, e, body) => {
                let inner: Vec<(String, Expr)> =
                    map.iter().filter(|(n, _)| n != a && Some(n) != b.as_ref()).cloned().collect();
                Member::For(a.clone(), b.clone(), e.subst_many(map), subst_members(body, &inner))
            }
        })
        .collect()
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_prec(f, 0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arm {
    pub pat: Pat,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pat {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatKind {
    Wild,
    Bind(String),
    /// `x: T` or `_: T`: a runtime type test.
    Typed(Option<String>, TypeExpr),
    Lit(Lit),
    Tuple(Vec<Pat>),
    /// `{ k: p, .. }`; the flag is "open".
    Record(Vec<(String, Pat)>, bool),
}

impl Pat {
    pub fn binders(&self) -> Vec<String> {
        match &self.kind {
            PatKind::Wild | PatKind::Lit(_) | PatKind::Typed(None, _) => vec![],
            PatKind::Bind(n) | PatKind::Typed(Some(n), _) => vec![n.clone()],
            PatKind::Tuple(ps) => ps.iter().flat_map(Pat::binders).collect(),
            PatKind::Record(fs, _) => fs.iter().flat_map(|(_, p)| p.binders()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Named(String),
    Lit(Lit),
    List(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Map(Box<TypeExpr>),
    Record { fields: Vec<FieldDecl>, open: bool },
    Union(Vec<TypeExpr>),
    /// Predicate binder is always `self` (the parser rewrites field/param binders).
    Refine(Box<TypeExpr>, Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeExpr,
    pub optional: bool,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Type { name: String, ty: TypeExpr, span: Span },
    Input { name: String, ty: TypeExpr, default: Option<Expr>, span: Span },
    Let { name: String, ty: Option<TypeExpr>, value: Expr, span: Span },
    Fn(FnDecl),
    /// `export e` or `export name (after a, b)? = e`. Named exports are
    /// components; `after` orders them (Argo sync waves, Flux `dependsOn`).
    Export { name: Option<String>, after: Vec<(String, Span)>, value: Expr, span: Span },
    /// `import name from "path"`: the module's types become `name.T`.
    Import { name: String, path: String, span: Span },
}
