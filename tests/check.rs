use test_lang::targets::{self, FluxOptions};
use test_lang::vc::VcStatus;
use test_lang::{ExportError, InputValue, Value, check_source, emit, export};

const PRELUDE: &str = r#"
type Port = Int where 1 <= self && self <= 65535
type Env = "dev" | "staging" | "prod"
type Meta = { name: Str, labels?: { [Str]: Str } }
type Svc = { apiVersion: "v1", kind: "Service", metadata: Meta, spec: { type: "ClusterIP" | "NodePort" = "ClusterIP", ports: [{ port: Port }] } }
type Cm = { apiVersion: "v1", kind: "ConfigMap", metadata: Meta, data: { [Str]: Str } }
"#;

fn src(s: &str) -> String {
    format!("{PRELUDE}\n{s}")
}

fn ok(s: &str) -> test_lang::Outcome {
    let out = check_source(&src(s));
    let msgs: Vec<_> = out.errors.iter().map(|e| format!("{}: {}", e.span, e.msg)).collect();
    assert!(msgs.is_empty(), "expected no errors, got:\n{}", msgs.join("\n"));
    out
}

fn err(s: &str, needle: &str) {
    let out = check_source(&src(s));
    assert!(
        out.errors.iter().any(|e| e.msg.contains(needle)),
        "expected an error containing {needle:?}, got: {:#?}",
        out.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
}

fn run(s: &str, inputs: &[(&str, &str)]) -> Result<Vec<Value>, String> {
    let inputs = inputs.iter().map(|(k, v)| (k.to_string(), InputValue::Raw(v.to_string()))).collect();
    match export(&src(s), inputs) {
        Ok((_, comps)) => Ok(targets::plain(&comps)),
        Err(ExportError::Check(o)) => Err(o.errors.iter().map(|e| e.msg.clone()).collect::<Vec<_>>().join("\n")),
        Err(ExportError::Eval(es)) => Err(es.iter().map(|e| e.msg.clone()).collect::<Vec<_>>().join("\n")),
    }
}

fn yaml(s: &str, inputs: &[(&str, &str)]) -> String {
    emit::to_yaml_stream(&run(s, inputs).unwrap())
}

#[test]
fn webapp_example_checks_and_exports() {
    let s = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/webapp.tl")).unwrap();
    let out = check_source(&s);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let inputs = vec![("env".into(), InputValue::Raw("prod".into())), ("image".into(), InputValue::Raw("api:1".into()))];
    let Ok((_, comps)) = export(&s, inputs) else { panic!("export failed") };
    let docs = targets::argocd(&comps).unwrap();
    assert_eq!(docs.len(), 5);
    let wave = |d: &Value| d.get("metadata").and_then(|m| m.get("annotations")).and_then(|a| a.get(targets::SYNC_WAVE)).cloned();
    assert_eq!(wave(&docs[0]), Some(Value::Str("0".into())));
    assert_eq!(wave(&docs[4]), Some(Value::Str("1".into())));
}

// ---- records: exact, optional, defaults ----

#[test]
fn typos_are_caught_with_suggestions() {
    err("let s = Svc { metdata: { name: \"x\" }, spec: { ports: [] } }", "unknown field `metdata` in `Svc`; did you mean `metadata`?");
    err("let s = Svc { metadata: { name: \"x\" }, spec: { prots: [] } }", "did you mean `ports`?");
}

#[test]
fn missing_required_fields() {
    err("let s = Svc { spec: { ports: [] } }", "missing required field `metadata`");
}

#[test]
fn defaults_and_singleton_fields_are_filled_in() {
    let out = yaml("export Svc { metadata.name: \"web\", spec.ports: [{ port: 80 }] }", &[]);
    assert_eq!(
        out,
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\nspec:\n  type: ClusterIP\n  ports:\n    - port: 80\n"
    );
}

#[test]
fn optional_fields_read_as_nullable() {
    err("fn f(m: Meta) -> Int = len(m.labels)", "got `{ [Str]: Str } | null`");
    ok("fn f(m: Meta) -> Int = len(m.labels ?? {})");
    ok("fn f(m: Meta) -> Int = if m.labels != null then 1 else 0");
}

#[test]
fn null_checks_narrow() {
    ok("fn f(x: Int | null) -> Int = if x != null then x + 1 else 0");
    err("fn f(x: Int | null) -> Int = x + 1", "may be null");
    err("fn f(m: { a?: { b: Int } }) -> Int = m.a.b", "`m.a` may be null");
    ok("fn f(m: { a?: { b: Int } }) -> Int = m.a?.b ?? 0");
}

#[test]
fn open_records_allow_extra_fields() {
    ok("fn f(x: { name: Str, .. }) -> Str = x.name\nlet y = f({ name: \"a\", extra: 1 })");
    err("fn f(x: { name: Str }) -> Str = x.name\nlet y = f({ name: \"a\", extra: 1 })", "unknown field `extra`");
}

// ---- literal and union types ----

#[test]
fn literal_unions_and_exhaustiveness() {
    ok("fn f(e: Env) -> Int = match e { \"dev\" => 1, \"staging\" => 2, \"prod\" => 3 }");
    err("fn f(e: Env) -> Int = match e { \"dev\" => 1, \"prod\" => 3 }", "`\"staging\"` are not covered");
    ok("fn f(b: Bool) -> Int = match b { true => 1, false => 0 }");
    err("let e: Env = \"production\"", "type mismatch");
    ok("fn f(e: Env) -> Bool = e == \"prod\"");
    err("fn f(e: Env) -> Bool = e == \"production\"", "always false");
}

#[test]
fn literal_equality_narrows() {
    ok("fn f(e: Env) -> \"prod\" = if e == \"prod\" then e else \"prod\"");
}

#[test]
fn discriminated_unions() {
    let s = "fn f(r: Svc | Cm) -> Int = match r { s: { kind: \"Service\", .. } => len(s.spec.ports), c: { kind: \"ConfigMap\", .. } => len(c.data) }";
    ok(s);
    // The object literal is checked against the member its `kind` selects.
    err("let r: Svc | Cm = { apiVersion: \"v1\", kind: \"ConfigMap\", metadata.name: \"x\", data: { a: 1 } }", "expected `Str`");
}

#[test]
fn never_and_uninhabited_types() {
    ok("type T = (Int, Never)\nfn f(x: T) -> Str = match x {}");
    ok("fn f() -> Int = error(\"no\")");
}

// ---- refinements ----

#[test]
fn static_refinement_violations() {
    err("let s = Svc { metadata.name: \"x\", spec.ports: [{ port: 70000 }] }", "refinement violated");
    err("fn f(p: Port) -> Port = p\nlet x = f(0)", "refinement violated");
    ok("fn f(p: Port) -> Port = p\nlet x = f(8080)");
}

#[test]
fn refinements_with_builtins_are_decided_statically_when_closed() {
    let s = "type Dns = Str where matches(self, \"^[a-z0-9-]+$\")\n";
    err(&format!("{s}let n: Dns = \"Web_App\""), "refinement violated");
    ok(&format!("{s}let n: Dns = \"web-app\""));
    err("let n: Str where matches(self, \"[\") = \"x\"", "invalid regex");
}

#[test]
fn unprovable_refinements_are_runtime_checks() {
    let out = ok("fn f(x: Int) -> Port = x");
    assert_eq!(out.vcs.iter().filter(|v| v.status == VcStatus::Runtime).count(), 1);
    assert!(run("fn f(x: Int) -> Port = x\nexport f(70000)", &[]).unwrap_err().contains("70000 does not satisfy"));
}

#[test]
fn dependent_parameters() {
    let f = "fn clamp(lo: Int, hi: Int where hi >= lo, x: Int) -> Int = max(lo, min(hi, x))";
    err(&format!("{f}\nlet a = clamp(5, 1, 3)"), "refinement violated");
    ok(&format!("{f}\nlet a = clamp(1, 5, 3)"));
    ok(&format!("{f}\nlet a = clamp(hi: 5, lo: 1, x: 3)"));
}

// ---- functions ----

#[test]
fn named_arguments_and_defaults() {
    ok("fn f(a: Int, b: Int = 2) -> Int = a + b\nlet x = f(1)\nlet y = f(1, b: 3)");
    err("fn f(a: Int, b: Int = 2) -> Int = a + b\nlet x = f(1, c: 3)", "`f` has no parameter `c`");
    err("fn f(a: Int) -> Int = a\nlet x = f()", "missing argument `a`");
}

#[test]
fn recursion_is_rejected() {
    err("fn f(n: Int) -> Int = f(n)", "recursion is not allowed");
    err("fn f(n: Int) -> Int = g(n)\nfn g(n: Int) -> Int = n", "declared before use");
}

#[test]
fn shadowing_is_allowed() {
    ok("let name = \"a\"\nfn f(name: Str) -> Str = name");
}

// ---- evaluation ----

#[test]
fn objects_dotted_keys_conditionals_and_comprehensions() {
    let out = yaml(
        r#"
input debug: Bool = false
let xs = ["a", "b"]
export {
  metadata.name: "x",
  metadata.labels.app: "x",
  if debug then { level: "debug" } else { level: "info" },
  items: [for i, x in xs => "\(i):\(x)", if debug then "extra"],
  for x in xs => { [x]: upper(x) },
}
"#,
        &[],
    );
    assert_eq!(out, "metadata:\n  name: x\n  labels:\n    app: x\nlevel: info\nitems:\n  - \"0:a\"\n  - \"1:b\"\na: A\nb: B\n");
}

#[test]
fn spreads_are_overridable() {
    let out = yaml("let base = { a: 1, b: 2 }\nexport { ...base, b: 3 }", &[]);
    assert_eq!(out, "a: 1\nb: 3\n");
}

#[test]
fn optional_null_fields_are_dropped() {
    let out = yaml("export Cm { metadata: { name: \"c\", labels: null }, data: {} }", &[]);
    assert_eq!(out, "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: c\ndata: {}\n");
}

// ---- inputs ----

#[test]
fn inputs_are_decoded_by_type() {
    let s = "input num: Int\ninput s: Str\ninput e: Env\ninput xs: [Int] = []\nexport { num, s, e, xs }";
    let out = yaml(s, &[("num", "3"), ("s", "3"), ("e", "prod"), ("xs", "[1,2]")]);
    assert_eq!(out, "num: 3\ns: \"3\"\ne: prod\nxs:\n  - 1\n  - 2\n");
}

#[test]
fn input_errors_are_all_reported() {
    let e = run("input n: Port\ninput e: Env\ninput s: Str\nexport { n, e, s }", &[("n", "0"), ("e", "qa"), ("x", "1")]).unwrap_err();
    assert!(e.contains("unknown input `x`"), "{e}");
    assert!(e.contains("input `n`: 0 does not satisfy"), "{e}");
    assert!(e.contains("input `e`: expected `Env`"), "{e}");
    assert!(e.contains("missing required input `s`"), "{e}");
}

// ---- components ----

#[test]
fn component_dependencies() {
    err("export a after b = []", "unknown component `b`");
    err("export a after b = []\nexport b after a = []", "dependency cycle between components: a -> b -> a");
    err("export a after a = []", "cannot depend on itself");
}

#[test]
fn flux_kustomizations_have_depends_on() {
    let s = src("export db = [{ metadata.name: \"db\" }]\nexport app after db = [{ metadata.name: \"app\" }]");
    let Ok((_, comps)) = export(&s, vec![]) else { panic!() };
    let opts = FluxOptions {
        source: "infra".into(),
        path: "deploy".into(),
        namespace: "flux-system".into(),
        prefix: "x-".into(),
        interval: "5m".into(),
    };
    let files = targets::flux(&comps, &opts);
    let ks = &files.iter().find(|(n, _)| n == "flux-kustomizations.yaml").unwrap().1;
    assert!(ks.contains("name: x-app"), "{ks}");
    assert!(ks.contains("path: ./deploy/app"), "{ks}");
    assert!(ks.contains("dependsOn:\n    - name: x-db"), "{ks}");
    assert!(files.iter().any(|(n, _)| n == "db/resources.yaml"));
}

// ---- output ----

#[test]
fn yaml_quotes_ambiguous_strings() {
    let v = Value::Obj(
        ["no", "1.0", "1", "~", "on", "2024-01-01", "a: b", "-x", "ok", "1.2.3"]
            .iter()
            .map(|s| (s.to_string(), Value::Str(s.to_string())))
            .collect(),
    );
    let out = emit::to_yaml(&v);
    for s in ["\"no\": \"no\"", "\"1.0\": \"1.0\"", "\"~\": \"~\"", "\"on\": \"on\"", "\"2024-01-01\"", "\"a: b\"", "\"-x\"", "ok: ok", "\"1.2.3\""] {
        assert!(out.contains(s), "missing {s} in\n{out}");
    }
}

#[test]
fn yaml_block_strings() {
    let v = Value::Obj(vec![("script".into(), Value::Str("echo hi\necho bye\n".into()))]);
    assert_eq!(emit::to_yaml(&v), "script: |\n  echo hi\n  echo bye\n");
}

#[test]
fn base64() {
    assert_eq!(emit::base64(b"hello"), "aGVsbG8=");
    assert_eq!(emit::base64(b"hi!"), "aGkh");
}
