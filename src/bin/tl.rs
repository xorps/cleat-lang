use std::process::ExitCode;

use test_lang::vc::VcStatus;

const USAGE: &str = "usage: tl check <file.tl> [--vcs]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show_vcs = args.iter().any(|a| a == "--vcs");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [cmd, path] = positional.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if cmd.as_str() != "check" {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let out = test_lang::check_source(&src);
    for w in &out.warnings {
        eprintln!("{path}:{}: warning: {}", w.span, w.msg);
    }
    for e in &out.errors {
        eprintln!("{path}:{}: error: {}", e.span, e.msg);
    }
    if show_vcs {
        for vc in &out.vcs {
            println!("{vc}\n");
        }
    }
    let count = |s| out.vcs.iter().filter(|v| v.status == s).count();
    println!(
        "{} error(s), {} warning(s); {} VC(s): {} proved, {} pending (no SMT backend yet), {} refuted",
        out.errors.len(),
        out.warnings.len(),
        out.vcs.len(),
        count(VcStatus::Trivial),
        count(VcStatus::Pending),
        count(VcStatus::Refuted),
    );
    if out.errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
