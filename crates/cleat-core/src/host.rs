//! The core does no I/O. Everything outside the program text (reading
//! imported files, telling whether they changed, parsing YAML) goes through a
//! [`Host`] supplied by the application: the CLI and language server use the
//! file system, the browser playground an in-memory set of files.
//!
//! A [`Session`] pairs a host with the caches that used to be global: loaded
//! and checked modules, compiled regexes, and the dependency recordings that
//! tell when a cached result is stale. Keep one session for as long as its
//! caches are useful (the language server keeps one for its lifetime).

use core::cell::RefCell;

use regex_automata::meta::Regex;

use crate::check::CheckedModule;
use crate::eval::Value;
use crate::modules::Module;
use crate::prelude::*;

/// Files are named by host-defined strings, with `/` (or `\`) between path
/// segments. Names returned by [`Host::resolve`] must be canonical: one file,
/// one name.
pub trait Host {
    /// The file that `spec` (an import path) names, written in a file in
    /// directory `base` (`None`: the host's default directory). With a project
    /// `root`, the file must be inside it (after resolving any links); imports
    /// that escape are errors.
    fn resolve(&self, base: Option<&str>, root: Option<&str>, spec: &str) -> Result<String, String>;

    /// What a file's contents currently are, so caches can tell when it changes.
    fn stamp(&self, file: &str) -> Stamp;

    /// A file's text.
    fn read(&self, file: &str) -> Result<String, String>;

    /// All documents of a YAML stream, as JSON values.
    fn parse_yaml(&self, _text: &str) -> Result<Vec<Value>, String> {
        Err("YAML is not supported here".into())
    }

    /// Somewhere an editor can open the Cleat source generated from a schema
    /// (for go to definition), if the host can provide one.
    fn generated(&self, _schema: &str, _source: &str) -> Option<String> {
        None
    }
}

/// A version of a file's contents. Equal stamps mean unchanged contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    Missing,
    Version(u64),
}

/// Checked modules by file and project root.
type CheckedCache = BTreeMap<(String, Option<String>), Rc<CheckedModule>>;

/// A host and the caches that live as long as it does.
#[derive(Clone)]
pub struct Session(Rc<Inner>);

struct Inner {
    host: Rc<dyn Host>,
    modules: RefCell<BTreeMap<String, (Stamp, Rc<Module>)>>,
    checked: RefCell<CheckedCache>,
    /// Modules being checked, innermost last (for import cycle detection).
    checking: RefCell<Vec<String>>,
    /// Active dependency recordings (nested: a module check inside a check).
    recorders: RefCell<Vec<Vec<(String, Stamp)>>>,
    regexes: RefCell<BTreeMap<String, Rc<Regex>>>,
}

impl Session {
    pub fn new(host: Rc<dyn Host>) -> Session {
        Session(Rc::new(Inner {
            host,
            modules: Default::default(),
            checked: Default::default(),
            checking: Default::default(),
            recorders: Default::default(),
            regexes: Default::default(),
        }))
    }

    pub fn host(&self) -> &dyn Host {
        &*self.0.host
    }

    /// Note that the current computation read `file`.
    pub fn record(&self, file: &str, stamp: Stamp) {
        for deps in self.0.recorders.borrow_mut().iter_mut() {
            deps.push((file.to_string(), stamp));
        }
    }

    /// Run `f` and return the files it read (directly or through imports).
    pub fn recording<T>(&self, f: impl FnOnce() -> T) -> (T, Vec<(String, Stamp)>) {
        self.0.recorders.borrow_mut().push(Vec::new());
        let out = f();
        let deps = self.0.recorders.borrow_mut().pop().unwrap_or_default();
        (out, deps)
    }

    /// Whether all recorded dependencies are unchanged.
    pub fn fresh(&self, deps: &[(String, Stamp)]) -> bool {
        deps.iter().all(|(f, s)| self.host().stamp(f) == *s)
    }

    pub(crate) fn cached_module(&self, file: &str, stamp: Stamp) -> Option<Rc<Module>> {
        match self.0.modules.borrow().get(file) {
            Some((s, m)) if *s == stamp => Some(m.clone()),
            _ => None,
        }
    }

