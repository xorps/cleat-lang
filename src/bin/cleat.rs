use std::process::ExitCode;

use cleat::targets::{self, FluxOptions};
use cleat::vc::VcStatus;
use cleat::{ExportError, InputValue, Outcome, Value, emit, json};

const USAGE: &str = "\
usage:
  cleat check  <file.cleat> [--vcs]
  cleat export <file.cleat> [options]
  cleat lsp                  language server on stdio (used by editors)

export options:
  --input <file.json>      input values as a JSON object
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
                match json::parse(&text).map_err(|e| format!("{path}: {e}"))? {
                    Value::Obj(es) => o.inputs.extend(es.into_iter().map(|(k, v)| (k, InputValue::Json(v)))),
                    _ => return Err(format!("{path}: inputs must be a JSON object")),
                }
            }
            "--set" => {
                let kv = val()?;
                let (k, v) = kv.split_once('=').ok_or(format!("--set expects name=value, got `{kv}`"))?;
                o.inputs.push((k.to_string(), InputValue::Raw(v.to_string())));
            }
            "--target" => o.target = val()?,
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
    let out = cleat::check_source(src);
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
    let (outcome, comps) = match cleat::export(src, opts.inputs) {
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
