//! Evaluating a program for display, and comparing its output with the same
//! program at another git revision (`cleat diff`, `cleat view --diff`).

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use cleat_core::{ExportError, InputValue, Session, Value, diff, emit, targets};

use crate::git::GitHost;

pub(crate) fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

pub(crate) fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn diag(span: cleat_core::diag::Span, msg: &str) -> Value {
    obj(vec![
        ("line", Value::Int(span.line as i64)),
        ("col", Value::Int(span.col as i64)),
        ("msg", s(msg)),
    ])
}

/// An input value as the user would type it (for showing command-line inputs on the page).
pub(crate) fn raw_input(v: &InputValue) -> String {
    match v {
        InputValue::Raw(r) => r.clone(),
        InputValue::Json(Value::Str(x)) => x.clone(),
        InputValue::Json(v) => emit::to_json(v, false),
        InputValue::RawList(xs) => emit::to_json(&Value::List(xs.iter().map(|x| s(x)).collect()), false),
        InputValue::RawMap(es) => {
            emit::to_json(&Value::Obj(es.iter().map(|(k, v)| (k.clone(), s(v))).collect()), false)
        }
    }
}

/// Check and evaluate `src` (the file `file`) in `sess`, as the fields the
/// view page reads: `inputs`, then `errors`, or `warnings`, `components` and
/// `resources`. With `only_declared`, inputs the program doesn't declare are
/// dropped rather than reported (the other side of a comparison may have
/// added them).
pub fn evaluate(
    sess: &Session,
    file: &str,
    src: &str,
    root: Option<&str>,
    inputs: Vec<(String, InputValue)>,
    target: &str,
    only_declared: bool,
) -> Vec<(&'static str, Value)> {
    let base = crate::host::dir_of(file);
    let base = base.as_deref();
    let mut out = Vec::new();
    let infos = match cleat_core::inputs(sess, src, base, root) {
        Ok(infos) => infos,
        Err(outcome) => {
            out.push((
                "errors",
                Value::List(outcome.errors.iter().map(|d| diag(d.span, &d.msg)).collect()),
            ));
            return out;
        }
    };
    out.push((
        "inputs",
        Value::List(
            infos
                .iter()
                .map(|i| {
                    let given = inputs
                        .iter()
                        .rev()
                        .find(|(n, _)| *n == i.name)
                        .map(|(_, v)| raw_input(v));
                    obj(vec![
                        ("name", s(&i.name)),
                        ("ty", s(&i.ty)),
                        ("definition", i.definition.as_deref().map_or(Value::Null, s)),
                        ("required", Value::Bool(i.required)),
                        (
                            "default",
                            i.default.as_ref().map_or(Value::Null, |d| s(&emit::to_json(d, false))),
                        ),
                        ("given", given.map_or(Value::Null, |g| s(&g))),
                    ])
                })
                .collect(),
        ),
    ));
    let inputs = if only_declared {
        inputs
            .into_iter()
            .filter(|(n, _)| infos.iter().any(|i| i.name == *n))
            .collect()
    } else {
        inputs
    };
    match cleat_core::export(sess, src, base, root, inputs) {
        Err(ExportError::Check(outcome)) => {
            out.push((
                "errors",
                Value::List(outcome.errors.iter().map(|d| diag(d.span, &d.msg)).collect()),
            ));
        }
        Err(ExportError::Eval(errs)) => {
            let list = errs
                .iter()
                .map(|e| diag(e.trace.first().map_or(e.span, |(_, at)| *at), &e.msg))
                .collect();
            out.push(("errors", Value::List(list)));
        }
        Ok((outcome, comps)) => {
            out.push((
                "warnings",
                Value::List(outcome.warnings.iter().map(|d| diag(d.span, &d.msg)).collect()),
            ));
            match targets::view_data(&comps, target) {
                Ok((components, resources)) => {
                    out.push(("components", components));
                    out.push(("resources", resources));
                }
                Err(e) => out.push(("errors", Value::List(vec![obj(vec![("msg", s(&e))])]))),
            }
        }
    }
    out
}

