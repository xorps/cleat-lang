//! Cleat on a real file system: the `cleat` CLI, the language server and the
//! Argo CD plugin are built on this. The language itself is `cleat-core`
//! (re-exported here); this crate supplies its I/O through [`fs::FsHost`].
//!
//! The functions here use a session per thread, so caches of loaded and
//! checked modules last across calls (the language server relies on that).

pub mod fs;
pub mod lsp;
pub mod yaml;

use std::path::Path;
use std::rc::Rc;

pub use cleat_core::{Component, EvalError, ExportError, InputInfo, InputValue, Outcome, Session, Value};
pub use cleat_core::{ast, check, diag, emit, eval, host, json, lexer, openapi, parser, targets, types, vc};
pub use fs::{find_root, inputs_file_for, read_inputs_file};

thread_local! {
    static SESSION: (Rc<fs::FsHost>, Session) = {
        let host = Rc::new(fs::FsHost::default());
        (host.clone(), Session::new(host))
    };
}

/// This thread's file-system session.
pub fn session() -> Session {
    SESSION.with(|(_, s)| s.clone())
}

/// This thread's file-system host (for the editor-buffer overlay).
pub fn fs_host() -> Rc<fs::FsHost> {
    SESSION.with(|(h, _)| h.clone())
}

fn name(p: Option<&Path>) -> Option<String> {
    p.map(|p| p.to_string_lossy().into_owned())
}

/// Parse and type-check a program; imports resolve relative to the current directory.
pub fn check_source(src: &str) -> Outcome {
    check_source_at(src, None)
}

/// Parse and type-check a program whose imports resolve relative to `base`.
pub fn check_source_at(src: &str, base: Option<&Path>) -> Outcome {
    check_source_in(src, base, None)
}

/// Like `check_source_at`, with every import required to stay inside `root`.
pub fn check_source_in(src: &str, base: Option<&Path>, root: Option<&Path>) -> Outcome {
    cleat_core::check_source(&session(), src, name(base).as_deref(), name(root).as_deref())
}

/// Check, then evaluate with the given inputs.
pub fn export(src: &str, inputs: Vec<(String, InputValue)>) -> Result<(Outcome, Vec<Component>), ExportError> {
    export_at(src, None, inputs)
}

pub fn export_at(
    src: &str,
    base: Option<&Path>,
    inputs: Vec<(String, InputValue)>,
) -> Result<(Outcome, Vec<Component>), ExportError> {
    export_in(src, base, None, inputs)
}

/// Like `export_at`, with every import required to stay inside `root`.
pub fn export_in(
    src: &str,
    base: Option<&Path>,
    root: Option<&Path>,
    inputs: Vec<(String, InputValue)>,
) -> Result<(Outcome, Vec<Component>), ExportError> {
    cleat_core::export(&session(), src, name(base).as_deref(), name(root).as_deref(), inputs)
}

/// The program's inputs, or the check errors if it doesn't check.
pub fn inputs(src: &str, base: Option<&Path>, root: Option<&Path>) -> Result<Vec<InputInfo>, Box<Outcome>> {
    cleat_core::inputs(&session(), src, name(base).as_deref(), name(root).as_deref())
}

/// The Cleat source a schema file is converted to (for `cleat gen`).
pub fn generate_source(path: &Path) -> Result<String, String> {
    cleat_core::modules::generate_source(&session(), &path.to_string_lossy())
}

pub mod complete {
    //! Editor completion and go to definition, on the file system.

    use super::{name, session};
    use std::path::{Path, PathBuf};

    pub use cleat_core::complete::Completions;
    use cleat_core::diag::Span;

    /// Completions at a 0-based (line, character) position, counting characters.
    pub fn complete(
        src: &str,
        line: usize,
        col: usize,
        base: Option<&Path>,
        root: Option<&Path>,
    ) -> Option<Completions> {
        cleat_core::complete::complete(&session(), src, line, col, name(base).as_deref(), name(root).as_deref())
    }

    /// Where the name at a 0-based (line, character) position is defined:
    /// `(None, span)` in this file, or `(Some(file), span)` in another.
    pub fn definition(
        src: &str,
        line: usize,
        col: usize,
        base: Option<&Path>,
        root: Option<&Path>,
    ) -> Option<(Option<PathBuf>, Span)> {
        let (file, span) =
            cleat_core::complete::definition(&session(), src, line, col, name(base).as_deref(), name(root).as_deref())?;
        Some((file.map(PathBuf::from), span))
    }
}
