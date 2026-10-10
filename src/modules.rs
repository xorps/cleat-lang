//! Loading imported modules: Cleat files, OpenAPI documents and CRDs.
//! Loaded modules are cached by path and modification time, so the language
//! server can re-check on every keystroke without re-reading large schemas.

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
}

type Cache = Mutex<HashMap<PathBuf, (SystemTime, Arc<Module>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub fn load(path: &Path) -> Result<Arc<Module>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if let Some((t, m)) = cache().lock().unwrap().get(path) {
        if *t == mtime {
            return Ok(m.clone());
        }
    }
    let module = Arc::new(load_uncached(path)?);
    cache().lock().unwrap().insert(path.to_path_buf(), (mtime, module.clone()));
    Ok(module)
}

/// The Cleat source a schema file is converted to (for `cleat gen`).
pub fn generate_source(path: &Path) -> Result<String, String> {
    Ok(openapi::generate(&read_docs(path)?)?.source)
}

fn read_docs(path: &Path) -> Result<Vec<crate::Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => Ok(vec![json::parse(&text).map_err(|e| format!("`{}`: {e}", path.display()))?]),
        Some("yaml" | "yml") => yaml::parse_docs(&text).map_err(|e| format!("`{}`: {e}", path.display())),
        _ => Err(format!("cannot import `{}`: expected a .cleat, .json, .yaml or .yml file", path.display())),
    }
}

fn load_uncached(path: &Path) -> Result<Module, String> {
    let is_source = path.extension().and_then(|e| e.to_str()) == Some("cleat");
    let (source, docs) = if is_source {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        (text, Vec::new())
    } else {
        let generated = openapi::generate(&read_docs(path)?).map_err(|e| format!("`{}`: {e}", path.display()))?;
        (generated.source, generated.docs)
    };
    let items = parser::parse_program(&source)
        .map_err(|d| format!("`{}`:{}: {}", path.display(), d.span, d.msg))?;
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
    Ok(Module { items, docs, is_source })
}
