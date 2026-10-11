//! A host that reads files as they are at a git revision, for comparing a
//! program's output there with the working tree (`cleat diff`, `cleat view
//! --diff`).
//!
//! Files git ignores (downloaded schemas, say) aren't in any revision, so
//! those are read from the working tree. Any other file that doesn't exist at
//! the revision, including new files not yet committed, is missing there, as
//! it should be.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use cleat_core::{Host, Stamp, Value};

pub struct GitHost {
    /// The repository's top level, with symlinks resolved.
    top: PathBuf,
    /// The commit `rev` names.
    commit: String,
    /// Paths (relative to `top`) of files at that commit.
    at_rev: HashSet<String>,
    /// Whether git ignores a path, as asked so far.
    ignored: RefCell<HashMap<String, bool>>,
    cache: RefCell<HashMap<String, String>>,
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
}

fn paths(z: Vec<u8>) -> HashSet<String> {
    z.split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// `path` with `.` and `..` resolved without touching the file system.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

impl GitHost {
    /// The repository containing `dir`, at revision `rev` (a branch, tag or commit).
    pub fn new(dir: &Path, rev: &str) -> Result<GitHost, String> {
        let top = String::from_utf8_lossy(&git(dir, &["rev-parse", "--show-toplevel"])?)
            .trim()
            .to_string();
        let top = PathBuf::from(top).canonicalize().map_err(|e| e.to_string())?;
        let commit = git(
            &top,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{rev}^{{commit}}"),
            ],
        )
        .map_err(|_| format!("`{rev}` isn't a branch, tag or commit in {}", top.display()))?;
        let commit = String::from_utf8_lossy(&commit).trim().to_string();
        let at_rev = paths(git(&top, &["ls-tree", "-r", "-z", "--name-only", &commit])?);
        Ok(GitHost {
            top,
            commit,
            at_rev,
            ignored: RefCell::new(HashMap::new()),
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// The commit being read, abbreviated.
    pub fn short_commit(&self) -> &str {
        &self.commit[..self.commit.len().min(10)]
    }

    /// `file` as a path inside the repository, if it is inside it.
    fn rel(&self, file: &str) -> Option<String> {
        let rel = lexical(Path::new(file)).strip_prefix(&self.top).ok()?.to_path_buf();
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// Ignored files (not in any revision) are read from the working tree.
    fn read_from_working_tree(&self, rel: &str) -> bool {
        if self.at_rev.contains(rel) {
            return false;
        }
        if let Some(&ignored) = self.ignored.borrow().get(rel) {
            return ignored;
        }
        // `--no-index`: ignored by the rules, even if someone force-added it.
        let ignored = Command::new("git")
            .arg("-C")
            .arg(&self.top)
            .args(["check-ignore", "-q", "--no-index", "--", rel])
            .status()
            .is_ok_and(|s| s.success());
        self.ignored.borrow_mut().insert(rel.to_string(), ignored);
        ignored
    }
}

impl Host for GitHost {
    fn resolve(&self, base: Option<&str>, root: Option<&str>, spec: &str) -> Result<String, String> {
        let full = match base {
            Some(b) => Path::new(b).join(spec),
            None => PathBuf::from(spec),
        };
        let full = lexical(&full);
        if let Some(root) = root {
            let root = Path::new(root).canonicalize().unwrap_or_else(|_| PathBuf::from(root));
            if !full.starts_with(&root) {
                return Err(format!(
                    "`{spec}` is outside the project root `{}`; imports must stay inside it (see --root)",
                    root.display()
                ));
            }
        }
        Ok(full.to_string_lossy().into_owned())
    }

    fn stamp(&self, file: &str) -> Stamp {
        // A revision never changes, so one version per file; working-tree files keep theirs.
        match self.rel(file) {
            Some(rel) if self.at_rev.contains(&rel) => Stamp::Version(1),
            Some(rel) if self.read_from_working_tree(&rel) && Path::new(file).exists() => Stamp::Version(1),
            _ => Stamp::Missing,
        }
    }

    fn read(&self, file: &str) -> Result<String, String> {
        let rel = self
            .rel(file)
            .ok_or_else(|| format!("cannot read `{file}`: outside the repository"))?;
        if let Some(text) = self.cache.borrow().get(&rel) {
            return Ok(text.clone());
        }
        let text = if self.at_rev.contains(&rel) {
            let bytes = git(&self.top, &["cat-file", "blob", &format!("{}:{rel}", self.commit)])?;
            String::from_utf8(bytes).map_err(|_| format!("`{file}` is not UTF-8 text"))?
        } else if self.read_from_working_tree(&rel) {
            std::fs::read_to_string(file).map_err(|e| format!("cannot read `{file}`: {e}"))?
        } else {
            return Err(format!("cannot read `{file}`: not in commit {}", self.short_commit()));
        };
        self.cache.borrow_mut().insert(rel, text.clone());
        Ok(text)
    }

    fn parse_yaml(&self, text: &str) -> Result<Vec<Value>, String> {
        crate::yaml::parse_docs(text)
    }
}
