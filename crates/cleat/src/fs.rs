//! The file-system host: imports are files on disk, seen through an overlay
//! of open editor buffers (set by the language server) so unsaved edits to an
//! imported file are seen at once.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use cleat_core::{Host, InputValue, Stamp, Value, json};

use crate::yaml;

/// Buffer versions and modification times share the stamp space; the top bit tells them apart.
const BUFFER: u64 = 1 << 63;

#[derive(Default)]
pub struct FsHost {
    overlay: RefCell<HashMap<PathBuf, (u64, String)>>,
    version: Cell<u64>,
}

fn canon(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn name(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl FsHost {
    /// Use `text` instead of the file's contents (an open, possibly unsaved, editor buffer).
    pub fn set_overlay(&self, path: &Path, text: String) {
        let v = self.version.get() + 1;
        self.version.set(v);
        self.overlay.borrow_mut().insert(canon(path), (v, text));
    }

    pub fn clear_overlay(&self, path: &Path) {
        self.overlay.borrow_mut().remove(&canon(path));
    }
}

impl Host for FsHost {
    /// Relative to `base` and, when there is a project `root`, required to be
    /// inside it after resolving symlinks.
    fn resolve(&self, base: Option<&str>, root: Option<&str>, spec: &str) -> Result<String, String> {
        let full = match base {
            Some(b) => Path::new(b).join(spec),
            None => PathBuf::from(spec),
        };
        let Some(root) = root else {
            return Ok(name(&canon(&full)));
        };
        let canon = full.canonicalize().map_err(|e| format!("cannot read `{spec}`: {e}"))?;
        let root = Path::new(root)
            .canonicalize()
            .map_err(|e| format!("project root `{root}`: {e}"))?;
        if canon.starts_with(&root) {
            Ok(name(&canon))
        } else {
            Err(format!(
                "`{spec}` is outside the project root `{}`; imports must stay inside it (see --root)",
                root.display()
            ))
        }
    }

    fn stamp(&self, file: &str) -> Stamp {
        let p = canon(Path::new(file));
        if let Some((v, _)) = self.overlay.borrow().get(&p) {
            return Stamp::Version(BUFFER | v);
        }
        match std::fs::metadata(&p).and_then(|m| m.modified()) {
            Ok(t) => Stamp::Version(t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64) & !BUFFER),
            Err(_) => Stamp::Missing,
        }
    }

    /// The editor buffer if one is open, else the file on disk.
    fn read(&self, file: &str) -> Result<String, String> {
        let path = Path::new(file);
        if let Some((_, text)) = self.overlay.borrow().get(&canon(path)) {
            return Ok(text.clone());
        }
        std::fs::read_to_string(path).map_err(|e| format!("cannot read `{file}`: {e}"))
    }

    fn parse_yaml(&self, text: &str) -> Result<Vec<Value>, String> {
        yaml::parse_docs(text)
    }

    /// Saved in the temporary directory, so editors can open it (go to definition).
    fn generated(&self, schema: &str, source: &str) -> Option<String> {
        let name: String = name(&canon(Path::new(schema)))
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
        Some(file.to_string_lossy().into_owned())
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

/// Inputs from a JSON or YAML file holding an object of input values.
pub fn read_inputs_file(path: &Path) -> Result<Vec<(String, InputValue)>, String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {shown}: {e}"))?;
    let is_yaml = matches!(path.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"));
    let value = if is_yaml {
        match yaml::parse_docs(&text).map_err(|e| format!("{shown}: {e}"))?.as_slice() {
            [v] => v.clone(),
            _ => return Err(format!("{shown}: expected exactly one YAML document")),
        }
    } else {
        json::parse(&text).map_err(|e| format!("{shown}: {e}"))?
    };
    cleat_core::inputs_from_value(value).map_err(|e| format!("{shown}: {e}"))
}

/// The inputs file that goes with an entry file: `app.inputs.json` or `app.inputs.yaml` next to `app.cleat`.
pub fn inputs_file_for(file: &Path) -> Option<PathBuf> {
    let stem = file.file_stem()?.to_str()?;
    ["json", "yaml", "yml"]
        .iter()
        .map(|ext| file.with_file_name(format!("{stem}.inputs.{ext}")))
        .find(|p| p.exists())
}
