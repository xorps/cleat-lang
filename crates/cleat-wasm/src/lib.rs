//! Cleat in the browser: `cleat-core` on an in-memory host, for the playground.
//!
//! The interface is one function taking and returning JSON, so the page needs
//! no generated bindings:
//!
//! 1. `cleat_alloc(len)` reserves `len` bytes; the page writes a UTF-8 JSON
//!    request there.
//! 2. `cleat_call(ptr, len)` handles it (and frees it); the response is then
//!    at `cleat_result_ptr()`, `cleat_result_len()` bytes long, until the next call.
//!
//! Requests (`op`):
//! - `set_file { name, text }` / `remove_file { name }`: the in-memory files.
//!   Imports resolve between them, confined to `/`.
//! - `analyze { file, inputs: { name: "raw value" }, target: "plain" | "argocd" }`:
//!   diagnostics, hovers, the declared inputs and, if it checks and
//!   evaluates, the YAML output plus the components and resources that the
//!   resource view (`cleat view`'s page) draws.
//! - `complete { file, line, col }` (0-based, in characters): completion items.

use std::cell::RefCell;
use std::rc::Rc;

use cleat_core::check::{Completion, CompletionKind, Outcome};
use cleat_core::diag::{Span, token_len};
use cleat_core::host::dir_of;
use cleat_core::{Host, InputValue, MemHost, Session, Value, check, emit, eval, json, parser, targets};

