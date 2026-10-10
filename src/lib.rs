//! A typed configuration language whose values are JSON. See `SPEC.md`.

pub mod ast;
pub mod check;
pub mod diag;
pub mod emit;
pub mod eval;
pub mod json;
pub mod lexer;
pub mod parser;
pub mod targets;
pub mod types;
pub mod vc;

pub use check::Outcome;
pub use eval::{Component, EvalError, InputValue, Value};

/// Parse and type-check a program.
pub fn check_source(src: &str) -> Outcome {
    match parser::parse_program(src) {
        Ok(items) => check::check_program(&items),
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
    let items = match parser::parse_program(src) {
        Ok(items) => items,
        Err(e) => return Err(ExportError::Check(Outcome { errors: vec![e], ..Default::default() })),
    };
    let outcome = check::check_program(&items);
    if !outcome.errors.is_empty() {
        return Err(ExportError::Check(outcome));
    }
    let comps = eval::run(&items, outcome.ctx.clone(), inputs).map_err(ExportError::Eval)?;
    Ok((outcome, comps))
}
