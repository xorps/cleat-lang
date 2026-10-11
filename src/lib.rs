//! A typed configuration language whose values are JSON. See `SPEC.md`.

pub mod ast;
pub mod check;
pub mod complete;
pub mod diag;
pub mod emit;
pub mod eval;
pub mod json;
pub mod lexer;
pub mod lsp;
pub mod modules;
pub mod openapi;
pub mod parser;
pub mod targets;
pub mod types;
pub mod vc;
pub mod yaml;

use std::path::Path;

pub use check::Outcome;
pub use eval::{Component, EvalError, InputValue, Value};

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
    match parser::parse_program(src) {
        Ok(items) => check::check_program_in(&items, base, root),
        Err(e) => Outcome { errors: vec![e], ..Default::default() },
    }
}

pub enum ExportError {
    /// Static errors: the program was not evaluated.
    Check(Outcome),
    /// Errors from input decoding or evaluation.
    Eval(Vec<EvalError>),
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
    let items = match parser::parse_program(src) {
        Ok(items) => items,
        Err(e) => return Err(ExportError::Check(Outcome { errors: vec![e], ..Default::default() })),
    };
    let outcome = check::check_program_in(&items, base, root);
    if !outcome.errors.is_empty() {
        return Err(ExportError::Check(outcome));
    }
    let comps = eval::run(&items, outcome.ctx.clone(), base, root, inputs).map_err(ExportError::Eval)?;
    Ok((outcome, comps))
}

/// An `input` declaration, as announced to tools such as the Argo CD UI.
#[derive(Debug, Clone)]
pub struct InputInfo {
    pub name: String,
    /// The declared type, as written in messages (`Env`, `Int where self >= 1`).
    pub ty: String,
    /// The type's definition when the type is a name (`"dev" | "prod"` for `Env`).
    pub definition: Option<String>,
    /// `string`, `array` (lists and tuples) or `map` (objects).
    pub collection: &'static str,
    /// The default, when it can be computed without evaluating the program.
    pub default: Option<Value>,
    pub required: bool,
}

/// The program's inputs, or the check errors if it doesn't check.
pub fn inputs(src: &str, base: Option<&Path>, root: Option<&Path>) -> Result<Vec<InputInfo>, Outcome> {
    let items = parser::parse_program(src).map_err(|e| Outcome { errors: vec![e], ..Default::default() })?;
    let outcome = check::check_program_in(&items, base, root);
    if !outcome.errors.is_empty() {
        return Err(outcome);
    }
    let mut ctx = outcome.ctx.clone();
    let mut out = Vec::new();
    for item in &items {
        let ast::Item::Input { name, ty, default, .. } = item else { continue };
        let t = ctx.resolve(ty);
        let definition = match &t {
            types::Ty::Alias(_) => Some(ctx.unfold(&t).to_string()),
            _ => None,
        };
        let collection = match ctx.shape(&t) {
            types::Ty::List(_) | types::Ty::Tuple(_) => "array",
            types::Ty::Map(_) | types::Ty::Record(_) => "map",
            _ => "string",
        };
        out.push(InputInfo {
            name: name.clone(),
            ty: t.to_string(),
            definition,
            collection,
            default: default.as_ref().and_then(eval::eval_closed),
            required: default.is_none(),
        });
    }
    Ok(out)
}
