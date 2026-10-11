//! Generate Cleat type declarations from OpenAPI documents (Swagger 2 /
//! OpenAPI 3) and Kubernetes CustomResourceDefinitions.
//!
//! The output is Cleat source, so it can be printed (`cleat gen`), vendored,
//! and checked like any other module.

use crate::prelude::*;

use crate::eval::Value;

pub struct Generated {
    /// Cleat source with `type` declarations (module-local names).
    pub source: String,
    /// Documentation: (`Type` or `Type.field`, text).
    pub docs: Vec<(String, String)>,
}

/// Generate types from parsed documents: one OpenAPI document, or any number of CRDs.
pub fn generate(docs: &[Value]) -> Result<Generated, String> {
    if let [doc] = docs
        && (doc.get("swagger").is_some() || doc.get("openapi").is_some())
    {
        return from_openapi(doc);
    }
    let mut crds = Vec::new();
    for d in docs {
        match str_of(d.get("kind")) {
            Some("CustomResourceDefinition") => crds.push(d),
            Some("List") | Some("CustomResourceDefinitionList") => {
                if let Some(Value::List(items)) = d.get("items") {
                    crds.extend(
                        items
                            .iter()
                            .filter(|i| str_of(i.get("kind")) == Some("CustomResourceDefinition")),
                    );
                }
            }
            _ => {}
        }
    }
    if crds.is_empty() {
        return Err("expected an OpenAPI document (with `swagger` or `openapi`) or CustomResourceDefinitions".into());
    }
    from_crds(&crds)
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

const KEYWORDS: [&str; 16] = [
    "type", "input", "let", "fn", "export", "if", "then", "else", "match", "for", "in", "where", "true", "false",
    "null", "import",
];

const BUILTIN: [&str; 6] = ["Never", "Json", "Bool", "Int", "Float", "Str"];

fn ident(seg: &str) -> String {
    let mut s: String = seg
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if KEYWORDS.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(if up { c.to_ascii_uppercase() } else { c });
            up = false;
        } else {
            up = true;
        }
    }
    out
}

/// A Cleat string literal (also valid as a literal type).
fn lit_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn key(k: &str) -> String {
    let plain = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain { k.to_string() } else { lit_str(k) }
}

/// Stability rank of a version segment: (GA=2/beta=1/alpha=0, major, minor). Higher is preferred.
type VersionRank = (u8, u32, u32);

fn version_rank(v: &str) -> Option<VersionRank> {
    let rest = v.strip_prefix('v')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let major = digits.parse().ok()?;
    let tail = &rest[digits.len()..];
    let (stability, minor) = if tail.is_empty() {
        (2, 0)
    } else if let Some(n) = tail.strip_prefix("beta") {
        (1, n.parse().unwrap_or(0))
    } else {
        let n = tail.strip_prefix("alpha")?;
        (0, n.parse().unwrap_or(0))
    };
    Some((stability, major, minor))
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > 120 {
        format!("{}…", line.chars().take(117).collect::<String>())
    } else {
        line.to_string()
    }
}

struct Gen {
    ref_prefix: String,
    /// Definition key -> local type name.
    names: BTreeMap<String, String>,
    /// References (from, to) that close a cycle; rendered as `Json`.
    broken: BTreeSet<(String, String)>,
    cur_def: String,
    /// Name inline objects as their own types (CRDs inline everything).
    name_nested: bool,
    taken: BTreeSet<String>,
    types: Vec<(String, String, Option<String>)>,
    docs: Vec<(String, String)>,
}

impl Gen {
    fn new(ref_prefix: &str, name_nested: bool) -> Self {
        Gen {
            ref_prefix: ref_prefix.to_string(),
            names: BTreeMap::new(),
            broken: BTreeSet::new(),
            cur_def: String::new(),
            name_nested,
            taken: BTreeSet::new(),
            types: Vec::new(),
            docs: Vec::new(),
        }
    }

    fn fresh(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut i = 2;
        while self.taken.contains(&name) {
            name = format!("{base}{i}");
            i += 1;
        }
        self.taken.insert(name.clone());
        name
    }

    fn emit(&mut self, name: String, body: String, doc: Option<&str>) {
        if let Some(d) = doc {
            self.docs.push((name.clone(), d.to_string()));
        }
        self.types.push((name, body, doc.map(first_line)));
    }

