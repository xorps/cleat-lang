//! Prototype checker for the language described in `SPEC.md`.

pub mod ast;
pub mod check;
pub mod diag;
pub mod lexer;
pub mod parser;
pub mod types;
pub mod vc;

pub use check::Outcome;

/// Parse and type-check a whole program.
pub fn check_source(src: &str) -> Outcome {
    match parser::parse_program(src) {
        Ok(decls) => check::check_program(&decls),
        Err(e) => Outcome { errors: vec![e], ..Default::default() },
    }
}
