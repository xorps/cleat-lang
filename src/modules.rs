//! Loading imported modules: Cleat files, OpenAPI documents and CRDs.
//!
//! Files are read through an overlay of open editor buffers (set by the
//! language server), so unsaved edits to an imported file are seen at once.
//! Loads are cached by a stamp (buffer version or file modification time), and
//! every load is recorded so callers can tell which files a result depends on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::ast::Item;
use crate::{json, openapi, parser, yaml};

pub struct Module {
    /// The module's items. Schema modules contain only `type` declarations.
    pub items: Vec<Item>,
    /// Documentation keyed by local `Type` or `Type.field`.
    pub docs: Vec<(String, String)>,
    /// A `.cleat` source module (checked and evaluated on its own), as opposed to a schema.
    pub is_source: bool,
    /// For a schema: the generated Cleat source, written to disk so editors can open it.
    pub generated: Option<PathBuf>,
}

/// What a cached result was computed from: an editor buffer version or a file's modification time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stamp {
    Buffer(u64),
    Disk(SystemTime),
    Missing,
}

type Overlay = Mutex<HashMap<PathBuf, (u64, Arc<String>)>>;

fn overlay() -> &'static Overlay {
    static OVERLAY: OnceLock<Overlay> = OnceLock::new();
    OVERLAY.get_or_init(Default::default)
}

fn canon(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Use `text` instead of the file's contents (an open, possibly unsaved, editor buffer).
pub fn set_overlay(path: &Path, text: String) {
    static VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let v = VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    overlay().lock().unwrap().insert(canon(path), (v, Arc::new(text)));
}

pub fn clear_overlay(path: &Path) {
    overlay().lock().unwrap().remove(&canon(path));
}

pub fn stamp(path: &Path) -> Stamp {
    let p = canon(path);
    if let Some((v, _)) = overlay().lock().unwrap().get(&p) {
        return Stamp::Buffer(*v);
    }
    match std::fs::metadata(&p).and_then(|m| m.modified()) {
        Ok(t) => Stamp::Disk(t),
        Err(_) => Stamp::Missing,
    }
}

/// A file's text: the editor buffer if one is open, else the file on disk.
pub fn read_source(path: &Path) -> Result<String, String> {
    if let Some((_, text)) = overlay().lock().unwrap().get(&canon(path)) {
        return Ok(text.as_str().to_string());
    }
    std::fs::read_to_string(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))
}

thread_local! {
    /// Active dependency recordings (nested: a module check inside a check).
    static RECORDERS: RefCell<Vec<Vec<(PathBuf, Stamp)>>> = const { RefCell::new(Vec::new()) };
}

/// Note that the current computation read `path`.
pub fn record(path: &Path, stamp: Stamp) {
    RECORDERS.with(|r| {
        for deps in r.borrow_mut().iter_mut() {
            deps.push((canon(path), stamp.clone()));
        }
    });
}

/// Run `f` and return the files it read (directly or through imports).
pub fn recording<T>(f: impl FnOnce() -> T) -> (T, Vec<(PathBuf, Stamp)>) {
    RECORDERS.with(|r| r.borrow_mut().push(Vec::new()));
    let out = f();
    let deps = RECORDERS.with(|r| r.borrow_mut().pop().unwrap_or_default());
    (out, deps)
}

/// Whether all recorded dependencies are unchanged.
pub fn fresh(deps: &[(PathBuf, Stamp)]) -> bool {
    deps.iter().all(|(p, s)| stamp(p) == *s)
}