    fn finish(self, header: &str) -> Generated {
        let mut source = format!("// {header}\n// Generated by `cleat gen`; do not edit.\n");
        for (name, body, doc) in &self.types {
            source.push('\n');
            if let Some(d) = doc {
                source.push_str(&format!("// {d}\n"));
            }
            source.push_str(&format!("type {name} = {body}\n"));
        }
        Generated {
            source,
            docs: self.docs,
        }
    }

    /// The Cleat type for a schema. `hint` names nested objects when `name_nested` is set.
    fn ty(&mut self, s: &Value, hint: &str) -> String {
        let t = self.ty_inner(s, hint);
        if s.get("nullable") == Some(&Value::Bool(true)) {
            format!("({t}) | null")
        } else {
            t
        }
    }

    fn ty_inner(&mut self, s: &Value, hint: &str) -> String {
        if let Some(r) = str_of(s.get("$ref")) {
            let target = r.strip_prefix(self.ref_prefix.as_str()).unwrap_or(r).to_string();
            if self.broken.contains(&(self.cur_def.clone(), target.clone())) {
                return "Json".into();
            }
            return self.names.get(&target).cloned().unwrap_or_else(|| "Json".into());
        }
        let truthy = |k: &str| s.get(k) == Some(&Value::Bool(true));
        if truthy("x-kubernetes-int-or-string") || str_of(s.get("format")) == Some("int-or-string") {
            return "Int | Str".into();
        }
        for combinator in ["allOf", "anyOf", "oneOf"] {
            if let Some(Value::List(xs)) = s.get(combinator) {
                if xs.len() == 1 || combinator == "allOf" {
                    // allOf with a single meaningful member is common (`allOf: [$ref]`).
                    if let Some(x) = xs.iter().find(|x| x.get("$ref").is_some()).or(xs.first()) {
                        return self.ty(x, hint);
                    }
                }
                if s.get("type").is_none() && s.get("properties").is_none() {
                    let ms: Vec<String> = xs.iter().map(|x| paren(&self.ty(x, hint))).collect();
                    return ms.join(" | ");
                }
            }
        }
        if let Some(Value::List(vals)) = s.get("enum") {
            let lits: Vec<String> = vals
                .iter()
                .filter_map(|v| match v {
                    Value::Str(x) => Some(lit_str(x)),
                    Value::Int(i) => Some(i.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Null => Some("null".into()),
                    _ => None,
                })
                .collect();
            if !lits.is_empty() && lits.len() == vals.len() {
                return lits.join(" | ");
            }
        }
        match str_of(s.get("type")) {
            Some("string") => refine("Str", string_preds(s)),
            Some("integer") => refine("Int", number_preds(s)),
            Some("number") => refine("Float", number_preds(s)),
            Some("boolean") => "Bool".into(),
            Some("array") => match s.get("items") {
                Some(items) => format!("[{}]", self.ty(items, &format!("{hint}Item"))),
                None => "[Json]".into(),
            },
            Some("object") | None if s.get("properties").is_some() => self.object(s, hint),
            Some("object") => match s.get("additionalProperties") {
                Some(v @ Value::Obj(_)) => format!("{{ [Str]: {} }}", self.ty(v, &format!("{hint}Value"))),
                _ => "{ [Str]: Json }".into(),
            },
            _ => "Json".into(),
        }
    }

    fn object(&mut self, s: &Value, hint: &str) -> String {
        if self.name_nested && !hint.is_empty() {
            let name = self.fresh(hint);
            let body = self.record(s, &name, "  ");
            let doc = str_of(s.get("description")).map(str::to_string);
            self.emit(name.clone(), body, doc.as_deref());
            return name;
        }
        self.record(s, hint, "")
    }

    /// A record type. `owner` names it for docs; `indent` empty means one line.
    fn record(&mut self, s: &Value, owner: &str, indent: &str) -> String {
        let required: BTreeSet<&str> = match s.get("required") {
            Some(Value::List(xs)) => xs.iter().filter_map(|x| str_of(Some(x))).collect(),
            _ => BTreeSet::new(),
        };
        let open = s.get("additionalProperties") == Some(&Value::Bool(true))
            || s.get("x-kubernetes-preserve-unknown-fields") == Some(&Value::Bool(true));
        let mut fields = Vec::new();
        if let Some(Value::Obj(props)) = s.get("properties") {
            for (k, ps) in props {
                let hint = format!("{owner}{}", pascal(k));
                let t = self.ty(ps, &hint);
                let q = if required.contains(k.as_str()) { "" } else { "?" };
                let desc = str_of(ps.get("description"));
                if let Some(d) = desc {
                    self.docs.push((format!("{owner}.{k}"), d.to_string()));
                }
                fields.push((format!("{}{q}: {t}", key(k)), desc.map(first_line)));
            }
        }
        if open {
            fields.push(("..".into(), None));
        }
        if fields.is_empty() {
            return "{}".into();
        }
        if indent.is_empty() {
            let fs: Vec<&str> = fields.iter().map(|(f, _)| f.as_str()).collect();
            return format!("{{ {} }}", fs.join(", "));
        }
        let mut out = String::from("{\n");
        for (f, d) in fields {
            if let Some(d) = d {
                out.push_str(&format!("{indent}// {d}\n"));
            }
            out.push_str(&format!("{indent}{f},\n"));
        }
        out.push('}');
        out
    }
}

fn paren(t: &str) -> String {
    if t.contains(" where ") || t.contains(" | ") {
        format!("({t})")
    } else {
        t.to_string()
    }
}

fn refine(base: &str, preds: Vec<String>) -> String {
    if preds.is_empty() {
        base.to_string()
    } else {
        format!("{base} where {}", preds.join(" && "))
    }
}

fn num(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Int(i)) => Some(i.to_string()),
        Some(Value::Float(f)) => Some(format!("{f:?}")),
        _ => None,
    }
}

