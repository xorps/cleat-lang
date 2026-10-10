use cleat::targets::{self, FluxOptions};
use cleat::vc::VcStatus;
use cleat::{ExportError, InputValue, Value, check_source, check_source_at, emit, export, export_at};
use std::path::{Path, PathBuf};

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

fn ok(s: &str) -> cleat::Outcome {
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
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    if !dir.join("../schemas/kubernetes-1.31.json").exists() {
        eprintln!("skipping: run schemas/fetch.sh to download the Kubernetes schema");
        return;
    }
    let s = std::fs::read_to_string(dir.join("webapp.cleat")).unwrap();
    let out = check_source_at(&s, Some(&dir));
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let inputs = vec![("env".into(), InputValue::Raw("prod".into())), ("image".into(), InputValue::Raw("api:1".into()))];
    let Ok((_, comps)) = export_at(&s, Some(&dir), inputs) else { panic!("export failed") };
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
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\nspec:\n  ports:\n    - port: 80\n  type: ClusterIP\n"
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

// ---- imports: OpenAPI and CRDs ----

/// Write fixture files into a fresh directory and return it.
fn fixture_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cleat-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (f, content) in files {
        std::fs::write(dir.join(f), content).unwrap();
    }
    dir
}

const SWAGGER: &str = r##"{
  "swagger": "2.0",
  "info": { "title": "Test", "version": "v1" },
  "definitions": {
    "io.test.api.web.v1.Site": {
      "description": "A website.",
      "x-kubernetes-group-version-kind": [{ "group": "web.test.io", "version": "v1", "kind": "Site" }],
      "required": ["spec"],
      "properties": {
        "apiVersion": { "type": "string" },
        "kind": { "type": "string" },
        "spec": { "$ref": "#/definitions/io.test.api.web.v1.SiteSpec" }
      }
    },
    "io.test.api.web.v1.SiteSpec": {
      "required": ["host"],
      "properties": {
        "host": { "type": "string", "pattern": "^[a-z.]+$", "maxLength": 20, "description": "The host name." },
        "port": { "$ref": "#/definitions/io.test.util.intstr.IntOrString" },
        "replicas": { "type": "integer", "minimum": 1, "maximum": 10 },
        "mode": { "type": "string", "enum": ["fast", "safe"] },
        "tree": { "$ref": "#/definitions/io.test.api.web.v1.Node" }
      }
    },
    "io.test.api.web.v1.Node": {
      "properties": { "children": { "type": "array", "items": { "$ref": "#/definitions/io.test.api.web.v1.Node" } } }
    },
    "io.test.util.intstr.IntOrString": { "type": "string", "format": "int-or-string" },
    "io.test.api.core.v1.Event": { "properties": { "a": { "type": "string" } } },
    "io.test.api.other.v1.Event": { "properties": { "b": { "type": "string" } } },
    "io.test.api.web.v1beta1.Site": { "properties": { "old": { "type": "boolean" } } }
  }
}"##;

#[test]
fn openapi_generation() {
    let doc = cleat::json::parse(SWAGGER).unwrap();
    let src = cleat::openapi::generate(&[doc]).unwrap().source;
    for want in [
        "type web.v1.Site = {",
        "apiVersion: \"web.test.io/v1\",",
        "kind: \"Site\",",
        "host: Str where len(self) <= 20 && matches(self, \"^[a-z.]+$\"),",
        "port?: intstr.IntOrString,",
        "replicas?: Int where self >= 1 && self <= 10,",
        "mode?: \"fast\" | \"safe\",",
        "children?: [Json],", // the self-reference is broken to keep types finite
        "type intstr.IntOrString = Int | Str",
        "type Site = web.v1.Site", // GA beats v1beta1
    ] {
        assert!(src.contains(want), "missing `{want}` in:\n{src}");
    }
    assert!(!src.contains("type Event ="), "ambiguous short name was generated:\n{src}");
}

