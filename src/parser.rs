use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{Tok, Token, lex};

type PResult<T> = Result<T, Diag>;

pub fn parse_program(src: &str) -> PResult<Vec<Decl>> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0, no_struct: false };
    let mut decls = Vec::new();
    while *p.peek() != Tok::Eof {
        decls.push(p.decl()?);
    }
    Ok(decls)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// When set, `E.V {` is not parsed as a variant record literal
    /// (so `if x == E.V { .. }` works), like Rust's struct-literal restriction.
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
            TypeExpr { kind: TypeKind::Refine(base, pred.subst(name, &this)), span: te.span }
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
        _ => return None,
    })
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
        Err(Diag::new(self.span(), format!("expected {expected}, found {}", self.peek().describe())))
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

    // ---------- declarations ----------

    fn decl(&mut self) -> PResult<Decl> {
        let span = self.span();
        match self.peek() {
            Tok::Type | Tok::Nominal => {
                let nominal = *self.peek() == Tok::Nominal;
                self.bump();
                let (name, _) = self.ident()?;
                self.expect(&Tok::Eq)?;
                let ty = self.ty()?;
                Ok(if nominal { Decl::Nominal { name, ty, span } } else { Decl::Type { name, ty, span } })
            }
            Tok::Enum => {
                self.bump();
                let (name, _) = self.ident()?;
                self.expect(&Tok::LBrace)?;
                let mut variants = Vec::new();
                loop {
                    if self.eat(&Tok::RBrace) {
                        break;
                    }
                    let (vname, vspan) = self.ident()?;
                    let payload = match self.peek() {
                        Tok::LBrace => {
                            let ps = self.span();
                            self.bump();
                            let (fields, open) = self.field_types()?;
                            if open {
                                return Err(Diag::new(ps, "enum variant payloads must be exact records"));
                            }
                            Some(TypeExpr { kind: TypeKind::Record { fields, open: false }, span: ps })
                        }
                        Tok::LParen => {
                            let ps = self.span();
                            self.bump();
                            let (tys, _) = self.ty_list()?;
                            Some(TypeExpr { kind: TypeKind::Record { fields: positional(tys), open: false }, span: ps })
                        }
                        _ => None,
                    };
                    variants.push(VariantDecl { name: vname, payload, span: vspan });
                    if !self.eat(&Tok::Comma) {
                        self.expect(&Tok::RBrace)?;
                        break;
                    }
                }
                Ok(Decl::Enum { name, variants, span })
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
                    params.push((p, ty, ps));
                    if !self.eat(&Tok::Comma) {
                        self.expect(&Tok::RParen)?;
                        break;
                    }
                }
                let ret = if self.eat(&Tok::Arrow) { Some(self.ty()?) } else { None };
                let body = self.block()?;
                Ok(Decl::Fn(FnDecl { name, params, ret, body, span }))
            }
            _ => self.err("declaration (`type`, `nominal`, `enum` or `fn`)"),
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
            TypeExpr { kind: TypeKind::Union(members), span }
        };
        if self.eat(&Tok::Where) {
            let pred = self.expr_ns()?;
            if *self.peek() == Tok::Pipe {
                return self.err("end of refined type (wrap refined union members in parentheses)");
            }
            return Ok(TypeExpr { kind: TypeKind::Refine(Box::new(base), pred), span });
        }
        Ok(base)
    }

    fn ty_atom(&mut self) -> PResult<TypeExpr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Ident(n) => {
                self.bump();
                if *self.peek() == Tok::Dot && matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                    let (v, _) = self.ident()?;
                    TypeKind::Path(n, v)
                } else {
                    TypeKind::Named(n)
                }
            }
            Tok::LParen => {
                self.bump();
                let (mut tys, trailing) = self.ty_list()?;
                if tys.len() == 1 && !trailing {
                    return Ok(tys.pop().unwrap());
                }
                TypeKind::Record { fields: positional(tys), open: false }
            }
            Tok::LBrace => {
                self.bump();
                let (fields, open) = self.field_types()?;
                TypeKind::Record { fields, open }
            }
            Tok::Fn => {
                self.bump();
                self.expect(&Tok::LParen)?;
                let (params, _) = self.ty_list()?;
                self.expect(&Tok::Arrow)?;
                let ret = self.ty()?;
                TypeKind::Fn(params, Box::new(ret))
            }
            _ => return self.err("type"),
        };
        Ok(TypeExpr { kind, span })
    }

    /// Comma-separated types up to and including `)`. Returns whether there was a trailing comma.
    fn ty_list(&mut self) -> PResult<(Vec<TypeExpr>, bool)> {
        let mut v = Vec::new();
        let mut trailing = false;
        loop {
            if self.eat(&Tok::RParen) {
                break;
            }
            v.push(self.ty()?);
            trailing = self.eat(&Tok::Comma);
            if !trailing {
                self.expect(&Tok::RParen)?;
                break;
            }
        }
        Ok((v, trailing))
    }

    /// Record type fields after `{`, up to and including `}`.
    fn field_types(&mut self) -> PResult<(Vec<(Label, TypeExpr)>, bool)> {
        let mut fields = Vec::new();
        let mut open = false;
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.eat(&Tok::DotDot) {
                open = true;
                self.expect(&Tok::RBrace)?;
                break;
            }
            let (name, _) = self.ident()?;
            self.expect(&Tok::Colon)?;
            let ty = rebind(self.ty()?, &name);
            fields.push((Label::Name(name), ty));
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok((fields, open))
    }

    // ---------- expressions ----------

    fn expr(&mut self) -> PResult<Expr> {
        self.bin(1)
    }

    fn with_struct<T>(&mut self, allow: bool, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let old = std::mem::replace(&mut self.no_struct, !allow);
        let r = f(self);
        self.no_struct = old;
        r
    }

    fn expr_s(&mut self) -> PResult<Expr> {
        self.with_struct(true, |p| p.expr())
    }

    fn expr_ns(&mut self) -> PResult<Expr> {
        self.with_struct(false, |p| p.expr())
    }

    fn bin(&mut self, min: u8) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        while let Some(op) = binop(self.peek()) {
            let p = op.prec();
            if p < min {
                break;
            }
            self.bump();
            let rhs = self.bin(p + 1)?;
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
                Tok::Dot => {
                    self.bump();
                    let label = match self.peek().clone() {
                        Tok::Int(n) if n >= 0 => Label::Pos(n as usize),
                        Tok::Ident(s) => Label::Name(s),
                        _ => return self.err("field name or tuple index"),
                    };
                    self.bump();
                    e = Expr::field(e, label);
                }
                Tok::LParen => {
                    let span = e.span;
                    self.bump();
                    let args = self.expr_list()?;
                    e = Expr::new(ExprKind::Call(Box::new(e), args), span);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// Comma-separated expressions up to and including `)`. Returns whether there was a trailing comma.
    fn expr_list_t(&mut self) -> PResult<(Vec<Expr>, bool)> {
        let mut v = Vec::new();
        let mut trailing = false;
        loop {
            if self.eat(&Tok::RParen) {
                break;
            }
            v.push(self.expr_s()?);
            trailing = self.eat(&Tok::Comma);
            if !trailing {
                self.expect(&Tok::RParen)?;
                break;
            }
        }
        Ok((v, trailing))
    }

    fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        Ok(self.expr_list_t()?.0)
    }

    /// Record literal fields after `{`, up to and including `}`.
    fn record_fields(&mut self) -> PResult<Vec<(Label, Expr)>> {
        let mut fields = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            let (name, _) = self.ident()?;
            self.expect(&Tok::Colon)?;
            fields.push((Label::Name(name), self.expr_s()?));
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok(fields)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            Tok::Float(x) => {
                self.bump();
                ExprKind::Float(x)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::True | Tok::False => {
                let b = *self.peek() == Tok::True;
                self.bump();
                ExprKind::Bool(b)
            }
            Tok::Ident(n) => {
                self.bump();
                let is_path = is_upper(&n)
                    && *self.peek() == Tok::Dot
                    && matches!(self.peek_at(1), Tok::Ident(v) if is_upper(v));
                if !is_path {
                    ExprKind::Var(n)
                } else {
                    self.bump();
                    let (variant, _) = self.ident()?;
                    let ps = self.span();
                    let payload = match self.peek() {
                        Tok::LParen => {
                            self.bump();
                            let args = self.expr_list()?;
                            Some(Box::new(Expr::new(ExprKind::Record(positional(args)), ps)))
                        }
                        Tok::LBrace if !self.no_struct => {
                            self.bump();
                            let fields = self.record_fields()?;
                            Some(Box::new(Expr::new(ExprKind::Record(fields), ps)))
                        }
                        _ => None,
                    };
                    ExprKind::Variant { enum_name: n, variant, payload }
                }
            }
            Tok::LParen => {
                self.bump();
                let (mut items, trailing) = self.expr_list_t()?;
                if items.len() == 1 && !trailing {
                    return Ok(items.pop().unwrap());
                }
                ExprKind::Record(positional(items))
            }
            Tok::LBrace => {
                let is_record = matches!(self.peek_at(1), Tok::Ident(_)) && *self.peek_at(2) == Tok::Colon;
                if !is_record {
                    return self.block();
                }
                self.bump();
                ExprKind::Record(self.record_fields()?)
            }
            Tok::If => return self.if_expr(),
            Tok::Match => return self.match_expr(),
            _ => return self.err("expression"),
        };
        Ok(Expr::new(kind, span))
    }

    fn block(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        let mut tail = None;
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            if *self.peek() == Tok::Let {
                let sp = self.span();
                self.bump();
                let (name, _) = self.ident()?;
                let ty = if self.eat(&Tok::Colon) { Some(rebind(self.ty()?, &name)) } else { None };
                self.expect(&Tok::Eq)?;
                let value = self.expr_s()?;
                self.expect(&Tok::Semi)?;
                stmts.push(Stmt::Let { name, ty, value, span: sp });
                continue;
            }
            let e = self.expr_s()?;
            if self.eat(&Tok::Semi) {
                stmts.push(Stmt::Expr(e));
            } else if self.eat(&Tok::RBrace) {
                tail = Some(Box::new(e));
                break;
            } else if e.is_blocklike() {
                stmts.push(Stmt::Expr(e));
            } else {
                return self.err("`;` or `}`");
            }
        }
        Ok(Expr::new(ExprKind::Block(stmts, tail), span))
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::If)?;
        let cond = self.expr_ns()?;
        let then = self.block()?;
        let els = if self.eat(&Tok::Else) {
            Some(Box::new(if *self.peek() == Tok::If { self.if_expr()? } else { self.block()? }))
        } else {
            None
        };
        Ok(Expr::new(ExprKind::If(Box::new(cond), Box::new(then), els), span))
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::Match)?;
        let scrut = self.expr_ns()?;
        self.expect(&Tok::LBrace)?;
        let mut arms = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            let pat = self.pat()?;
            self.expect(&Tok::FatArrow)?;
            let body = self.expr_s()?;
            let blocklike = body.is_blocklike();
            arms.push(Arm { pat, body });
            if !self.eat(&Tok::Comma) && *self.peek() != Tok::RBrace && !blocklike {
                return self.err("`,` or `}` after match arm");
            }
        }
        Ok(Expr::new(ExprKind::Match(Box::new(scrut), arms), span))
    }

    // ---------- patterns ----------

    fn pat(&mut self) -> PResult<Pat> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Ident(n) if n == "_" => {
                self.bump();
                if self.eat(&Tok::Colon) { PatKind::Typed(None, self.ty()?) } else { PatKind::Wild }
            }
            Tok::Ident(n) if is_upper(&n) => {
                self.bump();
                self.expect(&Tok::Dot)?;
                let (variant, _) = self.ident()?;
                let payload = match self.peek() {
                    Tok::LParen => {
                        self.bump();
                        let (ps, _) = self.pat_list()?;
                        Some((positional(ps), false))
                    }
                    Tok::LBrace => {
                        self.bump();
                        Some(self.field_pats()?)
                    }
                    _ => None,
                };
                PatKind::Variant { enum_name: n, variant, payload }
            }
            Tok::Ident(n) => {
                self.bump();
                if self.eat(&Tok::Colon) { PatKind::Typed(Some(n), self.ty()?) } else { PatKind::Bind(n) }
            }
            Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::True | Tok::False => PatKind::Lit(self.primary()?),
            Tok::Minus => {
                self.bump();
                let lit = self.primary()?;
                match lit.kind {
                    ExprKind::Int(n) => PatKind::Lit(Expr::new(ExprKind::Int(-n), span)),
                    ExprKind::Float(x) => PatKind::Lit(Expr::new(ExprKind::Float(-x), span)),
                    _ => return Err(Diag::new(span, "expected a numeric literal after `-` in pattern")),
                }
            }
            Tok::LParen => {
                self.bump();
                let (mut ps, trailing) = self.pat_list()?;
                if ps.len() == 1 && !trailing {
                    return Ok(ps.pop().unwrap());
                }
                PatKind::Record(positional(ps), false)
            }
            Tok::LBrace => {
                self.bump();
                let (fields, open) = self.field_pats()?;
                PatKind::Record(fields, open)
            }
            _ => return self.err("pattern"),
        };
        Ok(Pat { kind, span })
    }

    fn pat_list(&mut self) -> PResult<(Vec<Pat>, bool)> {
        let mut v = Vec::new();
        let mut trailing = false;
        loop {
            if self.eat(&Tok::RParen) {
                break;
            }
            v.push(self.pat()?);
            trailing = self.eat(&Tok::Comma);
            if !trailing {
                self.expect(&Tok::RParen)?;
                break;
            }
        }
        Ok((v, trailing))
    }

    /// Record pattern fields after `{`, up to and including `}`. `{ r }` puns to `{ r: r }`.
    fn field_pats(&mut self) -> PResult<(Vec<(Label, Pat)>, bool)> {
        let mut fields = Vec::new();
        let mut open = false;
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.eat(&Tok::DotDot) {
                open = true;
                self.expect(&Tok::RBrace)?;
                break;
            }
            let (name, span) = self.ident()?;
            let pat = if self.eat(&Tok::Colon) { self.pat()? } else { Pat { kind: PatKind::Bind(name.clone()), span } };
            fields.push((Label::Name(name), pat));
            if !self.eat(&Tok::Comma) {
                self.expect(&Tok::RBrace)?;
                break;
            }
        }
        Ok((fields, open))
    }
}