fn number_preds(s: &Value) -> Vec<String> {
    let mut p = Vec::new();
    let excl = |k: &str| s.get(k) == Some(&Value::Bool(true));
    if let Some(m) = num(s.get("minimum")) {
        p.push(format!(
            "self {} {m}",
            if excl("exclusiveMinimum") { ">" } else { ">=" }
        ));
    }
    if let Some(m) = num(s.get("maximum")) {
        p.push(format!(
            "self {} {m}",
            if excl("exclusiveMaximum") { "<" } else { "<=" }
        ));
    }
    p
}

fn string_preds(s: &Value) -> Vec<String> {
    let mut p = Vec::new();
    if let Some(n) = num(s.get("minLength")) {
        p.push(format!("len(self) >= {n}"));
    }
    if let Some(n) = num(s.get("maxLength")) {
        p.push(format!("len(self) <= {n}"));
    }
    // Patterns Rust's regex engine can't compile (e.g. lookaround) are skipped.
    if let Some(re) = str_of(s.get("pattern"))
        && crate::host::compile_regex(re).is_ok()
    {
        p.push(format!("matches(self, {})", lit_str(re)));
    }
    p
}

fn collect_refs(s: &Value, prefix: &str, out: &mut Vec<String>) {
    match s {
        Value::Obj(es) => {
            for (k, v) in es {
                if k == "$ref" {
                    if let Value::Str(r) = v {
                        out.push(r.strip_prefix(prefix).unwrap_or(r).to_string());
                    }
                } else {
                    collect_refs(v, prefix, out);
                }
            }
        }
        Value::List(xs) => xs.iter().for_each(|x| collect_refs(x, prefix, out)),
        _ => {}
    }
}

/// Mark references that close cycles, so the remaining graph is acyclic.
fn break_cycles(defs: &[(String, &Value)], prefix: &str) -> BTreeSet<(String, String)> {
    let index: BTreeMap<&str, &Value> = defs.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let mut state: BTreeMap<String, u8> = BTreeMap::new(); // 1 = on stack, 2 = done
    let mut broken = BTreeSet::new();
    fn dfs(
        k: &str,
        index: &BTreeMap<&str, &Value>,
        prefix: &str,
        state: &mut BTreeMap<String, u8>,
        broken: &mut BTreeSet<(String, String)>,
    ) {
        state.insert(k.to_string(), 1);
        let mut refs = Vec::new();
        if let Some(s) = index.get(k) {
            collect_refs(s, prefix, &mut refs);
        }
        refs.sort();
        refs.dedup();
        for r in refs {
            match state.get(&r) {
                Some(1) => {
                    broken.insert((k.to_string(), r));
                }
                Some(_) => {}
                None if index.contains_key(r.as_str()) => dfs(&r, index, prefix, state, broken),
                None => {}
            }
        }
        state.insert(k.to_string(), 2);
    }
    let mut keys: Vec<&str> = index.keys().copied().collect();
    keys.sort();
    for k in keys {
        if !state.contains_key(k) {
            dfs(k, &index, prefix, &mut state, &mut broken);
        }
    }
    broken
}