#[test]
fn openapi_import_checks_and_exports() {
    let dir = fixture_dir("openapi", &[("api.json", SWAGGER)]);
    let check = |body: &str| check_source_at(&format!("import t from \"api.json\"\n{body}"), Some(&dir));
    let errs = |body: &str| check(body).errors.into_iter().map(|e| e.msg).collect::<Vec<_>>().join("\n");

    assert_eq!(errs("let s = t.Site { spec: { host: \"a.b\", port: 80, mode: \"fast\" } }"), "");
    assert!(errs("let s = t.Site { spec: { hots: \"a\" } }").contains("unknown field `hots` in `t.web.v1.SiteSpec`; did you mean `host`?"));
    assert!(errs("let s = t.Site { spec: { host: \"A\" } }").contains("refinement violated"));
    assert!(errs("let s = t.Site { spec: { host: \"a\", mode: \"slow\" } }").contains("expected `\"fast\" | \"safe\"`"));
    assert!(errs("let s = t.Site {}").contains("missing required field `spec`"));
    assert!(errs("let e = t.Event {}").contains("`t.Event` is ambiguous; use one of `t.core.v1.Event`, `t.other.v1.Event`"));
    assert!(errs("let s = t.Siet {}").contains("did you mean `t.Site`?"));

    let src = "import t from \"api.json\"\ninput r: Int = 3\nexport t.Site { spec: { host: \"a\", replicas: r } }";
    let Ok((_, comps)) = export_at(src, Some(&dir), vec![]) else { panic!() };
    let out = emit::to_yaml_stream(&targets::plain(&comps));
    assert_eq!(out, "apiVersion: web.test.io/v1\nkind: Site\nspec:\n  host: a\n  replicas: 3\n");
    let Err(ExportError::Eval(es)) = export_at(src, Some(&dir), vec![("r".into(), InputValue::Raw("11".into()))]) else {
        panic!()
    };
    assert!(es[0].msg.contains("11 does not satisfy `self <= 10`"), "{}", es[0].msg);
}

#[test]
fn imported_docs_show_on_hover() {
    let dir = fixture_dir("hover", &[("api.json", SWAGGER)]);
    let out = check_source_at("import t from \"api.json\"\nlet s = t.Site { spec: { host: \"a\" } }", Some(&dir));
    let h = out.hovers.iter().find(|h| h.text.contains("host: Str")).expect("hover on `host`");
    assert_eq!(h.doc.as_deref(), Some("The host name."));
}

const CRD: &str = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: widgets.example.com
spec:
  group: example.com
  names: { kind: Widget, plural: widgets }
  scope: Namespaced
  versions:
    - name: v1alpha1
      served: true
      storage: false
      schema:
        openAPIV3Schema:
          type: object
          properties:
            spec: { type: object, properties: { size: { type: integer } } }
    - name: v1
      served: true
      storage: true
      schema:
        openAPIV3Schema:
          type: object
          properties:
            spec:
              type: object
              required: [size]
              properties:
                size: { type: integer, minimum: 1 }
                color: { type: string, enum: [red, blue] }
                port: { x-kubernetes-int-or-string: true }
                parts:
                  type: array
                  items:
                    type: object
                    properties: { name: { type: string } }
"#;

