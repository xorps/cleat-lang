//! `cleat view`: the exported resources in a browser, laid out like a cluster
//! dashboard. Either a local server that re-renders when files change, or a
//! self-contained HTML snapshot.
//!
//! The server is deliberately small: `std::net`, one request at a time,
//! bound to 127.0.0.1 only, and it rejects requests whose `Host` isn't the
//! loopback address it serves (so other sites can't reach it through DNS
//! rebinding). The page polls `/api/state` and redraws when it changes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use cleat_core::{ExportError, InputValue, Value, emit, json, targets};

const INDEX: &str = include_str!("view/index.html");
const CSS: &str = include_str!("view/view.css");
const JS: &str = include_str!("view/view.js");

pub struct ViewConfig {
    pub file: PathBuf,
    pub root: Option<PathBuf>,
    /// Inputs from the command line; the page starts from these and can change them.
    pub inputs: Vec<(String, InputValue)>,
    /// `plain` or `argocd`; Argo CD adds sync-wave annotations to the documents.
    pub target: String,
}

/// The page's data: problems, inputs, components and resources, as JSON.
/// `ui_inputs`, when the page sends them, replace the command-line inputs.
pub fn state(cfg: &ViewConfig, ui_inputs: Option<Vec<(String, String)>>) -> Value {
    let name = cfg.file.display().to_string();
    let s = |x: &str| Value::Str(x.to_string());
    let obj = |pairs: Vec<(&str, Value)>| Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    let diag = |span: cleat_core::diag::Span, msg: &str| {
        obj(vec![
            ("line", Value::Int(span.line as i64)),
            ("col", Value::Int(span.col as i64)),
            ("msg", s(msg)),
        ])
    };
    let mut out = vec![("file", s(&name)), ("target", s(&cfg.target))];
    let src = match std::fs::read_to_string(&cfg.file) {
        Ok(src) => src,
        Err(e) => {
            out.push((
                "errors",
                Value::List(vec![obj(vec![("msg", s(&format!("cannot read {name}: {e}")))])]),
            ));
            return obj(out);
        }
    };
    let base = cfg.file.parent().map(Path::to_path_buf);
    let (base, root) = (base.as_deref(), cfg.root.as_deref());

    let cli_raw = |v: &InputValue| match v {
        InputValue::Raw(r) => r.clone(),
        InputValue::Json(Value::Str(x)) => x.clone(),
        InputValue::Json(v) => emit::to_json(v, false),
        InputValue::RawList(xs) => emit::to_json(&Value::List(xs.iter().map(|x| s(x)).collect()), false),
        InputValue::RawMap(es) => {
            emit::to_json(&Value::Obj(es.iter().map(|(k, v)| (k.clone(), s(v))).collect()), false)
        }
    };
    let infos = match crate::inputs(&src, base, root) {
        Ok(infos) => infos,
        Err(outcome) => {
            out.push((
                "errors",
                Value::List(outcome.errors.iter().map(|d| diag(d.span, &d.msg)).collect()),
            ));
            return obj(out);
        }
    };
    let inputs: Vec<(String, InputValue)> = match &ui_inputs {
        Some(ui) => ui
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (k.clone(), InputValue::Raw(v.clone())))
            .collect(),
        None => cfg.inputs.clone(),
    };
    out.push((
        "inputs",
        Value::List(
            infos
                .iter()
                .map(|i| {
                    let given = cfg
                        .inputs
                        .iter()
                        .rev()
                        .find(|(n, _)| *n == i.name)
                        .map(|(_, v)| cli_raw(v));
                    obj(vec![
                        ("name", s(&i.name)),
                        ("ty", s(&i.ty)),
                        ("definition", i.definition.as_deref().map_or(Value::Null, s)),
                        ("required", Value::Bool(i.required)),
                        (
                            "default",
                            i.default.as_ref().map_or(Value::Null, |d| s(&emit::to_json(d, false))),
                        ),
                        ("given", given.map_or(Value::Null, |g| s(&g))),
                    ])
                })
                .collect(),
        ),
    ));
    match crate::export_in(&src, base, root, inputs) {
        Err(ExportError::Check(outcome)) => {
            out.push((
                "errors",
                Value::List(outcome.errors.iter().map(|d| diag(d.span, &d.msg)).collect()),
            ));
        }
        Err(ExportError::Eval(errs)) => {
            let list = errs
                .iter()
                .map(|e| {
                    let span = e.trace.first().map_or(e.span, |(_, at)| *at);
                    diag(span, &e.msg)
                })
                .collect();
            out.push(("errors", Value::List(list)));
        }
        Ok((outcome, comps)) => {
            out.push((
                "warnings",
                Value::List(outcome.warnings.iter().map(|d| diag(d.span, &d.msg)).collect()),
            ));
            let docs = match cfg.target.as_str() {
                "argocd" => targets::argocd(&comps),
                _ => Ok(targets::plain(&comps)),
            };
            let docs = match docs {
                Ok(d) => d,
                Err(e) => {
                    out.push(("errors", Value::List(vec![obj(vec![("msg", s(&e))])])));
                    return obj(out);
                }
            };
            // `plain` and `argocd` keep the documents of each component together, in wave order.
            let waves = targets::waves(&comps);
            let mut order: Vec<usize> = (0..comps.len()).collect();
            order.sort_by_key(|&i| waves[i]);
            let mut resources = Vec::new();
            let mut docs = docs.into_iter();
            for &i in &order {
                let c = &comps[i];
                let comp = c.name.clone().unwrap_or_else(|| "default".into());
                for _ in &c.docs {
                    let Some(doc) = docs.next() else { break };
                    resources.push(obj(vec![
                        ("component", s(&comp)),
                        ("wave", Value::Int(waves[i])),
                        ("yaml", s(&emit::to_yaml(&doc))),
                        ("doc", doc),
                    ]));
                }
            }
            let components = order
                .iter()
                .map(|&i| {
                    let c = &comps[i];
                    obj(vec![
                        ("name", s(c.name.as_deref().unwrap_or("default"))),
                        ("after", Value::List(c.after.iter().map(|a| s(a)).collect())),
                        ("wave", Value::Int(waves[i])),
                    ])
                })
                .collect();
            out.push(("components", Value::List(components)));
            out.push(("resources", Value::List(resources)));
        }
    }
    obj(out)
}

