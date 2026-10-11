//! Editor completion and go-to-definition.
//!
//! Completion works on unfinished code: the text before the cursor gets a
//! placeholder where the partial word is, open brackets and strings are
//! closed, and the result is type-checked. The checker records what fits
//! where it meets the placeholder (`check::COMPLETE_NAME` / `COMPLETE_VALUE`).

use crate::host::{Session, extension};
use crate::prelude::*;

use crate::ast::{ImportForm, Item};
use crate::check::{COMPLETE_NAME, COMPLETE_VALUE, Completion};
use crate::diag::Span;
use crate::{check_source, parser};

pub struct Completions {
    pub items: Vec<Completion>,
    /// Characters before the cursor that the chosen item replaces (the partial word or string).
    pub replace_len: usize,
}

enum Frame {
    /// An open bracket at an offset; whether a `:` and a `=>` were seen since the last `,`
    /// (object keys vs values; `match` patterns vs arm bodies).
    Bracket {
        ch: char,
        colon: bool,
        arrow: bool,
        at: usize,
    },
    /// `\(` inside a string; `triple` for `"""` strings.
    Interp(bool),
}

struct Scan {
    frames: Vec<Frame>,
    /// The cursor is in a string that started at this offset (not counting interpolations).
    string: Option<(bool, usize)>,
    in_comment: bool,
}

