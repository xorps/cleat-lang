//! Static checking (SPEC §7 steps 2–3): bidirectional type checking with
//! field-aware diagnostics, plus verification conditions for refinements.

use std::collections::HashSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::eval::{BUILTINS, suggest};
use crate::types::{BUILTIN_TYPES, Field, Mode, Presence, Rec, Ty, TypeCtx};
use crate::vc::{Vc, VcStatus, triage};

#[derive(Debug, Default)]
pub struct Outcome {
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
    pub vcs: Vec<Vc>,
    /// Hover text for identifiers, fields and keys (for the language server).
    pub hovers: Vec<Hover>,
    /// What this program offers when imported as a module.
    pub fns: Vec<(String, Signature)>,
    pub globals: Vec<(String, Ty)>,
    /// Resolved type aliases, for the evaluator.
    pub ctx: TypeCtx,
}

#[derive(Debug, Clone)]
pub struct Hover {
    pub span: Span,
    /// Length in characters of the token at `span`.
    pub len: usize,
    pub text: String,
    /// Documentation (Markdown), e.g. from OpenAPI descriptions.
    pub doc: Option<String>,
}

/// A function signature as seen by importers.
#[derive(Debug, Clone)]
pub struct Signature {
    pub params: Vec<(String, Ty, Option<Expr>)>,
    pub ret: Ty,
}

/// A checked `.cleat` module, cached by path and modification time.
pub struct CheckedModule {
    pub outcome: Outcome,
}

type CheckedCache = std::sync::Mutex<HashMap<PathBuf, (std::time::SystemTime, std::sync::Arc<CheckedModule>)>>;

