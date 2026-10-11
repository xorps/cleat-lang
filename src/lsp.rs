//! A small Language Server Protocol server over stdio: diagnostics on every
//! change, and hover. Uses the crate's own JSON reader/writer.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::check::{Hover, Outcome};
use crate::diag::Span;
use crate::eval::Value;
use crate::{check_source_in, emit, json};
use std::path::{Path, PathBuf};

struct Doc {
    text: String,
    hovers: Vec<Hover>,
}

pub fn run() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut out = io::stdout().lock();
    let mut docs: HashMap<String, Doc> = HashMap::new();

    while let Some(msg) = read_message(&mut input)? {
        let method = match msg.get("method") {
            Some(Value::Str(m)) => m.clone(),
            _ => continue, // a response to us; we send no requests
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let uri = || str_at(&params, &["textDocument", "uri"]).unwrap_or_default();

        match method.as_str() {
            "initialize" => {
                let caps = obj(vec![
                    (
                        "textDocumentSync",
                        obj(vec![("openClose", Value::Bool(true)), ("change", Value::Int(1))]),
                    ),
                    ("hoverProvider", Value::Bool(true)),
                ]);
                let result = obj(vec![
                    ("capabilities", caps),
                    ("serverInfo", obj(vec![("name", s("cleat")), ("version", s(env!("CARGO_PKG_VERSION")))])),
                ]);
                respond(&mut out, id, result)?;
            }
            "shutdown" => respond(&mut out, id, Value::Null)?,
            "exit" => return Ok(()),
            "textDocument/didOpen" => {
                let text = str_at(&params, &["textDocument", "text"]).unwrap_or_default();
                update(&mut out, &mut docs, uri(), text)?;
            }
            "textDocument/didChange" => {
                // Full sync: the last change holds the whole document.
                if let Some(Value::List(changes)) = params.get("contentChanges") {
                    if let Some(Value::Str(text)) = changes.last().and_then(|c| c.get("text")) {
                        update(&mut out, &mut docs, uri(), text.clone())?;
                    }
                }
            }
            "textDocument/didClose" => {
                let u = uri();
                docs.remove(&u);
                publish(&mut out, &u, Vec::new())?;
            }
            "textDocument/hover" => {
                let result = docs.get(&uri()).and_then(|d| hover(d, &params)).unwrap_or(Value::Null);
                respond(&mut out, id, result)?;
            }
            _ if id.is_some() => {
                let err = obj(vec![("code", Value::Int(-32601)), ("message", s(&format!("unsupported: {method}")))]);
                send(&mut out, &obj(vec![("jsonrpc", s("2.0")), ("id", id.unwrap()), ("error", err)]))?;
            }
            _ => {} // other notifications
        }
    }
    Ok(())
}

fn update(out: &mut impl Write, docs: &mut HashMap<String, Doc>, uri: String, text: String) -> io::Result<()> {
    let path = uri_to_path(&uri);
    let base = path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
    let root = path.as_deref().and_then(crate::modules::find_root);
    let result = catch_unwind(AssertUnwindSafe(|| check_source_in(&text, base.as_deref(), root.as_deref())));
    let (diags, hovers) = match result {
        Ok(outcome) => (diagnostics(&text, &outcome), outcome.hovers),
        Err(_) => {
            let d = diagnostic(&text, Span { line: 1, col: 1 }, 1, "internal error in the cleat checker (please report)");
            (vec![d], Vec::new())
        }
    };
    // Keep the previous hovers while the file doesn't parse.
    let hovers = if hovers.is_empty() { docs.get(&uri).map(|d| d.hovers.clone()).unwrap_or_default() } else { hovers };
    publish(out, &uri, diags)?;
    docs.insert(uri, Doc { text, hovers });
    Ok(())
}

fn diagnostics(text: &str, o: &Outcome) -> Vec<Value> {
    let mut v: Vec<Value> = o.errors.iter().map(|d| diagnostic(text, d.span, 1, &d.msg)).collect();
    v.extend(o.warnings.iter().map(|d| diagnostic(text, d.span, 2, &d.msg)));
    v
}

