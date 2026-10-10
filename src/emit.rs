//! Serialization to JSON and YAML (SPEC §7 step 5).

use crate::eval::Value;

pub fn to_json(v: &Value, pretty: bool) -> String {
    let mut out = String::new();
    write_json(v, pretty, 0, &mut out);
    out
}

fn write_json(v: &Value, pretty: bool, indent: usize, out: &mut String) {
    let nl = |out: &mut String, n: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&"  ".repeat(n));
        }
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&float(*f)),
        Value::Str(s) => out.push_str(&quote(s)),
        Value::List(xs) if xs.is_empty() => out.push_str("[]"),
        Value::Obj(es) if es.is_empty() => out.push_str("{}"),
        Value::List(xs) => {
            out.push('[');
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, indent + 1);
                write_json(x, pretty, indent + 1, out);
            }
            nl(out, indent);
            out.push(']');
        }
        Value::Obj(es) => {
            out.push('{');
            for (i, (k, x)) in es.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, indent + 1);
                out.push_str(&quote(k));
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                write_json(x, pretty, indent + 1, out);
            }
            nl(out, indent);
            out.push('}');
        }
    }
}

fn float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 { format!("{f:.1}") } else { format!("{f}") }
}

/// A JSON string literal, which is also a valid YAML double-quoted scalar.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One YAML document (no `---` header), ending in a newline.
pub fn to_yaml(v: &Value) -> String {
    let mut out = String::new();
    match v {
        Value::List(xs) if !xs.is_empty() => write_list(xs, 0, &mut out),
        Value::Obj(es) if !es.is_empty() => write_obj(es, 0, &mut out),
        v => {
            out.push_str(&yaml_scalar(v, 0));
            out.push('\n');
        }
    }
    out
}

/// Multiple documents separated by `---`.
pub fn to_yaml_stream(docs: &[Value]) -> String {
    docs.iter().map(to_yaml).collect::<Vec<_>>().join("---\n")
}

fn write_obj(es: &[(String, Value)], indent: usize, out: &mut String) {
    for (k, v) in es {
        out.push_str(&" ".repeat(indent));
        out.push_str(&yaml_str(k));
        out.push(':');
        match v {
            Value::Obj(inner) if !inner.is_empty() => {
                out.push('\n');
                write_obj(inner, indent + 2, out);
            }
            Value::List(xs) if !xs.is_empty() => {
                out.push('\n');
                write_list(xs, indent + 2, out);
            }
            v => {
                out.push(' ');
                out.push_str(&yaml_scalar(v, indent + 2));
                out.push('\n');
            }
        }
    }
}

fn write_list(xs: &[Value], indent: usize, out: &mut String) {
    for x in xs {
        let mut item = String::new();
        match x {
            Value::Obj(es) if !es.is_empty() => write_obj(es, indent + 2, &mut item),
            Value::List(ys) if !ys.is_empty() => write_list(ys, indent + 2, &mut item),
            v => {
                item.push_str(&" ".repeat(indent + 2));
                item.push_str(&yaml_scalar(v, indent + 2));
                item.push('\n');
            }
        }
        // Put the first line of the item on the `- ` line.
        out.push_str(&" ".repeat(indent));
        out.push_str("- ");
        out.push_str(&item[indent + 2..]);
    }
}

/// A scalar (or empty container) in YAML; multi-line strings become block literals.
fn yaml_scalar(v: &Value, indent: usize) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_nan() => ".nan".into(),
        Value::Float(f) if f.is_infinite() => (if *f > 0.0 { ".inf" } else { "-.inf" }).into(),
        Value::Float(f) => float(*f),
        Value::List(_) => "[]".into(),
        Value::Obj(_) => "{}".into(),
        Value::Str(s) if block_ok(s) => {
            let (body, chomp) = if let Some(b) = s.strip_suffix('\n') {
                if b.ends_with('\n') { (b, "+") } else { (b, "") }
            } else {
                (s.as_str(), "-")
            };
            let pad = " ".repeat(indent);
            let lines: Vec<String> =
                body.split('\n').map(|l| if l.is_empty() { String::new() } else { format!("{pad}{l}") }).collect();
            format!("|{chomp}\n{}", lines.join("\n"))
        }
        Value::Str(s) => yaml_str(s),
    }
}

fn block_ok(s: &str) -> bool {
    s.contains('\n')
        && !s.starts_with([' ', '\n'])
        && s.chars().all(|c| c == '\n' || c == '\t' || !c.is_control())
        && !s.lines().any(|l| l.ends_with(' '))
}

/// A string as a plain YAML scalar when that is unambiguous, otherwise double-quoted.
fn yaml_str(s: &str) -> String {
    if plain_ok(s) { s.to_string() } else { quote(s) }
}

fn plain_ok(s: &str) -> bool {
    if s.is_empty() || s.trim() != s {
        return false;
    }
    if s.starts_with(['-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`']) {
        return false;
    }
    if s.contains(": ") || s.contains(" #") || s.ends_with(':') || s.chars().any(|c| c.is_control()) {
        return false;
    }
    // Anything YAML 1.1 or 1.2 would read as a non-string.
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | ".inf" | "-.inf" | "+.inf" | ".nan" | "<<"
    ) {
        return false;
    }
    let numberish = s.trim_start_matches(['+', '-']);
    if numberish.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && numberish.chars().all(|c| c.is_ascii_hexdigit() || "._:-+xXoO".contains(c))
    {
        return false; // numbers, hex/octal, sexagesimal, dates, versions like 1.2.3
    }
    true
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