thread_local! {
    /// Modules being checked, innermost last (for import cycle detection).
    static CHECKING: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Check a `.cleat` module on its own (its imports resolve relative to its directory).
pub fn checked_module(path: &Path) -> Result<std::sync::Arc<CheckedModule>, String> {
    static CACHE: std::sync::OnceLock<CheckedCache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let canon = path.canonicalize().map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let mtime = std::fs::metadata(&canon).and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    if let Some((t, c)) = cache.lock().unwrap().get(&canon) {
        if *t == mtime {
            return Ok(c.clone());
        }
    }
    let cycle = CHECKING.with(|s| {
        let s = s.borrow();
        s.iter().position(|p| *p == canon).map(|i| {
            let mut names: Vec<String> = s[i..].iter().map(|p| p.display().to_string()).collect();
            names.push(canon.display().to_string());
            names.join(" -> ")
        })
    });
    if let Some(c) = cycle {
        return Err(format!("import cycle: {c}"));
    }
    let module = crate::modules::load(&canon)?;
    CHECKING.with(|s| s.borrow_mut().push(canon.clone()));
    let outcome = check_program(&module.items, canon.parent());
    CHECKING.with(|s| s.borrow_mut().pop());
    let checked = std::sync::Arc::new(CheckedModule { outcome });
    cache.lock().unwrap().insert(canon, (mtime, checked.clone()));
    Ok(checked)
}

#[derive(Clone)]
struct FnSig {
    params: Vec<(String, Ty, Option<Expr>)>,
    ret: Ty,
    index: usize,
}

/// A pattern with its type annotations resolved.
enum RPat {
    Wild,
    Bind(String, Span),
    Typed(Option<String>, Ty, Span),
    Lit(Lit, Span),
    Tuple(Vec<RPat>, Span),
    Record(Vec<(String, RPat)>, bool, Span),
}

/// Check a program; imports are resolved relative to `base` (the file's directory).
pub fn check_program(items: &[Item], base: Option<&Path>) -> Outcome {
    let mut c = Checker {
        base: base.map(Path::to_path_buf),
        ctx: TypeCtx::default(),
        fns: HashMap::new(),
        scope: Vec::new(),
        hyps: Vec::new(),
        cur_fn: "<top level>".into(),
        cur_index: None,
        labels: Vec::new(),
        namespaces: HashSet::new(),
        global_names: Vec::new(),
        patch_path: String::new(),
        out: Outcome::default(),
    };
    c.run(items);
    let mut fns: Vec<(String, Signature)> =
        c.fns.iter().map(|(n, s)| (n.clone(), Signature { params: s.params.clone(), ret: s.ret.clone() })).collect();
    fns.sort_by(|a, b| a.0.cmp(&b.0));
    c.out.fns = fns;
    let globals: Vec<(String, Ty)> = c
        .global_names
        .iter()
        .filter_map(|n| c.scope.iter().rev().find(|(s, _)| s == n).map(|(_, t)| (n.clone(), t.clone())))
        .collect();
    c.out.globals = globals;
    let mut out = c.out;
    out.ctx = c.ctx;
    out.errors.sort_by_key(|d| (d.span.line, d.span.col));
    out.warnings.sort_by_key(|d| (d.span.line, d.span.col));
    out
}

/// Prefer the field expression of an object literal over `e.f`, so VCs read naturally.
fn project(subject: &Expr, name: &str) -> Expr {
    if let ExprKind::Object(ms) = &subject.kind {
        if let Some(e) = ms.iter().rev().find_map(|m| match m {
            Member::Field(k, e, _) if k == name => Some(e),
            _ => None,
        }) {
            return e.clone();
        }
    }
    Expr::field(subject.clone(), Label::Name(name.to_string()))
}

fn mentions(e: &Expr, name: &str) -> bool {
    let probe = Expr::var("\u{0}", e.span);
    e.subst(name, &probe) != *e
}

fn did_you_mean<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> String {
    suggest(name, candidates).map(|s| format!("; did you mean `{s}`?")).unwrap_or_default()
}

struct Checker {
    ctx: TypeCtx,
    fns: HashMap<String, FnSig>,
    /// Variables in scope, innermost last.
    scope: Vec<(String, Ty)>,
    /// Path conditions and facts known at the current program point.
    hyps: Vec<Expr>,
    cur_fn: String,
    cur_index: Option<usize>,
    /// Names for anonymous nested types in messages (`Svc.spec.ports[]`), keyed by
    /// the span of the literal they describe.
    labels: Vec<(Span, String)>,
    /// Imported namespaces (`import ns from ...`).
    namespaces: HashSet<String>,
    /// Top-level `let`s and `input`s, offered to importers.
    global_names: Vec<String>,
    /// The `with` path being checked, for messages.
    patch_path: String,
    base: Option<PathBuf>,
    out: Outcome,
}

type Marks = (usize, Vec<Expr>);

impl Checker {
    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.out.errors.push(Diag::new(span, msg));
    }

    fn warn(&mut self, span: Span, msg: impl Into<String>) {
        self.out.warnings.push(Diag::new(span, msg));
    }

    fn norm(&self, t: &Ty) -> Ty {
        self.ctx.norm(t)
    }

    fn sub(&self, a: &Ty, b: &Ty) -> bool {
        self.ctx.sub(a, b, Mode::Erased)
    }

    fn union(&self, ts: Vec<Ty>) -> Ty {
        self.ctx.union(ts)
    }

    fn marks(&self) -> Marks {
        (self.scope.len(), self.hyps.clone())
    }

    fn reset(&mut self, m: Marks) {
        self.scope.truncate(m.0);
        self.hyps = m.1;
    }

    /// Union members without expanding `Bool`/`Json`, keeping alias names where possible.
    fn union_members(&self, t: &Ty) -> Vec<Ty> {
        match self.norm(t) {
            Ty::Never => vec![],
            Ty::Union(ms) => ms.iter().flat_map(|m| self.union_members(m)).collect(),
            a @ Ty::Alias(_) => match self.ctx.unfold(&a) {
                Ty::Union(_) => self.union_members(&self.ctx.unfold(&a)),
                _ => vec![a],
            },
            m => vec![m],
        }
    }

    fn hover(&mut self, span: Span, token: &str, text: String) {
        self.out.hovers.push(Hover { span, len: token.chars().count(), text, doc: None });
    }

    fn hover_doc(&mut self, span: Span, token: &str, text: String, doc: Option<String>) {
        self.out.hovers.push(Hover { span, len: token.chars().count(), text, doc });
    }

    /// Follow aliases that are just other names (`k8s.Deployment` -> `k8s.apps.v1.Deployment`).
    fn canonical(&self, name: &str) -> String {
        let mut n = name.to_string();
        for _ in 0..8 {
            match self.ctx.alias_src.get(&n).map(|te| &te.kind) {
                Some(TypeKind::Named(m)) if self.ctx.alias_src.contains_key(m) => n = m.clone(),
                _ => break,
            }
        }
        n
    }

    fn type_doc(&self, alias: &str) -> Option<String> {
        self.ctx.docs.get(&self.canonical(alias)).cloned()
    }

    fn field_doc(&self, record: &Ty, field: &str) -> Option<String> {
        match self.norm(record) {
            Ty::Alias(n) => self.ctx.docs.get(&format!("{}.{field}", self.canonical(&n))).cloned(),
            _ => None,
        }
    }

    /// `refine T with { ... }`: `T` becomes `T#n with { ... }`, where `T#n` is the old definition.
    fn refine(&mut self, name: &str, patches: &[Patch], span: Span) {
        // Refine the definition itself, so every reference sees it (`k8s.ServiceSpec` -> `k8s.core.v1.ServiceSpec`).
        let canon = self.canonical(name);
        let name = canon.as_str();
        let Some(old) = self.ctx.alias_src.get(name).cloned() else {
            let msg = self.ctx.unknown_type_message(name);
            return self.error(span, msg);
        };
        let mut n = 1;
        while self.ctx.alias_src.contains_key(&format!("{name}#{n}")) {
            n += 1;
        }
        let base = format!("{name}#{n}");
        self.ctx.alias_src.insert(base.clone(), old);
        let te = TypeExpr {
            kind: TypeKind::With(Box::new(TypeExpr { kind: TypeKind::Named(base), span }), patches.to_vec()),
            span,
        };
        self.ctx.alias_src.insert(name.to_string(), te);
    }

    /// Register an imported module's types as `ns.T`.
    fn import(&mut self, ns: &str, path: &str, span: Span) {
        let full = match &self.base {
            Some(b) => b.join(path),
            None => PathBuf::from(path),
        };
        if !self.namespaces.insert(ns.to_string()) {
            return self.error(span, format!("`{ns}` is already imported"));
        }
        let module = match crate::modules::load(&full) {
            Ok(m) => m,
            Err(e) => return self.error(span, format!("import `{ns}`: {e}")),
        };
        if module.is_source {
            return self.import_source(ns, &full, span);
        }
        let prefix = format!("{ns}.");
        let locals: HashSet<&str> = module
            .items
            .iter()
            .filter_map(|i| if let Item::Type { name, .. } = i { Some(name.as_str()) } else { None })
            .collect();
        for item in &module.items {
            if let Item::Type { name, ty, .. } = item {
                self.ctx.alias_src.insert(format!("{prefix}{name}"), qualify(ty, &prefix, &locals));
            }
        }
        for (k, d) in &module.docs {
            self.ctx.docs.insert(format!("{prefix}{k}"), d.clone());
        }
    }

    /// Import a `.cleat` module: its types, functions and constants become `ns.x`.
    fn import_source(&mut self, ns: &str, full: &Path, span: Span) {
        let checked = match checked_module(full) {
            Ok(c) => c,
            Err(e) => return self.error(span, format!("import `{ns}`: {e}")),
        };
        let out = &checked.outcome;
        for e in out.errors.iter().take(5) {
            self.error(span, format!("import `{ns}`: {}:{}: {}", full.display(), e.span, e.msg));
        }
        let prefix = format!("{ns}.");
        let keys: HashSet<&str> = out.ctx.alias_src.keys().map(String::as_str).collect();
        let q = |t: &Ty| t.map_aliases(&|n| keys.contains(n).then(|| format!("{prefix}{n}")));
        for (name, te) in &out.ctx.alias_src {
            self.ctx.alias_src.insert(format!("{prefix}{name}"), qualify(te, &prefix, &keys));
        }
        for (k, d) in &out.ctx.docs {
            self.ctx.docs.insert(format!("{prefix}{k}"), d.clone());
        }
        for (name, sig) in &out.fns {
            let params = sig.params.iter().map(|(p, t, d)| (p.clone(), q(t), d.clone())).collect();
            self.fns.insert(format!("{prefix}{name}"), FnSig { params, ret: q(&sig.ret), index: usize::MAX });
        }
        for (name, t) in &out.globals {
            let t = q(t);
            self.scope.push((format!("{prefix}{name}"), t));
        }
        // Resolve the types the signatures and constants mention, so they are checked against.
        let mut names = Vec::new();
        for (_, sig) in &out.fns {
            for (_, t, _) in &sig.params {
                q(t).alias_names(&mut names);
            }
            q(&sig.ret).alias_names(&mut names);
        }
        for (_, t) in &out.globals {
            q(t).alias_names(&mut names);
        }
        for n in names {
            self.ctx.resolve(&TypeExpr { kind: TypeKind::Named(n), span });
        }
        self.drain_type_diags();
    }

    fn field_sig(f: &Field) -> String {
        let q = if f.presence == Presence::Optional { "?" } else { "" };
        let d = match &f.presence {
            Presence::Default(e) if !matches!(f.ty, Ty::Lit(_)) => format!(" = {e}"),
            _ => String::new(),
        };
        format!("{}{q}: {}{d}", fmt_key(&f.name), f.ty)
    }

    fn fn_sig_text(name: &str, sig: &FnSig) -> String {
        let ps: Vec<String> = sig
            .params
            .iter()
            .map(|(p, t, d)| match d {
                Some(d) => format!("{p}: {t} = {d}"),
                None => format!("{p}: {t}"),
            })
            .collect();
        format!("fn {name}({}) -> {}", ps.join(", "), sig.ret)
    }

    fn alias_text(&self, name: &str) -> String {
        match self.ctx.aliases.get(name) {
            Some(t) => format!("type {name} = {t}"),
            None => format!("type {name}"),
        }
    }

    fn label_for(&self, span: Span) -> Option<String> {
        self.labels.iter().rev().find(|(s, _)| *s == span).map(|(_, l)| l.clone())
    }

    /// Check `e` against `t`, naming it `label` in messages if it is an anonymous literal.
    fn check_labeled(&mut self, e: &Expr, t: &Ty, label: String) {
        self.labels.push((e.span, label));
        self.check(e, t);
        self.labels.pop();
    }

    fn is_null(&self, t: &Ty) -> bool {
        self.ctx.shape(t) == Ty::null()
    }

    // ---------- items ----------

    fn run(&mut self, items: &[Item]) {
        for item in items {
            if let Item::Import { name, path, span } = item {
                self.import(name, path, *span);
            }
        }
        for item in items {
            if let Item::Type { name, ty, span } = item {
                if BUILTIN_TYPES.contains(&name.as_str()) || self.ctx.alias_src.contains_key(name) {
                    self.error(*span, format!("type `{name}` is already defined"));
                } else {
                    self.ctx.alias_src.insert(name.clone(), ty.clone());
                }
            }
        }
        for item in items {
            if let Item::Refine { name, patches, span } = item {
                self.refine(name, patches, *span);
            }
        }
        for item in items {
            if let Item::Type { name, ty, span } = item {
                let named = TypeExpr { kind: TypeKind::Named(name.clone()), span: *span };
                self.ctx.resolve(&named);
                self.drain_type_diags();
                self.check_type_preds(ty);
            }
        }

        let mut components: Vec<(String, Span, Vec<(String, Span)>)> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match item {
                Item::Type { .. } | Item::Import { .. } => {}
                Item::Refine { name, .. } => {
                    // Resolve now so patch errors are reported even if the type is unused.
                    self.ctx.resolve(&TypeExpr { kind: TypeKind::Named(name.clone()), span: item_span(item) });
                    self.drain_type_diags();
                }
                Item::Input { name, ty, default, span } => {
                    let t = self.resolve_te(ty);
                    if let Some(d) = default {
                        self.check(d, &t);
                    }
                    self.bind_var(name, t, *span);
                    self.global_names.push(name.clone());
                }
                Item::Let { name, ty, value, span } => {
                    let t = match ty {
                        Some(te) => {
                            let t = self.resolve_te(te);
                            self.check(value, &t);
                            t
                        }
                        None => self.synth(value),
                    };
                    self.bind_let(name, t, value, *span);
                    self.global_names.push(name.clone());
                }
                Item::Fn(f) => self.check_fn(f, index),
                Item::Export { name, after, value, span } => {
                    self.synth(value);
                    if let Some(n) = name {
                        if components.iter().any(|(c, _, _)| c == n) {
                            self.error(*span, format!("component `{n}` is already exported"));
                        } else {
                            components.push((n.clone(), *span, after.clone()));
                        }
                    }
                }
            }
        }
        self.check_components(&components);
    }

    /// `after` must name exported components and must not form a cycle.
    fn check_components(&mut self, comps: &[(String, Span, Vec<(String, Span)>)]) {
        let names: Vec<&str> = comps.iter().map(|(n, _, _)| n.as_str()).collect();
        for (n, _, after) in comps {
            for (d, dspan) in after {
                if d == n {
                    self.error(*dspan, format!("component `{n}` cannot depend on itself"));
                } else if !names.contains(&d.as_str()) {
                    let hint = did_you_mean(d, names.iter().copied());
                    self.error(*dspan, format!("unknown component `{d}` in `after`{hint}"));
                }
            }
        }
        // Depth-first search for cycles; report each once.
        fn visit<'a>(
            n: &'a str,
            comps: &'a [(String, Span, Vec<(String, Span)>)],
            stack: &mut Vec<&'a str>,
            done: &mut HashSet<&'a str>,
            cycles: &mut Vec<(Span, String)>,
        ) {
            if done.contains(n) {
                return;
            }
            if let Some(pos) = stack.iter().position(|s| *s == n) {
                let mut path: Vec<&str> = stack[pos..].to_vec();
                path.push(n);
                let span = comps.iter().find(|(c, _, _)| c == n).map_or(Span::default(), |(_, s, _)| *s);
                cycles.push((span, format!("dependency cycle between components: {}", path.join(" -> "))));
                return;
            }
            stack.push(n);
            if let Some((_, _, after)) = comps.iter().find(|(c, _, _)| c == n) {
                for (d, _) in after {
                    if d != n {
                        visit(d, comps, stack, done, cycles);
                    }
                }
            }
            stack.pop();
            done.insert(n);
        }
        let mut done = HashSet::new();
        let mut cycles = Vec::new();
        for (n, _, _) in comps {
            visit(n, comps, &mut Vec::new(), &mut done, &mut cycles);
        }
        for (span, msg) in cycles {
            self.error(span, msg);
        }
    }

    fn check_fn(&mut self, f: &FnDecl, index: usize) {
        if self.fns.contains_key(&f.name) || BUILTINS.contains(&f.name.as_str()) {
            self.error(f.span, format!("function `{}` is already defined", f.name));
            return;
        }
        let m = self.marks();
        let mut params = Vec::new();
        for p in &f.params {
            let t = self.resolve_te(&p.ty);
            if let Some(d) = &p.default {
                self.check(d, &t);
            }
            self.bind_var(&p.name, t.clone(), p.span);
            params.push((p.name.clone(), t, p.default.clone()));
        }
        let ret_ann = f.ret.as_ref().map(|te| self.resolve_te(te));
        self.fns.insert(f.name.clone(), FnSig { params, ret: ret_ann.clone().unwrap_or(Ty::Error), index });
        let saved = (std::mem::replace(&mut self.cur_fn, f.name.clone()), self.cur_index.replace(index));
        let ret = match ret_ann {
            Some(t) => {
                self.check(&f.body, &t);
                t
            }
            None => self.synth(&f.body),
        };
        (self.cur_fn, self.cur_index) = saved;
        self.fns.get_mut(&f.name).unwrap().ret = ret;
        self.reset(m);
    }

    // ---------- types ----------

    fn drain_type_diags(&mut self) {
        let ds = std::mem::take(&mut self.ctx.diags);
        self.out.errors.extend(ds);
    }

    fn quiet_resolve(&mut self, te: &TypeExpr) -> Ty {
        let n = self.ctx.diags.len();
        let t = self.ctx.resolve(te);
        self.ctx.diags.truncate(n);
        t
    }

    fn resolve_te(&mut self, te: &TypeExpr) -> Ty {
        let t = self.ctx.resolve(te);
        self.drain_type_diags();
        self.check_type_preds(te);
        t
    }

    /// Type-check refinement predicates and field defaults inside a type expression.
    fn check_type_preds(&mut self, te: &TypeExpr) {
        match &te.kind {
            TypeKind::Refine(b, p) => {
                self.check_type_preds(b);
                let base = self.quiet_resolve(b);
                self.scope.push(("self".to_string(), base));
                let t = self.synth(p);
                self.scope.pop();
                if !self.sub(&t, &Ty::Bool) {
                    self.error(p.span, format!("refinement predicate must be `Bool`, found `{t}`"));
                }
            }
            TypeKind::List(t) | TypeKind::Map(t) => self.check_type_preds(t),
            TypeKind::Tuple(ts) | TypeKind::Union(ts) => {
                for t in ts {
                    self.check_type_preds(t);
                }
            }
            TypeKind::Record { fields, .. } => {
                for fd in fields {
                    self.check_type_preds(&fd.ty);
                    if let Some(d) = &fd.default {
                        let ft = self.quiet_resolve(&fd.ty);
                        self.check(d, &ft);
                    }
                }
            }
            TypeKind::With(b, patches) => {
                self.check_type_preds(b);
                let base = self.quiet_resolve(b);
                for p in patches {
                    match &p.op {
                        PatchOp::Set(t, d) => {
                            self.check_type_preds(t);
                            if let Some(d) = d {
                                let ft = self.quiet_resolve(t);
                                self.check(d, &ft);
                            }
                        }
                        PatchOp::Where(pred) => {
                            let at = self.ctx.type_at(&base, &p.path).unwrap_or(Ty::Error);
                            self.scope.push(("self".to_string(), at));
                            let t = self.synth(pred);
                            self.scope.pop();
                            if !self.sub(&t, &Ty::Bool) {
                                self.error(pred.span, format!("refinement predicate must be `Bool`, found `{t}`"));
                            }
                        }
                        PatchOp::Remove => {}
                    }
                }
            }
            TypeKind::Named(n) => {
                if self.ctx.aliases.contains_key(n) {
                    let text = self.alias_text(n);
                    let doc = self.type_doc(n);
                    self.hover_doc(te.span, n, text, doc);
                }
            }
            TypeKind::Lit(_) => {}
        }
    }

    // ---------- environment ----------

    fn lookup(&self, name: &str) -> Option<&Ty> {
        self.scope.iter().rev().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    fn bind(&mut self, name: &str, t: Ty, span: Span) {
        if name == "self" {
            self.error(span, "`self` is reserved for refinement predicates");
        }
        if self.lookup(name).is_some() {
            // Facts about the outer variable no longer describe this name.
            self.hyps.retain(|h| !mentions(h, name));
        }
        self.scope.push((name.to_string(), t));
    }

    /// Bind a variable and record what its type tells us about it.
    fn bind_var(&mut self, name: &str, t: Ty, span: Span) {
        self.bind(name, t.clone(), span);
        let facts = self.facts(&t, &Expr::var(name, span), 0);
        self.hyps.extend(facts);
    }

    fn bind_let(&mut self, name: &str, t: Ty, value: &Expr, span: Span) {
        self.bind_var(name, t.clone(), span);
        if value.is_pure() && !mentions(value, name) && self.sub(&t, &Ty::Union(vec![Ty::Float, Ty::Str, Ty::Bool])) {
            self.hyps.push(Expr::binary(BinOp::Eq, Expr::var(name, span), value.clone()));
        }
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
            Ty::Alias(_) => self.facts(&self.ctx.unfold(ty), subject, depth + 1),
            Ty::Record(r) => r
                .fields
                .iter()
                .filter(|f| f.present())
                .flat_map(|f| self.facts(&f.ty, &project(subject, &f.name), depth + 1))
                .collect(),
            _ => vec![],
        }
    }

    /// Enter a branch where `c` is `positive`: record the fact and narrow variables.
    fn assume(&mut self, c: &Expr, positive: bool) {
        if c.is_pure() {
            self.hyps.push(if positive { c.clone() } else { c.negate() });
        }
        for (n, t) in self.narrowings(c, positive) {
            self.scope.push((n, t));
        }
    }

    /// `x != null`, `x == "lit"` and conjunctions of them narrow `x`.
    fn narrowings(&self, c: &Expr, positive: bool) -> Vec<(String, Ty)> {
        match &c.kind {
            ExprKind::Binary(op @ (BinOp::Eq | BinOp::Ne), l, r) => {
                let (var, lit) = match (&l.kind, &r.kind) {
                    (ExprKind::Var(v), ExprKind::Lit(lit)) | (ExprKind::Lit(lit), ExprKind::Var(v)) => (v, lit),
                    _ => return vec![],
                };
                let Some(t) = self.lookup(var) else { return vec![] };
                let lt = Ty::Lit(lit.clone());
                let equal = (*op == BinOp::Eq) == positive;
                let narrowed = if equal {
                    self.ctx.narrow(t, &lt)
                } else {
                    let ms = self.union_members(t);
                    self.union(ms.into_iter().filter(|m| self.ctx.shape(m) != lt).collect())
                };
                if narrowed == Ty::Never { vec![] } else { vec![(var.clone(), narrowed)] }
            }
            ExprKind::Binary(BinOp::And, a, b) if positive => {
                let mut v = self.narrowings(a, true);
                v.extend(self.narrowings(b, true));
                v
            }
            ExprKind::Binary(BinOp::Or, a, b) if !positive => {
                let mut v = self.narrowings(a, false);
                v.extend(self.narrowings(b, false));
                v
            }
            ExprKind::Unary(UnOp::Not, x) => self.narrowings(x, !positive),
            _ => vec![],
        }
    }

    // ---------- refinement obligations ----------

    fn push_vc(&mut self, hyps: Vec<Expr>, goal: Expr, span: Span, reason: String) {
        let status = triage(&hyps, &goal);
        if status == VcStatus::Refuted {
            self.error(span, format!("refinement violated: `{goal}` is false (required by {reason})"));
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
        match self.ctx.unfold(&expected) {
            Ty::Refine(base, pred) => {
                let goal = pred.subst("self", subject);
                let mut hyps = self.hyps.clone();
                hyps.extend(self.facts(&actual, subject, 0));
                self.push_vc(hyps, goal, subject.span, format!("`{expected}`"));
                self.gen_vcs(&actual, &base, subject);
            }
            Ty::Record(r) => {
                if let Ty::Record(ar) = self.ctx.shape(&actual) {
                    for f in &r.fields {
                        if let Some(af) = ar.get(&f.name) {
                            let s = project(subject, &f.name);
                            self.gen_vcs(&af.ty, &f.ty, &s);
                        }
                    }
                }
            }
            Ty::List(et) => {
                if let (Ty::List(at), ExprKind::List(items)) = (self.ctx.shape(&actual), &subject.kind) {
                    for item in items {
                        if let ListItem::Elem(e) = item {
                            self.gen_vcs(&at, &et, e);
                        }
                    }
                }
            }
            Ty::Tuple(ts) => {
                if let (Ty::Tuple(ats), ExprKind::Tuple(es)) = (self.ctx.shape(&actual), &subject.kind) {
                    for ((a, t), e) in ats.iter().zip(&ts).zip(es) {
                        self.gen_vcs(a, t, e);
                    }
                }
            }
            Ty::Union(ms) => {
                for am in self.ctx.members(&actual) {
                    let pick = ms
                        .iter()
                        .find(|m| self.ctx.sub(&am, m, Mode::Strict))
                        .or_else(|| ms.iter().find(|m| self.sub(&am, m)))
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
        if !self.sub(actual, expected) {
            let mut msg = format!("type mismatch: expected `{}`, found `{}`", self.norm(expected), self.norm(actual));
            let nullable = self.union_members(actual).iter().any(|m| self.is_null(m));
            if nullable && !self.union_members(expected).iter().any(|m| self.is_null(m)) {
                msg.push_str(" (the value may be null; use `??` to provide a default)");
            }
            self.error(subject.span, msg);
            return;
        }
        self.gen_vcs(actual, expected, subject);
    }

    // ---------- checking mode ----------

    fn check(&mut self, e: &Expr, expected: &Ty) {
        // A literal object/list is never null: check it against the non-null part of
        // an optional type (`T | null`), so errors stay field by field.
        if matches!(e.kind, ExprKind::Object(_) | ExprKind::List(_) | ExprKind::Tuple(_)) {
            let ms = self.union_members(expected);
            if ms.len() > 1 && ms.iter().any(|m| self.is_null(m)) {
                let non_null = self.union(ms.into_iter().filter(|m| !self.is_null(m)).collect());
                if non_null != Ty::Never {
                    return self.check(e, &non_null);
                }
            }
        }
        // A refinement on a composite literal: check the shape field by field, then the predicate.
        if let Ty::Refine(base, pred) = self.ctx.unfold(&self.norm(expected)) {
            if matches!(e.kind, ExprKind::Object(_) | ExprKind::List(_) | ExprKind::Tuple(_)) {
                self.check(e, &base);
                let goal = pred.subst("self", e);
                let hyps = self.hyps.clone();
                self.push_vc(hyps, goal, e.span, format!("`{}`", self.norm(expected)));
                return;
            }
        }
        match &e.kind {
            ExprKind::If(c, t, f) => {
                self.check(c, &Ty::Bool);
                let m = self.marks();
                self.assume(c, true);
                self.check(t, expected);
                self.reset(m.clone());
                self.assume(c, false);
                self.check(f, expected);
                self.reset(m);
            }
            ExprKind::Let(n, ty, v, body) => {
                let m = self.marks();
                let t = self.let_value(ty.as_deref(), v);
                self.bind_let(n, t, v, e.span);
                self.check(body, expected);
                self.reset(m);
            }
            ExprKind::With(b, patches) => {
                // The result has the expected type: check the base and every patch against it.
                self.check(b, expected);
                for p in patches {
                    self.patch_path = fmt_path(&p.path);
                    self.patch_value(expected, &p.path, &p.op, p.span, true);
                }
            }
            ExprKind::Match(s, arms) => {
                self.do_match(s, arms, Some(expected), e.span);
            }
            ExprKind::Object(ms) => self.check_object(ms, expected, e),
            ExprKind::List(items) => match self.ctx.shape(expected) {
                Ty::List(et) => {
                    let label = self.label_for(e.span);
                    for item in items {
                        match (item, &label) {
                            (ListItem::Elem(x), Some(l)) => self.check_labeled(x, &et, format!("{l}[]")),
                            _ => self.check_item(item, &et),
                        }
                    }
                }
                _ => {
                    let t = self.synth(e);
                    self.subsume(&t, expected, e);
                }
            },
            ExprKind::Tuple(es) => match self.ctx.shape(expected) {
                Ty::Tuple(ts) if ts.len() == es.len() => {
                    for (x, t) in es.iter().zip(&ts) {
                        self.check(x, t);
                    }
                }
                _ => {
                    let t = self.synth(e);
                    self.subsume(&t, expected, e);
                }
            },
            _ => {
                let t = self.synth(e);
                self.subsume(&t, expected, e);
            }
        }
    }

    fn let_value(&mut self, ty: Option<&TypeExpr>, v: &Expr) -> Ty {
        match ty {
            Some(te) => {
                let t = self.resolve_te(te);
                self.check(v, &t);
                t
            }
            None => self.synth(v),
        }
    }

    fn check_item(&mut self, item: &ListItem, et: &Ty) {
        match item {
            ListItem::Elem(e) => self.check(e, et),
            ListItem::Spread(e) => {
                let lt = Ty::Union(vec![Ty::List(Box::new(et.clone())), Ty::null()]);
                self.check(e, &lt);
            }
            ListItem::If(c, inner) => {
                self.check(c, &Ty::Bool);
                let m = self.marks();
                self.assume(c, true);
                self.check_item(inner, et);
                self.reset(m);
            }
            ListItem::For(a, b, src, inner) => {
                let m = self.marks();
                self.bind_loop(a, b.as_deref(), src);
                self.check_item(inner, et);
                self.reset(m);
            }
        }
    }

    fn check_object(&mut self, ms: &[Member], expected: &Ty, e: &Expr) {
        let shown = match self.norm(expected) {
            Ty::Alias(n) => n,
            t => self.label_for(e.span).unwrap_or_else(|| t.to_string()),
        };
        match self.ctx.unfold(&self.norm(expected)) {
            Ty::Record(r) => {
                let (guaranteed, dynamic) = self.check_members(ms, &r, &shown);
                if !dynamic {
                    // A misspelled key was already reported with a "did you mean"; don't
                    // also report the field it was meant to be as missing.
                    let unknown: Vec<&str> = ms
                        .iter()
                        .filter_map(|m| match m {
                            Member::Field(k, _, _) if r.get(k).is_none() => Some(k.as_str()),
                            _ => None,
                        })
                        .collect();
                    let missing: Vec<&Field> = r
                        .fields
                        .iter()
                        .filter(|f| f.presence == Presence::Required && !guaranteed.contains(&f.name))
                        .filter(|f| !unknown.iter().any(|u| suggest(u, std::iter::once(f.name.as_str())).is_some()))
                        .collect();
                    for f in missing {
                        let msg = format!("missing required field `{}` (`{}`) in `{shown}`", f.name, f.ty);
                        self.error(e.span, msg);
                    }
                }
            }
            Ty::Map(vt) => self.check_members_map(ms, &vt),
            Ty::Json | Ty::Error => {
                self.synth(e);
            }
            Ty::Union(us) => {
                // Discriminated unions: pick the record member whose literal fields agree.
                let objects: Vec<Ty> = us
                    .iter()
                    .filter(|u| matches!(self.ctx.shape(u), Ty::Record(_) | Ty::Map(_) | Ty::Json))
                    .cloned()
                    .collect();
                let candidates: Vec<Ty> = if objects.len() == 1 {
                    objects
                } else {
                    objects.into_iter().filter(|u| self.object_may_be(ms, u)).collect()
                };
                if candidates.len() == 1 {
                    self.check_object(ms, &candidates[0], e);
                } else {
                    let t = self.synth(e);
                    self.subsume(&t, expected, e);
                }
            }
            _ => {
                let t = self.synth(e);
                self.subsume(&t, expected, e);
            }
        }
    }

    /// Could an object literal with these members have type `t`? Uses keys and literal-typed fields.
    fn object_may_be(&self, ms: &[Member], t: &Ty) -> bool {
        match self.ctx.shape(t) {
            Ty::Record(r) => ms.iter().all(|m| match m {
                Member::Field(k, v, _) => match r.get(k) {
                    None => r.open,
                    Some(f) => match (&v.kind, self.ctx.shape(&f.ty)) {
                        (ExprKind::Lit(l), Ty::Lit(fl)) => *l == fl,
                        _ => true,
                    },
                },
                _ => true,
            }),
            Ty::Map(_) | Ty::Json => true,
            _ => false,
        }
    }

    /// Check object members against a record type. Returns the fields that are
    /// definitely set, and whether keys were computed dynamically.
    fn check_members(&mut self, ms: &[Member], r: &Rec, shown: &str) -> (HashSet<String>, bool) {
        let mut guaranteed = HashSet::new();
        let mut dynamic = false;
        for m in ms {
            match m {
                Member::Field(k, e, span) => match r.get(k) {
                    Some(f) => {
                        let text = format!("// {shown}\n{}", Self::field_sig(f));
                        let doc = self.ctx.docs.get(&format!("{}.{k}", self.canonical(shown))).cloned();
                        self.hover_doc(*span, k, text, doc);
                        // A literal (other than `null`) can't be absent: check it against the plain field type.
                        let literal = match &e.kind {
                            ExprKind::Lit(l) => *l != Lit::Null,
                            ExprKind::Interp(_) | ExprKind::Object(_) | ExprKind::List(_) | ExprKind::Tuple(_) => true,
                            _ => false,
                        };
                        let ft = if f.presence == Presence::Optional && !literal {
                            Ty::Union(vec![f.ty.clone(), Ty::null()])
                        } else {
                            f.ty.clone()
                        };
                        self.check_labeled(e, &ft, format!("{shown}.{k}"));
                        guaranteed.insert(k.clone());
                    }
                    None if r.open => {
                        self.synth(e);
                    }
                    None => {
                        let hint = did_you_mean(k, r.fields.iter().map(|f| f.name.as_str()));
                        self.error(*span, format!("unknown field `{k}` in `{shown}`{hint}"));
                        self.synth(e);
                    }
                },
                Member::Computed(k, v) => {
                    self.check(k, &Ty::Str);
                    self.synth(v);
                    dynamic = true;
                }
                Member::Spread(e) => {
                    let st = self.synth(e);
                    match self.ctx.shape(&st) {
                        Ty::Record(sr) => {
                            for sf in &sr.fields {
                                match r.get(&sf.name) {
                                    Some(f) => {
                                        if !self.sub(&sf.ty, &f.ty) {
                                            let msg = format!(
                                                "spread field `{}`: expected `{}`, found `{}`",
                                                sf.name, f.ty, sf.ty
                                            );
                                            self.error(e.span, msg);
                                        }
                                        if sf.present() {
                                            guaranteed.insert(sf.name.clone());
                                        }
                                    }
                                    None if !r.open => {
                                        self.error(e.span, format!("spread adds unknown field `{}` to `{shown}`", sf.name));
                                    }
                                    None => {}
                                }
                            }
                        }
                        Ty::Map(_) | Ty::Json => dynamic = true,
                        Ty::Error => {}
                        t if t == Ty::null() => {}
                        _ => self.error(e.span, format!("cannot spread `{st}` into an object")),
                    }
                }
                Member::If(c, t, els) => {
                    self.check(c, &Ty::Bool);
                    let mk = self.marks();
                    self.assume(c, true);
                    let (g1, d1) = self.check_members(t, r, shown);
                    self.reset(mk.clone());
                    dynamic |= d1;
                    if let Some(els) = els {
                        self.assume(c, false);
                        let (g2, d2) = self.check_members(els, r, shown);
                        self.reset(mk);
                        dynamic |= d2;
                        guaranteed.extend(g1.intersection(&g2).cloned());
                    }
                }
                Member::For(a, b, src, body) => {
                    let mk = self.marks();
                    self.bind_loop(a, b.as_deref(), src);
                    self.check_members(body, r, shown);
                    self.reset(mk);
                    dynamic = true;
                }
            }
        }
        (guaranteed, dynamic)
    }

    fn check_members_map(&mut self, ms: &[Member], vt: &Ty) {
        for m in ms {
            match m {
                Member::Field(_, e, _) => self.check(e, vt),
                Member::Computed(k, v) => {
                    self.check(k, &Ty::Str);
                    self.check(v, vt);
                }
                Member::Spread(e) => {
                    let t = Ty::Union(vec![Ty::Map(Box::new(vt.clone())), Ty::null()]);
                    self.check(e, &t);
                }
                Member::If(c, t, els) => {
                    self.check(c, &Ty::Bool);
                    let mk = self.marks();
                    self.assume(c, true);
                    self.check_members_map(t, vt);
                    self.reset(mk.clone());
                    if let Some(els) = els {
                        self.assume(c, false);
                        self.check_members_map(els, vt);
                        self.reset(mk);
                    }
                }
                Member::For(a, b, src, body) => {
                    let mk = self.marks();
                    self.bind_loop(a, b.as_deref(), src);
                    self.check_members_map(body, vt);
                    self.reset(mk);
                }
            }
        }
    }

    /// Bind the variables of `for a(, b) in src`.
    fn bind_loop(&mut self, a: &str, b: Option<&str>, src: &Expr) {
        let st = self.synth(src);
        let ms: Vec<Ty> = self.union_members(&st).into_iter().filter(|m| !self.is_null(m)).collect();
        let st = self.union(ms);
        let (ka, va) = match self.ctx.shape(&st) {
            Ty::List(t) => (Ty::Int, *t),
            Ty::Tuple(ts) => (Ty::Int, self.union(ts)),
            Ty::Map(t) => (Ty::Str, *t),
            Ty::Record(r) => (Ty::Str, self.union(r.fields.iter().map(Field::read_ty).collect())),
            Ty::Json => (Ty::Union(vec![Ty::Int, Ty::Str]), Ty::Json),
            Ty::Error | Ty::Never => (Ty::Error, Ty::Error),
            _ => {
                self.error(src.span, format!("cannot iterate over `{st}`"));
                (Ty::Error, Ty::Error)
            }
        };
        let is_list = matches!(self.ctx.shape(&st), Ty::List(_) | Ty::Tuple(_));
        match b {
            None => {
                // One variable: list elements, or object keys.
                let t = if is_list { va } else { ka };
                self.bind_var(a, t, src.span);
            }
            Some(b) => {
                self.bind_var(a, ka, src.span);
                self.bind_var(b, va, src.span);
            }
        }
    }

    // ---------- synthesis mode ----------

    fn synth(&mut self, e: &Expr) -> Ty {
        match &e.kind {
            ExprKind::Lit(l) => Ty::Lit(l.clone()),
            ExprKind::Interp(parts) => {
                let scalar = Ty::Union(vec![Ty::Str, Ty::Float, Ty::Bool]);
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        let t = self.synth(x);
                        if !self.sub(&t, &scalar) {
                            self.error(x.span, format!("cannot interpolate `{t}`; only strings, numbers and booleans"));
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Var(n) => self.var_ty(n, e.span),
            ExprKind::List(items) => {
                let mut tys = Vec::new();
                for item in items {
                    self.synth_item(item, &mut tys);
                }
                Ty::List(Box::new(self.union(tys)))
            }
            ExprKind::Tuple(es) => Ty::Tuple(es.iter().map(|x| self.synth(x)).collect()),
            ExprKind::Object(ms) => {
                let mut fields = Vec::new();
                let mut dynamic = Vec::new();
                let mut overridable = HashSet::new();
                self.synth_members(ms, &mut fields, &mut dynamic, &mut overridable, false);
                if dynamic.is_empty() {
                    Ty::Record(Rec { fields, open: false })
                } else {
                    let mut all: Vec<Ty> = fields.into_iter().map(|f| f.ty).collect();
                    all.extend(dynamic);
                    Ty::Map(Box::new(self.union(all)))
                }
            }
            ExprKind::Field(b, label, optional) => {
                // `ns.x`: a constant from an imported module.
                if let Some(q) = qualified_name(e) {
                    let root = q.split('.').next().unwrap_or("");
                    if self.lookup(root).is_none() && self.namespaces.contains(root) {
                        if let Some(t) = self.lookup(&q).cloned() {
                            let label_s = label.to_string();
                            self.hover(e.span, &label_s, format!("{q}: {t}"));
                            return t;
                        }
                        let candidates: Vec<String> =
                            self.scope.iter().map(|(n, _)| n.clone()).filter(|n| n.starts_with(&format!("{root}."))).collect();
                        let hint = did_you_mean(&q, candidates.iter().map(String::as_str));
                        self.error(e.span, format!("module `{root}` has no value `{}`{hint}", &q[root.len() + 1..]));
                        return Ty::Error;
                    }
                }
                let bt = self.synth(b);
                let t = self.field_ty(&bt, b, label, *optional, e.span);
                let field_text = match (self.ctx.shape(&bt), label) {
                    (Ty::Record(r), Label::Name(k)) => r.get(k).map(|f| format!("// {bt}\n{}", Self::field_sig(f))),
                    _ => None,
                };
                let doc = match label {
                    Label::Name(k) => self.field_doc(&bt, k),
                    Label::Pos(_) => None,
                };
                let label_s = label.to_string();
                self.hover_doc(e.span, &label_s, field_text.unwrap_or_else(|| format!("{label}: {t}")), doc);
                t
            }
            ExprKind::Index(b, i) => self.synth_index(b, i, e.span),
            ExprKind::Call(name, args) => self.synth_call(name, args, e.span),
            ExprKind::Typed(tname, obj) => {
                if !self.ctx.alias_src.contains_key(tname) {
                    let msg = self.ctx.unknown_type_message(tname);
                    self.error(e.span, msg);
                    self.synth(obj);
                    return Ty::Error;
                }
                // Resolve it (and everything it refers to) before checking against it.
                self.ctx.resolve(&TypeExpr { kind: TypeKind::Named(tname.clone()), span: e.span });
                self.drain_type_diags();
                let text = self.alias_text(tname);
                let doc = self.type_doc(tname);
                self.hover_doc(e.span, tname, text, doc);
                let t = Ty::Alias(tname.clone());
                self.check(obj, &t);
                t
            }
            ExprKind::Unary(op, x) => {
                let t = self.synth(x);
                match op {
                    UnOp::Neg if self.sub(&t, &Ty::Int) => Ty::Int,
                    UnOp::Neg if self.sub(&t, &Ty::Float) => Ty::Float,
                    UnOp::Not if self.sub(&t, &Ty::Bool) => Ty::Bool,
                    _ => {
                        let sym = if *op == UnOp::Neg { "-" } else { "!" };
                        self.error(e.span, format!("operator `{sym}` cannot be applied to `{t}`"));
                        Ty::Error
                    }
                }
            }
            ExprKind::Binary(op, l, r) => self.synth_binary(*op, l, r, e.span),
            ExprKind::If(c, t, f) => {
                self.check(c, &Ty::Bool);
                let m = self.marks();
                self.assume(c, true);
                let tt = self.synth(t);
                self.reset(m.clone());
                self.assume(c, false);
                let ft = self.synth(f);
                self.reset(m);
                self.union(vec![tt, ft])
            }
            ExprKind::Let(n, ty, v, body) => {
                let m = self.marks();
                let t = self.let_value(ty.as_deref(), v);
                self.bind_let(n, t, v, e.span);
                let bt = self.synth(body);
                self.reset(m);
                bt
            }
            ExprKind::Match(s, arms) => self.do_match(s, arms, None, e.span),
            ExprKind::With(b, patches) => {
                let mut t = self.synth(b);
                for p in patches {
                    self.patch_path = fmt_path(&p.path);
                    t = self.patch_value(&t, &p.path, &p.op, p.span, false);
                }
                t
            }
        }
    }

    fn var_ty(&mut self, n: &str, span: Span) -> Ty {
        if let Some(t) = self.lookup(n).cloned() {
            if n != "self" {
                self.hover(span, n, format!("{n}: {t}"));
            }
            return t;
        }
        if self.fns.contains_key(n) || BUILTINS.contains(&n) {
            self.error(span, format!("`{n}` is a function; call it with `{n}(...)`"));
        } else {
            let hint = did_you_mean(n, self.scope.iter().map(|(s, _)| s.as_str()).filter(|s| *s != "self"));
            self.error(span, format!("unknown variable `{n}`{hint}"));
        }
        Ty::Error
    }

    fn synth_item(&mut self, item: &ListItem, tys: &mut Vec<Ty>) {
        match item {
            ListItem::Elem(e) => {
                let t = self.synth(e);
                tys.push(t);
            }
            ListItem::Spread(e) => {
                let t = self.synth(e);
                for m in self.union_members(&t) {
                    match self.ctx.shape(&m) {
                        Ty::List(et) => tys.push(*et),
                        Ty::Tuple(ts) => tys.extend(ts),
                        Ty::Json => tys.push(Ty::Json),
                        Ty::Error => tys.push(Ty::Error),
                        s if s == Ty::null() => {}
                        _ => self.error(e.span, format!("cannot spread `{t}` into a list")),
                    }
                }
            }
            ListItem::If(c, inner) => {
                self.check(c, &Ty::Bool);
                let m = self.marks();
                self.assume(c, true);
                self.synth_item(inner, tys);
                self.reset(m);
            }
            ListItem::For(a, b, src, inner) => {
                let m = self.marks();
                self.bind_loop(a, b.as_deref(), src);
                self.synth_item(inner, tys);
                self.reset(m);
            }
        }
    }

    fn synth_members(
        &mut self,
        ms: &[Member],
        fields: &mut Vec<Field>,
        dynamic: &mut Vec<Ty>,
        overridable: &mut HashSet<String>,
        conditional: bool,
    ) {
        let presence = if conditional { Presence::Optional } else { Presence::Required };
        for m in ms {
            match m {
                Member::Field(k, e, span) => {
                    let t = self.synth(e);
                    self.add_field(fields, overridable, Field { name: k.clone(), ty: t, presence: presence.clone() }, false, *span);
                }
                Member::Computed(k, v) => {
                    self.check(k, &Ty::Str);
                    let t = self.synth(v);
                    dynamic.push(t);
                }
                Member::Spread(e) => {
                    let st = self.synth(e);
                    match self.ctx.shape(&st) {
                        Ty::Record(r) => {
                            for f in r.fields {
                                let presence = if conditional || !f.present() { Presence::Optional } else { Presence::Required };
                                self.add_field(fields, overridable, Field { presence, ..f }, true, e.span);
                            }
                        }
                        Ty::Map(t) => dynamic.push(*t),
                        Ty::Json => dynamic.push(Ty::Json),
                        Ty::Error => {}
                        t if t == Ty::null() => {}
                        _ => self.error(e.span, format!("cannot spread `{st}` into an object")),
                    }
                }
                Member::If(c, t, els) => {
                    self.check(c, &Ty::Bool);
                    let mk = self.marks();
                    let (mut ft, mut fe) = (Vec::new(), Vec::new());
                    let mut ov = HashSet::new();
                    self.assume(c, true);
                    self.synth_members(t, &mut ft, dynamic, &mut ov, conditional);
                    self.reset(mk.clone());
                    if let Some(els) = els {
                        self.assume(c, false);
                        self.synth_members(els, &mut fe, dynamic, &mut ov, conditional);
                        self.reset(mk);
                    }
                    // Present in both branches => present; otherwise optional.
                    let mut names: Vec<String> = ft.iter().map(|f| f.name.clone()).collect();
                    names.extend(fe.iter().map(|f| f.name.clone()).filter(|n| !ft.iter().any(|f| f.name == *n)));
                    for n in names {
                        let a = ft.iter().find(|f| f.name == n);
                        let b = fe.iter().find(|f| f.name == n);
                        let both = a.is_some_and(Field::present) && b.is_some_and(Field::present);
                        let ty = self.union(a.into_iter().chain(b).map(|f| f.ty.clone()).collect());
                        let presence = if both { Presence::Required } else { Presence::Optional };
                        self.add_field(fields, overridable, Field { name: n, ty, presence }, false, c.span);
                    }
                }
                Member::For(a, b, src, body) => {
                    let mk = self.marks();
                    self.bind_loop(a, b.as_deref(), src);
                    let mut fs = Vec::new();
                    self.synth_members(body, &mut fs, dynamic, &mut HashSet::new(), true);
                    self.reset(mk);
                    dynamic.extend(fs.into_iter().map(|f| f.ty));
                }
            }
        }
    }

    fn add_field(&mut self, fields: &mut Vec<Field>, overridable: &mut HashSet<String>, f: Field, from_spread: bool, span: Span) {
        let Some(existing) = fields.iter_mut().find(|x| x.name == f.name) else {
            if from_spread {
                overridable.insert(f.name.clone());
            }
            fields.push(f);
            return;
        };
        if overridable.contains(&f.name) || from_spread {
            // Spreads act as defaults: whichever comes later wins (as at runtime).
            if from_spread {
                overridable.insert(f.name.clone());
            } else {
                overridable.remove(&f.name);
            }
            *existing = f;
            return;
        }
        match (self.ctx.shape(&existing.ty), self.ctx.shape(&f.ty)) {
            (Ty::Record(a), Ty::Record(b)) => existing.ty = Ty::Record(merge_records(a, b)),
            _ => self.error(span, format!("duplicate key `{}`", f.name)),
        }
    }

    fn field_ty(&mut self, bt: &Ty, base: &Expr, label: &Label, optional: bool, span: Span) -> Ty {
        let ms = self.union_members(bt);
        let has_null = ms.iter().any(|m| self.is_null(m));
        if has_null && !optional {
            self.error(span, format!("`{base}` may be null; use `?.{label}` or `??`"));
            return Ty::Error;
        }
        let mut out = Vec::new();
        for m in ms.iter().filter(|m| !self.is_null(m)) {
            match self.field_of(m, label) {
                Ok(t) => out.push(t),
                Err(msg) => {
                    self.error(span, msg);
                    return Ty::Error;
                }
            }
        }
        if out.is_empty() && !matches!(self.norm(bt), Ty::Never) {
            if !optional {
                self.error(span, format!("cannot read `.{label}` of null"));
                return Ty::Error;
            }
        }
        if has_null {
            out.push(Ty::null());
        }
        self.union(out)
    }

    fn field_of(&self, m: &Ty, label: &Label) -> Result<Ty, String> {
        match (self.ctx.shape(m), label) {
            (Ty::Error, _) => Ok(Ty::Error),
            (Ty::Json, _) => Ok(Ty::Json),
            (Ty::Never, _) => Ok(Ty::Never),
            (Ty::Record(r), Label::Name(k)) => match r.get(k) {
                Some(f) => Ok(f.read_ty()),
                None if r.open => Ok(Ty::Json),
                None => {
                    let hint = did_you_mean(k, r.fields.iter().map(|f| f.name.as_str()));
                    Err(format!("type `{m}` has no field `{k}`{hint}"))
                }
            },
            (Ty::Map(t), Label::Name(_)) => Ok(Ty::Union(vec![*t, Ty::null()])),
            (Ty::Tuple(ts), Label::Pos(i)) => {
                ts.get(*i).cloned().ok_or_else(|| format!("tuple `{m}` has no element {i}"))
            }
            (Ty::List(_), Label::Pos(_)) => Err("use `xs[i]` to index a list".into()),
            _ => Err(format!("type `{m}` has no field `{label}`")),
        }
    }

    fn synth_index(&mut self, b: &Expr, i: &Expr, span: Span) -> Ty {
        let bt = self.synth(b);
        let lit_int = |e: &Expr| if let ExprKind::Lit(Lit::Int(n)) = e.kind { usize::try_from(n).ok() } else { None };
        match self.ctx.shape(&bt) {
            Ty::List(t) => {
                self.check(i, &Ty::Int);
                *t
            }
            Ty::Tuple(ts) => match lit_int(i) {
                Some(n) if n < ts.len() => ts[n].clone(),
                Some(n) => {
                    self.error(span, format!("index {n} is out of bounds for `{bt}`"));
                    Ty::Error
                }
                None => {
                    self.check(i, &Ty::Int);
                    self.union(ts)
                }
            },
            Ty::Map(t) => {
                self.check(i, &Ty::Str);
                Ty::Union(vec![*t, Ty::null()])
            }
            Ty::Record(r) => {
                if let ExprKind::Lit(Lit::Str(k)) = &i.kind {
                    return match self.field_of(&bt, &Label::Name(k.clone())) {
                        Ok(t) => t,
                        Err(m) => {
                            self.error(span, m);
                            Ty::Error
                        }
                    };
                }
                self.check(i, &Ty::Str);
                let mut ts: Vec<Ty> = r.fields.iter().map(Field::read_ty).collect();
                ts.push(Ty::null());
                if r.open {
                    ts.push(Ty::Json);
                }
                self.union(ts)
            }
            Ty::Json => {
                self.synth(i);
                Ty::Json
            }
            Ty::Error => Ty::Error,
            _ => {
                self.error(span, format!("cannot index a value of type `{bt}`"));
                Ty::Error
            }
        }
    }

    fn synth_binary(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span) -> Ty {
        use BinOp::*;
        match op {
            And | Or => {
                self.check(l, &Ty::Bool);
                // Short-circuit: the right side runs only when the left decided nothing.
                let m = self.marks();
                self.assume(l, op == And);
                self.check(r, &Ty::Bool);
                self.reset(m);
                return Ty::Bool;
            }
            Coalesce => {
                let lt = self.synth(l);
                let ms = self.union_members(&lt);
                if lt != Ty::Error && !ms.iter().any(|m| self.is_null(m)) {
                    self.warn(l.span, format!("left side of `??` has type `{lt}`, which is never null"));
                }
                let non_null: Vec<Ty> = ms.into_iter().filter(|m| !self.is_null(m)).collect();
                let rt = self.synth(r);
                let mut all = non_null;
                all.push(rt);
                return self.union(all);
            }
            _ => {}
        }
        let lt = self.synth(l);
        let rt = self.synth(r);
        let both = |c: &Self, t: &Ty| c.sub(&lt, t) && c.sub(&rt, t);
        let fail = |c: &mut Self| {
            let mut msg = format!("operator `{}` cannot be applied to `{lt}` and `{rt}`", op.symbol());
            if [&lt, &rt].iter().any(|t| c.union_members(t).iter().any(|m| c.is_null(m))) {
                msg.push_str(" (a value may be null; use `??` to provide a default)");
            }
            c.error(span, msg);
        };
        match op {
            Add | Sub | Mul | Div | Rem => {
                if both(self, &Ty::Int) {
                    Ty::Int
                } else if both(self, &Ty::Float) {
                    Ty::Float
                } else if op == Add && both(self, &Ty::Str) {
                    Ty::Str
                } else {
                    fail(self);
                    Ty::Error
                }
            }
            Lt | Le | Gt | Ge => {
                if !(both(self, &Ty::Float) || both(self, &Ty::Str)) {
                    fail(self);
                }
                Ty::Bool
            }
            Eq | Ne => {
                let related = self.sub(&lt, &rt) || self.sub(&rt, &lt) || self.ctx.narrow(&lt, &rt) != Ty::Never;
                if !related {
                    self.error(span, format!("comparing `{lt}` with `{rt}` is always {}", op != Eq));
                }
                Ty::Bool
            }
            And | Or | Coalesce => unreachable!(),
        }
    }

    // ---------- value patches (`e with { ... }`) ----------

    /// Type the patch `path op` applied to a value of type `t`, returning the result type.
    /// Inside a declared (named) type, patches are checked against it and the type is kept;
    /// in anonymous object types, the type is updated structurally.
    fn patch_value(&mut self, t: &Ty, path: &[PathSeg], op: &ValueOp, span: Span, declared: bool) -> Ty {
        self.patch_value_in(t, path, op, span, declared, None)
    }

    /// `name` is the nearest type name (or `Type.field` path) for messages.
    fn patch_value_in(
        &mut self,
        t: &Ty,
        path: &[PathSeg],
        op: &ValueOp,
        span: Span,
        declared: bool,
        name: Option<&str>,
    ) -> Ty {
        let Some((seg, rest)) = path.split_first() else { return t.clone() };
        let tn = self.norm(t);
        let shown = match name {
            Some(n) => n.to_string(),
            None if tn.to_string().len() <= 48 => tn.to_string(),
            None => "the value".to_string(),
        };
        let fail = |c: &mut Self, msg: String| {
            let full = c.patch_path.clone();
            c.error(span, format!("`{full}`: {msg}"));
            if let ValueOp::Set(v) | ValueOp::Merge(v) = op {
                c.synth(v);
            }
            Ty::Error
        };
        if let PathSeg::Select(keys) = seg {
            // Selector values are ordinary expressions.
            for (_, v) in keys {
                self.synth(v);
            }
        }
        match self.ctx.unfold(&tn) {
            Ty::Error | Ty::Json => {
                if let ValueOp::Set(v) | ValueOp::Merge(v) = op {
                    self.synth(v);
                }
                tn
            }
            _ if matches!(tn, Ty::Alias(_)) => {
                // Entering a named type: check against it, keep the name.
                let inner = self.ctx.unfold(&tn);
                let alias = match &tn {
                    Ty::Alias(n) => n.clone(),
                    _ => unreachable!(),
                };
                let r = self.patch_value_in(&inner, path, op, span, true, Some(&alias));
                if r == Ty::Error { Ty::Error } else { tn }
            }
            // Patching may break a refinement of the whole value, so the result drops it.
            Ty::Refine(b, _) => self.patch_value_in(&b, path, op, span, declared, name),
            Ty::Union(ms) if ms.contains(&Ty::null()) => {
                let non_null: Vec<Ty> = ms.iter().filter(|m| **m != Ty::null()).cloned().collect();
                let patched: Vec<Ty> =
                    non_null.iter().map(|m| self.patch_value_in(m, path, op, span, declared, name)).collect();
                let mut out = patched;
                if matches!(op, ValueOp::Remove) {
                    out.push(Ty::null());
                }
                self.union(out)
            }
            Ty::Union(ms) => {
                // A union of objects (e.g. different resource kinds): patch the members that have the path.
                let (cands, others): (Vec<Ty>, Vec<Ty>) = ms.into_iter().partition(|m| self.has_seg(m, seg));
                if cands.is_empty() {
                    return fail(self, format!("no member of `{shown}` has `{seg}`"));
                }
                let mut out: Vec<Ty> = cands.iter().map(|m| self.patch_value_in(m, path, op, span, declared, None)).collect();
                if declared {
                    return tn;
                }
                out.extend(others);
                self.union(out)
            }
            Ty::Record(mut r) => {
                let PathSeg::Field(k) = seg else {
                    return fail(self, format!("`{seg}` applies to lists, but `{shown}` is an object"));
                };
                let Some(i) = r.fields.iter().position(|f| f.name == *k) else {
                    if declared && !r.open {
                        let hint = did_you_mean(k, r.fields.iter().map(|f| f.name.as_str()));
                        return fail(self, format!("`{shown}` has no field `{k}`{hint}"));
                    }
                    if matches!(op, ValueOp::Remove) {
                        if !r.open {
                            return fail(self, format!("`{shown}` has no field `{k}`"));
                        }
                        return Ty::Record(r);
                    }
                    // Add the field (creating nested objects for the rest of the path).
                    let vt = self.new_path_ty(rest, op);
                    if !declared {
                        r.fields.push(Field { name: k.clone(), ty: vt, presence: Presence::Required });
                    }
                    return Ty::Record(r);
                };
                if rest.is_empty() {
                    let f = r.fields[i].clone();
                    match op {
                        ValueOp::Remove if declared && f.presence == Presence::Required => {
                            return fail(self, format!("`{k}` is required in `{shown}` and cannot be removed"));
                        }
                        ValueOp::Remove => {
                            if !declared {
                                r.fields.remove(i);
                            }
                        }
                        ValueOp::Set(v) => {
                            if declared {
                                let label = format!("{shown}.{k}");
                                self.check_labeled(v, &f.ty, label);
                            } else {
                                r.fields[i].ty = self.synth(v);
                                r.fields[i].presence = Presence::Required;
                            }
                        }
                        ValueOp::Merge(v) => {
                            let nt = self.merge_ty(&f.ty, v, declared, span);
                            if !declared {
                                r.fields[i].ty = nt;
                                r.fields[i].presence = Presence::Required;
                            }
                        }
                    }
                } else {
                    let child = format!("{shown}.{k}");
                    let nt = self.patch_value_in(&r.fields[i].read_ty(), rest, op, span, declared, Some(&child));
                    if !declared {
                        r.fields[i].ty = nt;
                        r.fields[i].presence = Presence::Required;
                    }
                }
                Ty::Record(r)
            }
            Ty::Map(vt) => match seg {
                PathSeg::Field(_) | PathSeg::Each => {
                    let nt = if rest.is_empty() {
                        self.patch_leaf(&vt, op, declared, span, path)
                    } else {
                        self.patch_value_in(&vt, rest, op, span, declared, None)
                    };
                    if declared { tn } else { Ty::Map(Box::new(self.union(vec![*vt, nt]))) }
                }
                _ => fail(self, format!("`{seg}` applies to lists, but `{shown}` is an object")),
            },
            Ty::List(et) => {
                let all_elems = (*et).clone();
                let mut et = et;
                if let PathSeg::Select(keys) = seg {
                    // Selector keys must be fields of the elements.
                    if let Ty::Record(er) = self.ctx.shape(&et) {
                        for (k, _) in keys {
                            if er.get(k).is_none() && !er.open {
                                let hint = did_you_mean(k, er.fields.iter().map(|f| f.name.as_str()));
                                return fail(self, format!("elements of `{shown}` have no field `{k}`{hint}"));
                            }
                        }
                    }
                    // `[kind: "Deployment"]` on a list of mixed kinds: only those elements.
                    let selected = self.select_members(&et, keys);
                    if selected == Ty::Never {
                        return fail(self, format!("no element of `{shown}` can match `{seg}`"));
                    }
                    et = Box::new(selected);
                }
                match seg {
                    PathSeg::Field(_) => {
                        fail(self, format!("`{shown}` is a list; select elements with `[]`, `[i]` or `[key: value]`"))
                    }
                    PathSeg::Each if rest.is_empty() && matches!(op, ValueOp::Remove) => {
                        fail(self, "`[]` cannot be removed; set the list to `[]` instead".into())
                    }
                    _ if rest.is_empty() && matches!(op, ValueOp::Remove) => tn,
                    _ => {
                        let nt = if rest.is_empty() {
                            self.patch_leaf(&et, op, declared, span, path)
                        } else {
                            let child = format!("{shown}[]");
                            self.patch_value_in(&et, rest, op, span, declared, Some(&child))
                        };
                        // Unselected elements keep their type.
                        if declared { tn } else { Ty::List(Box::new(self.union(vec![all_elems, nt]))) }
                    }
                }
            }
            Ty::Tuple(ts) => match seg {
                PathSeg::Index(n) if *n < ts.len() => {
                    let nt = if rest.is_empty() {
                        self.patch_leaf(&ts[*n], op, declared, span, path)
                    } else {
                        self.patch_value_in(&ts[*n], rest, op, span, declared, None)
                    };
                    let mut ts = ts;
                    if !declared {
                        ts[*n] = nt;
                    }
                    Ty::Tuple(ts)
                }
                _ => fail(self, format!("`{seg}` does not select an element of `{shown}`")),
            },
            _ => fail(self, format!("cannot patch inside `{shown}`")),
        }
    }

    /// Whether a patch path can continue through `m` with `seg`.
    fn has_seg(&self, m: &Ty, seg: &PathSeg) -> bool {
        match (self.ctx.shape(m), seg) {
            (Ty::Record(r), PathSeg::Field(k)) => r.open || r.get(k).is_some(),
            (Ty::Map(_) | Ty::Json | Ty::Error, _) => true,
            (Ty::List(_) | Ty::Tuple(_), PathSeg::Each | PathSeg::Index(_) | PathSeg::Select(_)) => true,
            _ => false,
        }
    }

    /// Element types that a selector with literal values can match.
    fn select_members(&self, et: &Ty, keys: &[(String, Expr)]) -> Ty {
        let ms = self.union_members(et);
        let keep: Vec<Ty> = ms
            .into_iter()
            .filter(|m| match self.ctx.shape(m) {
                Ty::Record(r) => keys.iter().all(|(k, v)| match (r.get(k), &v.kind) {
                    (None, _) => r.open,
                    (Some(f), ExprKind::Lit(l)) => self.ctx.narrow(&f.ty, &Ty::Lit(l.clone())) != Ty::Never,
                    (Some(_), _) => true,
                }),
                _ => true,
            })
            .collect();
        self.union(keep)
    }

    /// The type a list element / map value / tuple element gets from `op`.
    fn patch_leaf(&mut self, old: &Ty, op: &ValueOp, declared: bool, span: Span, path: &[PathSeg]) -> Ty {
        match op {
            ValueOp::Set(v) if declared => {
                self.check(v, old);
                old.clone()
            }
            ValueOp::Set(v) => self.synth(v),
            ValueOp::Merge(v) => self.merge_ty(old, v, declared, span),
            ValueOp::Remove => {
                self.error(span, format!("`{}`: only fields and selected list elements can be removed", fmt_path(path)));
                Ty::Error
            }
        }
    }

    /// Type of a value created where nothing was before (`a.b.c: v` with `a` missing).
    fn new_path_ty(&mut self, rest: &[PathSeg], op: &ValueOp) -> Ty {
        let leaf = match op {
            ValueOp::Set(v) | ValueOp::Merge(v) => self.synth(v),
            ValueOp::Remove => Ty::null(),
        };
        rest.iter().rev().fold(leaf, |t, seg| match seg {
            PathSeg::Field(k) => Ty::Record(Rec { fields: vec![Field { name: k.clone(), ty: t, presence: Presence::Required }], open: false }),
            _ => Ty::Json,
        })
    }

    /// `+:`: append to a list, or JSON-merge-patch an object.
    fn merge_ty(&mut self, target: &Ty, v: &Expr, declared: bool, span: Span) -> Ty {
        let ms: Vec<Ty> = self.union_members(target).into_iter().filter(|m| !self.is_null(m)).collect();
        let non_null = self.union(ms);
        match self.ctx.shape(&non_null) {
            Ty::List(et) => {
                if declared {
                    self.check(v, &Ty::List(et));
                    target.clone()
                } else {
                    let vt = self.synth(v);
                    let new_elems = match self.ctx.shape(&vt) {
                        Ty::List(e) => *e,
                        Ty::Tuple(ts) => self.union(ts),
                        _ => {
                            self.error(span, format!("`+:` on a list appends a list, got `{vt}`"));
                            Ty::Error
                        }
                    };
                    Ty::List(Box::new(self.union(vec![*et, new_elems])))
                }
            }
            Ty::Record(r) => {
                if declared {
                    // A merge patch: every given field must fit, but none is required.
                    match &v.kind {
                        ExprKind::Object(members) => {
                            let shown = match self.norm(&non_null) {
                                Ty::Alias(n) => n,
                                t => t.to_string(),
                            };
                            let mut optional = r.clone();
                            for f in &mut optional.fields {
                                f.presence = Presence::Optional;
                            }
                            self.check_members(members, &optional, &shown);
                        }
                        _ => {
                            let mut partial = r.clone();
                            for f in &mut partial.fields {
                                f.presence = Presence::Optional;
                            }
                            self.check(v, &Ty::Record(partial));
                        }
                    }
                    target.clone()
                } else {
                    let vt = self.synth(v);
                    match self.ctx.shape(&vt) {
                        Ty::Record(vr) => Ty::Record(merge_records(r, vr)),
                        _ => {
                            self.error(span, format!("`+:` on an object merges an object, got `{vt}`"));
                            Ty::Error
                        }
                    }
                }
            }
            Ty::Map(vt) if declared => {
                // A merge patch into a map: values must fit; `null` removes a key.
                let patch = Ty::Map(Box::new(Ty::Union(vec![*vt, Ty::null()])));
                self.check(v, &patch);
                target.clone()
            }
            Ty::Map(_) | Ty::Json | Ty::Error => {
                let vt = self.synth(v);
                if declared {
                    target.clone()
                } else {
                    self.union(vec![non_null, vt])
                }
            }
            _ => {
                self.error(span, format!("`+:` merges lists and objects, but this is `{target}`"));
                self.synth(v);
                Ty::Error
            }
        }
    }

    // ---------- calls ----------

    fn synth_call(&mut self, name: &str, args: &[Arg], span: Span) -> Ty {
        if let Some(sig) = self.fns.get(name).cloned() {
            if self.cur_index == Some(sig.index) {
                self.error(span, format!("`{name}` calls itself; recursion is not allowed, so evaluation always terminates"));
                return Ty::Error;
            }
            self.hover(span, name, Self::fn_sig_text(name, &sig));
            return self.call_user(name, &sig, args, span);
        }
        if BUILTINS.contains(&name) {
            self.hover(span, name, format!("// built-in\n{}", crate::eval::builtin_doc(name)));
            return self.call_builtin(name, args, span);
        }
        if self.lookup(name).is_some() {
            self.error(span, format!("`{name}` is a variable, not a function"));
        } else {
            let hint = did_you_mean(name, self.fns.keys().map(String::as_str).chain(BUILTINS));
            self.error(span, format!("unknown function `{name}`{hint} (functions must be declared before use)"));
        }
        for a in args {
            self.synth(&a.value);
        }
        Ty::Error
    }

    fn call_user(&mut self, name: &str, sig: &FnSig, args: &[Arg], span: Span) -> Ty {
        let n = sig.params.len();
        let mut slots: Vec<Option<&Expr>> = vec![None; n];
        let mut pos = 0;
        for a in args {
            match &a.name {
                None => {
                    while pos < n && slots[pos].is_some() {
                        pos += 1;
                    }
                    if pos >= n {
                        self.error(a.value.span, format!("too many arguments to `{name}` (it takes {n})"));
                        self.synth(&a.value);
                        continue;
                    }
                    slots[pos] = Some(&a.value);
                }
                Some(an) => match sig.params.iter().position(|(p, _, _)| p == an) {
                    Some(i) if slots[i].is_none() => slots[i] = Some(&a.value),
                    Some(_) => {
                        self.error(a.value.span, format!("argument `{an}` is given more than once"));
                        self.synth(&a.value);
                    }
                    None => {
                        let hint = did_you_mean(an, sig.params.iter().map(|(p, _, _)| p.as_str()));
                        self.error(a.value.span, format!("`{name}` has no parameter `{an}`{hint}"));
                        self.synth(&a.value);
                    }
                },
            }
        }
        // Instantiate dependent refinements with the actual arguments.
        let mut map: Vec<(String, Expr)> = Vec::new();
        for (i, (p, pt, default)) in sig.params.iter().enumerate() {
            match (slots[i], default) {
                (Some(arg), _) => {
                    self.check(arg, &pt.subst_many(&map));
                    map.push((p.clone(), arg.clone()));
                }
                (None, Some(d)) => map.push((p.clone(), d.subst_many(&map))),
                (None, None) => {
                    self.error(span, format!("missing argument `{p}` (`{pt}`) in call to `{name}`"));
                }
            }
        }
        sig.ret.subst_many(&map)
    }

    fn call_builtin(&mut self, name: &str, args: &[Arg], span: Span) -> Ty {
        if args.iter().any(|a| a.name.is_some()) {
            self.error(span, format!("built-in `{name}` takes positional arguments only"));
        }
        let es: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        let (s, f, b, null) = (Ty::Str, Ty::Float, Ty::Bool, Ty::null());
        let list = |t: Ty| Ty::List(Box::new(t));
        let mono: Option<(Vec<Ty>, Ty)> = match name {
            "str" => Some((vec![Ty::Union(vec![s.clone(), f.clone(), b.clone(), null])], s.clone())),
            "upper" | "lower" | "b64encode" => Some((vec![s.clone()], s.clone())),
            "join" => Some((vec![list(s.clone()), s.clone()], s.clone())),
            "split" => Some((vec![s.clone(), s.clone()], list(s.clone()))),
            "starts_with" | "ends_with" | "matches" => Some((vec![s.clone(), s.clone()], b.clone())),
            "replace" => Some((vec![s.clone(), s.clone(), s.clone()], s.clone())),
            "to_json" | "to_yaml" => Some((vec![Ty::Json], s.clone())),
            "error" => Some((vec![s.clone()], Ty::Never)),
            _ => None,
        };
        let arity = |c: &mut Self, n: usize| {
            if es.len() != n {
                c.error(span, format!("`{name}` takes {n} argument(s), got {}", es.len()));
                for e in &es {
                    c.synth(e);
                }
                false
            } else {
                true
            }
        };
        if let Some((ps, ret)) = mono {
            if !arity(self, ps.len()) {
                return ret;
            }
            for (e, p) in es.iter().zip(&ps) {
                self.check(e, p);
            }
            if name == "matches" {
                if let ExprKind::Lit(Lit::Str(re)) = &es[1].kind {
                    if let Err(err) = regex::Regex::new(re) {
                        self.error(es[1].span, format!("invalid regex: {err}"));
                    }
                }
            }
            return ret;
        }
        match name {
            "len" | "keys" | "values" | "sort" => {
                if !arity(self, 1) {
                    return Ty::Error;
                }
                let t = self.synth(es[0]);
                let shape = self.ctx.shape(&t);
                let ok_coll = self.sub(&t, &Ty::Union(vec![list(Ty::Json), Ty::Map(Box::new(Ty::Json))]));
                match name {
                    "len" if ok_coll || self.sub(&t, &s) => Ty::Int,
                    "keys" if self.sub(&t, &Ty::Map(Box::new(Ty::Json))) => list(s),
                    "values" => match shape {
                        Ty::Map(v) => list(*v),
                        Ty::Record(r) => list(self.union(r.fields.iter().map(Field::read_ty).collect())),
                        Ty::Json | Ty::Error => list(Ty::Json),
                        _ => {
                            self.error(es[0].span, format!("`values` expects an object, got `{t}`"));
                            Ty::Error
                        }
                    },
                    "sort" if self.sub(&t, &list(s.clone())) || self.sub(&t, &list(f.clone())) => t,
                    _ => {
                        let want = match name {
                            "len" => "a string, list or object",
                            "keys" => "an object",
                            _ => "a list of strings or numbers",
                        };
                        self.error(es[0].span, format!("`{name}` expects {want}, got `{t}`"));
                        Ty::Error
                    }
                }
            }
            "contains" => {
                if !arity(self, 2) {
                    return b;
                }
                let t = self.synth(es[0]);
                if self.sub(&t, &s) {
                    self.check(es[1], &s);
                } else if self.sub(&t, &list(Ty::Json)) {
                    self.synth(es[1]);
                } else {
                    self.error(es[0].span, format!("`contains` expects a string or a list, got `{t}`"));
                }
                b
            }
            "min" | "max" => {
                if !arity(self, 2) {
                    return Ty::Error;
                }
                let (x, y) = (self.synth(es[0]), self.synth(es[1]));
                if self.sub(&x, &Ty::Int) && self.sub(&y, &Ty::Int) {
                    Ty::Int
                } else if self.sub(&x, &f) && self.sub(&y, &f) {
                    f
                } else {
                    self.error(span, format!("`{name}` expects two numbers, got `{x}` and `{y}`"));
                    Ty::Error
                }
            }
            _ => unreachable!("every builtin is typed above"),
        }
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
            let m = self.marks();
            self.bind_pat(&rp, &target, Some(scrut));
            match expected {
                Some(x) => self.check(&arm.body, x),
                None => {
                    let t = self.synth(&arm.body);
                    arm_tys.push(t);
                }
            }
            self.reset(m);
            let left: Vec<Ty> = self.ctx.members(&remaining).into_iter().filter(|m| !self.covers(&rp, m)).collect();
            remaining = self.union(left);
        }
        if remaining != Ty::Never && remaining != Ty::Error {
            self.error(span, format!("non-exhaustive match: values of type `{remaining}` are not covered"));
        }
        match expected {
            Some(x) => x.clone(),
            None => self.union(arm_tys),
        }
    }

    fn resolve_pat(&mut self, p: &Pat) -> RPat {
        match &p.kind {
            PatKind::Wild => RPat::Wild,
            PatKind::Bind(n) => RPat::Bind(n.clone(), p.span),
            PatKind::Typed(n, te) => RPat::Typed(n.clone(), self.resolve_te(te), p.span),
            PatKind::Lit(l) => RPat::Lit(l.clone(), p.span),
            PatKind::Tuple(ps) => {
                let mut out = Vec::new();
                for x in ps {
                    out.push(self.resolve_pat(x));
                }
                RPat::Tuple(out, p.span)
            }
            PatKind::Record(fs, open) => {
                let mut out = Vec::new();
                for (k, x) in fs {
                    out.push((k.clone(), self.resolve_pat(x)));
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
            RPat::Lit(l, span) => {
                let lt = Ty::Lit(l.clone());
                if !self.ctx.members(t).iter().any(|m| self.sub(&lt, m)) {
                    self.error(*span, format!("literal `{l}` can never match a value of type `{t}`"));
                }
                if let Some(s) = subject.filter(|s| s.is_pure()) {
                    self.hyps.push(Expr::binary(BinOp::Eq, s.clone(), Expr::lit(l.clone(), *span)));
                }
            }
            RPat::Tuple(ps, span) => {
                let cands: Vec<Ty> = self
                    .ctx
                    .members(t)
                    .into_iter()
                    .filter(|m| match self.ctx.shape(m) {
                        Ty::Tuple(ts) => ts.len() == ps.len(),
                        Ty::List(_) | Ty::Error => true,
                        _ => false,
                    })
                    .collect();
                if cands.is_empty() {
                    self.error(*span, format!("a tuple pattern can never match a value of type `{t}`"));
                }
                for (i, sp) in ps.iter().enumerate() {
                    let et = self.union(
                        cands
                            .iter()
                            .map(|m| match self.ctx.shape(m) {
                                Ty::Tuple(ts) => ts[i].clone(),
                                Ty::List(e) => *e,
                                _ => Ty::Error,
                            })
                            .collect(),
                    );
                    self.bind_pat(sp, &et, None);
                }
            }
            RPat::Record(fps, open, span) => {
                let cands: Vec<Ty> =
                    self.ctx.members(t).into_iter().filter(|m| self.record_pat_ok(m, fps, *open)).collect();
                if cands.is_empty() {
                    self.error(*span, format!("this record pattern can never match a value of type `{t}`"));
                }
                for (k, fp) in fps {
                    let ft = self.union(cands.iter().map(|m| self.pat_field_ty(m, k)).collect());
                    let sub = subject.map(|s| project(s, k));
                    self.bind_pat(fp, &ft, sub.as_ref());
                }
            }
        }
    }

    fn pat_field_ty(&self, m: &Ty, k: &str) -> Ty {
        match self.ctx.shape(m) {
            Ty::Record(r) => r.get(k).map_or(Ty::Json, Field::read_ty),
            Ty::Map(t) => Ty::Union(vec![*t, Ty::null()]),
            _ => Ty::Error,
        }
    }

    fn record_pat_ok(&self, m: &Ty, fps: &[(String, RPat)], open: bool) -> bool {
        match self.ctx.shape(m) {
            Ty::Error => true,
            Ty::Record(r) => {
                fps.iter().all(|(k, fp)| match r.get(k) {
                    Some(f) => self.may_match(fp, &f.read_ty()),
                    None => r.open,
                }) && (open || r.fields.iter().all(|f| !f.present() || fps.iter().any(|(k, _)| *k == f.name)))
            }
            Ty::Map(t) => fps.iter().all(|(_, fp)| self.may_match(fp, &Ty::Union(vec![(*t).clone(), Ty::null()]))),
            _ => false,
        }
    }

    fn may_match(&self, p: &RPat, t: &Ty) -> bool {
        match p {
            RPat::Wild | RPat::Bind(..) => true,
            RPat::Typed(_, ty, _) => self.ctx.narrow(t, ty) != Ty::Never,
            RPat::Lit(l, _) => self.ctx.members(t).iter().any(|m| self.sub(&Ty::Lit(l.clone()), m)),
            RPat::Tuple(ps, _) => self.ctx.members(t).iter().any(|m| match self.ctx.shape(m) {
                Ty::Tuple(ts) => ts.len() == ps.len(),
                Ty::List(_) | Ty::Error => true,
                _ => false,
            }),
            RPat::Record(fps, open, _) => self.ctx.members(t).iter().any(|m| self.record_pat_ok(m, fps, *open)),
        }
    }

    /// Does `p` match every value of the single (non-union) type `m`?
    fn covers(&self, p: &RPat, m: &Ty) -> bool {
        match p {
            RPat::Wild | RPat::Bind(..) => true,
            RPat::Typed(_, ty, _) => self.ctx.sub(m, ty, Mode::Strict),
            RPat::Lit(l, _) => *m.strip() == Ty::Lit(l.clone()),
            RPat::Tuple(ps, _) => match self.ctx.shape(m) {
                Ty::Tuple(ts) => ts.len() == ps.len() && ps.iter().zip(&ts).all(|(p, t)| self.covers_ty(p, t)),
                _ => false,
            },
            RPat::Record(fps, open, _) => match self.ctx.shape(m) {
                Ty::Record(r) => {
                    (*open || !r.open)
                        && (*open || r.fields.iter().all(|f| fps.iter().any(|(k, _)| *k == f.name)))
                        && fps.iter().all(|(k, fp)| {
                            r.get(k).is_some_and(|f| f.present() && self.covers_ty(fp, &f.ty))
                        })
                }
                _ => false,
            },
        }
    }

    fn covers_ty(&self, p: &RPat, t: &Ty) -> bool {
        self.ctx.members(t).iter().all(|m| self.covers(p, m))
    }
}

fn merge_records(a: Rec, b: Rec) -> Rec {
    let mut fields = a.fields;
    for f in b.fields {
        match fields.iter_mut().find(|x| x.name == f.name) {
            Some(x) => match (&x.ty, &f.ty) {
                (Ty::Record(ra), Ty::Record(rb)) => x.ty = Ty::Record(merge_records(ra.clone(), rb.clone())),
                _ => *x = f,
            },
            None => fields.push(f),
        }
    }
    Rec { fields, open: a.open || b.open }
}

/// Rewrite references to a module's own types (`DeploymentSpec`) to their imported names (`k8s.DeploymentSpec`).
fn qualify(te: &TypeExpr, prefix: &str, locals: &HashSet<&str>) -> TypeExpr {
    let q = |t: &TypeExpr| qualify(t, prefix, locals);
    let kind = match &te.kind {
        TypeKind::Named(n) if locals.contains(n.as_str()) => TypeKind::Named(format!("{prefix}{n}")),
        TypeKind::Named(n) => TypeKind::Named(n.clone()),
        TypeKind::Lit(l) => TypeKind::Lit(l.clone()),
        TypeKind::List(t) => TypeKind::List(Box::new(q(t))),
        TypeKind::Map(t) => TypeKind::Map(Box::new(q(t))),
        TypeKind::Tuple(ts) => TypeKind::Tuple(ts.iter().map(q).collect()),
        TypeKind::Union(ts) => TypeKind::Union(ts.iter().map(q).collect()),
        TypeKind::Refine(b, p) => TypeKind::Refine(Box::new(q(b)), p.clone()),
        TypeKind::With(b, patches) => TypeKind::With(
            Box::new(q(b)),
            patches
                .iter()
                .map(|p| Patch {
                    op: match &p.op {
                        PatchOp::Set(t, d) => PatchOp::Set(q(t), d.clone()),
                        other => other.clone(),
                    },
                    ..p.clone()
                })
                .collect(),
        ),
        TypeKind::Record { fields, open } => TypeKind::Record {
            fields: fields.iter().map(|f| FieldDecl { ty: q(&f.ty), ..f.clone() }).collect(),
            open: *open,
        },
    };
    TypeExpr { kind, span: te.span }
}

fn item_span(item: &Item) -> Span {
    match item {
        Item::Type { span, .. }
        | Item::Input { span, .. }
        | Item::Let { span, .. }
        | Item::Export { span, .. }
        | Item::Import { span, .. }
        | Item::Refine { span, .. } => *span,
        Item::Fn(f) => f.span,
    }
}

/// `a.b.c` as a dotted name, if `e` is a chain of plain field accesses on a variable.
pub fn qualified_name(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Var(n) => Some(n.clone()),
        ExprKind::Field(b, Label::Name(k), false) => qualified_name(b).map(|q| format!("{q}.{k}")),
        _ => None,
    }
}
