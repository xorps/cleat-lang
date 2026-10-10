//! Evaluation (SPEC §7 step 4). Every value crossing an annotation is checked
//! against its type by `conform`, which also fills in defaults.

use std::collections::HashMap;
use std::fmt;

use crate::ast::*;
use crate::diag::Span;
use crate::emit;
use crate::json;
use crate::types::{Mode, Presence, Rec, Ty, TypeCtx};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// Insertion-ordered object.
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn from_lit(l: &Lit) -> Value {
        match l {
            Lit::Null => Value::Null,
            Lit::Bool(b) => Value::Bool(*b),
            Lit::Int(i) => Value::Int(*i),
            Lit::Float(x) => Value::Float(*x),
            Lit::Str(s) => Value::Str(s.clone()),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Obj(_) => "object",
        }
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        match self {
            Value::Obj(es) => es.iter().find(|(ek, _)| ek == k).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Structural equality where `1 == 1.0` and object key order is irrelevant.
    pub fn loose_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => (*a as f64) == *b,
            (Value::List(a), Value::List(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.loose_eq(y)),
            (Value::Obj(a), Value::Obj(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| other.get(k).is_some_and(|w| v.loose_eq(w)))
            }
            _ => self == other,
        }
    }

    /// A short rendering for error messages.
    pub fn short(&self) -> String {
        let s = emit::to_json(self, false);
        if s.chars().count() > 60 { format!("{}…", s.chars().take(57).collect::<String>()) } else { s }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", emit::to_json(self, false))
    }
}

#[derive(Debug, Clone)]
pub struct EvalError {
    pub span: Span,
    pub msg: String,
    /// Enclosing calls, innermost last: (function, call site).
    pub trace: Vec<(String, Span)>,
}

/// An input as supplied from outside, before decoding against its type.
#[derive(Debug, Clone)]
pub enum InputValue {
    /// Already-typed JSON (from `--input file.json`).
    Json(Value),
    /// A string to decode by type (from `--set` or an Argo CD string parameter).
    Raw(String),
    /// Argo CD array parameter.
    RawList(Vec<String>),
    /// Argo CD map parameter.
    RawMap(Vec<(String, String)>),
}

/// Evaluate a closed expression (no variables, no user functions). Used for
/// constant folding during checking.
pub fn eval_closed(e: &Expr) -> Option<Value> {
    let mut ev = Evaluator::new(TypeCtx::default());
    ev.eval(e).ok()
}

/// The documents of one `export`. Named exports are components that can depend on each other.
#[derive(Debug, Clone)]
pub struct Component {
    pub name: Option<String>,
    pub after: Vec<String>,
    pub docs: Vec<Value>,
}

/// Evaluate a checked program and return its exports in source order.
pub fn run(items: &[Item], ctx: TypeCtx, inputs: Vec<(String, InputValue)>) -> Result<Vec<Component>, Vec<EvalError>> {
    let mut ev = Evaluator::new(ctx);
    let mut errs = Vec::new();
    let mut comps = Vec::new();

    let declared: Vec<&str> = items
        .iter()
        .filter_map(|i| if let Item::Input { name, .. } = i { Some(name.as_str()) } else { None })
        .collect();
    let mut provided: HashMap<String, InputValue> = HashMap::new();
    for (name, v) in inputs {
        if !declared.contains(&name.as_str()) {
            let hint = suggest(&name, declared.iter().copied()).map(|s| format!("; did you mean `{s}`?")).unwrap_or_default();
            errs.push(EvalError { span: Span::default(), msg: format!("unknown input `{name}`{hint}"), trace: vec![] });
        } else {
            provided.insert(name, v);
        }
    }

    for item in items {
        match item {
            Item::Input { name, ty, default, span } => {
                let t = ev.ctx.resolve(ty);
                let value = match (provided.remove(name), default) {
                    (Some(iv), _) => ev.decode(iv, &t).map_err(|m| format!("input `{name}`: {m}")),
                    (None, Some(d)) => match ev.eval(d) {
                        Ok(v) => ev.conform_top(v, &t).map_err(|m| format!("default for input `{name}`: {m}")),
                        Err(e) => Err(e.msg),
                    },
                    (None, None) => Err(format!("missing required input `{name}` of type `{t}`")),
                };
                match value {
                    Ok(v) => ev.globals.push((name.clone(), v)),
                    Err(msg) => {
                        errs.push(EvalError { span: *span, msg, trace: vec![] });
                        ev.globals.push((name.clone(), Value::Null));
                    }
                }
            }
            _ if !errs.is_empty() => {}
            Item::Let { name, ty, value, span } => {
                let v = ev.eval(value).map_err(|e| vec![e])?;
                let v = match ty {
                    Some(te) => {
                        let t = ev.ctx.resolve(te);
                        ev.conform_top(v, &t).map_err(|m| vec![ev.error(*span, format!("`{name}`: {m}"))])?
                    }
                    None => v,
                };
                ev.globals.push((name.clone(), v));
            }
            Item::Fn(f) => {
                ev.fns.insert(f.name.clone(), f.clone());
            }
            Item::Export { name, after, value, .. } => {
                let docs = match ev.eval(value).map_err(|e| vec![e])? {
                    Value::List(vs) => vs,
                    v => vec![v],
                };
                let after = after.iter().map(|(a, _)| a.clone()).collect();
                comps.push(Component { name: name.clone(), after, docs });
            }
            Item::Type { .. } | Item::Import { .. } | Item::Refine { .. } => {}
        }
    }
    if errs.is_empty() { Ok(comps) } else { Err(errs) }
}

