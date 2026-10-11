use std::process::ExitCode;

use cleat::targets::{self, FluxOptions};
use cleat::vc::VcStatus;
use cleat::{ExportError, InputValue, Outcome, Value, emit, json};

const USAGE: &str = "\
usage:
  cleat check  <file.cleat> [--vcs]
  cleat export <file.cleat> [options]
  cleat gen <schema>         print Cleat types for an OpenAPI document or CRD file
  cleat lsp                  language server on stdio (used by editors)
  cleat argocd generate      Argo CD plugin: export $ARGOCD_ENV_CLEAT_FILE (default main.cleat)
  cleat argocd params        Argo CD plugin: announce the file's inputs as parameters

options:
  --root <dir>             imports must stay inside this directory
                           (default: nearest directory with .git, else the file's directory)

export options:
  --input <file>           input values: a JSON or YAML object
  --set <name>=<value>     one input (decoded by its declared type); repeatable
  --argocd-params          read inputs from $ARGOCD_APP_PARAMETERS (Argo CD plugin)
  --target <t>             plain (default) | argocd (sync waves) | flux (needs --out)
  --json                   emit JSON instead of YAML
  --out <dir>              output directory (flux)
  --flux-source <name>     GitRepository name (flux; required)
  --flux-path <path>       repository path of --out (flux; default: .)
  --flux-namespace <ns>    namespace for Kustomizations (flux; default: flux-system)
  --flux-prefix <prefix>   prefix for Kustomization names (flux)";

struct Opts {
    file: String,
    /// Imports must stay inside this directory (default: see `project_root`).
    root: Option<String>,
    vcs: bool,
    inputs: Vec<(String, InputValue)>,
    target: String,
    json: bool,
    out: Option<String>,
    flux: FluxOptions,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if cmd == "complete" {
        // Debugging aid for editor integrations: cleat complete <file> <line> <col> (0-based).
        let (Some(file), Some(line), Some(col)) = (args.get(1), args.get(2), args.get(3)) else {
            eprintln!("usage: cleat complete <file> <line> <col>");
            return ExitCode::from(2);
        };
        let src = std::fs::read_to_string(file).unwrap_or_default();
        let root = cleat::modules::find_root(std::path::Path::new(file));
        let c = cleat::complete::complete(&src, line.parse().unwrap_or(0), col.parse().unwrap_or(0), base_dir(file).as_deref(), root.as_deref());
        match c {
            Some(c) => {
                for i in c.items {
                    println!("{:<24} {:?}  {}", i.insert, i.kind, i.detail.unwrap_or_default());
                }
            }
            None => println!("(no completions)"),
        }
        return ExitCode::SUCCESS;
    }
    if cmd == "argocd" {
        return argocd(args.get(1).map(String::as_str));
    }
    if cmd == "gen" {
        let Some(path) = args.get(1) else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        return match cleat::modules::generate_source(std::path::Path::new(path)) {
            Ok(src) => {
                print!("{src}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if cmd == "lsp" {
        return match cleat::lsp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cleat lsp: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let opts = match parse_opts(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let src = match std::fs::read_to_string(&opts.file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", opts.file);
            return ExitCode::from(2);
        }
    };
    match cmd.as_str() {
        "check" => check(&opts, &src),
        "export" => export(opts, &src),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        file: String::new(),
        root: None,
        vcs: false,
        inputs: Vec::new(),
        target: "plain".into(),
        json: false,
        out: None,
        flux: FluxOptions {
            source: String::new(),
            path: ".".into(),
            namespace: "flux-system".into(),
            prefix: String::new(),
            interval: "10m".into(),
        },
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--vcs" => o.vcs = true,
            "--json" => o.json = true,
            "--argocd-params" => o.inputs.extend(argocd_params()?),
            "--input" => {
                let path = val()?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
                let value = if path.ends_with(".yaml") || path.ends_with(".yml") {
                    match cleat::yaml::parse_docs(&text).map_err(|e| format!("{path}: {e}"))?.as_slice() {
                        [v] => v.clone(),
                        _ => return Err(format!("{path}: expected exactly one YAML document")),
                    }
                } else {
                    json::parse(&text).map_err(|e| format!("{path}: {e}"))?
                };
                match value {
                    Value::Obj(es) => o.inputs.extend(es.into_iter().map(|(k, v)| (k, InputValue::Json(v)))),
                    _ => return Err(format!("{path}: inputs must be an object")),
                }
            }
            "--set" => {
                let kv = val()?;
                let (k, v) = kv.split_once('=').ok_or(format!("--set expects name=value, got `{kv}`"))?;
                o.inputs.push((k.to_string(), InputValue::Raw(v.to_string())));
            }
            "--target" => o.target = val()?,
            "--root" => o.root = Some(val()?),
            "--out" => o.out = Some(val()?),
            "--flux-source" => o.flux.source = val()?,
            "--flux-path" => o.flux.path = val()?,
            "--flux-namespace" => o.flux.namespace = val()?,
            "--flux-prefix" => o.flux.prefix = val()?,
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s if o.file.is_empty() => o.file = s.to_string(),
            s => return Err(format!("unexpected argument {s}")),
        }
    }
    if o.file.is_empty() {
        return Err("missing <file.cleat>".into());
    }
    Ok(o)
}

/// Argo CD passes plugin parameters as `[{"name": .., "string" | "array" | "map": ..}]`.
fn argocd_params() -> Result<Vec<(String, InputValue)>, String> {
    let Ok(raw) = std::env::var("ARGOCD_APP_PARAMETERS") else { return Ok(Vec::new()) };
    let Value::List(params) = json::parse(&raw).map_err(|e| format!("ARGOCD_APP_PARAMETERS: {e}"))? else {
        return Err("ARGOCD_APP_PARAMETERS must be a JSON array".into());
    };
    let mut out = Vec::new();
    for p in params {
        let Some(Value::Str(name)) = p.get("name") else { continue };
        let strs = |v: &Value| match v {
            Value::Str(s) => s.clone(),
            other => other.to_string(),
        };
        let iv = if let Some(Value::Str(s)) = p.get("string") {
            InputValue::Raw(s.clone())
        } else if let Some(Value::List(xs)) = p.get("array") {
            InputValue::RawList(xs.iter().map(strs).collect())
        } else if let Some(Value::Obj(es)) = p.get("map") {
            InputValue::RawMap(es.iter().map(|(k, v)| (k.clone(), strs(v))).collect())
        } else {
            continue;
        };
        out.push((name.clone(), iv));
    }
    Ok(out)
}

fn print_diags(file: &str, out: &Outcome) {
    for w in &out.warnings {
        eprintln!("{file}:{}: warning: {}", w.span, w.msg);
    }
    for e in &out.errors {
        eprintln!("{file}:{}: error: {}", e.span, e.msg);
    }
}

fn check(opts: &Opts, src: &str) -> ExitCode {
    let root = project_root(opts);
    let out = cleat::check_source_in(src, base_dir(&opts.file).as_deref(), root.as_deref());
    print_diags(&opts.file, &out);
    if opts.vcs {
        for vc in &out.vcs {
            println!("{vc}\n");
        }
    }
    let count = |s| out.vcs.iter().filter(|v| v.status == s).count();
    eprintln!(
        "{} error(s), {} warning(s); refinements: {} proved statically, {} checked at runtime",
        out.errors.len(),
        out.warnings.len(),
        count(VcStatus::Proved),
        count(VcStatus::Runtime),
    );
    if out.errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn export(opts: Opts, src: &str) -> ExitCode {
    let file = &opts.file;
    let base = base_dir(file);
    let root = project_root(&opts);
    let (outcome, comps) = match cleat::export_in(src, base.as_deref(), root.as_deref(), opts.inputs) {
        Ok(r) => r,
        Err(ExportError::Check(out)) => {
            print_diags(file, &out);
            return ExitCode::FAILURE;
        }
        Err(ExportError::Eval(errs)) => {
            for e in &errs {
                if e.span.line == 0 {
                    eprintln!("error: {}", e.msg);
                } else {
                    eprintln!("{file}:{}: error: {}", e.span, e.msg);
                }
                for (f, at) in e.trace.iter().rev() {
                    eprintln!("    in call to `{f}` at {file}:{at}");
                }
            }
            return ExitCode::FAILURE;
        }
    };
    print_diags(file, &outcome);

    let docs = match opts.target.as_str() {
        "plain" => targets::plain(&comps),
        "argocd" => match targets::argocd(&comps) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        "flux" => {
            let (Some(dir), false) = (&opts.out, opts.flux.source.is_empty()) else {
                eprintln!("error: --target flux needs --out <dir> and --flux-source <name>");
                return ExitCode::from(2);
            };
            for (rel, content) in targets::flux(&comps, &opts.flux) {
                let path = std::path::Path::new(dir).join(&rel);
                let res = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|_| std::fs::write(&path, content));
                if let Err(e) = res {
                    eprintln!("error: cannot write {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
                eprintln!("wrote {}", path.display());
            }
            return ExitCode::SUCCESS;
        }
        t => {
            eprintln!("error: unknown target `{t}` (expected plain, argocd or flux)");
            return ExitCode::from(2);
        }
    };
    if opts.json {
        let v = if docs.len() == 1 { docs[0].clone() } else { Value::List(docs) };
        println!("{}", emit::to_json(&v, true));
    } else {
        print!("{}", emit::to_yaml_stream(&docs));
    }
    ExitCode::SUCCESS
}

fn base_dir(file: &str) -> Option<std::path::PathBuf> {
    std::path::Path::new(file).parent().map(|p| if p.as_os_str().is_empty() { ".".into() } else { p.to_path_buf() })
}

// ---------- Argo CD config management plugin ----------
//
// Argo runs these in the application's source directory. Settings come from the
// Application's `plugin.env` (prefixed with ARGOCD_ENV_ by Argo):
//   CLEAT_FILE    entry file (default: main.cleat)
//   CLEAT_TARGET  output target (default: argocd, i.e. with sync waves)

fn argocd(sub: Option<&str>) -> ExitCode {
    let file = std::env::var("ARGOCD_ENV_CLEAT_FILE").unwrap_or_else(|_| "main.cleat".into());
    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cleat: cannot read {file}: {e} (set CLEAT_FILE in the Application's plugin env)");
            return ExitCode::FAILURE;
        }
    };
    match sub {
        Some("generate") => {
            let inputs = match argocd_params() {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("cleat: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let target = std::env::var("ARGOCD_ENV_CLEAT_TARGET").unwrap_or_else(|_| "argocd".into());
            let opts = Opts {
                root: argocd_root().map(|p| p.display().to_string()),
                file,
                vcs: false,
                inputs,
                target,
                json: false,
                out: None,
                flux: FluxOptions {
                    source: String::new(),
                    path: ".".into(),
                    namespace: "flux-system".into(),
                    prefix: String::new(),
                    interval: "10m".into(),
                },
            };
            export(opts, &src)
        }
        Some("params") => {
            let infos = match cleat::inputs(&src, base_dir(&file).as_deref(), argocd_root().as_deref()) {
                Ok(i) => i,
                Err(out) => {
                    print_diags(&file, &out);
                    return ExitCode::FAILURE;
                }
            };
            let s = |x: &str| Value::Str(x.to_string());
            let as_str = |v: &Value| match v {
                Value::Str(x) => x.clone(),
                other => other.to_string(),
            };
            let params: Vec<Value> = infos
                .iter()
                .map(|i| {
                    let mut tooltip = format!("Type: {}", i.ty);
                    if let Some(d) = &i.definition {
                        tooltip.push_str(&format!(" = {d}"));
                    }
                    let mut p = vec![
                        ("name".to_string(), s(&i.name)),
                        ("title".to_string(), s(&i.name)),
                        ("tooltip".to_string(), s(&tooltip)),
                        ("required".to_string(), Value::Bool(i.required)),
                        ("itemType".to_string(), s("string")),
                        ("collectionType".to_string(), s(i.collection)),
                    ];
                    match (&i.default, i.collection) {
                        (Some(Value::List(xs)), "array") => {
                            p.push(("array".into(), Value::List(xs.iter().map(|x| s(&as_str(x))).collect())));
                        }
                        (Some(Value::Obj(es)), "map") => {
                            p.push(("map".into(), Value::Obj(es.iter().map(|(k, v)| (k.clone(), s(&as_str(v)))).collect())));
                        }
                        (Some(v), "string") => p.push(("string".into(), s(&as_str(v)))),
                        _ => {}
                    }
                    Value::Obj(p)
                })
                .collect();
            println!("{}", emit::to_json(&Value::List(params), false));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: cleat argocd generate | cleat argocd params");
            ExitCode::from(2)
        }
    }
}

fn project_root(opts: &Opts) -> Option<std::path::PathBuf> {
    match &opts.root {
        Some(r) => Some(r.into()),
        None => cleat::modules::find_root(std::path::Path::new(&opts.file)),
    }
}

/// In Argo CD the working directory is the app's path inside the repository
/// checkout; the checkout is the root (so `apps/shop` may import `../../schemas`).
fn argocd_root() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let depth = std::env::var("ARGOCD_APP_SOURCE_PATH")
        .map(|p| std::path::Path::new(&p).components().filter(|c| matches!(c, std::path::Component::Normal(_))).count())
        .unwrap_or(0);
    cwd.ancestors().nth(depth).map(std::path::Path::to_path_buf)
}