/// `io.k8s.api.apps.v1.Deployment` -> `apps.v1.Deployment`; `Pet` -> `Pet`.
fn local_name(def: &str) -> String {
    let segs: Vec<&str> = def.split('.').collect();
    let n = segs.len();
    let take = if n >= 3 && version_rank(segs[n - 2]).is_some() {
        3
    } else if n >= 2 {
        2
    } else {
        1
    };
    segs[n - take..].iter().map(|s| ident(s)).collect::<Vec<_>>().join(".")
}

fn gvk(s: &Value) -> Option<(String, String)> {
    let Some(Value::List(gvks)) = s.get("x-kubernetes-group-version-kind") else {
        return None;
    };
    let [g] = gvks.as_slice() else { return None };
    let group = str_of(g.get("group")).unwrap_or("");
    let version = str_of(g.get("version"))?;
    let kind = str_of(g.get("kind"))?;
    let api = if group.is_empty() {
        version.to_string()
    } else {
        format!("{group}/{version}")
    };
    Some((api, kind.to_string()))
}

/// Replace `apiVersion`/`kind` with literal types, which fill themselves in.
fn with_gvk(s: &Value, api: &str, kind: &str) -> Value {
    let mut s = s.clone();
    if let Value::Obj(es) = &mut s {
        let mut props = match es.iter().find(|(k, _)| k == "properties") {
            Some((_, Value::Obj(p))) => p.clone(),
            _ => Vec::new(),
        };
        props.retain(|(k, _)| k != "apiVersion" && k != "kind");
        let lit = |v: &str| Value::Obj(vec![("enum".into(), Value::List(vec![Value::Str(v.into())]))]);
        props.insert(0, ("kind".into(), lit(kind)));
        props.insert(0, ("apiVersion".into(), lit(api)));
        es.retain(|(k, _)| k != "properties");
        es.push(("properties".into(), Value::Obj(props)));
        let mut req: Vec<Value> = match es.iter().find(|(k, _)| k == "required") {
            Some((_, Value::List(r))) => r.clone(),
            _ => Vec::new(),
        };
        req.retain(|r| str_of(Some(r)) != Some("apiVersion") && str_of(Some(r)) != Some("kind"));
        req.push(Value::Str("apiVersion".into()));
        req.push(Value::Str("kind".into()));
        es.retain(|(k, _)| k != "required");
        es.push(("required".into(), Value::List(req)));
    }
    s
}