/// The closest candidate within a small edit distance, for "did you mean" hints.
pub fn suggest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    candidates
        .map(|c| (levenshtein(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Edit distance where swapping two adjacent characters counts as one edit
/// (optimal string alignment), since `prots` for `ports` is the classic typo.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

fn fmt_path(path: &[String]) -> String {
    path.concat()
}

fn path_key(k: &str) -> String {
    let plain = fmt_key(k);
    if plain.starts_with('"') { format!("[{plain}]") } else { format!(".{k}") }
}

fn at(path: &[String], msg: String) -> String {
    if path.is_empty() { msg } else { format!("at `{}`: {msg}", fmt_path(path)) }
}

struct Evaluator {
    ctx: TypeCtx,
    fns: HashMap<String, FnDecl>,
    globals: Vec<(String, Value)>,
    locals: Vec<(String, Value)>,
    trace: Vec<(String, Span)>,
}

impl Evaluator {
    fn new(ctx: TypeCtx) -> Self {
        Evaluator { ctx, fns: HashMap::new(), globals: Vec::new(), locals: Vec::new(), trace: Vec::new() }
    }

    fn error(&self, span: Span, msg: impl Into<String>) -> EvalError {
        EvalError { span, msg: msg.into(), trace: self.trace.clone() }
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.locals
            .iter()
            .rev()
            .chain(self.globals.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    // ---------- inputs ----------

    fn decode(&mut self, iv: InputValue, t: &Ty) -> Result<Value, String> {
        match iv {
            InputValue::Json(v) => self.conform_top(v, t),
            InputValue::Raw(s) => self.decode_raw(s, t),
            InputValue::RawList(xs) => {
                let elem = match self.ctx.shape(t) {
                    Ty::List(e) => *e,
                    _ => Ty::Json,
                };
                let mut out = Vec::new();
                for (i, x) in xs.into_iter().enumerate() {
                    out.push(self.decode_raw(x, &elem).map_err(|m| format!("element {i}: {m}"))?);
                }
                self.conform_top(Value::List(out), t)
            }
            InputValue::RawMap(kvs) => {
                let shape = self.ctx.shape(t);
                let mut out = Vec::new();
                for (k, x) in kvs {
                    let vt = match &shape {
                        Ty::Map(v) => (**v).clone(),
                        Ty::Record(r) => r.get(&k).map_or(Ty::Json, |f| f.ty.clone()),
                        _ => Ty::Json,
                    };
                    out.push((k.clone(), self.decode_raw(x, &vt).map_err(|m| format!("key `{k}`: {m}"))?));
                }
                self.conform_top(Value::Obj(out), t)
            }
        }
    }

    /// A string is taken literally if the type accepts it; otherwise it is parsed as JSON.
    fn decode_raw(&mut self, s: String, t: &Ty) -> Result<Value, String> {
        match self.conform_top(Value::Str(s.clone()), t) {
            Ok(v) => Ok(v),
            Err(as_str) => match json::parse(&s) {
                Ok(v) => self.conform_top(v, t),
                Err(_) => Err(as_str),
            },
        }
    }

    // ---------- type conformance ----------

    fn conform_top(&mut self, v: Value, t: &Ty) -> Result<Value, String> {
        self.conform(v, t, &mut Vec::new(), None)
    }

    /// Check `v` against `t`, filling in defaults and dropping `null`s in optional fields.
    /// `alias` is the nearest named type, for messages.
    fn conform(&mut self, v: Value, t: &Ty, path: &mut Vec<String>, alias: Option<&str>) -> Result<Value, String> {
        // Name the alias, and show its definition when that is short (`Env` = "dev" | "prod").
        let shown = match alias {
            Some(a) if t.to_string().len() <= 60 && !matches!(t, Ty::Refine(..)) => format!("`{a}` (`{t}`)"),
            Some(a) => format!("`{a}`"),
            None => format!("`{t}`"),
        };
        let mismatch = |v: &Value, _: &Ty, path: &[String]| {
            at(path, format!("expected {shown}, got {} ({})", v.short(), v.kind()))
        };
        match t {
            Ty::Alias(n) => {
                if !self.ctx.aliases.contains_key(n) {
                    let te = TypeExpr { kind: TypeKind::Named(n.clone()), span: Span::default() };
                    self.ctx.resolve(&te);
                }
                let inner = self.ctx.unfold(t);
                let n = n.clone();
                self.conform(v, &inner, path, Some(&n))
            }
            Ty::Error | Ty::Json => Ok(v),
            Ty::Never => Err(at(path, format!("no value is allowed here, got {}", v.short()))),
            Ty::Lit(l) => {
                if v.loose_eq(&Value::from_lit(l)) { Ok(v) } else { Err(mismatch(&v, t, path)) }
            }
            Ty::Bool => if matches!(v, Value::Bool(_)) { Ok(v) } else { Err(mismatch(&v, t, path)) },
            Ty::Int => if matches!(v, Value::Int(_)) { Ok(v) } else { Err(mismatch(&v, t, path)) },
            Ty::Float => if matches!(v, Value::Int(_) | Value::Float(_)) { Ok(v) } else { Err(mismatch(&v, t, path)) },
            Ty::Str => if matches!(v, Value::Str(_)) { Ok(v) } else { Err(mismatch(&v, t, path)) },
            Ty::List(et) => {
                let Value::List(xs) = v else { return Err(mismatch(&v, t, path)) };
                let mut out = Vec::with_capacity(xs.len());
                for (i, x) in xs.into_iter().enumerate() {
                    path.push(format!("[{i}]"));
                    out.push(self.conform(x, et, path, None)?);
                    path.pop();
                }
                Ok(Value::List(out))
            }
            Ty::Tuple(ts) => {
                let Value::List(xs) = v else { return Err(mismatch(&v, t, path)) };
                if xs.len() != ts.len() {
                    return Err(at(path, format!("expected {} elements (`{t}`), got {}", ts.len(), xs.len())));
                }
                let mut out = Vec::with_capacity(xs.len());
                for (i, (x, et)) in xs.into_iter().zip(ts).enumerate() {
                    path.push(format!("[{i}]"));
                    out.push(self.conform(x, et, path, None)?);
                    path.pop();
                }
                Ok(Value::List(out))
            }
            Ty::Map(vt) => {
                let Value::Obj(es) = v else { return Err(mismatch(&v, t, path)) };
                let mut out = Vec::with_capacity(es.len());
                for (k, x) in es {
                    path.push(path_key(&k));
                    out.push((k, self.conform(x, vt, path, None)?));
                    path.pop();
                }
                Ok(Value::Obj(out))
            }
            Ty::Record(r) => self.conform_record(v, r, t, path, alias),
            Ty::Union(ms) => {
                let mut best: Option<String> = None;
                let mut best_depth = 0;
                for m in ms {
                    let mut p = path.clone();
                    match self.conform(v.clone(), m, &mut p, None) {
                        Ok(x) => return Ok(x),
                        Err(e) => {
                            // Prefer the member that got furthest into the value.
                            let depth = e.matches('.').count() + e.matches('[').count();
                            if self.same_kind(&v, m) && (best.is_none() || depth > best_depth) {
                                best_depth = depth;
                                best = Some(e);
                            }
                        }
                    }
                }
                Err(match best {
                    Some(e) => e,
                    None => at(path, format!("expected {shown}, got {} ({})", v.short(), v.kind())),
                })
            }
            Ty::Refine(b, p) => {
                let v = self.conform(v, b, path, alias)?;
                self.locals.push(("self".to_string(), v.clone()));
                let r = self.eval(p);
                self.locals.pop();
                let required_by = alias.map(|a| format!(" (required by `{a}`)")).unwrap_or_default();
                match r {
                    Ok(Value::Bool(true)) => Ok(v),
                    Ok(_) => {
                        // Name the conjunct that failed, not the whole predicate.
                        self.locals.push(("self".to_string(), v.clone()));
                        let failed = p.conjuncts().into_iter().find(|c| self.eval(c).ok() != Some(Value::Bool(true))).cloned();
                        self.locals.pop();
                        let shown = failed.unwrap_or_else(|| p.clone());
                        Err(at(path, format!("{} does not satisfy `{shown}`{required_by}", v.short())))
                    }
                    Err(e) => Err(at(path, format!("checking `{p}` failed: {}", e.msg))),
                }
            }
        }
    }

    /// Whether `v` is the same broad kind of value as type `t` accepts (for picking error messages).
    fn same_kind(&self, v: &Value, t: &Ty) -> bool {
        match (v, self.ctx.shape(t)) {
            (Value::Obj(_), Ty::Record(_) | Ty::Map(_)) => true,
            (Value::List(_), Ty::List(_) | Ty::Tuple(_)) => true,
            (Value::Str(_), Ty::Str) | (Value::Int(_), Ty::Int | Ty::Float) | (Value::Float(_), Ty::Float) => true,
            _ => false,
        }
    }

    fn conform_record(
        &mut self,
        v: Value,
        r: &Rec,
        t: &Ty,
        path: &mut Vec<String>,
        alias: Option<&str>,
    ) -> Result<Value, String> {
        let shown = alias.map_or_else(|| t.to_string(), str::to_string);
        let Value::Obj(entries) = v else {
            return Err(at(path, format!("expected `{shown}`, got {} ({})", v.short(), v.kind())));
        };
        if !r.open {
            if let Some((k, _)) = entries.iter().find(|(k, _)| r.get(k).is_none()) {
                let hint = suggest(k, r.fields.iter().map(|f| f.name.as_str()))
                    .map(|s| format!("; did you mean `{s}`?"))
                    .unwrap_or_default();
                return Err(at(path, format!("unknown field `{k}` in `{shown}`{hint}")));
            }
        }
        // Output order: fields that fill themselves in from a single-literal type
        // (`apiVersion`, `kind`) first, then the given fields in source order, then other defaults.
        let (mut front, mut back) = (Vec::new(), Vec::new());
        for f in &r.fields {
            if entries.iter().any(|(k, _)| *k == f.name) {
                continue;
            }
            path.push(path_key(&f.name));
            match &f.presence {
                Presence::Required => {
                    path.pop();
                    return Err(at(path, format!("missing required field `{}` (`{}`) in `{shown}`", f.name, f.ty)));
                }
                Presence::Optional => {}
                Presence::Default(e) => {
                    let saved = std::mem::take(&mut self.locals);
                    let d = self.eval(e);
                    self.locals = saved;
                    let d = d.map_err(|e| at(path, format!("evaluating default: {}", e.msg)))?;
                    let v = self.conform(d, &f.ty, path, None)?;
                    let singleton = matches!(self.ctx.unfold(&f.ty), Ty::Lit(_));
                    if singleton { &mut front } else { &mut back }.push((f.name.clone(), v));
                }
            }
            path.pop();
        }
        let mut out = front;
        for (k, x) in entries {
            let Some(f) = r.get(&k) else {
                out.push((k, x)); // extra field of an open record
                continue;
            };
            if x == Value::Null && f.presence == Presence::Optional && !self.ctx.sub(&Ty::null(), &f.ty, Mode::Erased) {
                continue;
            }
            path.push(path_key(&k));
            let v = self.conform(x, &f.ty, path, None)?;
            path.pop();
            out.push((k, v));
        }
        out.extend(back);
        Ok(Value::Obj(out))
    }

    // ---------- expressions ----------

    fn eval(&mut self, e: &Expr) -> Result<Value, EvalError> {
        match &e.kind {
            ExprKind::Lit(l) => Ok(Value::from_lit(l)),
            ExprKind::Interp(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        InterpPart::Lit(l) => s.push_str(l),
                        InterpPart::Expr(x) => {
                            let v = self.eval(x)?;
                            s.push_str(&self.stringify(&v, x.span)?);
                        }
                    }
                }
                Ok(Value::Str(s))
            }
            ExprKind::Var(n) => self.lookup(n).cloned().ok_or_else(|| self.error(e.span, format!("unknown variable `{n}`"))),
            ExprKind::List(items) => {
                let mut out = Vec::new();
                for item in items {
                    self.eval_item(item, &mut out)?;
                }
                Ok(Value::List(out))
            }
            ExprKind::Tuple(es) => Ok(Value::List(es.iter().map(|x| self.eval(x)).collect::<Result<_, _>>()?)),
            ExprKind::Object(ms) => {
                let mut entries = Vec::new();
                self.eval_members(ms, &mut entries)?;
                Ok(Value::Obj(entries.into_iter().map(|(k, v, _)| (k, v)).collect()))
            }
            ExprKind::Field(b, label, optional) => {
                let v = self.eval(b)?;
                match (&v, label) {
                    (Value::Null, _) if *optional => Ok(Value::Null),
                    (Value::Obj(_), Label::Name(k)) => Ok(v.get(k).cloned().unwrap_or(Value::Null)),
                    (Value::List(xs), Label::Pos(i)) => xs
                        .get(*i)
                        .cloned()
                        .ok_or_else(|| self.error(e.span, format!("index {i} out of bounds (length {})", xs.len()))),
                    _ => Err(self.error(e.span, format!("cannot read `.{label}` of {} ({})", v.short(), v.kind()))),
                }
            }
            ExprKind::Index(b, i) => {
                let v = self.eval(b)?;
                let idx = self.eval(i)?;
                match (&v, &idx) {
                    (Value::List(xs), Value::Int(n)) => usize::try_from(*n)
                        .ok()
                        .and_then(|n| xs.get(n))
                        .cloned()
                        .ok_or_else(|| self.error(e.span, format!("index {n} out of bounds (length {})", xs.len()))),
                    (Value::Obj(_), Value::Str(k)) => Ok(v.get(k).cloned().unwrap_or(Value::Null)),
                    _ => Err(self.error(e.span, format!("cannot index {} with {}", v.kind(), idx.short()))),
                }
            }
            ExprKind::Call(name, args) => self.call(name, args, e.span),
            ExprKind::Typed(tname, obj) => {
                let v = self.eval(obj)?;
                let t = Ty::Alias(tname.clone());
                self.conform_top(v, &t).map_err(|m| self.error(e.span, m))
            }
            ExprKind::Unary(op, x) => {
                let v = self.eval(x)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(i)) => {
                        i.checked_neg().map(Value::Int).ok_or_else(|| self.error(e.span, "integer overflow"))
                    }
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (_, v) => Err(self.error(e.span, format!("invalid operand {}", v.short()))),
                }
            }
            ExprKind::Binary(op, l, r) => self.binary(*op, l, r, e.span),
            ExprKind::If(c, t, f) => match self.eval(c)? {
                Value::Bool(true) => self.eval(t),
                Value::Bool(false) => self.eval(f),
                v => Err(self.error(c.span, format!("condition must be a bool, got {}", v.short()))),
            },
            ExprKind::Let(n, ty, v, body) => {
                let mut val = self.eval(v)?;
                if let Some(te) = ty {
                    let t = self.ctx.resolve(te);
                    val = self.conform_top(val, &t).map_err(|m| self.error(v.span, format!("`{n}`: {m}")))?;
                }
                self.locals.push((n.clone(), val));
                let r = self.eval(body);
                self.locals.pop();
                r
            }
            ExprKind::Match(s, arms) => {
                let v = self.eval(s)?;
                for arm in arms {
                    let mut binds = Vec::new();
                    if self.match_pat(&arm.pat, &v, &mut binds) {
                        let mark = self.locals.len();
                        self.locals.extend(binds);
                        let r = self.eval(&arm.body);
                        self.locals.truncate(mark);
                        return r;
                    }
                }
                Err(self.error(e.span, format!("no `match` arm matches {}", v.short())))
            }
        }
    }

    fn stringify(&self, v: &Value, span: Span) -> Result<String, EvalError> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => Ok(emit::to_json(v, false)),
            _ => Err(self.error(span, format!("cannot convert {} to a string", v.kind()))),
        }
    }

    fn eval_item(&mut self, item: &ListItem, out: &mut Vec<Value>) -> Result<(), EvalError> {
        match item {
            ListItem::Elem(e) => out.push(self.eval(e)?),
            ListItem::Spread(e) => match self.eval(e)? {
                Value::List(xs) => out.extend(xs),
                Value::Null => {}
                v => return Err(self.error(e.span, format!("cannot spread {} into a list", v.kind()))),
            },
            ListItem::If(c, inner) => {
                if self.cond(c)? {
                    self.eval_item(inner, out)?;
                }
            }
            ListItem::For(a, b, src, inner) => {
                for binds in self.iterate(a, b.as_deref(), src)? {
                    let mark = self.locals.len();
                    self.locals.extend(binds);
                    let r = self.eval_item(inner, out);
                    self.locals.truncate(mark);
                    r?;
                }
            }
        }
        Ok(())
    }

    fn cond(&mut self, c: &Expr) -> Result<bool, EvalError> {
        match self.eval(c)? {
            Value::Bool(b) => Ok(b),
            v => Err(self.error(c.span, format!("condition must be a bool, got {}", v.short()))),
        }
    }

    /// The variable bindings for each iteration of `for a(, b) in src`.
    fn iterate(&mut self, a: &str, b: Option<&str>, src: &Expr) -> Result<Vec<Vec<(String, Value)>>, EvalError> {
        let v = self.eval(src)?;
        let rows = match (v, b) {
            (Value::List(xs), None) => xs.into_iter().map(|x| vec![(a.to_string(), x)]).collect(),
            (Value::List(xs), Some(b)) => xs
                .into_iter()
                .enumerate()
                .map(|(i, x)| vec![(a.to_string(), Value::Int(i as i64)), (b.to_string(), x)])
                .collect(),
            (Value::Obj(es), None) => es.into_iter().map(|(k, _)| vec![(a.to_string(), Value::Str(k))]).collect(),
            (Value::Obj(es), Some(b)) => {
                es.into_iter().map(|(k, x)| vec![(a.to_string(), Value::Str(k)), (b.to_string(), x)]).collect()
            }
            (Value::Null, _) => vec![],
            (v, _) => return Err(self.error(src.span, format!("cannot iterate over {}", v.kind()))),
        };
        Ok(rows)
    }

    /// Object assembly. The flag marks entries that came from a spread and may be overridden.
    fn eval_members(&mut self, ms: &[Member], entries: &mut Vec<(String, Value, bool)>) -> Result<(), EvalError> {
        for m in ms {
            match m {
                Member::Field(k, e, span) => {
                    let v = self.eval(e)?;
                    set_key(entries, k.clone(), v, false).map_err(|m| self.error(*span, m))?;
                }
                Member::Computed(ke, ve) => {
                    let k = match self.eval(ke)? {
                        Value::Str(s) => s,
                        v => return Err(self.error(ke.span, format!("object keys must be strings, got {}", v.short()))),
                    };
                    let v = self.eval(ve)?;
                    set_key(entries, k, v, false).map_err(|m| self.error(ke.span, m))?;
                }
                Member::Spread(e) => match self.eval(e)? {
                    Value::Obj(es) => {
                        for (k, v) in es {
                            set_key(entries, k, v, true).map_err(|m| self.error(e.span, m))?;
                        }
                    }
                    Value::Null => {}
                    v => return Err(self.error(e.span, format!("cannot spread {} into an object", v.kind()))),
                },
                Member::If(c, t, els) => {
                    if self.cond(c)? {
                        self.eval_members(t, entries)?;
                    } else if let Some(els) = els {
                        self.eval_members(els, entries)?;
                    }
                }
                Member::For(a, b, src, body) => {
                    for binds in self.iterate(a, b.as_deref(), src)? {
                        let mark = self.locals.len();
                        self.locals.extend(binds);
                        let r = self.eval_members(body, entries);
                        self.locals.truncate(mark);
                        r?;
                    }
                }
            }
        }
        Ok(())
    }

    fn match_pat(&mut self, p: &Pat, v: &Value, binds: &mut Vec<(String, Value)>) -> bool {
        match &p.kind {
            PatKind::Wild => true,
            PatKind::Bind(n) => {
                binds.push((n.clone(), v.clone()));
                true
            }
            PatKind::Typed(n, te) => {
                let t = self.ctx.resolve(te);
                let ok = self.conform_top(v.clone(), &t).is_ok();
                if let (true, Some(n)) = (ok, n) {
                    binds.push((n.clone(), v.clone()));
                }
                ok
            }
            PatKind::Lit(l) => v.loose_eq(&Value::from_lit(l)),
            PatKind::Tuple(ps) => match v {
                Value::List(xs) if xs.len() == ps.len() => ps.iter().zip(xs).all(|(p, x)| self.match_pat(p, x, binds)),
                _ => false,
            },
            PatKind::Record(fs, open) => match v {
                Value::Obj(es) => {
                    let keys_ok = *open || (es.len() == fs.len() && es.iter().all(|(k, _)| fs.iter().any(|(f, _)| f == k)));
                    keys_ok
                        && fs.iter().all(|(k, p)| {
                            let x = v.get(k).cloned().unwrap_or(Value::Null);
                            self.match_pat(p, &x, binds)
                        })
                }
                _ => false,
            },
        }
    }

    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span) -> Result<Value, EvalError> {
        use BinOp::*;
        match op {
            And | Or => {
                let lv = self.cond(l)?;
                if lv == (op == Or) {
                    return Ok(Value::Bool(lv));
                }
                return Ok(Value::Bool(self.cond(r)?));
            }
            Coalesce => {
                let lv = self.eval(l)?;
                return if lv == Value::Null { self.eval(r) } else { Ok(lv) };
            }
            _ => {}
        }
        let a = self.eval(l)?;
        let b = self.eval(r)?;
        let bad = || format!("operator `{}` cannot be applied to {} and {}", op.symbol(), a.short(), b.short());
        Ok(match op {
            Eq => Value::Bool(a.loose_eq(&b)),
            Ne => Value::Bool(!a.loose_eq(&b)),
            Lt | Le | Gt | Ge => {
                let ord = match (&a, &b) {
                    (Value::Str(x), Value::Str(y)) => x.partial_cmp(y),
                    _ => match (num(&a), num(&b)) {
                        (Some(x), Some(y)) => x.partial_cmp(&y),
                        _ => None,
                    },
                };
                let Some(ord) = ord else { return Err(self.error(span, bad())) };
                Value::Bool(match op {
                    Lt => ord.is_lt(),
                    Le => ord.is_le(),
                    Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                })
            }
            Add | Sub | Mul | Div | Rem => match (&a, &b) {
                (Value::Str(x), Value::Str(y)) if op == Add => Value::Str(format!("{x}{y}")),
                (Value::Int(x), Value::Int(y)) => {
                    if matches!(op, Div | Rem) && *y == 0 {
                        return Err(self.error(span, "division by zero"));
                    }
                    let r = match op {
                        Add => x.checked_add(*y),
                        Sub => x.checked_sub(*y),
                        Mul => x.checked_mul(*y),
                        Div => x.checked_div(*y),
                        _ => x.checked_rem(*y),
                    };
                    Value::Int(r.ok_or_else(|| self.error(span, "integer overflow"))?)
                }
                _ => match (num(&a), num(&b)) {
                    (Some(x), Some(y)) => Value::Float(match op {
                        Add => x + y,
                        Sub => x - y,
                        Mul => x * y,
                        Div => x / y,
                        _ => x % y,
                    }),
                    _ => return Err(self.error(span, bad())),
                },
            },
            And | Or | Coalesce => unreachable!(),
        })
    }

    // ---------- calls ----------

    fn call(&mut self, name: &str, args: &[Arg], span: Span) -> Result<Value, EvalError> {
        if let Some(f) = self.fns.get(name).cloned() {
            return self.call_user(&f, args, span);
        }
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.eval(&a.value)?);
        }
        builtin(name, vals).map_err(|m| self.error(span, format!("{name}: {m}")))
    }

    fn call_user(&mut self, f: &FnDecl, args: &[Arg], span: Span) -> Result<Value, EvalError> {
        if self.trace.len() > 256 {
            return Err(self.error(span, "call depth limit exceeded"));
        }
        // Evaluate arguments in the caller's scope.
        let mut slots: Vec<Option<(Value, Span)>> = vec![None; f.params.len()];
        let mut next = 0;
        for a in args {
            let i = match &a.name {
                Some(n) => f.params.iter().position(|p| p.name == *n),
                None => {
                    while next < slots.len() && slots[next].is_some() {
                        next += 1;
                    }
                    Some(next)
                }
            };
            let Some(i) = i.filter(|i| *i < slots.len()) else {
                return Err(self.error(a.value.span, format!("bad argument to `{}`", f.name)));
            };
            slots[i] = Some((self.eval(&a.value)?, a.value.span));
        }
        // Bind parameters in the callee's scope; defaults and refinements may use earlier parameters.
        let saved = std::mem::take(&mut self.locals);
        self.trace.push((f.name.clone(), span));
        let result = (|| {
            for (p, slot) in f.params.iter().zip(slots) {
                let (v, at) = match slot {
                    Some(x) => x,
                    None => match &p.default {
                        Some(d) => (self.eval(d)?, d.span),
                        None => return Err(self.error(span, format!("missing argument `{}`", p.name))),
                    },
                };
                let t = self.ctx.resolve(&p.ty);
                let v = self
                    .conform_top(v, &t)
                    .map_err(|m| self.error(at, format!("argument `{}` of `{}`: {m}", p.name, f.name)))?;
                self.locals.push((p.name.clone(), v));
            }
            let v = self.eval(&f.body)?;
            match &f.ret {
                Some(te) => {
                    let t = self.ctx.resolve(te);
                    self.conform_top(v, &t).map_err(|m| self.error(f.body.span, format!("result of `{}`: {m}", f.name)))
                }
                None => Ok(v),
            }
        })();
        self.trace.pop();
        self.locals = saved;
        result
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn set_key(entries: &mut Vec<(String, Value, bool)>, k: String, v: Value, from_spread: bool) -> Result<(), String> {
    let Some(slot) = entries.iter_mut().find(|(ek, _, _)| *ek == k) else {
        entries.push((k, v, from_spread));
        return Ok(());
    };
    if slot.2 || from_spread {
        // Spreads are defaults: later members replace them (and vice versa).
        slot.1 = v;
        slot.2 = from_spread;
        return Ok(());
    }
    let merged = deep_merge(std::mem::replace(&mut slot.1, Value::Null), v, &mut vec![path_key(&k)])?;
    slot.1 = merged;
    Ok(())
}

fn deep_merge(a: Value, b: Value, path: &mut Vec<String>) -> Result<Value, String> {
    match (a, b) {
        (Value::Obj(mut xs), Value::Obj(ys)) => {
            for (k, y) in ys {
                match xs.iter().position(|(xk, _)| *xk == k) {
                    Some(i) => {
                        path.push(path_key(&k));
                        let x = std::mem::replace(&mut xs[i].1, Value::Null);
                        xs[i].1 = deep_merge(x, y, path)?;
                        path.pop();
                    }
                    None => xs.push((k, y)),
                }
            }
            Ok(Value::Obj(xs))
        }
        _ => Err(format!("duplicate key `{}`", fmt_path(path).trim_start_matches('.'))),
    }
}

// ---------- built-in functions (SPEC §5.4) ----------

pub const BUILTINS: [&str; 20] = [
    "len", "keys", "values", "str", "upper", "lower", "join", "split", "contains", "starts_with", "ends_with",
    "replace", "matches", "min", "max", "to_json", "to_yaml", "b64encode", "error", "sort",
];

/// Signature and one-line description of a built-in, for hover.
pub fn builtin_doc(name: &str) -> &'static str {
    match name {
        "len" => "len(x: Str | [Json] | { [Str]: Json }) -> Int\n// characters, elements or keys",
        "keys" => "keys(o: { [Str]: Json }) -> [Str]",
        "values" => "values(o: { [Str]: T }) -> [T]",
        "str" => "str(x: Str | Float | Bool | null) -> Str",
        "upper" => "upper(s: Str) -> Str",
        "lower" => "lower(s: Str) -> Str",
        "join" => "join(xs: [Str], sep: Str) -> Str",
        "split" => "split(s: Str, sep: Str) -> [Str]",
        "contains" => "contains(s: Str, sub: Str) -> Bool\ncontains(xs: [T], x: T) -> Bool",
        "starts_with" => "starts_with(s: Str, prefix: Str) -> Bool",
        "ends_with" => "ends_with(s: Str, suffix: Str) -> Bool",
        "replace" => "replace(s: Str, from: Str, to: Str) -> Str",
        "matches" => "matches(s: Str, regex: Str) -> Bool\n// unanchored; use ^ and $ to match the whole string",
        "min" => "min(a: Float, b: Float) -> Float  // Int if both are Int",
        "max" => "max(a: Float, b: Float) -> Float  // Int if both are Int",
        "sort" => "sort(xs: [Str] | [Float]) -> same type",
        "to_json" => "to_json(v: Json) -> Str",
        "to_yaml" => "to_yaml(v: Json) -> Str",
        "b64encode" => "b64encode(s: Str) -> Str",
        "error" => "error(msg: Str) -> Never\n// aborts evaluation with msg",
        _ => "",
    }
}