fn diagnostic(text: &str, span: Span, severity: i64, msg: &str) -> Value {
    let len = token_len(text, span);
    obj(vec![
        ("range", range(text, span, len)),
        ("severity", Value::Int(severity)),
        ("source", s("cleat")),
        ("message", s(msg)),
    ])
}

fn publish(out: &mut impl Write, uri: &str, diags: Vec<Value>) -> io::Result<()> {
    let params = obj(vec![("uri", s(uri)), ("diagnostics", Value::List(diags))]);
    send(
        out,
        &obj(vec![("jsonrpc", s("2.0")), ("method", s("textDocument/publishDiagnostics")), ("params", params)]),
    )
}

fn hover(doc: &Doc, params: &Value) -> Option<Value> {
    let pos = params.get("position")?;
    let (Some(Value::Int(line)), Some(Value::Int(ch))) = (pos.get("line"), pos.get("character")) else {
        return None;
    };
    let line_text = doc.text.lines().nth(*line as usize)?;
    let col = utf16_to_col(line_text, *ch as usize);
    let line = *line as u32 + 1;
    let h = doc
        .hovers
        .iter()
        .filter(|h| h.span.line == line && h.span.col <= col && col < h.span.col + h.len as u32)
        .min_by_key(|h| h.len)?;
    let mut md = format!("```cleat\n{}\n```", h.text);
    if let Some(doc) = &h.doc {
        md.push_str("\n\n");
        md.push_str(doc);
    }
    let contents = obj(vec![("kind", s("markdown")), ("value", s(&md))]);
    Some(obj(vec![("contents", contents), ("range", range(&doc.text, h.span, h.len))]))
}

// ---------- positions ----------
// Spans are 1-based line/column in characters; LSP uses 0-based lines and UTF-16 offsets.

fn range(text: &str, span: Span, len: usize) -> Value {
    let line_text = text.lines().nth(span.line.saturating_sub(1) as usize).unwrap_or("");
    let start = col_to_utf16(line_text, span.col.saturating_sub(1) as usize);
    let end = col_to_utf16(line_text, span.col.saturating_sub(1) as usize + len);
    let line = Value::Int(i64::from(span.line.saturating_sub(1)));
    let pos = |c: usize| obj(vec![("line", line.clone()), ("character", Value::Int(c as i64))]);
    obj(vec![("start", pos(start)), ("end", pos(end))])
}

fn col_to_utf16(line: &str, chars: usize) -> usize {
    line.chars().take(chars).map(char::len_utf16).sum()
}

/// 1-based character column for a UTF-16 offset.
fn utf16_to_col(line: &str, units: usize) -> u32 {
    let mut seen = 0;
    for (i, c) in line.chars().enumerate() {
        if seen >= units {
            return i as u32 + 1;
        }
        seen += c.len_utf16();
    }
    line.chars().count() as u32 + 1
}

/// Length of the token starting at `span`, so diagnostics underline a whole word.
fn token_len(text: &str, span: Span) -> usize {
    let Some(line) = text.lines().nth(span.line.saturating_sub(1) as usize) else { return 1 };
    let rest: Vec<char> = line.chars().skip(span.col.saturating_sub(1) as usize).collect();
    match rest.first() {
        Some(c) if c.is_alphanumeric() || *c == '_' => {
            rest.iter().take_while(|c| c.is_alphanumeric() || **c == '_' || **c == '.').count()
        }
        Some('"') => rest.iter().skip(1).position(|c| *c == '"').map_or(rest.len(), |i| i + 2),
        Some(_) => 1,
        None => 1,
    }
}

// ---------- JSON-RPC framing ----------

fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok();
        }
    }
    let len = len.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    json::parse(&String::from_utf8_lossy(&buf)).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn send(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let body = emit::to_json(v, false);
    write!(w, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    w.flush()
}

fn respond(w: &mut impl Write, id: Option<Value>, result: Value) -> io::Result<()> {
    send(w, &obj(vec![("jsonrpc", s("2.0")), ("id", id.unwrap_or(Value::Null)), ("result", result)]))
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

fn str_at(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(k)?;
    }
    match cur {
        Value::Str(x) => Some(x.clone()),
        _ => None,
    }
}

/// `file:///a/b%20c.cleat` -> `/a/b c.cleat`.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}
