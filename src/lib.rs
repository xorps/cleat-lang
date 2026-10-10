//! A typed configuration language whose values are JSON. See `SPEC.md`.

pub mod ast;
pub mod check;
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
    match parser::parse_program(src) {
        Ok(items) => check::check_program(&items, base),
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
    let items = match parser::parse_program(src) {
        Ok(items) => items,
        Err(e) => return Err(ExportError::Check(Outcome { errors: vec![e], ..Default::default() })),
    };
    let outcome = check::check_program(&items, base);
    if !outcome.errors.is_empty() {
        return Err(ExportError::Check(outcome));
    }
    let comps = eval::run(&items, outcome.ctx.clone(), inputs).map_err(ExportError::Eval)?;
    Ok((outcome, comps))
}