    pub(crate) fn cache_module(&self, file: &str, stamp: Stamp, module: Rc<Module>) {
        self.0.modules.borrow_mut().insert(file.to_string(), (stamp, module));
    }

    pub(crate) fn cached_check(&self, key: &(String, Option<String>)) -> Option<Rc<CheckedModule>> {
        self.0.checked.borrow().get(key).cloned()
    }

    pub(crate) fn cache_check(&self, key: (String, Option<String>), checked: Rc<CheckedModule>) {
        self.0.checked.borrow_mut().insert(key, checked);
    }

    pub(crate) fn checking(&self) -> &RefCell<Vec<String>> {
        &self.0.checking
    }

    /// A compiled regex, cached, since refinements from schemas run `matches` for every value.
    pub(crate) fn regex(&self, re: &str) -> Result<Rc<Regex>, String> {
        if let Some(r) = self.0.regexes.borrow().get(re) {
            return Ok(r.clone());
        }
        let r = Rc::new(compile_regex(re)?);
        self.0.regexes.borrow_mut().insert(re.to_string(), r.clone());
        Ok(r)
    }
}

pub(crate) fn compile_regex(re: &str) -> Result<Regex, String> {
    Regex::new(re).map_err(|e| format!("{e}"))
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// The directory part of a file name (`/a/b.cleat` -> `/a`).
pub fn dir_of(file: &str) -> Option<String> {
    let i = file.rfind(is_sep)?;
    Some(if i == 0 {
        file[..1].to_string()
    } else {
        file[..i].to_string()
    })
}

/// The last segment of a file name (`/a/b.cleat` -> `b.cleat`).
pub fn file_name(file: &str) -> &str {
    file.rsplit(is_sep).next().unwrap_or(file)
}

/// A file name's extension, without the dot.
pub fn extension(file: &str) -> Option<&str> {
    let name = file_name(file);
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext)
}

/// Files held in memory, for the browser playground and tests. Names are
/// absolute (`/main.cleat`); `.` and `..` are resolved lexically.
#[derive(Default)]
pub struct MemHost {
    files: RefCell<BTreeMap<String, (u64, String)>>,
    version: core::cell::Cell<u64>,
}

impl MemHost {
    pub fn new() -> MemHost {
        MemHost::default()
    }

    /// Add or replace a file. `name` is normalized (`main.cleat` -> `/main.cleat`).
    pub fn set(&self, name: &str, text: impl Into<String>) {
        let v = self.version.get() + 1;
        self.version.set(v);
        let name = normalize("/", name).unwrap_or_else(|| name.to_string());
        self.files.borrow_mut().insert(name, (v, text.into()));
    }

    pub fn remove(&self, name: &str) {
        if let Some(name) = normalize("/", name) {
            self.files.borrow_mut().remove(&name);
        }
    }
}

/// `spec` relative to directory `base`, with `.` and `..` resolved; `None` if it climbs above `/`.
fn normalize(base: &str, spec: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let joined = if spec.starts_with('/') {
        spec.to_string()
    } else {
        format!("{base}/{spec}")
    };
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

impl Host for MemHost {
    fn resolve(&self, base: Option<&str>, root: Option<&str>, spec: &str) -> Result<String, String> {
        let file = normalize(base.unwrap_or("/"), spec).ok_or_else(|| format!("cannot read `{spec}`: outside `/`"))?;
        if let Some(root) = root {
            let root = normalize("/", root).unwrap_or_default();
            let inside = root == "/" || file.starts_with(&format!("{root}/"));
            if !inside {
                return Err(format!(
                    "`{spec}` is outside the project root `{root}`; imports must stay inside it (see --root)"
                ));
            }
        }
        Ok(file)
    }

    fn stamp(&self, file: &str) -> Stamp {
        self.files
            .borrow()
            .get(file)
            .map_or(Stamp::Missing, |(v, _)| Stamp::Version(*v))
    }

    fn read(&self, file: &str) -> Result<String, String> {
        self.files
            .borrow()
            .get(file)
            .map(|(_, text)| text.clone())
            .ok_or_else(|| format!("cannot read `{file}`: No such file or directory"))
    }
}