fn scan(cs: &[char]) -> Scan {
    let mut frames: Vec<Frame> = Vec::new();
    let mut string: Option<(bool, usize)> = None;
    let starts = |i: usize, pat: &str| pat.chars().enumerate().all(|(j, p)| cs.get(i + j) == Some(&p));
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if let Some((triple, _)) = string {
            if c == '\\' {
                if cs.get(i + 1) == Some(&'(') {
                    frames.push(Frame::Interp(triple));
                    string = None;
                }
                i += 2;
                continue;
            }
            if triple && starts(i, "\"\"\"") {
                string = None;
                i += 3;
                continue;
            }
            if !triple && (c == '"' || c == '\n') {
                string = None;
            }
            i += 1;
            continue;
        }
        match c {
            '/' if cs.get(i + 1) == Some(&'/') => match cs[i..].iter().position(|&c| c == '\n') {
                Some(n) => i += n,
                None => {
                    return Scan {
                        frames,
                        string,
                        in_comment: true,
                    };
                }
            },
            '"' if starts(i, "\"\"\"") => {
                string = Some((true, i));
                i += 2;
            }
            '"' => string = Some((false, i)),
            '{' | '[' | '(' => frames.push(Frame::Bracket {
                ch: c,
                colon: false,
                arrow: false,
                at: i,
            }),
            '}' | ']' => {
                if matches!(frames.last(), Some(Frame::Bracket { .. })) {
                    frames.pop();
                }
            }
            ')' => match frames.pop() {
                Some(Frame::Interp(triple)) => string = Some((triple, i)),
                Some(Frame::Bracket { .. }) | None => {}
            },
            ':' => {
                if let Some(Frame::Bracket { colon, .. }) = frames.last_mut() {
                    *colon = true;
                }
            }
            '=' if cs.get(i + 1) == Some(&'>') => {
                if let Some(Frame::Bracket { arrow, .. }) = frames.last_mut() {
                    *arrow = true;
                }
            }
            ',' => {
                if let Some(Frame::Bracket { colon, arrow, .. }) = frames.last_mut() {
                    *colon = false;
                    *arrow = false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    Scan {
        frames,
        string,
        in_comment: false,
    }
}

/// Close everything that is open at the end of `probe`.
fn closers(frames: &[Frame]) -> String {
    let mut out = String::new();
    for f in frames.iter().rev() {
        match f {
            Frame::Bracket { ch: '{', .. } => out.push_str(" }"),
            Frame::Bracket { ch: '[', .. } => out.push(']'),
            Frame::Bracket { .. } => out.push(')'),
            Frame::Interp(false) => out.push_str(")\""),
            Frame::Interp(true) => out.push_str(")\n\"\"\""),
        }
    }
    out
}

/// Keys set in the rest of the object the cursor is in (`{ ‸, b: 1, c.d: 2 }` -> `b`, `c`).
fn keys_after(cs: &[char], from: usize) -> Vec<String> {
    let mut keys = Vec::new();
    let mut i = from;
    while i < cs.len() && is_ident(cs[i]) {
        i += 1; // the rest of the word under the cursor
    }
    let (mut depth, mut member_start) = (0usize, true);
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' => {
                // Skip a string (interpolations can't contain keys of this object).
                i += 1;
                while i < cs.len() && cs[i] != '"' && cs[i] != '\n' {
                    i += if cs[i] == '\\' { 2 } else { 1 };
                }
                member_start = false;
            }
            '/' if cs.get(i + 1) == Some(&'/') => {
                while i < cs.len() && cs[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' if depth == 0 => break,
            '}' | ']' | ')' => depth -= 1,
            ',' if depth == 0 => member_start = true,
            c if c.is_whitespace() => {}
            c if depth == 0 && member_start && (c.is_ascii_alphabetic() || c == '_') => {
                let start = i;
                while i < cs.len() && is_ident(cs[i]) {
                    i += 1;
                }
                let key: String = cs[start..i].iter().collect();
                let mut j = i;
                while j < cs.len() && cs[j].is_whitespace() {
                    j += 1;
                }
                if matches!(cs.get(j), Some(':' | '.')) {
                    keys.push(key);
                }
                member_start = false;
                continue;
            }
            _ => member_start = false,
        }
        i += 1;
    }
    keys
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Character offset of a 0-based (line, character) position.
fn offset(cs: &[char], line: usize, col: usize) -> usize {
    let mut l = 0;
    let mut i = 0;
    while l < line && i < cs.len() {
        if cs[i] == '\n' {
            l += 1;
        }
        i += 1;
    }
    let mut c = 0;
    while c < col && i < cs.len() && cs[i] != '\n' {
        i += 1;
        c += 1;
    }
    i
}

/// Completions at a 0-based (line, character) position, counting characters.
pub fn complete(
    sess: &Session,
    src: &str,
    line: usize,
    col: usize,
    base: Option<&str>,
    root: Option<&str>,
) -> Option<Completions> {
    let cs: Vec<char> = src.chars().collect();
    let cur = offset(&cs, line, col);
    let before = &cs[..cur];
    let sc = scan(before);
    if sc.in_comment {
        return None;
    }
    let text = |a: usize, b: usize| before[a..b].iter().collect::<String>();
    let mut set_below: Vec<String> = Vec::new();
    let (mut probe, replace_len) = if let Some((triple, start)) = sc.string {
        if triple {
            return None;
        }
        // In a string literal: offer literal values, replacing the whole string so far.
        (format!("{}{COMPLETE_VALUE}", text(0, start)), cur - start)
    } else {
        let mut w = cur;
        while w > 0 && is_ident(before[w - 1]) {
            w -= 1;
        }
        let after_dot = w > 0 && before[w - 1] == '.';
        let prev = before[..w].iter().rev().find(|c| !c.is_whitespace()).copied();
        // A word alone at the start of a line after a finished value (no comma typed yet)
        // starts a new key: `{ a: 1⏎ ‸`.
        let starts_line = before[..w]
            .iter()
            .rev()
            .take_while(|c| **c != '\n')
            .all(|c| c.is_whitespace())
            && before[..w].contains(&'\n');
        let value_ended = prev.is_some_and(|c| !"{[(,:=+-*/%<>!&|?".contains(c));
        // `match e {`: is the cursor in a pattern (no `=>` yet in this arm)?
        let in_match_pattern = match sc.frames.last() {
            Some(Frame::Bracket {
                ch: '{',
                arrow: false,
                at,
                ..
            }) => {
                let head: String = before[..*at].iter().collect();
                head.rfind("match").is_some_and(|m| {
                    let after = &head[m + 5..];
                    let boundary = m == 0 || !is_ident(head[..m].chars().last().unwrap_or(' '));
                    boundary && after.starts_with(char::is_whitespace) && !after.contains(['{', '}', ';', '\n'])
                })
            }
            _ => false,
        };
        let key_position = !in_match_pattern
            && match sc.frames.last() {
                Some(Frame::Bracket {
                    ch: '{', colon: false, ..
                }) => true,
                Some(Frame::Bracket {
                    ch: '{', colon: true, ..
                }) => starts_line && value_ended && !after_dot,
                _ => false,
            };
        let head = text(0, w);
        if key_position && !after_dot {
            set_below = keys_after(&cs, cur);
        }
        let p = if in_match_pattern {
            // A pattern needs an arm body to parse.
            let name = if after_dot { COMPLETE_NAME } else { COMPLETE_VALUE };
            format!("{head}{name} => null")
        } else if key_position && prev == Some('-') {
            format!("{head}{COMPLETE_NAME}") // `-path` in a patch
        } else if key_position {
            let sep = if after_dot || matches!(prev, Some('{' | ',')) {
                ""
            } else {
                ", "
            };
            format!("{head}{sep}{COMPLETE_NAME}: null")
        } else if after_dot {
            format!("{head}{COMPLETE_NAME}")
        } else {
            format!("{head}{COMPLETE_VALUE}")
        };
        (p, cur - w)
    };
    probe.push_str(&closers(&sc.frames));
    // A declaration cut off before its value (`fn f(x: ‸)`, `let s: T‸`) needs one to parse.
    let mut items = match check_source(sess, &format!("{probe}\n"), base, root).completions {
        Some(items) => items,
        None => check_source(sess, &format!("{probe} = null\n"), base, root).completions?,
    };
    // The probe ends at the cursor; leave out keys the object sets further down.
    items.retain(|i| !(i.insert.ends_with(": ") && set_below.contains(&i.label)));
    items.sort_by_key(|i| !i.preferred);
    Some(Completions { items, replace_len })
}

/// Where the name at a 0-based (line, character) position is defined:
/// `(None, span)` in this file, or `(Some(file), span)` in another.
pub fn definition(
    sess: &Session,
    src: &str,
    line: usize,
    col: usize,
    base: Option<&str>,
    root: Option<&str>,
) -> Option<(Option<String>, Span)> {
    let cs: Vec<char> = src.chars().collect();
    let cur = offset(&cs, line, col);
    let items = parser::parse_program(src).ok()?;
    let resolve = |p: &str| sess.host().resolve(base, root, p).ok();

    // On an import's path: open that file.
    let line_text: String = src.lines().nth(line)?.to_string();
    if line_text.trim_start().starts_with("import") {
        let quote_spans: Vec<usize> = line_text
            .char_indices()
            .filter(|(_, c)| *c == '"')
            .map(|(i, _)| i)
            .collect();
        if let [a, b, ..] = quote_spans.as_slice()
            && (*a..=*b).contains(&col)
        {
            return Some((Some(resolve(&line_text[a + 1..*b])?), Span { line: 1, col: 1 }));
        }
    }

    // The dotted name under the cursor, up to the segment the cursor is on.
    let mut a = cur;
    while a > 0 && (is_ident(cs[a - 1]) || cs[a - 1] == '.') {
        a -= 1;
    }
    let mut b = cur;
    while b < cs.len() && is_ident(cs[b]) {
        b += 1;
    }
    let name: String = cs[a..b].iter().collect();
    let name = name.trim_matches('.');
    if name.is_empty() {
        return None;
    }
    let segs: Vec<&str> = name.split('.').collect();

    let find_in = |items: &[Item], n: &str| -> Option<Span> {
        items.iter().find_map(|it| match it {
            Item::Type { name, span, .. }
            | Item::Input { name, span, .. }
            | Item::Let { name, span, .. }
            | Item::Export {
                name: Some(name), span, ..
            } if name == n => Some(*span),
            Item::Fn(f) if f.name == n => Some(f.span),
            Item::Import {
                form: ImportForm::Default(x),
                span,
                ..
            } if x == n => Some(*span),
            _ => None,
        })
    };
    let in_module = |path: &str, n: &str| -> Option<(Option<String>, Span)> {
        let full = resolve(path)?;
        if extension(&full) != Some("cleat") {
            return None;
        }
        let module_items = parser::parse_program(&sess.host().read(&full).ok()?).ok()?;
        Some((Some(full), find_in(&module_items, n)?))
    };

    for it in &items {
        match it {
            // `ns.x` where `ns` is a namespace import of a Cleat module.
            Item::Import {
                form: ImportForm::Namespace(ns),
                path,
                span,
                ..
            } if segs[0] == ns => {
                return if segs.len() > 1 {
                    in_module(path, segs[1])
                } else {
                    Some((None, *span))
                };
            }
            // A name brought in by `import { orig as local }`.
            Item::Import {
                form: ImportForm::Named(names),
                path,
                span,
                ..
            } => {
                if let Some((orig, _, _)) = names.iter().find(|(_, local, _)| local == segs[0]) {
                    return in_module(path, orig).or(Some((None, *span)));
                }
            }
            _ => {}
        }
    }
    find_in(&items, segs[0]).map(|s| (None, s))
}
