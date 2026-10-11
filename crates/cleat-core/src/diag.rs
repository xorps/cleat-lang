use crate::prelude::*;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub span: Span,
    pub msg: String,
}

impl Diag {
    pub fn new(span: Span, msg: impl Into<String>) -> Self {
        Diag { span, msg: msg.into() }
    }
}

/// Length in characters of the token starting at `span` in `text`, so
/// editors underline a whole word or string rather than one character.
pub fn token_len(text: &str, span: Span) -> usize {
    let Some(line) = text.lines().nth(span.line.saturating_sub(1) as usize) else {
        return 1;
    };
    let rest: Vec<char> = line.chars().skip(span.col.saturating_sub(1) as usize).collect();
    match rest.first() {
        Some(c) if c.is_alphanumeric() || *c == '_' => rest
            .iter()
            .take_while(|c| c.is_alphanumeric() || **c == '_' || **c == '.')
            .count(),
        Some('"') => rest
            .iter()
            .skip(1)
            .position(|c| *c == '"')
            .map_or(rest.len(), |i| i + 2),
        Some(_) => 1,
        None => 1,
    }
}