type Cache = Mutex<HashMap<PathBuf, (Stamp, Arc<Module>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub fn load(path: &Path) -> Result<Arc<Module>, String> {
    let st = stamp(path);
    record(path, st.clone());
    if st == Stamp::Missing {
        return Err(format!("cannot read `{}`: No such file or directory", path.display()));
    }
    if let Some((t, m)) = cache().lock().unwrap().get(&canon(path))
        && *t == st
    {
        return Ok(m.clone());
    }
    let module = Arc::new(load_uncached(path)?);
    cache().lock().unwrap().insert(canon(path), (st, module.clone()));
    Ok(module)
}

/// The Cleat source a schema file is converted to (for `cleat gen`).
pub fn generate_source(path: &Path) -> Result<String, String> {
    Ok(openapi::generate(&read_docs(path)?)?.source)
}

fn read_docs(path: &Path) -> Result<Vec<crate::Value>, String> {
    let text = read_source(path)?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => Ok(vec![
            json::parse(&text).map_err(|e| format!("`{}`: {e}", path.display()))?,
        ]),
        Some("yaml" | "yml") => yaml::parse_docs(&text).map_err(|e| format!("`{}`: {e}", path.display())),
        _ => Err(format!(
            "cannot import `{}`: expected a .cleat, .json, .yaml or .yml file",
            path.display()
        )),
    }
}

fn load_uncached(path: &Path) -> Result<Module, String> {
    let is_source = path.extension().and_then(|e| e.to_str()) == Some("cleat");
    let (source, docs) = if is_source {
        (read_source(path)?, Vec::new())
    } else {
        let generated = openapi::generate(&read_docs(path)?).map_err(|e| format!("`{}`: {e}", path.display()))?;
        (generated.source, generated.docs)
    };
    let items = parser::parse_program(&source).map_err(|d| format!("`{}`:{}: {}", path.display(), d.span, d.msg))?;
    for item in &items {
        match item {
            Item::Input { .. } => {
                return Err(format!(
                    "`{}`: a module cannot declare `input`s; take values as function parameters instead",
                    path.display()
                ));
            }
            Item::Export { .. } => {
                return Err(format!(
                    "`{}`: a module cannot `export`; return documents from functions instead",
                    path.display()
                ));
            }
            _ => {}
        }
    }
    let generated = if is_source {
        None
    } else {
        write_generated(path, &source)
    };
    Ok(Module {
        items,
        docs,
        is_source,
        generated,
    })
}

/// The value of `import x from "path" with { type: kind }`.
pub fn load_data(path: &Path, kind: &str) -> Result<crate::Value, String> {
    record(path, stamp(path));
    let text = read_source(path)?;
    let at = |e: String| format!("`{}`: {e}", path.display());
    match kind {
        "text" => Ok(crate::Value::Str(text)),
        "json" => json::parse(&text).map_err(at),
        "yaml" => {
            let mut docs = yaml::parse_docs(&text).map_err(at)?;
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
    let file = Path::new(path).file_name().and_then(|f| f.to_str()).unwrap_or("module");
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

/// Resolve an import path relative to `base` and, when there is a project
/// `root`, require the file (after resolving symlinks) to be inside it.
pub fn resolve_import(base: Option<&Path>, root: Option<&Path>, path: &str) -> Result<PathBuf, String> {
    let full = match base {
        Some(b) => b.join(path),
        None => PathBuf::from(path),
    };
    let Some(root) = root else { return Ok(full) };
    let canon = full.canonicalize().map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("project root `{}`: {e}", root.display()))?;
    if canon.starts_with(&root) {
        Ok(canon)
    } else {
        Err(format!(
            "`{path}` is outside the project root `{}`; imports must stay inside it (see --root)",
            root.display()
        ))
    }
}

/// The default project root for an entry file: the nearest enclosing
/// directory containing `.git`, else the file's own directory.
pub fn find_root(file: &Path) -> Option<PathBuf> {
    let dir = file.canonicalize().ok()?.parent()?.to_path_buf();
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
        .or(Some(dir))
}

/// Save a schema's generated Cleat source where editors can open it (go to definition).
fn write_generated(schema: &Path, source: &str) -> Option<PathBuf> {
    let name: String = canon(schema)
        .display()
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = std::env::temp_dir().join("cleat-generated");
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("{name}.cleat"));
    if std::fs::read_to_string(&file).ok().as_deref() != Some(source) {
        std::fs::write(&file, source).ok()?;
    }
    Some(file)
}