/// Enum fields the Kubernetes OpenAPI spec types as plain strings, although the
/// API only accepts these values (each list is the one given in the field's own
/// description). `[]` marks the elements of a list. Only long-stable enums are
/// listed: a value missing here would reject valid manifests, and `with` can't widen.
pub const KUBERNETES_ENUMS: &[(&str, &str, &[&str])] = &[
    (
        "core.v1.ServiceSpec",
        "type",
        &["ClusterIP", "NodePort", "LoadBalancer", "ExternalName"],
    ),
    ("core.v1.ServiceSpec", "sessionAffinity", &["ClientIP", "None"]),
    ("core.v1.ServiceSpec", "externalTrafficPolicy", &["Cluster", "Local"]),
    ("core.v1.ServiceSpec", "internalTrafficPolicy", &["Cluster", "Local"]),
    (
        "core.v1.ServiceSpec",
        "ipFamilyPolicy",
        &["SingleStack", "PreferDualStack", "RequireDualStack"],
    ),
    ("core.v1.ServiceSpec", "ipFamilies[]", &["IPv4", "IPv6"]),
    ("core.v1.ServicePort", "protocol", &["TCP", "UDP", "SCTP"]),
    ("core.v1.ContainerPort", "protocol", &["TCP", "UDP", "SCTP"]),
    (
        "core.v1.Container",
        "imagePullPolicy",
        &["Always", "IfNotPresent", "Never"],
    ),
    (
        "core.v1.Container",
        "terminationMessagePolicy",
        &["File", "FallbackToLogsOnError"],
    ),
    (
        "core.v1.EphemeralContainer",
        "imagePullPolicy",
        &["Always", "IfNotPresent", "Never"],
    ),
    ("core.v1.PodSpec", "restartPolicy", &["Always", "OnFailure", "Never"]),
    (
        "core.v1.PodSpec",
        "dnsPolicy",
        &["ClusterFirstWithHostNet", "ClusterFirst", "Default", "None"],
    ),
    (
        "core.v1.PodSpec",
        "preemptionPolicy",
        &["PreemptLowerPriority", "Never"],
    ),
    ("core.v1.Toleration", "operator", &["Exists", "Equal"]),
    (
        "core.v1.Toleration",
        "effect",
        &["", "NoSchedule", "PreferNoSchedule", "NoExecute"],
    ),
    (
        "core.v1.Taint",
        "effect",
        &["NoSchedule", "PreferNoSchedule", "NoExecute"],
    ),
    (
        "core.v1.NodeSelectorRequirement",
        "operator",
        &["In", "NotIn", "Exists", "DoesNotExist", "Gt", "Lt"],
    ),
    (
        "meta.v1.LabelSelectorRequirement",
        "operator",
        &["In", "NotIn", "Exists", "DoesNotExist"],
    ),
    (
        "core.v1.TopologySpreadConstraint",
        "whenUnsatisfiable",
        &["DoNotSchedule", "ScheduleAnyway"],
    ),
    (
        "core.v1.TopologySpreadConstraint",
        "nodeAffinityPolicy",
        &["Honor", "Ignore"],
    ),
    (
        "core.v1.TopologySpreadConstraint",
        "nodeTaintsPolicy",
        &["Honor", "Ignore"],
    ),
    (
        "core.v1.PersistentVolumeClaimSpec",
        "accessModes[]",
        &["ReadWriteOnce", "ReadOnlyMany", "ReadWriteMany", "ReadWriteOncePod"],
    ),
    (
        "core.v1.PersistentVolumeClaimSpec",
        "volumeMode",
        &["Filesystem", "Block"],
    ),
    (
        "core.v1.PodSecurityContext",
        "fsGroupChangePolicy",
        &["OnRootMismatch", "Always"],
    ),
    (
        "core.v1.SeccompProfile",
        "type",
        &["Localhost", "RuntimeDefault", "Unconfined"],
    ),
    ("core.v1.HTTPGetAction", "scheme", &["HTTP", "HTTPS"]),
    ("apps.v1.DeploymentStrategy", "type", &["Recreate", "RollingUpdate"]),
    (
        "apps.v1.StatefulSetSpec",
        "podManagementPolicy",
        &["OrderedReady", "Parallel"],
    ),
    (
        "apps.v1.StatefulSetUpdateStrategy",
        "type",
        &["OnDelete", "RollingUpdate"],
    ),
    (
        "apps.v1.DaemonSetUpdateStrategy",
        "type",
        &["OnDelete", "RollingUpdate"],
    ),
    ("batch.v1.JobSpec", "completionMode", &["NonIndexed", "Indexed"]),
    (
        "batch.v1.JobSpec",
        "podReplacementPolicy",
        &["TerminatingOrFailed", "Failed"],
    ),
    (
        "batch.v1.CronJobSpec",
        "concurrencyPolicy",
        &["Allow", "Forbid", "Replace"],
    ),
    (
        "networking.v1.HTTPIngressPath",
        "pathType",
        &["Exact", "Prefix", "ImplementationSpecific"],
    ),
    (
        "networking.v1.NetworkPolicySpec",
        "policyTypes[]",
        &["Ingress", "Egress"],
    ),
    ("networking.v1.NetworkPolicyPort", "protocol", &["TCP", "UDP", "SCTP"]),
    (
        "autoscaling.v2.MetricSpec",
        "type",
        &["ContainerResource", "External", "Object", "Pods", "Resource"],
    ),
    (
        "autoscaling.v2.MetricTarget",
        "type",
        &["AverageValue", "Utilization", "Value"],
    ),
    (
        "policy.v1.PodDisruptionBudgetSpec",
        "unhealthyPodEvictionPolicy",
        &["IfHealthyBudget", "AlwaysAllow"],
    ),
];

