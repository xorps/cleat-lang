//! A typed configuration language whose values are JSON. See `SPEC.md`.
//!
//! This is the language itself: lexer, parser, checker, evaluator and output.
//! It is `no_std` and does no I/O; imports are read through a [`Host`] that
//! the application provides (see `host`), so it runs anywhere `alloc` does,
//! including WebAssembly in a browser.

#![no_std]

extern crate alloc;

pub mod ast;
pub mod check;
pub mod complete;
pub mod diag;
pub mod emit;
pub mod eval;
pub mod host;
pub mod json;
pub mod lexer;
pub mod modules;
pub mod openapi;
pub mod parser;
pub mod prelude;
pub mod targets;
pub mod types;
pub mod vc;

use crate::prelude::*;

pub use check::Outcome;
pub use eval::{Component, EvalError, InputValue, Value};
pub use host::{Host, MemHost, Session, Stamp};

/// Parse and type-check a program whose imports resolve relative to
/// directory `base`, each required to stay inside `root` when there is one.
pub fn check_source(sess: &Session, src: &str, base: Option<&str>, root: Option<&str>) -> Outcome {
    match parser::parse_program(src) {
        Ok(items) => check::check_program(sess, &items, base, root),
        Err(e) => Outcome {
            errors: vec![e],
            ..Default::default()
        },
    }
}

pub enum ExportError {
    /// Static errors: the program was not evaluated.
    Check(Box<Outcome>),
    /// Errors from input decoding or evaluation.
    Eval(Vec<EvalError>),
}

/// Check, then evaluate with the given inputs (see `check_source` for `base` and `root`).
pub fn export(
    sess: &Session,
    src: &str,
    base: Option<&str>,
    root: Option<&str>,
    inputs: Vec<(String, InputValue)>,
) -> Result<(Outcome, Vec<Component>), ExportError> {
    let items = match parser::parse_program(src) {
        Ok(items) => items,
        Err(e) => {
            return Err(ExportError::Check(Box::new(Outcome {
                errors: vec![e],
                ..Default::default()
            })));
        }
    };
    let outcome = check::check_program(sess, &items, base, root);
    if !outcome.errors.is_empty() {
        return Err(ExportError::Check(Box::new(outcome)));
    }
    let comps = eval::run(sess, &items, outcome.ctx.clone(), base, root, inputs).map_err(ExportError::Eval)?;
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
pub fn inputs(
    sess: &Session,
    src: &str,
    base: Option<&str>,
    root: Option<&str>,
) -> Result<Vec<InputInfo>, Box<Outcome>> {
    let items = parser::parse_program(src).map_err(|e| {
        Box::new(Outcome {
            errors: vec![e],
            ..Default::default()
        })
    })?;
    let outcome = check::check_program(sess, &items, base, root);
    if !outcome.errors.is_empty() {
        return Err(Box::new(outcome));
    }
    Ok(input_infos(&items, &outcome))
}

/// The inputs declared by a checked program.
pub fn input_infos(items: &[ast::Item], outcome: &Outcome) -> Vec<InputInfo> {
    let mut ctx = outcome.ctx.clone();
    let mut out = Vec::new();
    for item in items {
        let ast::Item::Input { name, ty, default, .. } = item else {
            continue;
        };
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
    out
}

/// Inputs from a parsed inputs file (an object of input values).
pub fn inputs_from_value(value: Value) -> Result<Vec<(String, InputValue)>, String> {
    match value {
        Value::Obj(es) => Ok(es.into_iter().map(|(k, v)| (k, InputValue::Json(v))).collect()),
        _ => Err("inputs must be an object".into()),
    }
}
