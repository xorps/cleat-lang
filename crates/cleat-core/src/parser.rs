use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{StrPart, Tok, Token, lex};
use crate::prelude::*;

type PResult<T> = Result<T, Diag>;

pub fn parse_program(src: &str) -> PResult<Vec<Item>> {
    let mut p = Parser {
        toks: lex(src)?,
        pos: 0,
        no_struct: false,
    };
    let mut items = Vec::new();
    while *p.peek() != Tok::Eof {
        items.push(p.item()?);
    }
    Ok(items)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// When set, `T {` is not parsed as a typed object literal (match scrutinees, predicates).
    no_struct: bool,
}

fn is_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// In `name: T where φ`, `φ` may refer to the value as `name`; canonicalize it to `self`.
fn rebind(te: TypeExpr, name: &str) -> TypeExpr {
    match te.kind {
        TypeKind::Refine(base, pred) => {
            let this = Expr::var("self", pred.span);
            TypeExpr {
                kind: TypeKind::Refine(base, pred.subst(name, &this)),
                span: te.span,
            }
        }
        kind => TypeExpr { kind, span: te.span },
    }
}

fn binop(t: &Tok) -> Option<BinOp> {
    Some(match t {
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Star => BinOp::Mul,
        Tok::Slash => BinOp::Div,
        Tok::Percent => BinOp::Rem,
        Tok::EqEq => BinOp::Eq,
        Tok::Ne => BinOp::Ne,
        Tok::Lt => BinOp::Lt,
        Tok::Le => BinOp::Le,
        Tok::Gt => BinOp::Gt,
        Tok::Ge => BinOp::Ge,
        Tok::AndAnd => BinOp::And,
        Tok::OrOr => BinOp::Or,
        Tok::QQ => BinOp::Coalesce,
        _ => return None,
    })
}

/// Add `path: value` to an object's members, nesting dotted paths and merging
/// into an existing object literal for the same key. Each path segment keeps its own span.
fn insert_member(members: &mut Vec<Member>, path: &[(String, Span)], value: Expr) {
    let (key, span) = (&path[0].0, path[0].1);
    let idx = members
        .iter()
        .rposition(|m| matches!(m, Member::Field(k, Expr { kind: ExprKind::Object(_), .. }, _) if k == key));
    if path.len() == 1 {
        match (idx, value.kind) {
            (Some(i), ExprKind::Object(new)) => {
                let ms = nested_members(members, i);
                for m in new {
                    match m {
                        Member::Field(k, v, s) => insert_member(ms, &[(k, s)], v),
                        other => ms.push(other),
                    }
                }
            }
            (_, kind) => members.push(Member::Field(key.clone(), Expr::new(kind, value.span), span)),
        }
        return;
    }
    let i = idx.unwrap_or_else(|| {
        members.push(Member::Field(
            key.clone(),
            Expr::new(ExprKind::Object(Vec::new()), span),
            span,
        ));
        members.len() - 1
    });
    insert_member(nested_members(members, i), &path[1..], value);
}