/// Add `enum` to the curated Kubernetes fields of definition `local` (e.g. `core.v1.ServiceSpec`).
/// Returns how many fields were changed.
fn apply_kubernetes_enums(local: &str, s: &mut Value) -> usize {
    let mut n = 0;
    for (_, field, values) in KUBERNETES_ENUMS.iter().filter(|(d, _, _)| *d == local) {
        let (name, list) = match field.strip_suffix("[]") {
            Some(f) => (f, true),
            None => (*field, false),
        };
        let Value::Obj(es) = s else { continue };
        let Some((_, Value::Obj(props))) = es.iter_mut().find(|(k, _)| k == "properties") else {
            continue;
        };
        let Some((_, prop)) = props.iter_mut().find(|(k, _)| k == name) else {
            continue;
        };
        let target = if list {
            match prop {
                Value::Obj(p) => match p.iter_mut().find(|(k, _)| k == "items") {
                    Some((_, items)) => items,
                    None => continue,
                },
                _ => continue,
            }
        } else {
            prop
        };
        if let Value::Obj(t) = target
            && str_of(t.iter().find(|(k, _)| k == "type").map(|(_, v)| v)) == Some("string")
        {
            t.retain(|(k, _)| k != "enum");
            t.push((
                "enum".into(),
                Value::List(values.iter().map(|v| Value::Str(v.to_string())).collect()),
            ));
            n += 1;
        }
    }
    n
}

fn from_openapi(doc: &Value) -> Result<Generated, String> {
    let (defs_v, prefix) = match (
        doc.get("definitions"),
        doc.get("components").and_then(|c| c.get("schemas")),
    ) {
        (Some(d), _) => (d, "#/definitions/"),
        (None, Some(d)) => (d, "#/components/schemas/"),
        _ => return Err("OpenAPI document has no `definitions` or `components.schemas`".into()),
    };
    let Value::Obj(entries) = defs_v else {
        return Err("schema definitions must be an object".into());
    };
    let defs: Vec<(String, &Value)> = entries.iter().map(|(k, v)| (k.clone(), v)).collect();

    let mut g = Gen::new(prefix, false);
    g.broken = break_cycles(&defs, prefix);
    for (k, _) in &defs {
        let name = g.fresh(&local_name(k));
        g.names.insert(k.clone(), name);
    }

    let kubernetes = str_of(doc.get("info").and_then(|i| i.get("title"))) == Some("Kubernetes");
    for (k, s) in &defs {
        g.cur_def = k.clone();
        let name = g.names[k].clone();
        let mut owned;
        let s = if kubernetes && KUBERNETES_ENUMS.iter().any(|(d, _, _)| *d == name) {
            owned = (*s).clone();
            apply_kubernetes_enums(&name, &mut owned);
            &owned
        } else {
            *s
        };
        let body = if k.ends_with("api.resource.Quantity") {
            "Str | Float".to_string() // Kubernetes accepts `cpu: 1` as well as `cpu: "500m"`
        } else if let Some((api, kind)) = gvk(s) {
            g.record(&with_gvk(s, &api, &kind), &name, "  ")
        } else if s.get("properties").is_some() {
            g.record(s, &name, "  ")
        } else {
            g.ty(s, &name)
        };
        let doc = str_of(s.get("description")).map(str::to_string);
        g.emit(name, body, doc.as_deref());
    }

    // Short names: `Deployment` for `apps.v1.Deployment` when one version clearly wins.
    let mut by_short: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in g.names.values() {
        if let Some(short) = name.rsplit('.').next().filter(|s| *s != name) {
            by_short.entry(short.to_string()).or_default().push(name.clone());
        }
    }
    let mut shorts: Vec<(String, String)> = Vec::new();
    for (short, mut cands) in by_short {
        if g.taken.contains(&short) || BUILTIN.contains(&short.as_str()) {
            continue;
        }
        let rank = |n: &String| {
            let segs: Vec<&str> = n.split('.').collect();
            segs.iter()
                .rev()
                .nth(1)
                .and_then(|v| version_rank(v))
                .unwrap_or((2, 0, 0))
        };
        cands.sort_by_key(|c| core::cmp::Reverse(rank(c)));
        if cands.len() == 1 || rank(&cands[0]) > rank(&cands[1]) {
            shorts.push((short, cands[0].clone()));
        }
    }
    shorts.sort();
    for (short, target) in shorts {
        g.emit(short, target, None);
    }
    let title = str_of(doc.get("info").and_then(|i| i.get("title"))).unwrap_or("OpenAPI");
    let version = str_of(doc.get("info").and_then(|i| i.get("version"))).unwrap_or("");
    Ok(g.finish(&format!("Types from {title} {version}")))
}

const OBJECT_META: &str = "{\n  name?: Str,\n  generateName?: Str,\n  namespace?: Str,\n  labels?: { [Str]: Str },\n  annotations?: { [Str]: Str },\n  finalizers?: [Str],\n  ownerReferences?: [Json],\n  ..\n}";

