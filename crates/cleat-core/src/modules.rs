//! Loading imported modules: Cleat files, OpenAPI documents and CRDs.
//!
//! Files are read through the session's host (see `host`). Loads are cached
//! by the file's stamp, and every load is recorded so callers can tell which
//! files a result depends on.

use crate::ast::Item;
use crate::host::{Session, Stamp, extension, file_name};
use crate::prelude::*;
use crate::{json, openapi, parser};

pub struct Module {
    /// The module's items. Schema modules contain only `type` declarations.
    pub items: Vec<Item>,
    /// Documentation keyed by local `Type` or `Type.field`.
    pub docs: Vec<(String, String)>,
    /// A `.cleat` source module (checked and evaluated on its own), as opposed to a schema.
    pub is_source: bool,
    /// For a schema: where the host saved the generated Cleat source, so editors can open it.
    pub generated: Option<String>,
}

/// A file's text, recorded as a dependency of the current computation.
pub fn read_source(sess: &Session, file: &str) -> Result<String, String> {
    sess.record(file, sess.host().stamp(file));
    sess.host().read(file)
}

pub fn load(sess: &Session, file: &str) -> Result<Rc<Module>, String> {
    let st = sess.host().stamp(file);
    sess.record(file, st);
    if st == Stamp::Missing {
        return Err(format!("cannot read `{file}`: No such file or directory"));
    }
    if let Some(m) = sess.cached_module(file, st) {
        return Ok(m);
    }
    let module = Rc::new(load_uncached(sess, file)?);
    sess.cache_module(file, st, module.clone());
    Ok(module)
}

/// The Cleat source a schema file is converted to (for `cleat gen`).
pub fn generate_source(sess: &Session, path: &str) -> Result<String, String> {
    Ok(openapi::generate(&read_docs(sess, path)?)?.source)
}

fn read_docs(sess: &Session, path: &str) -> Result<Vec<crate::Value>, String> {
    let text = sess.host().read(path)?;
    match extension(path) {
        Some("json") => Ok(vec![json::parse(&text).map_err(|e| format!("`{}`: {e}", path))?]),
        Some("yaml" | "yml") => sess.host().parse_yaml(&text).map_err(|e| format!("`{}`: {e}", path)),
        _ => Err(format!(
            "cannot import `{}`: expected a .cleat, .json, .yaml or .yml file",
            path
        )),
    }
}

fn load_uncached(sess: &Session, path: &str) -> Result<Module, String> {
    let is_source = extension(path) == Some("cleat");
    let (source, docs) = if is_source {
        (sess.host().read(path)?, Vec::new())
    } else {
        let generated = openapi::generate(&read_docs(sess, path)?).map_err(|e| format!("`{}`: {e}", path))?;
        (generated.source, generated.docs)
    };
    let items = parser::parse_program(&source).map_err(|d| format!("`{}`:{}: {}", path, d.span, d.msg))?;
    for item in &items {
        match item {
            Item::Input { .. } => {
                return Err(format!(
                    "`{}`: a module cannot declare `input`s; take values as function parameters instead",
                    path
                ));
            }
            Item::Export { .. } => {
                return Err(format!(
                    "`{}`: a module cannot `export`; return documents from functions instead",
                    path
                ));
            }
            _ => {}
        }
    }
    let generated = if is_source {
        None
    } else {
        sess.host().generated(path, &source)
    };
    Ok(Module {
        items,
        docs,
        is_source,
        generated,
    })
}

/// The value of `import x from "path" with { type: kind }`.
pub fn load_data(sess: &Session, path: &str, kind: &str) -> Result<crate::Value, String> {
    let text = read_source(sess, path)?;
    let at = |e: String| format!("`{}`: {e}", path);
    match kind {
        "text" => Ok(crate::Value::Str(text)),
        "json" => json::parse(&text).map_err(at),
        "yaml" => {
            let mut docs = sess.host().parse_yaml(&text).map_err(at)?;
            Ok(if docs.len() == 1 {
                docs.pop().unwrap()
            } else {
                crate::Value::List(docs)
            })
        }
        other => Err(format!(
            "unsupported import type `{other}` (expected \"json\", \"yaml\" or \"text\")"
        )),
    }
}

/// The namespace a named import (`import { A } from "path"`) is loaded under:
/// the file name as an identifier (`kubernetes-1.37.json` -> `kubernetes_1_31`).
pub fn hidden_namespace(path: &str, taken: impl Fn(&str) -> bool) -> String {
    let file = file_name(path);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _ext)| stem);
    let mut base: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if base.is_empty() || base.starts_with(|c: char| c.is_ascii_digit()) {
        base.insert(0, '_');
    }
    let mut name = base.clone();
    let mut i = 2;
    while taken(&name) {
        name = format!("{base}_{i}");
        i += 1;
    }
    name
}