#[test]
fn crd_import() {
    let dir = fixture_dir("crd", &[("widgets.yaml", CRD)]);
    let src = cleat::modules::generate_source(&dir.join("widgets.yaml")).unwrap();
    assert!(src.contains("type v1.WidgetSpec = {"), "{src}");
    assert!(src.contains("parts?: [v1.WidgetSpecPartsItem],"), "{src}");
    assert!(src.contains("type Widget = v1.Widget"), "storage version wins:\n{src}");

    let errs = |body: &str| {
        check_source_at(&format!("import w from \"widgets.yaml\"\n{body}"), Some(&dir))
            .errors
            .into_iter()
            .map(|e| e.msg)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(errs("let a = w.Widget { metadata.name: \"a\", spec: { size: 2, color: \"red\", port: \"http\", parts: [{ name: \"x\" }] } }"), "");
    assert!(errs("let a = w.Widget { spec: { size: 2, color: \"green\" } }").contains("expected `\"red\" | \"blue\"`"));
    assert!(errs("let a = w.Widget { spec: { color: \"red\" } }").contains("missing required field `size`"));
    assert!(errs("let a = w.Widget { spec: { size: 0 } }").contains("refinement violated"));
    assert!(errs("let a = w.v1alpha1.Widget { spec: { size: 0 } }").is_empty());

    let Ok((_, comps)) = export_at("import w from \"widgets.yaml\"\nexport w.Widget { metadata.name: \"a\", spec.size: 1 }", Some(&dir), vec![]) else {
        panic!()
    };
    let out = emit::to_yaml_stream(&targets::plain(&comps));
    assert_eq!(out, "apiVersion: example.com/v1\nkind: Widget\nmetadata:\n  name: a\nspec:\n  size: 1\n");
}

#[test]
fn import_errors() {
    let out = check_source_at("import x from \"missing.json\"", Some(Path::new("/nonexistent")));
    assert!(out.errors[0].msg.contains("import `x`: cannot read"), "{:?}", out.errors);
}

// ---- refining types: `with` and `refine` ----

#[test]
fn with_narrows_record_types() {
    let base = "type T = { a?: Str, b: Int, c?: { d: [Int] }, m?: { [Str]: Str } }\n";
    ok(&format!("{base}type U = T with {{ a: \"x\" | \"y\", b where self > 0, c.d[] where self < 10, m[] where len(self) < 5, -a }}"));
    err(&format!("{base}type U = T with {{ b: Str }}"), "`b`: `Str` is not a subtype of `Int`; `with` can only narrow types");
    err(&format!("{base}type U = T with {{ -b }}"), "`b`: only optional fields can be removed");
    err(&format!("{base}type U = T with {{ bb where self > 0 }}"), "has no field `bb`; did you mean `b`?");
    err(&format!("{base}type U = T with {{ b[] where self > 0 }}"), "cannot refine `[]` inside `Int`");
    err(&format!("{base}type U = T with {{ b where self }}"), "refinement predicate must be `Bool`");
    // The refined type is a subtype of the original.
    ok(&format!("{base}type U = T with {{ b where self > 0 }}\nfn f(u: U) -> T = u"));
    err(&format!("{base}type U = T with {{ b where self > 0 }}\nlet u: U = {{ b: 0 }}"), "refinement violated");
    err(&format!("{base}type U = T with {{ -a }}\nlet u: U = {{ a: \"x\", b: 1 }}"), "unknown field `a`");
}

#[test]
fn with_defaults_fill_in() {
    let out = yaml("type T = { a?: Str, b: Int }\ntype U = T with { a: Str = \"x\" }\nexport U { b: 1 }", &[]);
    assert_eq!(out, "b: 1\na: x\n");
}

#[test]
fn refine_applies_to_every_reference() {
    let dir = fixture_dir("refine", &[("api.json", SWAGGER)]);
    let errs = |body: &str| {
        check_source_at(&format!("import t from \"api.json\"\n{body}"), Some(&dir))
            .errors
            .into_iter()
            .map(|e| e.msg)
            .collect::<Vec<_>>()
            .join("\n")
    };
    // Refining the short name refines the definition `t.web.v1.SiteSpec`, which `t.Site` uses.
    let r = "refine t.SiteSpec with { host: \"a\" | \"b\", replicas where self <= 5 }\n";
    assert_eq!(errs(&format!("{r}let s = t.Site {{ spec: {{ host: \"a\", replicas: 5 }} }}")), "");
    assert!(errs(&format!("{r}let s = t.Site {{ spec: {{ host: \"c\" }} }}")).contains("expected `\"a\" | \"b\"`"));
    assert!(errs(&format!("{r}let s = t.Site {{ spec: {{ host: \"a\", replicas: 6 }} }}")).contains("refinement violated"));
    // Successive refinements stack.
    let r2 = "refine t.SiteSpec with { replicas where self <= 5 }\nrefine t.SiteSpec with { replicas where self >= 2 }\n";
    assert!(errs(&format!("{r2}let s = t.Site {{ spec: {{ host: \"a\", replicas: 1 }} }}")).contains("refinement violated"));
    assert!(errs("refine t.Nope with { a: Str }").contains("unknown type `t.Nope`"));
    // Runtime checks see the refinement too.
    let src = "import t from \"api.json\"\nrefine t.SiteSpec with { replicas where self <= 5 }\ninput r: Int\nexport t.Site { spec: { host: \"a\", replicas: r } }";
    let Err(ExportError::Eval(es)) = export_at(src, Some(&dir), vec![("r".into(), InputValue::Raw("7".into()))]) else { panic!() };
    assert!(es[0].msg.contains("7 does not satisfy `self <= 5`"), "{}", es[0].msg);
}

// ---- value patches: `e with { ... }` ----

const DEPLOY: &str = r#"
type Container = { name: Str, image?: Str, env?: [{ name: Str, value: Str }] }
type Dep = { metadata: { name: Str, labels?: { [Str]: Str } }, spec: { replicas?: Int, selector: Str, containers: [Container] } }
let base = Dep {
  metadata: { name: "api", labels: { app: "api" } },
  spec: { selector: "app", containers: [{ name: "api", image: "api:1", env: [{ name: "A", value: "1" }] }, { name: "proxy" }] },
}
"#;

#[test]
fn value_patches_apply() {
    let out = yaml(
        &format!(
            "{DEPLOY}export base with {{
  spec.replicas: 3,
  metadata.labels +: {{ tier: \"prod\", app: null }},
  metadata.labels.team: \"a\",
  spec.containers[name: \"api\"].image: \"api:2\",
  spec.containers[name: \"api\"].env +: [{{ name: \"B\", value: \"2\" }}],
  -spec.containers[name: \"proxy\"],
}}"
        ),
        &[],
    );
    assert_eq!(
        out,
        "metadata:\n  name: api\n  labels:\n    tier: prod\n    team: a\nspec:\n  selector: app\n  containers:\n    - name: api\n      image: api:2\n      env:\n        - name: A\n          value: \"1\"\n        - name: B\n          value: \"2\"\n  replicas: 3\n"
    );
}

#[test]
fn value_patches_are_typed_against_declared_types() {
    err(&format!("{DEPLOY}let x = base with {{ spec.replcias: 3 }}"), "`spec.replcias`: `Dep.spec` has no field `replcias`; did you mean `replicas`?");
    err(&format!("{DEPLOY}let x = base with {{ spec.replicas: \"3\" }}"), "expected `Int`");
    err(&format!("{DEPLOY}let x = base with {{ spec.containers[nmae: \"api\"].image: \"x\" }}"), "have no field `nmae`; did you mean `name`?");
    err(&format!("{DEPLOY}let x = base with {{ -spec.selector }}"), "`selector` is required in `Dep.spec` and cannot be removed");
    err(&format!("{DEPLOY}let x = base with {{ metadata.labels +: {{ a: 1 }} }}"), "expected `Str | null`");
    err(&format!("{DEPLOY}let x = base with {{ spec.containers.image: \"x\" }}"), "is a list; select elements");
    // The result keeps the declared type.
    ok(&format!("{DEPLOY}let x: Dep = base with {{ spec.replicas: 3 }}"));
}

#[test]
fn value_patches_on_anonymous_objects_update_the_type() {
    ok("let a = { n: 1, xs: [1] } with { n: \"one\", xs +: [2], extra.deep: true }\nlet s: Str = a.n\nlet b: Bool = a.extra.deep");
    err("let a = { n: 1 } with { -m }", "has no field `m`");
}

#[test]
fn value_patch_runtime_errors() {
    let e = run("input want: Str = \"b\"\nlet b = { xs: [{ name: \"a\" }] }\nexport b with { xs[name: want].v: 1 }", &[]).unwrap_err();
    assert!(e.contains("no element of `xs` matches `[name: \"b\"]`"), "{e}");
    // With a literal selector the same mistake is caught statically.
    err("let b = { xs: [{ name: \"a\" }] }\nlet c = b with { xs[name: \"b\"].v: 1 }", "can match `[name: \"b\"]`");
    let e = run("let b = { xs: [1] }\nexport b with { xs[3]: 1 }", &[]).unwrap_err();
    assert!(e.contains("index 3 is out of bounds"), "{e}");
}

// ---- modules with functions and constants ----

const LIB: &str = r#"
type Kind = "Deployment" | "Service"
type Res = { kind: Kind, metadata: { name: Str, labels?: { [Str]: Str } }, spec?: { replicas?: Int, type?: "ClusterIP" | "LoadBalancer" } }
type Name = Str where len(self) <= 5
let team = "shop"
fn labels(app: Str) -> { [Str]: Str } = { app, team }
fn app(name: Name, replicas: Int = 1) -> [Res] = [
  { kind: "Deployment", metadata: { name, labels: labels(name) }, spec.replicas: replicas },
  { kind: "Service", metadata.name: name, spec.type: "ClusterIP" },
]
"#;

fn module_dir(name: &str) -> PathBuf {
    fixture_dir(name, &[("lib.cleat", LIB)])
}

#[test]
fn modules_export_functions_constants_and_types() {
    let dir = module_dir("mod-ok");
    let src = "import lib from \"lib.cleat\"\nlet t: Str = lib.team\nlet r: lib.Res = { kind: \"Service\", metadata.name: \"x\" }\nexport lib.app(\"api\", replicas: 2) with {\n  [kind: \"Deployment\"].metadata.labels +: { tier: \"prod\" },\n  [kind: \"Service\"].spec.type: \"LoadBalancer\",\n}";
    let Ok((_, comps)) = export_at(src, Some(&dir), vec![]) else { panic!("{:?}", check_source_at(src, Some(&dir)).errors) };
    let out = emit::to_yaml_stream(&targets::plain(&comps));
    assert_eq!(
        out,
        "kind: Deployment\nmetadata:\n  name: api\n  labels:\n    app: api\n    team: shop\n    tier: prod\nspec:\n  replicas: 2\n---\nkind: Service\nmetadata:\n  name: api\nspec:\n  type: LoadBalancer\n"
    );
}

#[test]
fn module_errors_are_reported_at_the_importer() {
    let dir = module_dir("mod-err");
    let errs = |body: &str| {
        check_source_at(&format!("import lib from \"lib.cleat\"\n{body}"), Some(&dir))
            .errors
            .into_iter()
            .map(|e| e.msg)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(errs("let a = lib.ap(\"x\")").contains("unknown function `lib.ap`; did you mean `lib.app`?"));
    assert!(errs("let a = lib.teem").contains("module `lib` has no value `teem`; did you mean `lib.team`?"));
    assert!(errs("let a = lib.app(\"toolong\")").contains("refinement violated"));
    assert!(errs("let a = lib.app(\"x\") with { [kind: \"Deploymnt\"].spec.replicas: 3 }").contains("can match `[kind: \"Deploymnt\"]`"));
    assert!(errs("let a = lib.app(\"x\") with { [kind: \"Service\"].spec.type: \"NodePort\" }").contains("type mismatch"));
    // A selector patch on one kind leaves the other kinds patchable.
    assert_eq!(errs("let a = lib.app(\"x\") with { [kind: \"Deployment\"].spec.replicas: 2, [kind: \"Service\"].spec.type: \"ClusterIP\" }"), "");

    let dir = fixture_dir(
        "mod-bad",
        &[
            ("broken.cleat", "fn f() -> Int = \"no\""),
            ("input.cleat", "input x: Int"),
            ("a.cleat", "import b from \"b.cleat\"\nfn f() -> Int = 1"),
            ("b.cleat", "import a from \"a.cleat\"\nfn g() -> Int = 1"),
        ],
    );
    let errs = |body: &str| check_source_at(body, Some(&dir)).errors.into_iter().map(|e| e.msg).collect::<Vec<_>>().join("\n");
    assert!(errs("import m from \"broken.cleat\"").contains("broken.cleat:1:17: type mismatch"));
    assert!(errs("import m from \"input.cleat\"").contains("a module cannot declare `input`s"));
    assert!(errs("import m from \"a.cleat\"").contains("import cycle"));
}