fn from_crds(crds: &[&Value]) -> Result<Generated, String> {
    let mut g = Gen::new("#/", true);
    g.taken.insert("ObjectMeta".into());
    g.emit(
        "ObjectMeta".into(),
        OBJECT_META.into(),
        Some("Standard object metadata (subset; other fields are allowed)."),
    );

    // kind -> (type name, version rank, is the storage version)
    let mut kinds: BTreeMap<String, Vec<(String, VersionRank, bool)>> = BTreeMap::new();
    for crd in crds {
        let spec = crd.get("spec").ok_or("CRD without `spec`")?;
        let group = str_of(spec.get("group")).ok_or("CRD without `spec.group`")?;
        let kind = str_of(spec.get("names").and_then(|n| n.get("kind"))).ok_or("CRD without `spec.names.kind`")?;
        let mut versions: Vec<(String, Option<&Value>, bool)> = Vec::new();
        match spec.get("versions") {
            Some(Value::List(vs)) => {
                for v in vs {
                    let Some(name) = str_of(v.get("name")) else { continue };
                    let schema = v
                        .get("schema")
                        .and_then(|s| s.get("openAPIV3Schema"))
                        .or_else(|| spec.get("validation").and_then(|s| s.get("openAPIV3Schema")));
                    versions.push((name.to_string(), schema, v.get("storage") == Some(&Value::Bool(true))));
                }
            }
            _ => {
                let name = str_of(spec.get("version")).ok_or("CRD without `spec.versions`")?;
                versions.push((
                    name.to_string(),
                    spec.get("validation").and_then(|s| s.get("openAPIV3Schema")),
                    true,
                ));
            }
        }
        for (ver, schema, storage) in versions {
            let api = format!("{group}/{ver}");
            let base = format!("{}.{}", ident(&ver), ident(kind));
            let name = if g.taken.contains(&base) {
                format!("{}.{base}", ident(&group.replace('.', "_")))
            } else {
                base
            };
            let name = g.fresh(&name);
            g.cur_def = name.clone();
            let empty = Value::Obj(vec![("type".into(), Value::Str("object".into()))]);
            let mut s = with_gvk(schema.unwrap_or(&empty), &api, kind);
            // `metadata` in CRD schemas is untyped; use the standard fields.
            if let Value::Obj(es) = &mut s
                && let Some((_, Value::Obj(props))) = es.iter_mut().find(|(k, _)| k == "properties")
            {
                props.retain(|(k, _)| k != "metadata");
                props.insert(
                    2,
                    (
                        "metadata".into(),
                        Value::Obj(vec![("$ref".into(), Value::Str("#/ObjectMeta".into()))]),
                    ),
                );
            }
            g.names.insert("ObjectMeta".into(), "ObjectMeta".into());
            let body = g.record(&s, &name, "  ");
            let doc = str_of(s.get("description")).map(str::to_string);
            g.emit(name.clone(), body, doc.as_deref());
            let rank = version_rank(&ver).unwrap_or((0, 0, 0));
            kinds.entry(kind.to_string()).or_default().push((name, rank, storage));
        }
    }
    // Short name `Kind` -> storage version (else the most stable) when the kind is unique.
    let mut shorts: Vec<(String, String)> = Vec::new();
    for (kind, mut vs) in kinds {
        if g.taken.contains(&kind) || BUILTIN.contains(&kind.as_str()) {
            continue;
        }
        // A group-qualified name means the same kind exists in several groups: ambiguous.
        if vs.iter().any(|(n, _, _)| n.split('.').count() > 2) {
            continue;
        }
        vs.sort_by_key(|(_, r, storage)| core::cmp::Reverse((*storage, *r)));
        shorts.push((kind, vs[0].0.clone()));
    }
    shorts.sort();
    for (short, target) in shorts {
        g.taken.insert(short.clone());
        g.emit(short, target, None);
    }
    Ok(g.finish("Types from CustomResourceDefinitions"))
}

/// How many of `KUBERNETES_ENUMS` apply to a schema (for tests: a renamed field shows up here).
pub fn kubernetes_enums_applied(doc: &Value) -> usize {
    let Some(Value::Obj(defs)) = doc.get("definitions") else {
        return 0;
    };
    defs.iter()
        .map(|(k, s)| {
            let mut s = s.clone();
            apply_kubernetes_enums(&local_name(k), &mut s)
        })
        .sum()
}