/// One of the fields `evaluate` returns.
pub fn field<'a>(fields: &'a [(&'static str, Value)], name: &str) -> Option<&'a Value> {
    fields.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut x = v;
    for p in path {
        match x.get(p) {
            Some(y) => x = y,
            None => return "",
        }
    }
    match x {
        Value::Str(s) => s,
        _ => "",
    }
}

/// How a resource is identified across versions: `kind/namespace/name`.
pub fn resource_key(doc: &Value) -> String {
    format!(
        "{}/{}/{}",
        str_at(doc, &["kind"]),
        str_at(doc, &["metadata", "namespace"]),
        str_at(doc, &["metadata", "name"])
    )
}

/// The working tree's output compared with the base's: each head resource
/// gets `change` (`added`, `changed` or `same`) and, when changed, `diff` (a
/// unified diff of its YAML). Returns the base resources that are gone.
pub fn compare(head: &mut [Value], base: &[Value]) -> Vec<Value> {
    let key = |r: &Value| r.get("doc").map(resource_key).unwrap_or_default();
    let yaml = |r: &Value| match r.get("yaml") {
        Some(Value::Str(y)) => y.clone(),
        _ => String::new(),
    };
    for r in head.iter_mut() {
        let k = key(r);
        let (change, d) = match base.iter().find(|b| key(b) == k) {
            None => ("added", None),
            Some(b) => {
                let d = diff::unified(&yaml(b), &yaml(r), 3);
                if d.is_empty() {
                    ("same", None)
                } else {
                    ("changed", Some(d))
                }
            }
        };
        if let Value::Obj(es) = r {
            es.push(("change".into(), s(change)));
            if let Some(d) = d {
                es.push(("diff".into(), s(&d)));
            }
        }
    }
    base.iter()
        .filter(|b| !head.iter().any(|r| key(r) == key(b)))
        .cloned()
        .collect()
}

/// `evaluate`'s fields for one set of inputs, keyed by those inputs.
type Cached = Option<(String, Vec<(&'static str, Value)>)>;

/// The program at a git revision, evaluated on demand. A revision doesn't
/// change, so results are kept for each set of inputs.
pub struct Base {
    /// `main (1a2b3c4d5e)`, for messages.
    pub label: String,
    sess: Session,
    host: Rc<GitHost>,
    cache: RefCell<Cached>,
}

impl Base {
    pub fn new(file: &Path, rev: &str) -> Result<Base, String> {
        let dir = file
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let host = Rc::new(GitHost::new(dir, rev)?);
        Ok(Base {
            label: format!("{rev} ({})", host.short_commit()),
            sess: Session::new(host.clone()),
            host,
            cache: RefCell::new(None),
        })
    }

    /// The base's fields, as `evaluate` gives them. If the file doesn't exist
    /// at the revision, there are simply no resources.
    pub fn evaluate(
        &self,
        file: &Path,
        root: Option<&Path>,
        inputs: &[(String, InputValue)],
        target: &str,
    ) -> Vec<(&'static str, Value)> {
        let key = format!(
            "{target}\u{0}{}",
            inputs
                .iter()
                .map(|(k, v)| format!("{k}={}", raw_input(v)))
                .collect::<Vec<_>>()
                .join("\u{0}")
        );
        if let Some((k, v)) = &*self.cache.borrow()
            && *k == key
        {
            return v.clone();
        }
        let file = file
            .canonicalize()
            .unwrap_or_else(|_| file.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let root = root.map(|r| {
            r.canonicalize()
                .unwrap_or_else(|_| r.to_path_buf())
                .to_string_lossy()
                .into_owned()
        });
        let out = match cleat_core::Host::read(&*self.host, &file) {
            Err(_) => vec![("resources", Value::List(Vec::new())), ("missing", Value::Bool(true))],
            Ok(src) => evaluate(&self.sess, &file, &src, root.as_deref(), inputs.to_vec(), target, true),
        };
        *self.cache.borrow_mut() = Some((key, out.clone()));
        out
    }
}
