use crate::diag::{Diag, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// Tokens of an `\( ... )` interpolation, terminated by `Eof`.
    Interp(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(Vec<StrPart>),
    // keywords
    Type,
    Import,
    Input,
    Let,
    Fn,
    Export,
    If,
    Then,
    Else,
    Match,
    For,
    In,
    Where,
    True,
    False,
    Null,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semi,
    Dot,
    DotDot,
    Ellipsis,
    QDot,
    QQ,
    Question,
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

const KEYWORDS: [(&str, Tok); 16] = [
    ("type", Tok::Type),
    ("import", Tok::Import),
    ("input", Tok::Input),
    ("let", Tok::Let),
    ("fn", Tok::Fn),
    ("export", Tok::Export),
    ("if", Tok::If),
    ("then", Tok::Then),
    ("else", Tok::Else),
    ("match", Tok::Match),
    ("for", Tok::For),
    ("in", Tok::In),
    ("where", Tok::Where),
    ("true", Tok::True),
    ("false", Tok::False),
    ("null", Tok::Null),
];

impl Tok {
    /// The source text of a keyword, so keywords can be used as object keys (`type: "ClusterIP"`).
    pub fn keyword_text(&self) -> Option<&'static str> {
        KEYWORDS.iter().find(|(_, t)| t == self).map(|(s, _)| *s)
    }

    pub fn describe(&self) -> String {
        if let Some(k) = self.keyword_text() {
            return format!("`{k}`");
        }
        let s = match self {
            Tok::Ident(s) => return format!("identifier `{s}`"),
            Tok::Int(n) => return format!("`{n}`"),
            Tok::Float(x) => return format!("`{x}`"),
            Tok::Str(_) => return "string literal".to_string(),
            Tok::Eof => return "end of input".to_string(),
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::Comma => ",",
            Tok::Colon => ":",
            Tok::Semi => ";",
            Tok::Dot => ".",
            Tok::DotDot => "..",
            Tok::Ellipsis => "...",
            Tok::QDot => "?.",
            Tok::QQ => "??",
            Tok::Question => "?",
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
            _ => unreachable!("keywords handled above"),
        };
        format!("`{s}`")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn lex(src: &str) -> Result<Vec<Token>, Diag> {
    let mut lx = Lexer { cs: src.chars().collect(), i: 0, line: 1, col: 1 };
    lx.tokens(false)
}

struct Lexer {
    cs: Vec<char>,
    i: usize,
    line: u32,
    col: u32,
}

impl Lexer {
    fn peek(&self, n: usize) -> Option<char> {
        self.cs.get(self.i + n).copied()
    }

    fn span(&self) -> Span {
        Span { line: self.line, col: self.col }
    }

    fn bump(&mut self) -> char {
        let c = self.cs[self.i];
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }

    /// Lex until end of input or, inside an interpolation, the unmatched `)`.
    fn tokens(&mut self, in_interp: bool) -> Result<Vec<Token>, Diag> {
        let mut out: Vec<Token> = Vec::new();
        let mut depth = 0usize;
        loop {
            let Some(c) = self.peek(0) else {
                if in_interp {
                    return Err(Diag::new(self.span(), "unterminated `\\(` interpolation"));
                }
                break;
            };
            let span = self.span();
            if c.is_whitespace() {
                self.bump();
                continue;
            }
            if c == '/' && self.peek(1) == Some('/') {
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.bump();
                }
                continue;
            }
            if in_interp && c == ')' && depth == 0 {
                self.bump();
                break;
            }

            let tok = if c.is_ascii_alphabetic() || c == '_' {
                let mut s = String::new();
                while self.peek(0).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    s.push(self.bump());
                }
                KEYWORDS.iter().find(|(k, _)| *k == s).map(|(_, t)| t.clone()).unwrap_or(Tok::Ident(s))
            } else if c.is_ascii_digit() {
                self.number(&out, span)?
            } else if c == '"' {
                self.string(span)?
            } else {
                self.punct(span)?
            };
            match tok {
                Tok::LParen => depth += 1,
                Tok::RParen => depth = depth.saturating_sub(1),
                _ => {}
            }
            out.push(Token { tok, span });
        }
        out.push(Token { tok: Tok::Eof, span: self.span() });
        Ok(out)
    }

    fn number(&mut self, out: &[Token], span: Span) -> Result<Tok, Diag> {
        let mut s = String::new();
        while self.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '_') {
            let c = self.bump();
            if c != '_' {
                s.push(c);
            }
        }
        // After `.`, digits are a tuple index: `x.0.1` is not `x.(0.1)`.
        let after_dot = matches!(out.last(), Some(Token { tok: Tok::Dot | Tok::QDot, .. }));
        let mut is_float = false;
        if !after_dot && self.peek(0) == Some('.') && self.peek(1).is_some_and(|d| d.is_ascii_digit()) {
            is_float = true;
            s.push(self.bump());
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                s.push(self.bump());
            }
        }
        if !after_dot && matches!(self.peek(0), Some('e' | 'E')) {
            let sign = matches!(self.peek(1), Some('+' | '-'));
            if self.peek(if sign { 2 } else { 1 }).is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                s.push(self.bump());
                if sign {
                    s.push(self.bump());
                }
                while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    s.push(self.bump());
                }
            }
        }
        if is_float {
            s.parse().map(Tok::Float).map_err(|_| Diag::new(span, "invalid float literal"))
        } else {
            s.parse().map(Tok::Int).map_err(|_| Diag::new(span, "integer literal out of range"))
        }
    }

    fn string(&mut self, span: Span) -> Result<Tok, Diag> {
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut cur = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => return Err(Diag::new(span, "unterminated string literal")),
                Some('"') => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    let esc_span = self.span();
                    self.bump();
                    let Some(e) = self.peek(0) else {
                        return Err(Diag::new(span, "unterminated string literal"));
                    };
                    self.bump();
                    match e {
                        'n' => cur.push('\n'),
                        't' => cur.push('\t'),
                        'r' => cur.push('\r'),
                        '"' => cur.push('"'),
                        '\\' => cur.push('\\'),
                        '(' => {
                            if !cur.is_empty() {
                                parts.push(StrPart::Lit(std::mem::take(&mut cur)));
                            }
                            parts.push(StrPart::Interp(self.tokens(true)?));
                        }
                        _ => return Err(Diag::new(esc_span, format!("invalid escape `\\{e}` in string literal"))),
                    }
                }
                Some(_) => cur.push(self.bump()),
            }
        }
        if !cur.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(cur));
        }
        Ok(Tok::Str(parts))
    }

    fn punct(&mut self, span: Span) -> Result<Tok, Diag> {
        let c = self.peek(0).unwrap();
        let n1 = self.peek(1).unwrap_or('\0');
        let n2 = self.peek(2).unwrap_or('\0');
        let (tok, n) = match (c, n1, n2) {
            ('.', '.', '.') => (Tok::Ellipsis, 3),
            ('.', '.', _) => (Tok::DotDot, 2),
            ('?', '.', _) => (Tok::QDot, 2),
            ('?', '?', _) => (Tok::QQ, 2),
            ('-', '>', _) => (Tok::Arrow, 2),
            ('=', '>', _) => (Tok::FatArrow, 2),
            ('=', '=', _) => (Tok::EqEq, 2),
            ('!', '=', _) => (Tok::Ne, 2),
            ('<', '=', _) => (Tok::Le, 2),
            ('>', '=', _) => (Tok::Ge, 2),
            ('&', '&', _) => (Tok::AndAnd, 2),
            ('|', '|', _) => (Tok::OrOr, 2),
            ('(', _, _) => (Tok::LParen, 1),
            (')', _, _) => (Tok::RParen, 1),
            ('{', _, _) => (Tok::LBrace, 1),
            ('}', _, _) => (Tok::RBrace, 1),
            ('[', _, _) => (Tok::LBracket, 1),
            (']', _, _) => (Tok::RBracket, 1),
            (',', _, _) => (Tok::Comma, 1),
            (':', _, _) => (Tok::Colon, 1),
            (';', _, _) => (Tok::Semi, 1),
            ('.', _, _) => (Tok::Dot, 1),
            ('?', _, _) => (Tok::Question, 1),
            ('=', _, _) => (Tok::Eq, 1),
            ('<', _, _) => (Tok::Lt, 1),
            ('>', _, _) => (Tok::Gt, 1),
            ('+', _, _) => (Tok::Plus, 1),
            ('-', _, _) => (Tok::Minus, 1),
            ('*', _, _) => (Tok::Star, 1),
            ('/', _, _) => (Tok::Slash, 1),
            ('%', _, _) => (Tok::Percent, 1),
            ('!', _, _) => (Tok::Bang, 1),
            ('|', _, _) => (Tok::Pipe, 1),
            _ => return Err(Diag::new(span, format!("unexpected character `{c}`"))),
        };
        for _ in 0..n {
            self.bump();
        }
        Ok(tok)
    }
}
