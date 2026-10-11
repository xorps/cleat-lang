//! Output targets: how components and their dependencies reach the cluster.
//!
//! - `plain`: all documents, dependencies first.
//! - `argocd`: like `plain`, with `argocd.argoproj.io/sync-wave` set per component.
//! - `flux`: one directory per component plus Flux `Kustomization`s with `dependsOn`.

use crate::emit;
use crate::eval::{Component, Value};
use crate::prelude::*;

pub const SYNC_WAVE: &str = "argocd.argoproj.io/sync-wave";

pub struct FluxOptions {
    /// Name of the Flux `GitRepository` the manifests are committed to.
    pub source: String,
    /// Repository path of the output directory, e.g. `deploy/prod`.
    pub path: String,
    pub namespace: String,
    /// Prefix for Kustomization names, e.g. `myapp-`.
    pub prefix: String,
    pub interval: String,
}

fn comp_name(c: &Component) -> &str {
    c.name.as_deref().unwrap_or("default")
}

/// Each component's wave: 0 without dependencies, else one more than its latest dependency.
/// The checker guarantees names resolve and there are no cycles.
pub fn waves(comps: &[Component]) -> Vec<i64> {
    fn wave(i: usize, comps: &[Component], memo: &mut Vec<Option<i64>>) -> i64 {
        if let Some(w) = memo[i] {
            return w;
        }
        let w = comps[i]
            .after
            .iter()
            .filter_map(|d| comps.iter().position(|c| c.name.as_deref() == Some(d)))
            .map(|j| wave(j, comps, memo) + 1)
            .max()
            .unwrap_or(0);
        memo[i] = Some(w);
        w
    }
    let mut memo = vec![None; comps.len()];
    (0..comps.len()).map(|i| wave(i, comps, &mut memo)).collect()
}

/// Components in dependency order (stable within a wave).
fn ordered(comps: &[Component]) -> Vec<(i64, &Component)> {
    let ws = waves(comps);
    let mut v: Vec<(i64, &Component)> = ws.into_iter().zip(comps).collect();
    v.sort_by_key(|(w, _)| *w);
    v
}

pub fn plain(comps: &[Component]) -> Vec<Value> {
    ordered(comps).into_iter().flat_map(|(_, c)| c.docs.clone()).collect()
}

pub fn argocd(comps: &[Component]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for (w, c) in ordered(comps) {
        for (i, doc) in c.docs.iter().enumerate() {
            let mut doc = doc.clone();
            set_annotation(&mut doc, SYNC_WAVE, &w.to_string())
                .map_err(|m| format!("component `{}`, document {i}: {m}", comp_name(c)))?;
            out.push(doc);
        }
    }
    Ok(out)
}

/// Set `metadata.annotations[key]` unless the document already sets it.
fn set_annotation(doc: &mut Value, key: &str, val: &str) -> Result<(), String> {
    let Value::Obj(es) = doc else {
        return Err("not a Kubernetes object".into());
    };
    let Some((_, Value::Obj(meta))) = es.iter_mut().find(|(k, _)| k == "metadata") else {
        return Err("not a Kubernetes object (no `metadata`)".into());
    };
    if !meta.iter().any(|(k, _)| k == "annotations") {
        meta.push(("annotations".into(), Value::Obj(Vec::new())));
    }
    let Some((_, Value::Obj(ann))) = meta.iter_mut().find(|(k, _)| k == "annotations") else {
        return Err("`metadata.annotations` is not an object".into());
    };
    if !ann.iter().any(|(k, _)| k == key) {
        ann.push((key.to_string(), Value::Str(val.to_string())));
    }
    Ok(())
}

/// Files to write, relative to the output directory.
pub fn flux(comps: &[Component], o: &FluxOptions) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut kustomizations = Vec::new();
    let s = |x: &str| Value::Str(x.to_string());
    let base = o.path.trim_matches('/');
    for (_, c) in ordered(comps) {
        let name = comp_name(c);
        files.push((format!("{name}/resources.yaml"), emit::to_yaml_stream(&c.docs)));
        let path = if base.is_empty() || base == "." {
            format!("./{name}")
        } else {
            format!("./{base}/{name}")
        };
        let mut spec = vec![
            ("interval".to_string(), s(&o.interval)),
            ("path".to_string(), s(&path)),
            ("prune".to_string(), Value::Bool(true)),
            // `wait` makes dependents wait until this component is healthy, not just applied.
            ("wait".to_string(), Value::Bool(true)),
            (
                "sourceRef".to_string(),
                Value::Obj(vec![("kind".into(), s("GitRepository")), ("name".into(), s(&o.source))]),
            ),
        ];
        if !c.after.is_empty() {
            let deps = c
                .after
                .iter()
                .map(|d| Value::Obj(vec![("name".into(), s(&format!("{}{d}", o.prefix)))]));
            spec.push(("dependsOn".to_string(), Value::List(deps.collect())));
        }
        kustomizations.push(Value::Obj(vec![
            ("apiVersion".into(), s("kustomize.toolkit.fluxcd.io/v1")),
            ("kind".into(), s("Kustomization")),
            (
                "metadata".into(),
                Value::Obj(vec![
                    ("name".into(), s(&format!("{}{name}", o.prefix))),
                    ("namespace".into(), s(&o.namespace)),
                ]),
            ),
            ("spec".into(), Value::Obj(spec)),
        ]));
    }
    files.push((
        "flux-kustomizations.yaml".to_string(),
        emit::to_yaml_stream(&kustomizations),
    ));
    files
}
