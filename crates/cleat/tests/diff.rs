//! Comparing output with a git revision: `cleat diff` and `cleat view --diff`.

use std::path::{Path, PathBuf};
use std::process::Command;

use cleat::compare::{Base, compare, evaluate, field};
use cleat::{InputValue, Value};

/// A git repository with `files` committed on `main`, plus an ignored `schemas/` directory.
fn repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cleat-diff-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join(".gitignore"), "schemas/\n").unwrap();
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    dir.canonicalize().unwrap()
}

const LIB: &str = "fn labels(app: Str) -> { [Str]: Str } = { app }\n";
const MAIN: &str = r#"import * as lib from "./lib.cleat"
input image: Str
let cm = { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "settings", labels: lib.labels("api") }, data.image: image }
let old = { apiVersion: "v1", kind: "ConfigMap", metadata.name: "old" }
export [cm, old]
"#;

fn changes(dir: &Path, file: &str, inputs: &[(&str, &str)]) -> (Vec<(String, String)>, Vec<String>) {
    let inputs: Vec<(String, InputValue)> = inputs
        .iter()
        .map(|(k, v)| (k.to_string(), InputValue::Raw(v.to_string())))
        .collect();
    let path = dir.join(file);
    let src = std::fs::read_to_string(&path).unwrap();
    let name = path.to_string_lossy().into_owned();
    let root = dir.to_string_lossy().into_owned();
    let head = evaluate(
        &cleat::session(),
        &name,
        &src,
        Some(&root),
        inputs.clone(),
        "plain",
        false,
    );
    assert!(field(&head, "errors").is_none(), "{head:?}");
    let base = Base::new(&path, "main").unwrap();
    let b = base.evaluate(&path, Some(dir), &inputs, "plain");
    assert!(field(&b, "errors").is_none(), "{b:?}");
    let list = |f: &[(&'static str, Value)]| match field(f, "resources") {
        Some(Value::List(rs)) => rs.clone(),
        _ => vec![],
    };
    let mut head_rs = list(&head);
    let removed = compare(&mut head_rs, &list(&b));
    let name_of = |r: &Value| match r.get("doc").and_then(|d| d.get("metadata")).and_then(|m| m.get("name")) {
        Some(Value::Str(n)) => n.clone(),
        _ => String::new(),
    };
    let ch = head_rs
        .iter()
        .map(|r| {
            let c = match r.get("change") {
                Some(Value::Str(c)) => c.clone(),
                _ => String::new(),
            };
            (name_of(r), c)
        })
        .collect();
    (ch, removed.iter().map(name_of).collect())
}

fn pairs(xs: &[(&str, &str)]) -> Vec<(String, String)> {
    xs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn changes_are_found_through_imports() {
    let dir = repo("imports", &[("lib.cleat", LIB), ("main.cleat", MAIN)]);
    let (ch, removed) = changes(&dir, "main.cleat", &[("image", "a")]);
    assert_eq!(ch, pairs(&[("settings", "same"), ("old", "same")]));
    assert!(removed.is_empty());
    // A change in an imported module, a removed and an added document.
    std::fs::write(
        dir.join("lib.cleat"),
        "fn labels(app: Str) -> { [Str]: Str } = { app, team: \"x\" }\n",
    )
    .unwrap();
    let main = MAIN.replace(r#"metadata.name: "old""#, r#"metadata.name: "new""#);
    std::fs::write(dir.join("main.cleat"), main).unwrap();
    let (ch, removed) = changes(&dir, "main.cleat", &[("image", "a")]);
    assert_eq!(ch, pairs(&[("settings", "changed"), ("new", "added")]));
    assert_eq!(removed, ["old"]);
}

#[test]
fn ignored_files_come_from_the_working_tree_and_new_files_are_new() {
    let dir = repo(
        "ignored",
        &[(
            "main.cleat",
            "import d from \"schemas/d.json\" with { type: \"json\" }\nexport { kind: \"ConfigMap\", metadata.name: d.name }\n",
        )],
    );
    // Ignored, so not in the commit: read from the working tree on both sides.
    std::fs::write(dir.join("schemas/d.json"), r#"{ "name": "cm" }"#).unwrap();
    let (ch, _) = changes(&dir, "main.cleat", &[]);
    assert_eq!(ch, pairs(&[("cm", "same")]));
    // A new, uncommitted (not ignored) file isn't at the revision: everything in it is new.
    std::fs::write(
        dir.join("other.cleat"),
        "export { kind: \"ConfigMap\", metadata.name: \"x\" }\n",
    )
    .unwrap();
    let (ch, _) = changes(&dir, "other.cleat", &[]);
    assert_eq!(ch, pairs(&[("x", "added")]));
}

#[test]
fn inputs_only_the_working_tree_declares_are_left_out_of_the_base() {
    let dir = repo("inputs", &[("lib.cleat", LIB), ("main.cleat", MAIN)]);
    std::fs::write(dir.join("main.cleat"), format!("{MAIN}input region: Str\n")).unwrap();
    let (ch, _) = changes(&dir, "main.cleat", &[("image", "a"), ("region", "eu")]);
    assert_eq!(ch, pairs(&[("settings", "same"), ("old", "same")]));
}

#[test]
fn cleat_diff_prints_changes_and_can_fail_on_them() {
    let dir = repo("cli", &[("lib.cleat", LIB), ("main.cleat", MAIN)]);
    let run = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_cleat"))
            .current_dir(&dir)
            .args(["diff", "main.cleat", "--base", "main", "--set", "image=a"])
            .args(extra)
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code())
    };
    let (out, code) = run(&["--exit-code"]);
    assert!(out.contains("no changes (2 resources unchanged)"), "{out}");
    assert_eq!(code, Some(0));
    std::fs::write(
        dir.join("main.cleat"),
        MAIN.replace("data.image: image", "data.image: \"pinned\""),
    )
    .unwrap();
    let (out, code) = run(&["--exit-code"]);
    assert!(out.contains("  ~ ConfigMap settings\n"), "{out}");
    assert!(out.contains("0 added, 1 changed, 0 removed (1 unchanged)"), "{out}");
    assert!(out.contains("-  image: a\n+  image: pinned\n"), "{out}");
    assert_eq!(code, Some(1));
    // Option-like revisions are not passed to git as options.
    let out = Command::new(env!("CARGO_BIN_EXE_cleat"))
        .current_dir(&dir)
        .args(["diff", "main.cleat", "--base", "--output=/tmp/x", "--set", "image=a"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't a branch, tag or commit"));
}
