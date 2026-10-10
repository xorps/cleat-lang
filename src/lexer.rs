use crate::diag::{Diag, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    // keywords
    Type,
    Nominal,
    Enum,
    Fn,
    Let,
    If,
    Else,
    Match,
    Where,
    True,
    False,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Semi,
    Dot,
    DotDot,
    Arrow,
    FatArrow,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    AndAnd,
    OrOr,
    Pipe,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        let s = match self {
            Tok::Ident(s) => return format!("identifier `{s}`"),
            Tok::Int(n) => return format!("`{n}`"),
            Tok::Float(x) => return format!("`{x}`"),
            Tok::Str(s) => return format!("string {s:?}"),
            Tok::Eof => return "end of file".to_string(),
            Tok::Type => "type",
            Tok::Nominal => "nominal",
            Tok::Enum => "enum",
            Tok::Fn => "fn",
            Tok::Let => "let",
            Tok::If => "if",
            Tok::Else => "else",
            Tok::Match => "match",
            Tok::Where => "where",
            Tok::True => "true",
            Tok::False => "false",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::Comma => ",",
            Tok::Colon => ":",
            Tok::Semi => ";",
            Tok::Dot => ".",
            Tok::DotDot => "..",
            Tok::Arrow => "->",
            Tok::FatArrow => "=>",
            Tok::Eq => "=",
            Tok::EqEq => "==",
            Tok::Ne => "!=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Bang => "!",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::Pipe => "|",
        };
        format!("`{s}`")
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn lex(src: &str) -> Result<Vec<Token>, Diag> {
    let cs: Vec<char> = src.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);

    while i < cs.len() {
        let c = cs[i];
        let span = Span { line, col };
        if c == '\n' {
            i += 1;
            line += 1;
            col = 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            col += 1;
            continue;
        }
        if c == '/' && cs.get(i + 1) == Some(&'/') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
            continue;
        }

        let start = i;
        let tok = if c.is_ascii_alphabetic() || c == '_' {
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            let s: String = cs[start..i].iter().collect();
            match s.as_str() {
                "type" => Tok::Type,
                "nominal" => Tok::Nominal,
                "enum" => Tok::Enum,
                "fn" => Tok::Fn,
                "let" => Tok::Let,
                "if" => Tok::If,
                "else" => Tok::Else,
                "match" => Tok::Match,
                "where" => Tok::Where,
                "true" => Tok::True,
                "false" => Tok::False,
                _ => Tok::Ident(s),
            }
        } else if c.is_ascii_digit() {
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
            // After `.`, digits are a tuple index: `x.0.1` is not `x.(0.1)`.
            let after_dot = matches!(out.last(), Some(Token { tok: Tok::Dot, .. }));
            if !after_dot && cs.get(i) == Some(&'.') && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                i += 1;
                while i < cs.len() && cs[i].is_ascii_digit() {
                    i += 1;
                }
                let s: String = cs[start..i].iter().collect();
                Tok::Float(s.parse().map_err(|_| Diag::new(span, "invalid float literal"))?)
            } else {
                let s: String = cs[start..i].iter().collect();
                Tok::Int(s.parse().map_err(|_| Diag::new(span, "integer literal out of range"))?)
            }
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            loop {
                match cs.get(i) {
                    None | Some('\n') => return Err(Diag::new(span, "unterminated string literal")),
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some('\\') => {
                        let esc = match cs.get(i + 1) {
                            Some('n') => '\n',
                            Some('t') => '\t',
                            Some('"') => '"',
                            Some('\\') => '\\',
                            _ => return Err(Diag::new(span, "invalid escape in string literal")),
                        };
                        s.push(esc);
                        i += 2;
                    }
                    Some(ch) => {
                        s.push(*ch);
                        i += 1;
                    }
                }
            }
            Tok::Str(s)
        } else {
            let next = cs.get(i + 1).copied().unwrap_or('\0');
            let (tok, n) = match (c, next) {
                ('-', '>') => (Tok::Arrow, 2),
                ('=', '>') => (Tok::FatArrow, 2),
                ('=', '=') => (Tok::EqEq, 2),
                ('!', '=') => (Tok::Ne, 2),
                ('<', '=') => (Tok::Le, 2),
                ('>', '=') => (Tok::Ge, 2),
                ('&', '&') => (Tok::AndAnd, 2),
                ('|', '|') => (Tok::OrOr, 2),
                ('.', '.') => (Tok::DotDot, 2),
                ('(', _) => (Tok::LParen, 1),
                (')', _) => (Tok::RParen, 1),
                ('{', _) => (Tok::LBrace, 1),
                ('}', _) => (Tok::RBrace, 1),
                (',', _) => (Tok::Comma, 1),
                (':', _) => (Tok::Colon, 1),
                (';', _) => (Tok::Semi, 1),
                ('.', _) => (Tok::Dot, 1),
                ('=', _) => (Tok::Eq, 1),
                ('<', _) => (Tok::Lt, 1),
                ('>', _) => (Tok::Gt, 1),
                ('+', _) => (Tok::Plus, 1),
                ('-', _) => (Tok::Minus, 1),
                ('*', _) => (Tok::Star, 1),
                ('/', _) => (Tok::Slash, 1),
                ('%', _) => (Tok::Percent, 1),
                ('!', _) => (Tok::Bang, 1),
                ('|', _) => (Tok::Pipe, 1),
                _ => return Err(Diag::new(span, format!("unexpected character `{c}`"))),
            };
            i += n;
            tok
        };
        col += (i - start) as u32;
        out.push(Token { tok, span });
    }
    out.push(Token { tok: Tok::Eof, span: Span { line, col } });
    Ok(out)
}
