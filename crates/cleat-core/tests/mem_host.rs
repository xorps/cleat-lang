//! The core with no file system: everything is read through an in-memory host.

use std::rc::Rc;

use cleat_core::{ExportError, MemHost, Session, Value, check_source, emit, export};

fn session(files: &[(&str, &str)]) -> (Rc<MemHost>, Session) {
    let host = Rc::new(MemHost::new());
    for (name, text) in files {
        host.set(name, *text);
    }
    (host.clone(), Session::new(host))
}

fn errors(sess: &Session, src: &str) -> Vec<String> {
    check_source(sess, src, Some("/app"), Some("/"))
        .errors
        .into_iter()
        .map(|d| d.msg)
        .collect()
}

#[test]
fn modules_and_data_files_import_from_memory() {
    let (_, sess) = session(&[
        ("/lib/names.cleat", "fn greet(n: Str) -> Str = \"hello \\(n)\""),
        ("/app/data.json", r#"{ "who": "world" }"#),
    ]);
    let src = r#"
import * as names from "../lib/names.cleat"
import data from "./data.json" with { type: "json" }
export { msg: names.greet(data.who) }
"#;
    let (_, comps) = export(&sess, src, Some("/app"), Some("/"), vec![])
        .ok()
        .expect("exports");
    assert_eq!(emit::to_yaml(&comps[0].docs[0]), "msg: hello world\n");
}

#[test]
fn imports_stay_inside_the_root() {
    let (_, sess) = session(&[("/secret.cleat", "let x = 1")]);
    let src = "import * as s from \"../secret.cleat\"\nexport { a: 1 }";
    let errs = check_source(&sess, src, Some("/app"), Some("/app")).errors;
    assert!(errs[0].msg.contains("outside the project root `/app`"), "{errs:?}");
    let errs = errors(&sess, "import * as s from \"../../../x.cleat\"\nexport { a: 1 }");
    assert!(errs[0].contains("outside `/`"), "{errs:?}");
}

#[test]
fn import_cycles_are_reported() {
    let (_, sess) = session(&[
        ("/app/a.cleat", "import * as b from \"./b.cleat\"\nlet x = 1"),
        ("/app/b.cleat", "import * as a from \"./a.cleat\"\nlet y = 2"),
    ]);
    let errs = errors(&sess, "import * as a from \"./a.cleat\"\nexport { v: a.x }");
    assert!(
        errs.iter()
            .any(|e| e.contains("import cycle: /app/a.cleat -> /app/b.cleat -> /app/a.cleat")),
        "{errs:?}"
    );
}

#[test]
fn cached_modules_notice_changes() {
    let (host, sess) = session(&[("/app/lib.cleat", "let n = 1")]);
    let src = "import * as lib from \"./lib.cleat\"\nexport { v: lib.n }";
    assert!(errors(&sess, src).is_empty());
    host.set("/app/lib.cleat", "let m = 1");
    let errs = errors(&sess, src);
    assert!(errs.iter().any(|e| e.contains("`n`")), "{errs:?}");
    host.set("/app/lib.cleat", "let n = 2");
    let (_, comps) = export(&sess, src, Some("/app"), Some("/"), vec![])
        .ok()
        .expect("exports");
    assert_eq!(comps[0].docs[0], Value::Obj(vec![("v".into(), Value::Int(2))]));
}

#[test]
fn yaml_needs_a_host_that_parses_it() {
    let (_, sess) = session(&[("/app/v.yaml", "a: 1")]);
    let src = "import v from \"./v.yaml\" with { type: \"yaml\" }\nexport { a: v.a }";
    match export(&sess, src, Some("/app"), None, vec![]) {
        Err(ExportError::Check(o)) => assert!(o.errors[0].msg.contains("YAML is not supported here"), "{:?}", o.errors),
        _ => panic!("expected a check error"),
    }
}

#[test]
fn regexes_work_without_std() {
    let (_, sess) = session(&[]);
    let src = "export { a: matches(\"web-1\", \"^[a-z]+-[0-9]$\"), b: matches(\"x\", \"^y\") }";
    let (_, comps) = export(&sess, src, None, None, vec![]).ok().expect("exports");
    assert_eq!(emit::to_yaml(&comps[0].docs[0]), "a: true\nb: false\n");
    assert!(errors(&sess, "export { a: matches(\"x\", \"(\") }")[0].contains("invalid regex"));
}

#[test]
fn unified_diffs_of_rendered_documents() {
    use cleat_core::diff::unified;
    assert_eq!(unified("a\nb\n", "a\nb\n", 3), "");
    let old = "kind: Deployment\nspec:\n  replicas: 1\n  paused: false\nmetadata:\n  name: api\n";
    let new =
        "kind: Deployment\nspec:\n  replicas: 3\n  paused: false\nmetadata:\n  name: api\n  labels:\n    tier: web\n";
    // Three unchanged lines apart, more than twice the context: two hunks, as `diff -U1` gives.
    assert_eq!(
        unified(old, new, 1),
        "@@ -2,3 +2,3 @@\n spec:\n-  replicas: 1\n+  replicas: 3\n   paused: false\n\
         @@ -6,1 +6,3 @@\n   name: api\n+  labels:\n+    tier: web\n"
    );
    assert!(unified(old, new, 3).starts_with("@@ -1,6 +1,8 @@\n kind: Deployment\n"));
    // Far-apart changes become separate hunks.
    let a: String = (0..20).map(|i| format!("l{i}\n")).collect();
    let b = a.replace("l2\n", "x\n").replace("l17\n", "y\n");
    assert_eq!(unified(&a, &b, 1).matches("@@ -").count(), 2);
}
