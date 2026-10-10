use std::fmt;

use crate::diag::Span;

/// Record label. Tuples are records with positional labels `0, 1, ...`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// True if the labels are exactly `0, 1, ..., n-1` in order (i.e. a tuple).
pub fn is_positional<'a>(labels: impl Iterator<Item = &'a Label>) -> bool {
    labels.enumerate().all(|(i, l)| *l == Label::Pos(i))
}

pub fn positional<T>(items: Vec<T>) -> Vec<(Label, T)> {
    items.into_iter().enumerate().map(|(i, x)| (Label::Pos(i), x)).collect()
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
        }
    }

    pub fn prec(self) -> u8 {
        match self {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
            BinOp::Add | BinOp::Sub => 4,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 5,
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
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Var(String),
    /// Record literal; tuples use positional labels.
    Record(Vec<(Label, Expr)>),
    Field(Box<Expr>, Label),
    Call(Box<Expr>, Vec<Expr>),
    /// `E.V`, `E.V(..)`, `E.V { .. }`. The payload, if any, is a `Record` expression.
    Variant {
        enum_name: String,
        variant: String,
        payload: Option<Box<Expr>>,
    },
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<Arm>),
    Block(Vec<Stmt>, Option<Box<Expr>>),
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
    }

    pub fn var(name: &str, span: Span) -> Self {
        Expr::new(ExprKind::Var(name.to_string()), span)
    }

    pub fn field(base: Expr, label: Label) -> Self {
        let span = base.span;
        Expr::new(ExprKind::Field(Box::new(base), label), span)
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

    pub fn is_blocklike(&self) -> bool {
        matches!(self.kind, ExprKind::If(..) | ExprKind::Match(..) | ExprKind::Block(..))
    }

    /// Side-effect-free and expressible in the refinement logic.
    pub fn is_pure(&self) -> bool {
        match &self.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Var(_) => true,
            ExprKind::Field(b, _) => b.is_pure(),
            ExprKind::Unary(_, x) => x.is_pure(),
            ExprKind::Binary(_, l, r) => l.is_pure() && r.is_pure(),
            ExprKind::Record(fs) => fs.iter().all(|(_, e)| e.is_pure()),
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

    /// Simultaneous substitution of variables. Predicates contain no binders,
    /// so capture is not a concern.
    pub fn subst_many(&self, map: &[(String, Expr)]) -> Expr {
        let go = |e: &Expr| Box::new(e.subst_many(map));
        let kind = match &self.kind {
            ExprKind::Var(n) => {
                if let Some((_, e)) = map.iter().find(|(m, _)| m == n) {
                    return e.clone();
                }
                ExprKind::Var(n.clone())
            }
            ExprKind::Record(fs) => ExprKind::Record(fs.iter().map(|(l, e)| (l.clone(), e.subst_many(map))).collect()),
            ExprKind::Field(b, l) => ExprKind::Field(go(b), l.clone()),
            ExprKind::Call(f, args) => ExprKind::Call(go(f), args.iter().map(|a| a.subst_many(map)).collect()),
            ExprKind::Variant { enum_name, variant, payload } => ExprKind::Variant {
                enum_name: enum_name.clone(),
                variant: variant.clone(),
                payload: payload.as_ref().map(|p| go(p)),
            },
            ExprKind::Unary(op, x) => ExprKind::Unary(*op, go(x)),
            ExprKind::Binary(op, l, r) => ExprKind::Binary(*op, go(l), go(r)),
            ExprKind::If(c, t, e) => ExprKind::If(go(c), go(t), e.as_ref().map(|e| go(e))),
            // Match/Block never occur inside predicates; leave them alone.
            other => other.clone(),
        };
        Expr::new(kind, self.span)
    }

    fn fmt_prec(&self, f: &mut fmt::Formatter<'_>, prec: u8) -> fmt::Result {
        match &self.kind {
            ExprKind::Int(i) => write!(f, "{i}"),
            ExprKind::Float(x) => write!(f, "{x:?}"),
            ExprKind::Str(s) => write!(f, "{s:?}"),
            ExprKind::Bool(b) => write!(f, "{b}"),
            ExprKind::Var(n) => write!(f, "{n}"),
            ExprKind::Record(fs) => fmt_record_expr(f, fs),
            ExprKind::Field(b, l) => {
                b.fmt_prec(f, 7)?;
                write!(f, ".{l}")
            }
            ExprKind::Call(func, args) => {
                func.fmt_prec(f, 7)?;
                write!(f, "(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    a.fmt_prec(f, 0)?;
                }
                write!(f, ")")
            }
            ExprKind::Variant { enum_name, variant, payload } => {
                write!(f, "{enum_name}.{variant}")?;
                match payload.as_deref() {
                    Some(Expr { kind: ExprKind::Record(fs), .. }) if !is_positional(fs.iter().map(|(l, _)| l)) => {
                        write!(f, " ")?;
                        fmt_record_expr(f, fs)
                    }
                    Some(p) => p.fmt_prec(f, 0),
                    None => Ok(()),
                }
            }
            ExprKind::Unary(op, x) => {
                write!(f, "{}", if *op == UnOp::Neg { "-" } else { "!" })?;
                x.fmt_prec(f, 6)
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
            ExprKind::If(..) => write!(f, "<if>"),
            ExprKind::Match(..) => write!(f, "<match>"),
            ExprKind::Block(..) => write!(f, "<block>"),
        }
    }
}

fn fmt_record_expr(f: &mut fmt::Formatter<'_>, fs: &[(Label, Expr)]) -> fmt::Result {
    if is_positional(fs.iter().map(|(l, _)| l)) {
        write!(f, "(")?;
        for (i, (_, e)) in fs.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            e.fmt_prec(f, 0)?;
        }
        if fs.len() == 1 {
            write!(f, ",")?;
        }
        return write!(f, ")");
    }
    write!(f, "{{ ")?;
    for (i, (l, e)) in fs.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{l}: ")?;
        e.fmt_prec(f, 0)?;
    }
    write!(f, " }}")
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_prec(f, 0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let { name: String, ty: Option<TypeExpr>, value: Expr, span: Span },
    Expr(Expr),
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
    /// `x: T` or `_: T` — a runtime type test.
    Typed(Option<String>, TypeExpr),
    Lit(Expr),
    /// `E.V`, `E.V(p, ..)`, `E.V { f: p, .. }`; the bool is "open" (`..`).
    Variant { enum_name: String, variant: String, payload: Option<(Vec<(Label, Pat)>, bool)> },
    Record(Vec<(Label, Pat)>, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Named(String),
    Path(String, String),
    Record { fields: Vec<(Label, TypeExpr)>, open: bool },
    Union(Vec<TypeExpr>),
    /// Predicate binder is always `self` (the parser rewrites field/param binders).
    Refine(Box<TypeExpr>, Expr),
    Fn(Vec<TypeExpr>, Box<TypeExpr>),
}

#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub name: String,
    /// A record type expression (positional for tuple variants); `None` for unit variants.
    pub payload: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<(String, TypeExpr, Span)>,
    pub ret: Option<TypeExpr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Type { name: String, ty: TypeExpr, span: Span },
    Nominal { name: String, ty: TypeExpr, span: Span },
    Enum { name: String, variants: Vec<VariantDecl>, span: Span },
    Fn(FnDecl),
}