/// The page as one self-contained HTML file with the current state built in.
pub fn snapshot(cfg: &ViewConfig) -> String {
    // `</` can't appear inside a <script> element, so escape it in the embedded JSON.
    let data = emit::to_json(&state(cfg, None), false).replace("</", "<\\/");
    INDEX
        .replace(
            r#"<link rel="stylesheet" href="view.css">"#,
            &format!("<style>\n{CSS}</style>"),
        )
        .replace(
            r#"<script src="view.js"></script>"#,
            &format!("<script>window.CLEAT_SNAPSHOT = {data};</script>\n<script>\n{JS}</script>"),
        )
}

/// Serve the page on 127.0.0.1 until interrupted. `on_ready` gets the URL.
pub fn serve(cfg: &ViewConfig, port: u16, on_ready: impl FnOnce(&str)) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/");
    on_ready(&url);
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // A broken connection only affects that request.
        let _ = handle(cfg, stream, &hosts);
    }
    Ok(())
}

fn handle(cfg: &ViewConfig, mut stream: TcpStream, hosts: &[String]) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (mut host, mut length) = (String::new(), 0usize);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "host" => host = v.trim().to_string(),
                "content-length" => length = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if !hosts.contains(&host) {
        return respond(&mut stream, "403 Forbidden", "text/plain", b"unexpected Host header");
    }
    let path = target.split('?').next().unwrap_or("");
    match (method, path) {
        ("GET", "/") => respond(&mut stream, "200 OK", "text/html; charset=utf-8", INDEX.as_bytes()),
        ("GET", "/view.css") => respond(&mut stream, "200 OK", "text/css; charset=utf-8", CSS.as_bytes()),
        ("GET", "/view.js") => respond(&mut stream, "200 OK", "text/javascript; charset=utf-8", JS.as_bytes()),
        ("POST", "/api/state") => {
            let mut body = vec![0; length.min(1 << 20)];
            reader.read_exact(&mut body)?;
            // The body is `{ "inputs": { name: "raw value" } }`, or `{}` to use the command line's.
            let ui = json::parse(&String::from_utf8_lossy(&body))
                .ok()
                .and_then(|v| match v.get("inputs") {
                    Some(Value::Obj(es)) => Some(
                        es.iter()
                            .map(|(k, v)| match v {
                                Value::Str(x) => (k.clone(), x.clone()),
                                other => (k.clone(), emit::to_json(other, false)),
                            })
                            .collect(),
                    ),
                    _ => None,
                });
            let data = emit::to_json(&state(cfg, ui), false);
            respond(&mut stream, "200 OK", "application/json", data.as_bytes())
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

fn respond(stream: &mut TcpStream, status: &str, ty: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}