fn builtin(name: &str, args: Vec<Value>) -> Result<Value, String> {
    use Value::*;
    let bad = |args: &[Value]| {
        format!("invalid arguments ({})", args.iter().map(Value::kind).collect::<Vec<_>>().join(", "))
    };
    Ok(match (name, args.as_slice()) {
        ("len", [Str(s)]) => Int(s.chars().count() as i64),
        ("len", [List(xs)]) => Int(xs.len() as i64),
        ("len", [Obj(es)]) => Int(es.len() as i64),
        ("keys", [Obj(es)]) => List(es.iter().map(|(k, _)| Str(k.clone())).collect()),
        ("values", [Obj(es)]) => List(es.iter().map(|(_, v)| v.clone()).collect()),
        ("str", [Str(s)]) => Str(s.clone()),
        ("str", [v @ (Null | Bool(_) | Int(_) | Float(_))]) => Str(emit::to_json(v, false)),
        ("upper", [Str(s)]) => Str(s.to_uppercase()),
        ("lower", [Str(s)]) => Str(s.to_lowercase()),
        ("join", [List(xs), Str(sep)]) => {
            let parts: Option<Vec<&str>> = xs.iter().map(|x| if let Str(s) = x { Some(s.as_str()) } else { None }).collect();
            Str(parts.ok_or("join expects a list of strings")?.join(sep))
        }
        ("split", [Str(s), Str(sep)]) => List(s.split(sep.as_str()).map(|p| Str(p.to_string())).collect()),
        ("contains", [Str(s), Str(x)]) => Bool(s.contains(x.as_str())),
        ("contains", [List(xs), x]) => Bool(xs.iter().any(|y| y.loose_eq(x))),
        ("starts_with", [Str(s), Str(p)]) => Bool(s.starts_with(p.as_str())),
        ("ends_with", [Str(s), Str(p)]) => Bool(s.ends_with(p.as_str())),
        ("replace", [Str(s), Str(from), Str(to)]) => Str(s.replace(from.as_str(), to)),
        ("matches", [Str(s), Str(re)]) => Bool(regex_cached(re)?.is_match(s)),
        ("min" | "max", [Int(a), Int(b)]) => Int(if name == "min" { *a.min(b) } else { *a.max(b) }),
        ("min" | "max", [a, b]) => match (num(a), num(b)) {
            (Some(x), Some(y)) => Float(if name == "min" { x.min(y) } else { x.max(y) }),
            _ => return Err(bad(&args)),
        },
        ("sort", [List(xs)]) => {
            let mut xs = xs.clone();
            if xs.iter().all(|x| matches!(x, Str(_))) {
                xs.sort_by(|a, b| match (a, b) {
                    (Str(x), Str(y)) => x.cmp(y),
                    _ => unreachable!(),
                });
            } else if xs.iter().all(|x| num(x).is_some()) {
                xs.sort_by(|a, b| num(a).unwrap().total_cmp(&num(b).unwrap()));
            } else {
                return Err("sort expects a list of strings or a list of numbers".into());
            }
            List(xs)
        }
        ("to_json", [v]) => Str(emit::to_json(v, true)),
        ("to_yaml", [v]) => Str(emit::to_yaml(v)),
        ("b64encode", [Str(s)]) => Str(emit::base64(s.as_bytes())),
        ("error", [Str(msg)]) => return Err(msg.clone()),
        _ if BUILTINS.contains(&name) => return Err(bad(&args)),
        _ => return Err(format!("unknown function `{name}`")),
    })
}

/// Compiled regexes, since refinements from schemas run `matches` for every value.
fn regex_cached(re: &str) -> Result<regex::Regex, String> {
    thread_local! {
        static CACHE: std::cell::RefCell<HashMap<String, regex::Regex>> = Default::default();
    }
    CACHE.with(|c| {
        if let Some(r) = c.borrow().get(re) {
            return Ok(r.clone());
        }
        let r = regex::Regex::new(re).map_err(|e| format!("invalid regex: {e}"))?;
        c.borrow_mut().insert(re.to_string(), r.clone());
        Ok(r)
    })
}
