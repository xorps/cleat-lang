//! `cleat view`: the state the page draws from, snapshots, and the local server.

use std::io::{Read, Write};
use std::net::TcpStream;

use cleat::view::{ViewConfig, serve, snapshot, state};
use cleat::{InputValue, Value, json};

fn app(name: &str, src: &str) -> ViewConfig {
    let dir = std::env::temp_dir().join(format!("cleat-view-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("app.cleat");
    std::fs::write(&file, src).unwrap();
    ViewConfig {
        file,
        root: Some(dir),
        inputs: vec![("env".into(), InputValue::Raw("prod".into()))],
        target: "argocd".into(),
        diff: None,
        source: None,
    }
}

const SRC: &str = r#"
input env: "dev" | "prod"
let cm = { apiVersion: "v1", kind: "ConfigMap", metadata.name: "settings", data.ENV: env }
let api = { apiVersion: "apps/v1", kind: "Deployment", metadata.name: "api" }
export config = [cm]
export app after config = [api]
"#;

fn strs(v: &Value, path: &[&str]) -> Vec<String> {
    let Some(Value::List(xs)) = v.get(path[0]) else {
        return vec![];
    };
    xs.iter()
        .map(|x| {
            let mut x = x;
            for p in &path[1..] {
                x = x.get(p).unwrap_or(&Value::Null);
            }
            match x {
                Value::Str(s) => s.clone(),
                other => other.to_string(),
            }
        })
        .collect()
}

#[test]
fn state_has_components_in_wave_order_and_their_resources() {
    let cfg = app("state", SRC);
    let s = state(&cfg, None);
    assert_eq!(strs(&s, &["components", "name"]), ["config", "app"]);
    assert_eq!(strs(&s, &["components", "wave"]), ["0", "1"]);
    assert_eq!(strs(&s, &["resources", "doc", "kind"]), ["ConfigMap", "Deployment"]);
    assert_eq!(strs(&s, &["resources", "component"]), ["config", "app"]);
    // The Argo CD target's sync waves are in the documents and their YAML.
    assert!(strs(&s, &["resources", "yaml"])[1].contains("argocd.argoproj.io/sync-wave: \"1\""));
    // Inputs come from the command line until the page sends its own.
    assert_eq!(strs(&s, &["inputs", "given"]), ["prod"]);
    assert!(strs(&s, &["resources", "yaml"])[0].contains("ENV: prod"));
    let s = state(&cfg, Some(vec![("env".into(), "dev".into())]));
    assert!(strs(&s, &["resources", "yaml"])[0].contains("ENV: dev"));
    let s = state(&cfg, Some(vec![("env".into(), "staging".into())]));
    assert!(strs(&s, &["errors", "msg"])[0].contains("got \"staging\""));
}

#[test]
fn state_reports_check_errors_with_positions() {
    let cfg = app("errors", "let x: Int = \"no\"\nexport { a: x }");
    let s = state(&cfg, None);
    assert_eq!(strs(&s, &["errors", "line"]), ["1"]);
    assert!(strs(&s, &["errors", "msg"])[0].contains("type mismatch"));
}

#[test]
fn snapshots_embed_the_state_safely() {
    let mut cfg = app(
        "snapshot",
        "export { kind: \"ConfigMap\", metadata.name: \"x\", data.html: \"</script><b>\" }",
    );
    cfg.inputs.clear();
    let html = snapshot(&cfg);
    assert!(html.contains("window.CLEAT_SNAPSHOT = {"));
    assert!(
        !html.contains("view.js\"") && !html.contains("view.css\""),
        "assets are inlined"
    );
    // A value can't end the script element early.
    assert_eq!(
        html.matches("</script>").count(),
        2,
        "only the two script elements close"
    );
    assert!(html.contains(r"<\/script><b>"));
}

#[test]
fn the_server_answers_only_for_its_own_host() {
    let (tx, rx) = std::sync::mpsc::channel();
    // A config isn't Send (it can hold a git session), so make it on the server's thread.
    std::thread::spawn(move || serve(&app("serve", SRC), 0, |url| tx.send(url.to_string()).unwrap()).unwrap());
    let url = rx.recv().unwrap();
    let addr = url.trim_start_matches("http://").trim_end_matches('/').to_string();
    let request = |host: &str, req: &str| {
        let mut s = TcpStream::connect(&addr).unwrap();
        write!(s, "{}", req.replace("{host}", host)).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    };
    let get = "GET / HTTP/1.1\r\nHost: {host}\r\n\r\n";
    assert!(request(&addr, get).starts_with("HTTP/1.1 200"));
    assert!(request("evil.example", get).starts_with("HTTP/1.1 403"));
    let body = r#"{"inputs":{"env":"dev"}}"#;
    let post = format!(
        "POST /api/state HTTP/1.1\r\nHost: {{host}}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let resp = request(&addr, &post);
    let data = json::parse(resp.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(strs(&data, &["resources", "yaml"])[0].contains("ENV: dev"));
    assert!(request(&addr, "GET /../../etc/passwd HTTP/1.1\r\nHost: {host}\r\n\r\n").starts_with("HTTP/1.1 404"));
}

#[test]
fn resources_point_at_where_they_are_defined() {
    // A `let` named in an exported list points at the `let`; anything else at itself.
    let src = "let cm = { kind: \"ConfigMap\", metadata.name: \"a\" }\nexport [\n  cm,\n  { kind: \"Secret\", metadata.name: \"b\" },\n]\nexport { kind: \"Service\", metadata.name: \"c\" }\n";
    let mut cfg = app("spans", src);
    cfg.inputs.clear();
    let s = state(&cfg, None);
    let at: Vec<(String, String)> = strs(&s, &["resources", "line"])
        .into_iter()
        .zip(strs(&s, &["resources", "col"]))
        .collect();
    let pair = |l: &str, c: &str| (l.to_string(), c.to_string());
    assert_eq!(at, [pair("1", "1"), pair("4", "3"), pair("6", "8")]);
    // The editor's unsaved text is used instead of the file.
    cfg.source = Some("export { kind: \"ConfigMap\", metadata.name: \"unsaved\" }".into());
    assert_eq!(
        strs(&state(&cfg, None), &["resources", "doc", "metadata", "name"]),
        ["unsaved"]
    );
}
