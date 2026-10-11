use std::process::ExitCode;

use cleat::targets::{self, FluxOptions};
use cleat::vc::VcStatus;
use cleat::{ExportError, InputValue, Outcome, Value, emit, json};

const USAGE: &str = "\
usage:
  cleat check  <file.cleat> [--vcs]
  cleat export <file.cleat> [options]
  cleat view   <file.cleat>  the exported resources in a browser, live
  cleat diff   <file.cleat>  what the output changes compared with a git revision
  cleat gen <schema>         print Cleat types for an OpenAPI document or CRD file
  cleat lsp                  language server on stdio (used by editors)
  cleat argocd generate      Argo CD plugin: export $ARGOCD_ENV_CLEAT_FILE (default main.cleat)
  cleat argocd params        Argo CD plugin: announce the file's inputs as parameters

options:
  --stdin                  read the source from stdin (the file name still places imports)
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
  --flux-prefix <prefix>   prefix for Kustomization names (flux)

view options (also: --input, --set, --target plain|argocd):
  --port <n>               port on 127.0.0.1 (default: any free port)
  --no-open                don't open a browser
  --out <file.html>        write a self-contained snapshot instead of serving
  --diff <rev>             mark what changed compared with a git revision (branch, tag, commit)

diff options (also: --input, --set, --target plain|argocd):
  --base <rev>             the git revision to compare with (required)
  --exit-code              exit with status 1 when anything changed";

struct Opts {
    file: String,
    /// Read the source from stdin; `file` still decides where imports resolve (editor previews).
    stdin: bool,
    /// Imports must stay inside this directory (default: see `project_root`).
    root: Option<String>,
    vcs: bool,
    inputs: Vec<(String, InputValue)>,
    target: String,
    json: bool,
    out: Option<String>,
    flux: FluxOptions,
    port: u16,
    open: bool,
    /// `view --diff` / `diff --base`: the git revision to compare with.
    base_rev: Option<String>,
    exit_code: bool,
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
        let root = cleat::find_root(std::path::Path::new(file));
        let c = cleat::complete::complete(
            &src,
            line.parse().unwrap_or(0),
            col.parse().unwrap_or(0),
            base_dir(file).as_deref(),
            root.as_deref(),
        );
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
        return match cleat::generate_source(std::path::Path::new(path)) {
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
    let read = if opts.stdin {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map(|_| s)
    } else {
        std::fs::read_to_string(&opts.file)
    };
    let src = match read {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", opts.file);
            return ExitCode::from(2);
        }
    };
    match cmd.as_str() {
        "check" => check(&opts, &src),
        "export" => export(opts, &src),
        "view" => view(opts),
        "diff" => diff(opts),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        file: String::new(),
        stdin: false,
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
        port: 0,
        open: true,
        base_rev: None,
        exit_code: false,
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
                o.inputs.extend(cleat::read_inputs_file(std::path::Path::new(&path))?);
            }
            "--stdin" => o.stdin = true,
            "--set" => {
                let kv = val()?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or(format!("--set expects name=value, got `{kv}`"))?;
                o.inputs.push((k.to_string(), InputValue::Raw(v.to_string())));
            }
            "--target" => o.target = val()?,
            "--root" => o.root = Some(val()?),
            "--out" => o.out = Some(val()?),
            "--flux-source" => o.flux.source = val()?,
            "--flux-path" => o.flux.path = val()?,
            "--flux-namespace" => o.flux.namespace = val()?,
            "--flux-prefix" => o.flux.prefix = val()?,
            "--port" => o.port = val()?.parse().map_err(|_| "--port expects a port number".to_string())?,
            "--no-open" => o.open = false,
            "--diff" | "--base" => o.base_rev = Some(val()?),
            "--exit-code" => o.exit_code = true,
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
    let Ok(raw) = std::env::var("ARGOCD_APP_PARAMETERS") else {
        return Ok(Vec::new());
    };
    let Value::List(params) = json::parse(&raw).map_err(|e| format!("ARGOCD_APP_PARAMETERS: {e}"))? else {
        return Err("ARGOCD_APP_PARAMETERS must be a JSON array".into());
    };
    let mut out = Vec::new();
    for p in params {
        let Some(Value::Str(name)) = p.get("name") else {
            continue;
        };
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
    if out.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
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
        let v = if docs.len() == 1 {
            docs[0].clone()
        } else {
            Value::List(docs)
        };
        println!("{}", emit::to_json(&v, true));
    } else {
        print!("{}", emit::to_yaml_stream(&docs));
    }
    ExitCode::SUCCESS
}