thread_local! {
    static STATE: (Rc<MemHost>, Session) = {
        let host = Rc::new(MemHost::new());
        (host.clone(), Session::new(host))
    };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn cleat_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr` must come from `cleat_alloc(len)` and hold `len` bytes written by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cleat_call(ptr: *mut u8, len: usize) {
    // SAFETY: allocated by `cleat_alloc` with capacity `len`, and filled by the caller.
    let bytes = unsafe { Vec::from_raw_parts(ptr, len, len) };
    let response = match std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())
        .and_then(json::parse)
    {
        Ok(req) => handle(&req).unwrap_or_else(|e| obj(vec![("error", s(&e))])),
        Err(e) => obj(vec![("error", s(&format!("bad request: {e}")))]),
    };
    RESULT.with(|r| *r.borrow_mut() = emit::to_json(&response, false).into_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn cleat_result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn cleat_result_len() -> usize {
    RESULT.with(|r| r.borrow().len())
}

fn handle(req: &Value) -> Result<Value, String> {
    let (host, sess) = STATE.with(|(h, s)| (h.clone(), s.clone()));
    let field = |k: &str| match req.get(k) {
        Some(Value::Str(v)) => Ok(v.as_str()),
        _ => Err(format!("missing `{k}`")),
    };
    match field("op")? {
        "set_file" => {
            host.set(field("name")?, field("text")?);
            Ok(Value::Null)
        }
        "remove_file" => {
            host.remove(field("name")?);
            Ok(Value::Null)
        }
        "analyze" => {
            let target = field("target").unwrap_or("plain");
            let inputs = match req.get("inputs") {
                Some(Value::Obj(es)) => es
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Value::Str(raw) if !raw.trim().is_empty() => Some((k.clone(), InputValue::Raw(raw.clone()))),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Ok(analyze(&sess, field("file")?, inputs, target))
        }
        "complete" => {
            let file = field("file")?;
            let num = |k: &str| match req.get(k) {
                Some(Value::Int(n)) if *n >= 0 => Ok(*n as usize),
                _ => Err(format!("missing `{k}`")),
            };
            let text = host.read(file)?;
            let base = dir_of(file);
            let items =
                cleat_core::complete::complete(&sess, &text, num("line")?, num("col")?, base.as_deref(), Some("/"));
            Ok(match items {
                Some(c) => obj(vec![
                    ("items", Value::List(c.items.iter().map(completion).collect())),
                    ("replace_len", Value::Int(c.replace_len as i64)),
                ]),
                None => obj(vec![("items", Value::List(vec![])), ("replace_len", Value::Int(0))]),
            })
        }
        other => Err(format!("unknown op `{other}`")),
    }
}

fn analyze(sess: &Session, file: &str, inputs: Vec<(String, InputValue)>, target: &str) -> Value {
    let text = sess.host().read(file).unwrap_or_default();
    let base = dir_of(file);
    let (base, root) = (base.as_deref(), Some("/"));
    let mut out = vec![];
    let items = match parser::parse_program(&text) {
        Ok(items) => items,
        Err(e) => {
            out.push(("errors", Value::List(vec![diag(&text, e.span, &e.msg)])));
            return obj(out);
        }
    };
    let outcome = check::check_program(sess, &items, base, root);
    out.push(("errors", diags(&text, &outcome.errors)));
    out.push(("warnings", diags(&text, &outcome.warnings)));
    out.push(("hovers", hovers(&outcome)));
    if !outcome.errors.is_empty() {
        return obj(out);
    }
    let infos = cleat_core::input_infos(&items, &outcome);
    out.push((
        "inputs",
        Value::List(
            infos
                .iter()
                .map(|i| {
                    obj(vec![
                        ("name", s(&i.name)),
                        ("ty", s(&i.ty)),
                        ("definition", i.definition.as_deref().map_or(Value::Null, s)),
                        ("required", Value::Bool(i.required)),
                        (
                            "default",
                            i.default.as_ref().map_or(Value::Null, |d| s(&emit::to_json(d, false))),
                        ),
                    ])
                })
                .collect(),
        ),
    ));
    match eval::run(sess, &items, outcome.ctx.clone(), base, root, inputs) {
        Ok(comps) => {
            let docs = match target {
                "argocd" => targets::argocd(&comps),
                _ => Ok(targets::plain(&comps)),
            };
            match docs.and_then(|docs| Ok((docs, targets::view_data(&comps, target)?))) {
                Ok((docs, (components, resources))) => {
                    out.push(("output", s(&emit::to_yaml_stream(&docs))));
                    // For the resource view (the same page as `cleat view`).
                    out.push(("components", components));
                    out.push(("resources", resources));
                }
                Err(e) => out.push(("eval_errors", Value::List(vec![obj(vec![("msg", s(&e))])]))),
            }
        }
        Err(errs) => {
            let list = errs
                .iter()
                .map(|e| {
                    // Inside a function, point at the outermost call in this file.
                    let span = e.trace.first().map_or(e.span, |(_, at)| *at);
                    if span.line == 0 {
                        obj(vec![("msg", s(&e.msg))])
                    } else {
                        diag(&text, span, &e.msg)
                    }
                })
                .collect();
            out.push(("eval_errors", Value::List(list)));
        }
    }
    obj(out)
}

fn diags(text: &str, ds: &[cleat_core::diag::Diag]) -> Value {
    Value::List(ds.iter().map(|d| diag(text, d.span, &d.msg)).collect())
}

fn diag(text: &str, span: Span, msg: &str) -> Value {
    obj(vec![
        ("line", Value::Int(span.line as i64)),
        ("col", Value::Int(span.col as i64)),
        ("len", Value::Int(token_len(text, span) as i64)),
        ("msg", s(msg)),
    ])
}

fn hovers(o: &Outcome) -> Value {
    Value::List(
        o.hovers
            .iter()
            .map(|h| {
                obj(vec![
                    ("line", Value::Int(h.span.line as i64)),
                    ("col", Value::Int(h.span.col as i64)),
                    ("len", Value::Int(h.len as i64)),
                    ("text", s(&h.text)),
                    ("doc", h.doc.as_deref().map_or(Value::Null, s)),
                ])
            })
            .collect(),
    )
}

fn completion(c: &Completion) -> Value {
    let kind = match c.kind {
        CompletionKind::Field => "field",
        CompletionKind::Value => "value",
        CompletionKind::Variable => "variable",
        CompletionKind::Function => "function",
        CompletionKind::Module => "module",
        CompletionKind::Type => "type",
    };
    obj(vec![
        ("label", s(&c.label)),
        ("kind", s(kind)),
        ("detail", c.detail.as_deref().map_or(Value::Null, s)),
        ("doc", c.doc.as_deref().map_or(Value::Null, s)),
        ("insert", s(&c.insert)),
        ("preferred", Value::Bool(c.preferred)),
    ])
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}