fn nested_members(members: &mut [Member], i: usize) -> &mut Vec<Member> {
    match &mut members[i] {
        Member::Field(
            _,
            Expr {
                kind: ExprKind::Object(ms),
                ..
            },
            _,
        ) => ms,
        _ => unreachable!("index points at an object-valued field"),
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if t.tok != Tok::Eof {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.bump();
            true
        } else {
            false
        }
    }

    fn err<T>(&self, expected: &str) -> PResult<T> {
        Err(Diag::new(
            self.span(),
            format!("expected {expected}, found {}", self.peek().describe()),
        ))
    }

    fn expect(&mut self, t: &Tok) -> PResult<()> {
        if self.eat(t) { Ok(()) } else { self.err(&t.describe()) }
    }

    fn ident(&mut self) -> PResult<(String, Span)> {
        if let Tok::Ident(s) = self.peek().clone() {
            let sp = self.span();
            self.bump();
            Ok((s, sp))
        } else {
            self.err("identifier")
        }
    }

    /// `a` or `a.b.c` (qualified type names).
    fn dotted_ident(&mut self) -> PResult<String> {
        let (mut name, _) = self.ident()?;
        while *self.peek() == Tok::Dot && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            name.push('.');
            name.push_str(&self.ident()?.0);
        }
        Ok(name)
    }

    /// Number of tokens in a `.Ident` chain at the cursor that ends in an
    /// uppercase name followed by `{`: a qualified typed literal (`k8s.Deployment {`).
    fn qualified_struct_len(&self) -> Option<usize> {
        let mut i = 0;
        let mut last_upper = false;
        while *self.peek_at(i) == Tok::Dot {
            match self.peek_at(i + 1) {
                Tok::Ident(n) => last_upper = is_upper(n),
                _ => return None,
            }
            i += 2;
        }
        (i > 0 && last_upper && *self.peek_at(i) == Tok::LBrace && !self.no_struct).then_some(i)
    }

    /// `.a.b(` at the cursor: a call to a function from an imported module (`lib.f(...)`).
    fn qualified_call(&self) -> bool {
        let mut i = 0;
        while *self.peek_at(i) == Tok::Dot && matches!(self.peek_at(i + 1), Tok::Ident(_)) {
            i += 2;
        }
        i > 0 && *self.peek_at(i) == Tok::LParen
    }

    /// A plain string literal (no interpolation), e.g. a quoted key.
    fn plain_str(&self) -> Option<String> {
        match self.peek() {
            Tok::Str(parts) => match parts.as_slice() {
                [StrPart::Lit(s)] => Some(s.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// An object key: identifier, keyword, or plain string.
    fn key(&mut self) -> PResult<(String, Span)> {
        let span = self.span();
        let k = match self.peek() {
            Tok::Ident(s) => s.clone(),
            t if t.keyword_text().is_some() => t.keyword_text().unwrap().to_string(),
            Tok::Str(_) => match self.plain_str() {
                Some(s) => s,
                None => {
                    return Err(Diag::new(
                        span,
                        "object keys cannot use interpolation; use `[expr]: value`",
                    ));
                }
            },
            _ => return self.err("key"),
        };
        self.bump();
        Ok((k, span))
    }

    fn is_key_start(&self) -> bool {
        matches!(self.peek(), Tok::Ident(_) | Tok::Str(_)) || self.peek().keyword_text().is_some()
    }

    // ---------- items ----------

    fn item(&mut self) -> PResult<Item> {
        let span = self.span();
        match self.peek() {
            Tok::Import => {
                self.bump();
                let kw = |w: &str| Tok::Ident(w.to_string());
                let form = if self.eat(&Tok::Star) {
                    if !self.eat(&kw("as")) {
                        return self.err("`as` (as in `import * as name from ...`)");
                    }
                    ImportForm::Namespace(self.ident()?.0)
                } else if self.eat(&Tok::LBrace) {
                    let mut names = Vec::new();
                    loop {
                        if self.eat(&Tok::RBrace) {
                            break;
                        }
                        let sp = self.span();
                        let orig = self.dotted_ident()?;
                        let local = if self.eat(&kw("as")) {
                            self.ident()?.0
                        } else {
                            orig.rsplit('.').next().unwrap_or(&orig).to_string()
                        };
                        names.push((orig, local, sp));
                        if !self.eat(&Tok::Comma) {
                            self.expect(&Tok::RBrace)?;
                            break;
                        }
                    }
                    ImportForm::Named(names)
                } else {
                    ImportForm::Default(self.ident()?.0)
                };
                if !self.eat(&kw("from")) {
                    return self.err("`from`");
                }
                let Some(path) = self.plain_str() else {
                    return self.err("a file path string");
                };
                self.bump();
                let mut attrs = Vec::new();
                if self.eat(&kw("with")) {
                    self.expect(&Tok::LBrace)?;
                    loop {
                        if self.eat(&Tok::RBrace) {
                            break;
                        }
                        let (k, sp) = self.key()?;
                        self.expect(&Tok::Colon)?;
                        let Some(v) = self.plain_str() else {
                            return self.err("a string attribute value");
                        };
                        self.bump();
                        attrs.push((k, v, sp));
                        if !self.eat(&Tok::Comma) {
                            self.expect(&Tok::RBrace)?;
                            break;
                        }
                    }
                }
                Ok(Item::Import {
                    form,
                    path,
                    attrs,
                    span,
                })
            }
            Tok::Ident(r) if r == "refine" && matches!(self.peek_at(1), Tok::Ident(_)) => {
                self.bump();
                let name = self.dotted_ident()?;
                if !self.eat(&Tok::Ident("with".into())) {
                    return self.err("`with`");
                }
                let patches = self.patches()?;
                Ok(Item::Refine { name, patches, span })
            }
            Tok::Type => {
                self.bump();
                let name = self.dotted_ident()?;
                self.expect(&Tok::Eq)?;
                Ok(Item::Type {
                    name,
                    ty: self.ty()?,
                    span,
                })
            }
            Tok::Input => {
                self.bump();
                let (name, _) = self.ident()?;
                self.expect(&Tok::Colon)?;
                let ty = rebind(self.ty()?, &name);
                let default = if self.eat(&Tok::Eq) { Some(self.expr_s()?) } else { None };
                Ok(Item::Input {
                    name,
                    ty,
                    default,
                    span,
                })
            }
            Tok::Let => {
                self.bump();
                let (name, _) = self.ident()?;
                let ty = if self.eat(&Tok::Colon) {
                    Some(rebind(self.ty()?, &name))
                } else {
                    None
                };
                self.expect(&Tok::Eq)?;
                Ok(Item::Let {
                    name,
                    ty,
                    value: self.expr_s()?,
                    span,
                })
            }
            Tok::Fn => {
                self.bump();
                let (name, _) = self.ident()?;
                self.expect(&Tok::LParen)?;
                let mut params = Vec::new();
                loop {
                    if self.eat(&Tok::RParen) {
                        break;
                    }
                    let (p, ps) = self.ident()?;
                    self.expect(&Tok::Colon)?;
                    let ty = rebind(self.ty()?, &p);
                    let default = if self.eat(&Tok::Eq) { Some(self.expr_s()?) } else { None };
                    params.push(Param {
                        name: p,
                        ty,
                        default,
                        span: ps,
                    });
                    if !self.eat(&Tok::Comma) {
                        self.expect(&Tok::RParen)?;
                        break;
                    }
                }
                let ret = if self.eat(&Tok::Arrow) { Some(self.ty()?) } else { None };
                self.expect(&Tok::Eq)?;
                let body = self.expr_s()?;
                Ok(Item::Fn(FnDecl {
                    name,
                    params,
                    ret,
                    body,
                    span,
                }))
            }
            Tok::Export => {
                self.bump();
                // `export name =` / `export name after a, b =` declare a component;
                // `after` is a contextual keyword.
                let named = matches!(self.peek(), Tok::Ident(_))
                    && (matches!(self.peek_at(1), Tok::Eq) || matches!(self.peek_at(1), Tok::Ident(a) if a == "after"));
                let mut name = None;
                let mut after = Vec::new();
                if named {
                    name = Some(self.ident()?.0);
                    if self.eat(&Tok::Ident("after".into())) {
                        loop {
                            after.push(self.ident()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::Eq)?;
                }
                Ok(Item::Export {
                    name,
                    after,
                    value: self.expr_s()?,
                    span,
                })
            }
            _ => self.err("`import`, `type`, `refine`, `input`, `let`, `fn` or `export`"),
        }
    }

    // ---------- types ----------

    fn ty(&mut self) -> PResult<TypeExpr> {
        let span = self.span();
        self.eat(&Tok::Pipe); // optional leading `|`
        let mut members = vec![self.ty_atom()?];
        while self.eat(&Tok::Pipe) {
            members.push(self.ty_atom()?);
        }
        let base = if members.len() == 1 {
            members.pop().unwrap()
        } else {
            TypeExpr {
                kind: TypeKind::Union(members),
                span,
            }
        };
        if self.eat(&Tok::Where) {
            let pred = self.with_struct(false, |p| p.expr())?;
            if *self.peek() == Tok::Pipe {
                return self.err("end of refined type (wrap refined union members in parentheses)");
            }
            return Ok(TypeExpr {
                kind: TypeKind::Refine(Box::new(base), pred),
                span,
            });
        }
        Ok(base)
    }

    /// An atom, optionally followed by `with { ... }` refinements.
    fn ty_atom(&mut self) -> PResult<TypeExpr> {
        let span = self.span();
        let mut te = self.ty_primary()?;
        while matches!(self.peek(), Tok::Ident(w) if w == "with") && *self.peek_at(1) == Tok::LBrace {
            self.bump();
            let patches = self.patches()?;
            te = TypeExpr {
                kind: TypeKind::With(Box::new(te), patches),
                span,
            };
        }
        Ok(te)
    }

    /// `{ path: T, path where φ, -path, ... }` after `with`.
    fn patches(&mut self) -> PResult<Vec<Patch>> {
        self.expect(&Tok::LBrace)?;
        let mut patches = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            let span = self.span();
            let remove = self.eat(&Tok::Minus);
            let path = self.patch_path()?;
            let last = path
                .iter()
                .rev()
                .find_map(|s| {
                    if let PathSeg::Field(k) = s {
                        Some(k.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let op = if remove {
                PatchOp::Remove
            } else if self.eat(&Tok::Where) {
                let pred = self.with_struct(false, |p| p.expr())?;
                PatchOp::Where(pred.subst(&last, &Expr::var("self", span)))
            } else {
                self.expect(&Tok::Colon)?;
                let ty = rebind(self.ty()?, &last);
                let default = if self.eat(&Tok::Eq) { Some(self.expr_s()?) } else { None };
                PatchOp::Set(ty, default)
            };
            patches.push(Patch { path, op, span });
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok(patches)
    }

    /// `a.b[].c[2].d[name: "x"]` in a `with` entry.
    fn patch_path(&mut self) -> PResult<Vec<PathSeg>> {
        // A path may start with a selector when patching a list: `[kind: "Service"].spec...`.
        let mut path = if *self.peek() == Tok::LBracket {
            Vec::new()
        } else {
            vec![PathSeg::Field(self.key()?.0)]
        };
        loop {
            if self.eat(&Tok::Dot) {
                path.push(PathSeg::Field(self.key()?.0));
            } else if self.eat(&Tok::LBracket) {
                if self.eat(&Tok::RBracket) {
                    path.push(PathSeg::Each);
                    continue;
                }
                if let (Tok::Int(n), Tok::RBracket) = (self.peek().clone(), self.peek_at(1))
                    && n >= 0
                {
                    self.bump();
                    self.bump();
                    path.push(PathSeg::Index(n as usize));
                    continue;
                }
                let mut keys = Vec::new();
                loop {
                    let (k, _) = self.key()?;
                    self.expect(&Tok::Colon)?;
                    keys.push((k, self.expr_s()?));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket)?;
                path.push(PathSeg::Select(keys));
            } else {
                return Ok(path);
            }
        }
    }

    /// `{ path: e, path +: e, -path }` after a value-level `with`.
    fn value_patches(&mut self) -> PResult<Vec<ValuePatch>> {
        self.expect(&Tok::LBrace)?;
        let mut patches = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            let span = self.span();
            let remove = self.eat(&Tok::Minus);
            let path = self.patch_path()?;
            let op = if remove {
                ValueOp::Remove
            } else if self.eat(&Tok::Plus) {
                self.expect(&Tok::Colon)?;
                ValueOp::Merge(self.expr_s()?)
            } else {
                self.expect(&Tok::Colon)?;
                ValueOp::Set(self.expr_s()?)
            };
            patches.push(ValuePatch { path, op, span });
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok(patches)
    }

    fn ty_primary(&mut self) -> PResult<TypeExpr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Ident(_) => TypeKind::Named(self.dotted_ident()?),
            Tok::Null => {
                self.bump();
                TypeKind::Lit(Lit::Null)
            }
            Tok::True | Tok::False => {
                let b = *self.peek() == Tok::True;
                self.bump();
                TypeKind::Lit(Lit::Bool(b))
            }
            Tok::Int(n) => {
                self.bump();
                TypeKind::Lit(Lit::Int(n))
            }
            Tok::Float(x) => {
                self.bump();
                TypeKind::Lit(Lit::Float(x))
            }
            Tok::Minus => {
                self.bump();
                match self.peek().clone() {
                    Tok::Int(n) => {
                        self.bump();
                        TypeKind::Lit(Lit::Int(-n))
                    }
                    Tok::Float(x) => {
                        self.bump();
                        TypeKind::Lit(Lit::Float(-x))
                    }
                    _ => return self.err("number"),
                }
            }
            Tok::Str(_) => match self.plain_str() {
                Some(s) => {
                    self.bump();
                    TypeKind::Lit(Lit::Str(s))
                }
                None => return Err(Diag::new(span, "string literal types cannot use interpolation")),
            },
            Tok::LBracket => {
                self.bump();
                let elem = self.ty()?;
                self.expect(&Tok::RBracket)?;
                TypeKind::List(Box::new(elem))
            }
            Tok::LParen => {
                self.bump();
                let mut tys = Vec::new();
                let mut trailing = false;
                loop {
                    if self.eat(&Tok::RParen) {
                        break;
                    }
                    tys.push(self.ty()?);
                    trailing = self.eat(&Tok::Comma);
                    if !trailing {
                        self.expect(&Tok::RParen)?;
                        break;
                    }
                }
                if tys.len() == 1 && !trailing {
                    return Ok(tys.pop().unwrap());
                }
                TypeKind::Tuple(tys)
            }
            Tok::LBrace => {
                self.bump();
                if self.eat(&Tok::LBracket) {
                    let key = self.ty()?;
                    if !matches!(&key.kind, TypeKind::Named(n) if n == "Str") {
                        return Err(Diag::new(key.span, "map keys must be `Str`"));
                    }
                    self.expect(&Tok::RBracket)?;
                    self.expect(&Tok::Colon)?;
                    let val = self.ty()?;
                    self.eat(&Tok::Comma);
                    self.expect(&Tok::RBrace)?;
                    TypeKind::Map(Box::new(val))
                } else {
                    self.record_type()?
                }
            }
            _ => return self.err("type"),
        };
        Ok(TypeExpr { kind, span })
    }

    /// Record type fields after `{`, up to and including `}`.
    fn record_type(&mut self) -> PResult<TypeKind> {
        let mut fields = Vec::new();
        let mut open = false;
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.eat(&Tok::DotDot) {
                open = true;
                self.eat(&Tok::Comma);
                self.expect(&Tok::RBrace)?;
                break;
            }
            let (name, span) = self.key()?;
            let optional = self.eat(&Tok::Question);
            self.expect(&Tok::Colon)?;
            let ty = rebind(self.ty()?, &name);
            let default = if self.eat(&Tok::Eq) { Some(self.expr_s()?) } else { None };
            fields.push(FieldDecl {
                name,
                ty,
                optional,
                default,
                span,
            });
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok(TypeKind::Record { fields, open })
    }

    // ---------- expressions ----------

    fn with_struct<T>(&mut self, allow: bool, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let old = core::mem::replace(&mut self.no_struct, !allow);
        let r = f(self);
        self.no_struct = old;
        r
    }

    /// An expression in a context where `T { ... }` is allowed.
    fn expr_s(&mut self) -> PResult<Expr> {
        self.with_struct(true, |p| p.expr())
    }

    fn expr(&mut self) -> PResult<Expr> {
        match self.peek() {
            Tok::If => self.if_expr(),
            Tok::Let => self.let_expr(),
            _ => self.bin(1),
        }
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::If)?;
        let cond = self.expr_s()?;
        self.expect(&Tok::Then)?;
        let then = self.expr()?;
        if *self.peek() != Tok::Else {
            return Err(Diag::new(
                self.span(),
                "`if` expression needs an `else` (an `if` without `else` is only allowed inside list and object literals)",
            ));
        }
        self.bump();
        let els = self.expr()?;
        Ok(Expr::new(
            ExprKind::If(Box::new(cond), Box::new(then), Box::new(els)),
            span,
        ))
    }

    fn let_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::Let)?;
        let (name, _) = self.ident()?;
        let ty = if self.eat(&Tok::Colon) {
            Some(rebind(self.ty()?, &name))
        } else {
            None
        };
        self.expect(&Tok::Eq)?;
        let value = self.expr_s()?;
        self.expect(&Tok::Semi)?;
        let body = self.expr()?;
        Ok(Expr::new(
            ExprKind::Let(name, ty.map(Box::new), Box::new(value), Box::new(body)),
            span,
        ))
    }

    fn bin(&mut self, min: u8) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        while let Some(op) = binop(self.peek()) {
            let p = op.prec();
            if p < min {
                break;
            }
            self.bump();
            // `a ?? if c then x else y` and similar: allow if/let on the right.
            let rhs = if matches!(self.peek(), Tok::If | Tok::Let) {
                self.expr()?
            } else {
                self.bin(p + 1)?
            };
            lhs = Expr::binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Bang => UnOp::Not,
            _ => return self.postfix(),
        };
        self.bump();
        let e = self.unary()?;
        Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), span))
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Tok::Dot | Tok::QDot => {
                    let optional = *self.peek() == Tok::QDot;
                    self.bump();
                    let span = self.span(); // the label, so hover and errors point at it
                    let label = match self.peek().clone() {
                        Tok::Int(n) if n >= 0 => {
                            self.bump();
                            Label::Pos(n as usize)
                        }
                        _ if self.is_key_start() => Label::Name(self.key()?.0),
                        _ => return self.err("field name or tuple index"),
                    };
                    e = Expr::new(ExprKind::Field(Box::new(e), label, optional), span);
                }
                Tok::Ident(w) if w == "with" && *self.peek_at(1) == Tok::LBrace && !self.no_struct => {
                    let span = self.span();
                    self.bump();
                    let patches = self.value_patches()?;
                    e = Expr::new(ExprKind::With(Box::new(e), patches), span);
                }
                Tok::LBracket => {
                    let span = e.span;
                    self.bump();
                    let idx = self.expr_s()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr::new(ExprKind::Index(Box::new(e), Box::new(idx)), span);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Lit(Lit::Int(n))
            }
            Tok::Float(x) => {
                self.bump();
                ExprKind::Lit(Lit::Float(x))
            }
            Tok::True | Tok::False => {
                let b = *self.peek() == Tok::True;
                self.bump();
                ExprKind::Lit(Lit::Bool(b))
            }
            Tok::Null => {
                self.bump();
                ExprKind::Lit(Lit::Null)
            }
            Tok::Str(parts) => {
                self.bump();
                self.string(parts)?
            }
            Tok::Ident(n) => {
                self.bump();
                match self.peek() {
                    Tok::LParen => {
                        self.bump();
                        ExprKind::Call(n, self.args()?)
                    }
                    Tok::LBrace if is_upper(&n) && !self.no_struct => {
                        let obj = self.object()?;
                        ExprKind::Typed(n, Box::new(obj))
                    }
                    Tok::Dot if self.qualified_call() => {
                        let mut name = n;
                        while self.eat(&Tok::Dot) {
                            name.push('.');
                            name.push_str(&self.ident()?.0);
                        }
                        self.expect(&Tok::LParen)?;
                        ExprKind::Call(name, self.args()?)
                    }
                    Tok::Dot if self.qualified_struct_len().is_some() => {
                        let mut name = n;
                        while self.eat(&Tok::Dot) {
                            name.push('.');
                            name.push_str(&self.ident()?.0);
                        }
                        let obj = self.object()?;
                        ExprKind::Typed(name, Box::new(obj))
                    }
                    _ => ExprKind::Var(n),
                }
            }
            Tok::LParen => {
                self.bump();
                let mut items = Vec::new();
                let mut trailing = false;
                loop {
                    if self.eat(&Tok::RParen) {
                        break;
                    }
                    items.push(self.expr_s()?);
                    trailing = self.eat(&Tok::Comma);
                    if !trailing {
                        self.expect(&Tok::RParen)?;
                        break;
                    }
                }
                if items.len() == 1 && !trailing {
                    return Ok(items.pop().unwrap());
                }
                ExprKind::Tuple(items)
            }
            Tok::LBracket => return self.list(),
            Tok::LBrace => return self.object(),
            Tok::Match => return self.match_expr(),
            Tok::If | Tok::Let => return self.expr(),
            _ => return self.err("expression"),
        };
        Ok(Expr::new(kind, span))
    }

    fn string(&mut self, parts: Vec<StrPart>) -> PResult<ExprKind> {
        if let [StrPart::Lit(s)] = parts.as_slice() {
            return Ok(ExprKind::Lit(Lit::Str(s.clone())));
        }
        let mut out = Vec::new();
        for part in parts {
            match part {
                StrPart::Lit(s) => out.push(InterpPart::Lit(s)),
                StrPart::Interp(toks) => {
                    let mut sub = Parser {
                        toks,
                        pos: 0,
                        no_struct: false,
                    };
                    let e = sub.expr()?;
                    if *sub.peek() != Tok::Eof {
                        return sub.err("`)` to close the interpolation");
                    }
                    out.push(InterpPart::Expr(e));
                }
            }
        }
        Ok(ExprKind::Interp(out))
    }

    /// Call arguments after `(`, up to and including `)`. `name: e` is a named argument.
    fn args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        loop {
            if self.eat(&Tok::RParen) {
                break;
            }
            let name = match (self.peek(), self.peek_at(1)) {
                (Tok::Ident(n), Tok::Colon) => {
                    let n = n.clone();
                    self.bump();
                    self.bump();
                    Some(n)
                }
                _ => None,
            };
            args.push(Arg {
                name,
                value: self.expr_s()?,
            });
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RParen)?;
                break;
            }
        }
        Ok(args)
    }

    fn list(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::LBracket)?;
        let mut items = Vec::new();
        loop {
            if self.eat(&Tok::RBracket) {
                break;
            }
            items.push(self.list_item()?);
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBracket)?;
                break;
            }
        }
        Ok(Expr::new(ExprKind::List(items), span))
    }

    fn list_item(&mut self) -> PResult<ListItem> {
        match self.peek() {
            Tok::Ellipsis => {
                self.bump();
                Ok(ListItem::Spread(self.expr_s()?))
            }
            Tok::If => {
                let span = self.span();
                self.bump();
                let cond = self.expr_s()?;
                self.expect(&Tok::Then)?;
                let inner = self.list_item()?;
                if self.eat(&Tok::Else) {
                    let ListItem::Elem(then) = inner else {
                        return Err(Diag::new(
                            span,
                            "an `if` with `else` in a list must produce a single element",
                        ));
                    };
                    let els = self.expr_s()?;
                    return Ok(ListItem::Elem(Expr::new(
                        ExprKind::If(Box::new(cond), Box::new(then), Box::new(els)),
                        span,
                    )));
                }
                Ok(ListItem::If(cond, Box::new(inner)))
            }
            Tok::For => {
                self.bump();
                let (a, b, src) = self.for_head()?;
                Ok(ListItem::For(a, b, src, Box::new(self.list_item()?)))
            }
            _ => Ok(ListItem::Elem(self.expr_s()?)),
        }
    }

    /// `x in e =>` or `k, v in e =>` (after `for`).
    fn for_head(&mut self) -> PResult<(String, Option<String>, Expr)> {
        let (a, _) = self.ident()?;
        let b = if self.eat(&Tok::Comma) {
            Some(self.ident()?.0)
        } else {
            None
        };
        self.expect(&Tok::In)?;
        let src = self.expr_s()?;
        self.expect(&Tok::FatArrow)?;
        Ok((a, b, src))
    }

    fn object(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::LBrace)?;
        let members = self.members()?;
        Ok(Expr::new(ExprKind::Object(members), span))
    }

    /// Object members after `{`, up to and including `}`.
    fn members(&mut self) -> PResult<Vec<Member>> {
        let mut members = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            self.member(&mut members)?;
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok(members)
    }

    fn member(&mut self, members: &mut Vec<Member>) -> PResult<()> {
        let keyword_as_key = matches!(self.peek_at(1), Tok::Colon | Tok::Dot);
        match self.peek() {
            Tok::Ellipsis => {
                self.bump();
                members.push(Member::Spread(self.expr_s()?));
            }
            Tok::If if !keyword_as_key => {
                self.bump();
                let cond = self.expr_s()?;
                self.expect(&Tok::Then)?;
                self.expect(&Tok::LBrace)?;
                let then = self.members()?;
                let els = if self.eat(&Tok::Else) {
                    self.expect(&Tok::LBrace)?;
                    Some(self.members()?)
                } else {
                    None
                };
                members.push(Member::If(cond, then, els));
            }
            Tok::For if !keyword_as_key => {
                self.bump();
                let (a, b, src) = self.for_head()?;
                self.expect(&Tok::LBrace)?;
                let body = self.members()?;
                members.push(Member::For(a, b, src, body));
            }
            Tok::LBracket => {
                self.bump();
                let k = self.expr_s()?;
                self.expect(&Tok::RBracket)?;
                self.expect(&Tok::Colon)?;
                members.push(Member::Computed(k, self.expr_s()?));
            }
            _ => {
                let (first, span) = self.key()?;
                let mut path = vec![(first, span)];
                while self.eat(&Tok::Dot) {
                    path.push(self.key()?);
                }
                if *self.peek() != Tok::Colon && path.len() == 1 {
                    // Punning: `{ name }` means `{ name: name }`.
                    if matches!(self.peek(), Tok::Comma | Tok::RBrace) {
                        let v = Expr::var(&path[0].0, span);
                        insert_member(members, &path, v);
                        return Ok(());
                    }
                }
                self.expect(&Tok::Colon)?;
                let value = self.expr_s()?;
                insert_member(members, &path, value);
            }
        }
        Ok(())
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::Match)?;
        let scrut = self.with_struct(false, |p| p.expr())?;
        self.expect(&Tok::LBrace)?;
        let mut arms = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            let pat = self.pat()?;
            self.expect(&Tok::FatArrow)?;
            let body = self.expr_s()?;
            arms.push(Arm { pat, body });
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok(Expr::new(ExprKind::Match(Box::new(scrut), arms), span))
    }

    // ---------- patterns ----------

    fn pat(&mut self) -> PResult<Pat> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Ident(n) => {
                self.bump();
                let name = if n == "_" { None } else { Some(n) };
                if self.eat(&Tok::Colon) {
                    PatKind::Typed(name, self.ty()?)
                } else {
                    name.map_or(PatKind::Wild, PatKind::Bind)
                }
            }
            Tok::Null => {
                self.bump();
                PatKind::Lit(Lit::Null)
            }
            Tok::True | Tok::False => {
                let b = *self.peek() == Tok::True;
                self.bump();
                PatKind::Lit(Lit::Bool(b))
            }
            Tok::Int(n) => {
                self.bump();
                PatKind::Lit(Lit::Int(n))
            }
            Tok::Float(x) => {
                self.bump();
                PatKind::Lit(Lit::Float(x))
            }
            Tok::Minus => {
                self.bump();
                match self.peek().clone() {
                    Tok::Int(n) => {
                        self.bump();
                        PatKind::Lit(Lit::Int(-n))
                    }
                    Tok::Float(x) => {
                        self.bump();
                        PatKind::Lit(Lit::Float(-x))
                    }
                    _ => return self.err("number"),
                }
            }
            Tok::Str(_) => match self.plain_str() {
                Some(s) => {
                    self.bump();
                    PatKind::Lit(Lit::Str(s))
                }
                None => return Err(Diag::new(span, "string patterns cannot use interpolation")),
            },
            Tok::LParen => {
                self.bump();
                let mut ps = Vec::new();
                let mut trailing = false;
                loop {
                    if self.eat(&Tok::RParen) {
                        break;
                    }
                    ps.push(self.pat()?);
                    trailing = self.eat(&Tok::Comma);
                    if !trailing {
                        self.expect(&Tok::RParen)?;
                        break;
                    }
                }
                if ps.len() == 1 && !trailing {
                    return Ok(ps.pop().unwrap());
                }
                PatKind::Tuple(ps)
            }
            Tok::LBrace => {
                self.bump();
                let mut fields = Vec::new();
                let mut open = false;
                loop {
                    if self.eat(&Tok::RBrace) {
                        break;
                    }
                    if self.eat(&Tok::DotDot) {
                        open = true;
                        self.eat(&Tok::Comma);
                        self.expect(&Tok::RBrace)?;
                        break;
                    }
                    let (name, span) = self.key()?;
                    let pat = if self.eat(&Tok::Colon) {
                        self.pat()?
                    } else {
                        Pat {
                            kind: PatKind::Bind(name.clone()),
                            span,
                        }
                    };
                    fields.push((name, pat));
                    if !self.eat(&Tok::Comma) {
                        self.expect(&Tok::RBrace)?;
                        break;
                    }
                }
                PatKind::Record(fields, open)
            }
            _ => return self.err("pattern"),
        };
        Ok(Pat { kind, span })
    }
}