fn view(opts: Opts) -> ExitCode {
    if !matches!(opts.target.as_str(), "plain" | "argocd") {
        eprintln!("error: cleat view supports --target plain or argocd");
        return ExitCode::from(2);
    }
    let root = project_root(&opts);
    let diff = match &opts.base_rev {
        None => None,
        Some(rev) => match cleat::compare::Base::new(std::path::Path::new(&opts.file), rev) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("error: --diff: {e}");
                return ExitCode::from(2);
            }
        },
    };
    let cfg = cleat::view::ViewConfig {
        file: std::path::PathBuf::from(&opts.file),
        root,
        inputs: opts.inputs,
        target: opts.target,
        diff,
    };
    if let Some(out) = &opts.out {
        return match std::fs::write(out, cleat::view::snapshot(&cfg)) {
            Ok(()) => {
                eprintln!("wrote {out}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: cannot write {out}: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let open = opts.open;
    let res = cleat::view::serve(&cfg, opts.port, |url| {
        eprintln!("Viewing {} at {url} (Ctrl-C to stop)", opts.file);
        if open {
            open_browser(url);
        }
    });
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: cannot serve: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `cleat diff`: what the working tree's output changes compared with a git revision.
fn diff(opts: Opts) -> ExitCode {
    use cleat::compare::{Base, compare, evaluate, field, resource_key};
    let Some(rev) = &opts.base_rev else {
        eprintln!("error: cleat diff needs --base <rev>");
        return ExitCode::from(2);
    };
    if !matches!(opts.target.as_str(), "plain" | "argocd") {
        eprintln!("error: cleat diff supports --target plain or argocd");
        return ExitCode::from(2);
    }
    let file = std::path::Path::new(&opts.file);
    let base = match Base::new(file, rev) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let root = project_root(&opts);
    let src = std::fs::read_to_string(file).unwrap_or_default();
    let root_s = root.as_ref().map(|r| r.to_string_lossy().into_owned());
    let head = evaluate(
        &cleat::session(),
        &opts.file,
        &src,
        root_s.as_deref(),
        opts.inputs.clone(),
        &opts.target,
        false,
    );
    let b = base.evaluate(file, root.as_deref(), &opts.inputs, &opts.target);
    let print_errors = |which: &str, errors: &Value| {
        if let Value::List(es) = errors {
            for e in es {
                let at = match (e.get("line"), e.get("col")) {
                    (Some(Value::Int(l)), Some(Value::Int(c))) if *l > 0 => format!("{}:{l}:{c}: ", opts.file),
                    _ => String::new(),
                };
                let msg = match e.get("msg") {
                    Some(Value::Str(m)) => m.as_str(),
                    _ => "",
                };
                eprintln!("{at}error ({which}): {msg}");
            }
        }
    };
    let mut failed = false;
    if let Some(e) = field(&head, "errors") {
        print_errors("working tree", e);
        failed = true;
    }
    if let Some(e) = field(&b, "errors") {
        print_errors(&base.label, e);
        failed = true;
    }
    if failed {
        return ExitCode::from(2);
    }
    let list = |f: &[(&'static str, Value)]| match field(f, "resources") {
        Some(Value::List(rs)) => rs.clone(),
        _ => Vec::new(),
    };
    let mut head_rs = list(&head);
    let removed = compare(&mut head_rs, &list(&b));
    let color = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let paint = |code: &str, text: &str| {
        if color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    };
    let label = |r: &Value| {
        let k = r.get("doc").map(resource_key).unwrap_or_default();
        let mut parts = k.splitn(3, '/');
        let (kind, ns, name) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        if ns.is_empty() {
            format!("{kind} {name}")
        } else {
            format!("{kind} {ns}/{name}")
        }
    };
    let change = |r: &Value| match r.get("change") {
        Some(Value::Str(c)) => c.clone(),
        _ => String::new(),
    };
    let (mut added, mut changed, mut same) = (0, 0, 0);
    let mut details = String::new();
    println!("Comparing {} with {}:", opts.file, base.label);
    if matches!(field(&b, "missing"), Some(Value::Bool(true))) {
        println!("  ({} doesn't exist at {rev}, so everything is new)", opts.file);
    }
    for r in &head_rs {
        match change(r).as_str() {
            "added" => {
                added += 1;
                println!("  {}", paint("32", &format!("+ {}", label(r))));
            }
            "changed" => {
                changed += 1;
                println!("  {}", paint("33", &format!("~ {}", label(r))));
                details.push_str(&format!("\n{}\n", paint("1", &format!("~ {}", label(r)))));
                if let Some(Value::Str(d)) = r.get("diff") {
                    for line in d.lines() {
                        details.push_str(&match line.chars().next() {
                            Some('+') => paint("32", line),
                            Some('-') => paint("31", line),
                            Some('@') => paint("36", line),
                            _ => line.to_string(),
                        });
                        details.push('\n');
                    }
                }
            }
            _ => same += 1,
        }
    }
    for r in &removed {
        println!("  {}", paint("31", &format!("- {}", label(r))));
    }
    let n = added + changed + removed.len();
    if n == 0 {
        println!(
            "  no changes ({same} resource{} unchanged)",
            if same == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "{added} added, {changed} changed, {} removed ({same} unchanged)",
            removed.len()
        );
    }
    print!("{details}");
    if opts.exit_code && n > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Best effort: the URL is printed either way.
fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn base_dir(file: &str) -> Option<std::path::PathBuf> {
    std::path::Path::new(file).parent().map(|p| {
        if p.as_os_str().is_empty() {
            ".".into()
        } else {
            p.to_path_buf()
        }
    })
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
                stdin: false,
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
                port: 0,
                open: false,
                base_rev: None,
                exit_code: false,
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
                            p.push((
                                "map".into(),
                                Value::Obj(es.iter().map(|(k, v)| (k.clone(), s(&as_str(v)))).collect()),
                            ));
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
        None => cleat::find_root(std::path::Path::new(&opts.file)),
    }
}

/// In Argo CD the working directory is the app's path inside the repository
/// checkout; the checkout is the root (so `apps/shop` may import `../../schemas`).
fn argocd_root() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let depth = std::env::var("ARGOCD_APP_SOURCE_PATH")
        .map(|p| {
            std::path::Path::new(&p)
                .components()
                .filter(|c| matches!(c, std::path::Component::Normal(_)))
                .count()
        })
        .unwrap_or(0);
    cwd.ancestors().nth(depth).map(std::path::Path::to_path_buf)
}
