// cleat view: the exported resources as a cluster-style dashboard.
// Three ways to get its data: served by `cleat view` (polls /api/state), a
// snapshot (window.CLEAT_SNAPSHOT), or embedded with `?embed=1`, where the
// page that opened or framed it posts the state (the playground does this).
// No dependencies.
(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const get = (o, ...path) => path.reduce((x, k) => (x == null ? undefined : x[k]), o);
  const list = (x) => (Array.isArray(x) ? x : []);
  const entries = (x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.entries(x) : []);

  const CATEGORIES = [
    ["Workloads", ["Deployment", "StatefulSet", "DaemonSet", "ReplicaSet", "Job", "CronJob", "Pod", "HorizontalPodAutoscaler", "PodDisruptionBudget"]],
    ["Network", ["Service", "Ingress", "IngressClass", "NetworkPolicy", "Gateway", "HTTPRoute", "GRPCRoute", "Endpoints", "EndpointSlice"]],
    ["Config", ["ConfigMap", "Secret"]],
    ["Storage", ["PersistentVolumeClaim", "PersistentVolume", "StorageClass"]],
    ["Access control", ["ServiceAccount", "Role", "RoleBinding", "ClusterRole", "ClusterRoleBinding"]],
    ["Cluster", ["Namespace", "CustomResourceDefinition", "PriorityClass", "LimitRange", "ResourceQuota"]],
  ];
  const CLUSTER_SCOPED = new Set([
    "Namespace", "ClusterRole", "ClusterRoleBinding", "PersistentVolume", "StorageClass",
    "CustomResourceDefinition", "PriorityClass", "IngressClass",
  ]);
  const WORKLOADS = new Set(["Deployment", "StatefulSet", "DaemonSet", "ReplicaSet", "Job", "CronJob", "Pod"]);

  // ---------- model ----------

  function podTemplate(doc) {
    switch (doc.kind) {
      case "Pod": return { meta: doc.metadata, spec: doc.spec };
      case "CronJob": return { meta: get(doc, "spec", "jobTemplate", "spec", "template", "metadata"), spec: get(doc, "spec", "jobTemplate", "spec", "template", "spec") };
      default:
        if (!WORKLOADS.has(doc.kind)) return null;
        return { meta: get(doc, "spec", "template", "metadata"), spec: get(doc, "spec", "template", "spec") };
    }
  }

  function containersOf(spec) {
    return [
      ...list(get(spec, "initContainers")).map((c) => ({ ...c, init: true })),
      ...list(get(spec, "containers")),
    ];
  }

  function build(state) {
    const res = list(state.resources).map((r, i) => {
      const doc = r.doc && typeof r.doc === "object" ? r.doc : {};
      const kind = doc.kind || "(no kind)";
      const ns = CLUSTER_SCOPED.has(kind) ? "" : get(doc, "metadata", "namespace") || "";
      const name = get(doc, "metadata", "name") || `#${i + 1}`;
      return {
        i, doc, kind, ns, name, yaml: r.yaml, component: r.component, wave: r.wave,
        key: `${kind}/${ns}/${name}`, out: [], in: [], notes: [], missing: [],
      };
    });
    const problems = [];
    const find = (kind, ns, name) => res.find((r) => r.kind === kind && r.name === name && (CLUSTER_SCOPED.has(kind) || r.ns === ns));
    const link = (from, to, label) => {
      from.out.push({ to, label });
      to.in.push({ from, label });
    };
    const note = (r, level, msg) => {
      r.notes.push({ level, msg });
      problems.push({ level, msg, r });
    };
    const ref = (r, kind, name, label, optional) => {
      if (!name) return;
      const to = find(kind, r.ns, name);
      if (to) link(r, to, label);
      else if (!optional) {
        r.missing.push({ kind, name, label, level: "note" });
        note(r, "note", `${label} ${kind} \`${name}\`, which isn't in this output (it may already exist in the cluster)`);
      }
    };
    const matches = (labels, selector) => entries(selector).length > 0 && entries(selector).every(([k, v]) => get(labels, k) === v);

    for (const r of res) {
      const d = r.doc;
      const tpl = podTemplate(d);
      if (tpl && tpl.spec) {
        const spec = tpl.spec;
        ref(r, "ServiceAccount", spec.serviceAccountName, "runs as");
        for (const s of list(spec.imagePullSecrets)) ref(r, "Secret", s.name, "pulls images with");
        for (const v of list(spec.volumes)) {
          ref(r, "ConfigMap", get(v, "configMap", "name"), "mounts", get(v, "configMap", "optional"));
          ref(r, "Secret", get(v, "secret", "secretName"), "mounts", get(v, "secret", "optional"));
          ref(r, "PersistentVolumeClaim", get(v, "persistentVolumeClaim", "claimName"), "mounts");
          for (const src of list(get(v, "projected", "sources"))) {
            ref(r, "ConfigMap", get(src, "configMap", "name"), "mounts", get(src, "configMap", "optional"));
            ref(r, "Secret", get(src, "secret", "name"), "mounts", get(src, "secret", "optional"));
          }
        }
        for (const c of containersOf(spec)) {
          for (const e of list(c.env)) {
            const cm = get(e, "valueFrom", "configMapKeyRef");
            const sec = get(e, "valueFrom", "secretKeyRef");
            if (cm) ref(r, "ConfigMap", cm.name, "reads", cm.optional);
            if (sec) ref(r, "Secret", sec.name, "reads", sec.optional);
          }
          for (const e of list(c.envFrom)) {
            if (e.configMapRef) ref(r, "ConfigMap", e.configMapRef.name, "reads", e.configMapRef.optional);
            if (e.secretRef) ref(r, "Secret", e.secretRef.name, "reads", e.secretRef.optional);
          }
        }
        for (const t of list(get(d, "spec", "volumeClaimTemplates"))) {
          if (get(t, "metadata", "name")) r.claimTemplates = (r.claimTemplates || []).concat(t.metadata.name);
        }
      }
      if (r.kind === "Service") {
        const sel = get(d, "spec", "selector");
        const targets = res.filter((w) => w.ns === r.ns && WORKLOADS.has(w.kind) && w.kind !== "CronJob" && w.kind !== "Job" && matches(get(podTemplate(w.doc), "meta", "labels"), sel));
        for (const w of targets) link(r, w, "routes to");
        if (entries(sel).length && !targets.length && get(d, "spec", "type") !== "ExternalName") {
          note(r, "warn", `selects no workload in this output (selector ${entries(sel).map(([k, v]) => `${k}=${v}`).join(", ")})`);
        }
        for (const p of list(get(d, "spec", "ports"))) {
          if (typeof p.targetPort === "string" && targets.length) {
            const named = targets.some((w) => containersOf(get(podTemplate(w.doc), "spec")).some((c) => list(c.ports).some((cp) => cp.name === p.targetPort)));
            if (!named) note(r, "warn", `targets port \`${p.targetPort}\`, but no container it selects has a port with that name`);
          }
        }
      }
      if (r.kind === "Ingress") {
        const backends = [];
        const add = (b, where) => b && get(b, "service", "name") && backends.push({ svc: b.service.name, port: get(b, "service", "port"), where });
        add(get(d, "spec", "defaultBackend"), "default backend");
        for (const rule of list(get(d, "spec", "rules"))) {
          for (const p of list(get(rule, "http", "paths"))) add(p.backend, `${rule.host || "*"}${p.path || "/"}`);
        }
        for (const b of backends) {
          const svc = find("Service", r.ns, b.svc);
          if (!svc) {
            r.missing.push({ kind: "Service", name: b.svc, label: "sends traffic to", level: "warn" });
            note(r, "warn", `sends traffic for ${b.where} to Service \`${b.svc}\`, which isn't in this output`);
            continue;
          }
          link(r, svc, "sends traffic to");
          const ports = list(get(svc.doc, "spec", "ports"));
          const ok = !b.port || ports.some((p) => (b.port.name != null && p.name === b.port.name) || (b.port.number != null && p.port === b.port.number));
          if (!ok) note(r, "warn", `uses port ${b.port.name ?? b.port.number} of Service \`${b.svc}\` (for ${b.where}), which doesn't have it`);
        }
      }
      if (r.kind === "HorizontalPodAutoscaler") {
        const t = get(d, "spec", "scaleTargetRef");
        if (t) {
          const to = find(t.kind, r.ns, t.name);
          if (to) link(r, to, "scales");
          else {
            r.missing.push({ kind: t.kind, name: t.name, label: "scales", level: "warn" });
            note(r, "warn", `scales ${t.kind} \`${t.name}\`, which isn't in this output`);
          }
        }
      }
      if (r.kind === "PodDisruptionBudget") {
        const sel = get(d, "spec", "selector", "matchLabels");
        const ws = res.filter((w) => w.ns === r.ns && WORKLOADS.has(w.kind) && matches(get(podTemplate(w.doc), "meta", "labels"), sel));
        for (const w of ws) link(r, w, "protects");
        if (entries(sel).length && !ws.length) note(r, "warn", `selects no workload in this output`);
      }
      if (r.kind === "RoleBinding" || r.kind === "ClusterRoleBinding") {
        const role = get(d, "roleRef");
        if (role) ref(r, role.kind, role.name, "grants");
        for (const s of list(d.subjects)) {
          if (s.kind === "ServiceAccount") {
            const to = find("ServiceAccount", s.namespace || r.ns, s.name);
            if (to) link(r, to, "binds");
          }
        }
      }
    }
    // Checks that need the relations above.
    const first = new Map();
    for (const r of res) {
      const earlier = first.get(r.key);
      if (!earlier) first.set(r.key, r);
      else note(earlier, "warn", `is defined again in component \`${r.component}\` (first in \`${earlier.component}\`); applying keeps only the last one`);
    }
    for (const r of res) {
      const tpl = podTemplate(r.doc);
      if (!tpl || !tpl.spec) continue;
      const podLabels = get(tpl, "meta", "labels");
      const sel = get(r.doc, "spec", "selector", "matchLabels");
      if (["Deployment", "StatefulSet", "DaemonSet", "ReplicaSet"].includes(r.kind) && entries(sel).length && !matches(podLabels, sel)) {
        note(r, "warn", `selector ${entries(sel).map(([k, v]) => `\`${k}=${v}\``).join(", ")} doesn't match its pod template's labels, so the API server will reject it`);
      }
      const routedBy = r.in.filter((e) => e.label === "routes to").map((e) => e.from);
      for (const c of containersOf(tpl.spec)) {
        const image = c.image || "";
        const ref = image.split("/").pop();
        if (image && !image.includes("@") && (!ref.includes(":") || ref.endsWith(":latest"))) {
          note(r, "warn", `container \`${c.name}\` runs \`${image}\` without a fixed tag, so what runs can change with no change here`);
        }
        if (!entries(get(c, "resources", "requests")).length && !entries(get(c, "resources", "limits")).length) {
          note(r, "note", `container \`${c.name}\` has no resource requests or limits`);
        }
        if (!c.init && routedBy.length && !c.readinessProbe) {
          note(r, "note", `container \`${c.name}\` has no readinessProbe, so Service \`${routedBy[0].name}\` may send it traffic before it's ready`);
        }
      }
    }
    for (const r of res.filter((x) => x.kind === "NetworkPolicy")) {
      const d = r.doc;
      const podSel = get(d, "spec", "podSelector") || {};
      if (list(podSel.matchExpressions).length) continue; // not evaluated here
      const sel = podSel.matchLabels || {};
      const selected = res.filter((w) => w.ns === r.ns && WORKLOADS.has(w.kind) && entries(sel).every(([k, v]) => get(podTemplate(w.doc), "meta", "labels", k) === v));
      for (const w of selected) link(r, w, "isolates");
      const types = list(get(d, "spec", "policyTypes"));
      const ingress = types.length ? types.includes("Ingress") : true;
      if (ingress && !list(get(d, "spec", "ingress")).length) {
        for (const w of selected) {
          const svc = w.in.find((e) => e.label === "routes to");
          if (svc) note(r, "warn", `blocks all incoming traffic to ${w.kind} \`${w.name}\`, which Service \`${svc.from.name}\` routes to`);
        }
      }
    }
    return { res, problems };
  }

  // ---------- formatting ----------

  const chip = (s, cls = "") => `<span class="chip ${cls}">${esc(s)}</span>`;
  const resLink = (r) => `<a class="res" href="${hrefFor({ sel: r.key })}">${chip(r.kind, "kind")} <span class="rn">${esc(r.name)}</span></a>`;
  const images = (r) => [...new Set(containersOf(get(podTemplate(r.doc), "spec")).map((c) => c.image || "(no image)"))].map((i) => chip(i)).join(" ");
  const cports = (r) => containersOf(get(podTemplate(r.doc), "spec")).flatMap((c) => list(c.ports).map((p) => chip(`${p.name ? p.name + ":" : ""}${p.containerPort}/${p.protocol || "TCP"}`))).join(" ") || '<span class="muted">—</span>';
  const svcPorts = (d) => list(get(d, "spec", "ports")).map((p) => chip(`${p.name ? p.name + " " : ""}${p.port}→${p.targetPort ?? p.port}/${p.protocol || "TCP"}`)).join(" ");
  const labelsOf = (o) => entries(o).map(([k, v]) => chip(`${k}=${v}`)).join(" ") || '<span class="muted">—</span>';
  const keysOf = (d) => [...Object.keys(d.data || {}), ...Object.keys(d.binaryData || d.stringData || {})];
  const dash = '<span class="muted">—</span>';

  const COLUMNS = {
    Deployment: [["Replicas", (r) => esc(get(r.doc, "spec", "replicas") ?? 1)], ["Images", images], ["Ports", cports]],
    StatefulSet: [["Replicas", (r) => esc(get(r.doc, "spec", "replicas") ?? 1)], ["Images", images], ["Volume claims", (r) => (r.claimTemplates || []).map((n) => chip(n)).join(" ") || dash]],
    ReplicaSet: [["Replicas", (r) => esc(get(r.doc, "spec", "replicas") ?? 1)], ["Images", images]],
    DaemonSet: [["Images", images], ["Ports", cports]],
    Pod: [["Images", images], ["Ports", cports]],
    Job: [["Images", images], ["Completions", (r) => esc(get(r.doc, "spec", "completions") ?? 1)]],
    CronJob: [["Schedule", (r) => `<code>${esc(get(r.doc, "spec", "schedule"))}</code>`], ["Images", images]],
    Service: [
      ["Type", (r) => esc(get(r.doc, "spec", "type") || "ClusterIP")],
      ["Ports", (r) => svcPorts(r.doc) || dash],
      ["Routes to", (r) => r.out.filter((e) => e.label === "routes to").map((e) => resLink(e.to)).join(" ") || dash],
    ],
    Ingress: [
      ["Class", (r) => esc(get(r.doc, "spec", "ingressClassName") || "—")],
      ["Routes", (r) => list(get(r.doc, "spec", "rules")).flatMap((rule) => list(get(rule, "http", "paths")).map((p) => chip(`${rule.host || "*"}${p.path || "/"} → ${get(p, "backend", "service", "name")}:${get(p, "backend", "service", "port", "name") ?? get(p, "backend", "service", "port", "number")}`))).join(" ") || dash],
    ],
    ConfigMap: [["Keys", (r) => keysOf(r.doc).map((k) => chip(k)).join(" ") || dash]],
    Secret: [["Type", (r) => esc(r.doc.type || "Opaque")], ["Keys", (r) => keysOf(r.doc).map((k) => chip(k)).join(" ") || dash]],
    PersistentVolumeClaim: [
      ["Size", (r) => esc(get(r.doc, "spec", "resources", "requests", "storage") ?? "—")],
      ["Access", (r) => list(get(r.doc, "spec", "accessModes")).map((m) => chip(m)).join(" ") || dash],
      ["Class", (r) => esc(get(r.doc, "spec", "storageClassName") ?? "—")],
    ],
    HorizontalPodAutoscaler: [
      ["Target", (r) => r.out.map((e) => resLink(e.to)).join(" ") || esc(get(r.doc, "spec", "scaleTargetRef", "name"))],
      ["Replicas", (r) => esc(`${get(r.doc, "spec", "minReplicas") ?? 1}–${get(r.doc, "spec", "maxReplicas") ?? "?"}`)],
    ],
  };

  function yamlHtml(text) {
    return esc(text)
      .split("\n")
      .map((l) => {
        const m = /^(\s*(?:- )?)([^\s:#][^:]*?|&quot;[^&]*&quot;):(\s|$)(.*)$/.exec(l);
        if (!m) return l.replace(/^(\s*- )(.*)$/, (_, a, v) => a + scalar(v));
        return `${m[1]}<span class="k">${m[2]}</span>:${m[3]}${scalar(m[4])}`;
      })
      .join("\n");
  }
  function scalar(v) {
    if (/^(-?\d+(\.\d+)?|true|false|null)$/.test(v)) return `<span class="n">${v}</span>`;
    if (/^&quot;.*&quot;$/.test(v)) return `<span class="s">${v}</span>`;
    return v;
  }

  // ---------- view state ----------

  let model = { res: [], problems: [] };
  let data = null;
  let route = { view: "overview", sel: null };

  function parseHash() {
    const p = new URLSearchParams(location.hash.slice(1));
    route = { view: p.get("view") || "overview", sel: p.get("sel") };
  }
  function hrefFor(change) {
    const next = { ...route, ...change };
    const p = new URLSearchParams();
    p.set("view", next.view);
    if (next.sel) p.set("sel", next.sel);
    return "#" + p.toString();
  }

  function visible() {
    const ns = $("ns").value;
    const q = $("search").value.trim().toLowerCase();
    return model.res.filter((r) => {
      if (ns !== "*" && r.ns !== ns) return false;
      if (!q) return true;
      const hay = [r.name, r.kind, r.component, ...entries(get(r.doc, "metadata", "labels")).map(([k, v]) => `${k}=${v}`)].join(" ").toLowerCase();
      return hay.includes(q);
    });
  }

  function categoryOf(kind) {
    for (const [cat, kinds] of CATEGORIES) if (kinds.includes(kind)) return cat;
    return "Custom resources";
  }

  // ---------- rendering ----------

  function renderNav(rs) {
    const byKind = new Map();
    for (const r of rs) byKind.set(r.kind, (byKind.get(r.kind) || 0) + 1);
    const link = (view, label, count) =>
      `<a href="${hrefFor({ view })}"${route.view === view ? ' aria-current="page"' : ""}><span>${esc(label)}</span>${count != null ? `<span class="count">${count}</span>` : ""}</a>`;
    let html = link("overview", "Overview") + link("map", "Map") + link("all", "All resources", rs.length);
    const groups = new Map();
    for (const kind of byKind.keys()) {
      const c = categoryOf(kind);
      if (!groups.has(c)) groups.set(c, []);
      groups.get(c).push(kind);
    }
    for (const cat of [...CATEGORIES.map((c) => c[0]), "Custom resources"]) {
      const kinds = groups.get(cat);
      if (!kinds) continue;
      const order = (CATEGORIES.find((c) => c[0] === cat) || [null, []])[1];
      kinds.sort((a, b) => (order.indexOf(a) + 1 || 99) - (order.indexOf(b) + 1 || 99) || a.localeCompare(b));
      html += `<h3>${esc(cat)}</h3>` + kinds.map((k) => link(`kind:${k}`, k, byKind.get(k))).join("");
    }
    $("nav").innerHTML = html;
  }

  function problemsHtml(ps) {
    if (!ps.length) return '<p class="muted">No problems found between these resources.</p>';
    const order = { warn: 0, note: 1 };
    return `<ul class="problems">${ps
      .slice()
      .sort((a, b) => order[a.level] - order[b.level])
      .map((p) => `<li class="${p.level}">${p.r ? resLink(p.r) + " " : ""}${md(p.msg)}</li>`)
      .join("")}</ul>`;
  }
  const md = (s) => esc(s).replace(/`([^`]+)`/g, "<code>$1</code>");

  function renderOverview(rs) {
    const namespaces = new Set(rs.map((r) => r.ns || "—"));
    const byKind = new Map();
    for (const r of rs) byKind.set(r.kind, (byKind.get(r.kind) || 0) + 1);
    const warnCount = model.problems.filter((p) => p.level === "warn").length;
    let html = `<h1>Overview</h1><div class="cards">
      <div class="card"><div class="n">${rs.length}</div><div class="l">resources</div></div>
      <div class="card"><div class="n">${byKind.size}</div><div class="l">kinds</div></div>
      <div class="card"><div class="n">${namespaces.size}</div><div class="l">namespace${namespaces.size === 1 ? "" : "s"}</div></div>
      <div class="card"><div class="n">${list(data.components).length}</div><div class="l">components</div></div>
      <div class="card"><div class="n">${warnCount}</div><div class="l">warnings</div></div>
    </div>`;
    html += `<h2>Kinds</h2><div class="cards">${[...byKind.entries()]
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .map(([k, n]) => `<a class="card" href="${hrefFor({ view: `kind:${k}` })}"><div class="n">${n}</div><div class="l">${esc(k)}</div></a>`)
      .join("")}</div>`;

    const comps = list(data.components);
    const waves = [...new Set(comps.map((c) => c.wave))].sort((a, b) => a - b);
    html += `<h2>Sync order</h2>`;
    if (comps.length <= 1 && waves.length <= 1) {
      html += '<p class="muted">One component, applied together. Use <code>export name after other = ...</code> to order components; they become Argo CD sync waves or Flux <code>dependsOn</code>.</p>';
    }
    html += `<div class="waves">${waves
      .map((w) => `<div class="wave"><div class="wl"><b>Wave ${w}</b>${w === 0 ? " · applied first" : ""}</div>${comps
        .filter((c) => c.wave === w)
        .map((c) => {
          const members = rs.filter((r) => r.component === c.name);
          return `<div class="comp"><div class="cn">${esc(c.name)}</div>${
            c.after.length ? `<div class="after">after ${c.after.map(esc).join(", ")}</div>` : '<div class="after">no dependencies</div>'
          }<ul>${members.map((r) => `<li>${resLink(r)}</li>`).join("") || '<li class="muted">nothing in this filter</li>'}</ul></div>`;
        })
        .join("")}</div>`)
      .join("")}</div>`;
    html += `<h2>Checks between resources</h2>${problemsHtml(model.problems.filter((p) => rs.includes(p.r)))}`;
    return html;
  }

  function renderTable(rs, title) {
    const kind = route.view.startsWith("kind:") ? route.view.slice(5) : null;
    const showNs = model.res.some((r) => r.ns);
    const cols = kind ? COLUMNS[kind] || [["API version", (r) => esc(r.doc.apiVersion || "—")]] : [["Kind", (r) => chip(r.kind, "kind")]];
    if (!rs.length) return `<h1>${esc(title)}</h1><p class="empty">Nothing matches the current filter.</p>`;
    const head = ["Name", ...(showNs ? ["Namespace"] : []), ...cols.map((c) => c[0]), "Component"];
    const rows = rs
      .map((r) => {
        const flags = r.notes.filter((n) => n.level === "warn").length;
        return `<tr data-key="${esc(r.key)}"${route.sel === r.key ? ' aria-selected="true"' : ""}>
        <td class="name">${esc(r.name)}${flags ? ` <span class="flag warn">${flags} warning${flags > 1 ? "s" : ""}</span>` : ""}</td>
        ${showNs ? `<td>${esc(r.ns || "—")}</td>` : ""}
        ${cols.map((c) => `<td>${c[1](r)}</td>`).join("")}
        <td>${esc(r.component)}<span class="sub">wave ${esc(r.wave)}</span></td></tr>`;
      })
      .join("");
    return `<h1>${esc(title)}</h1><table><thead><tr>${head.map((h) => `<th>${esc(h)}</th>`).join("")}</tr></thead><tbody>${rows}</tbody></table>`;
  }

  function containerHtml(c) {
    const kv = [];
    kv.push(["Image", `<code>${esc(c.image || "(none)")}</code>`]);
    if (list(c.command).length || list(c.args).length) kv.push(["Command", `<code>${esc([...list(c.command), ...list(c.args)].join(" "))}</code>`]);
    if (list(c.ports).length) kv.push(["Ports", list(c.ports).map((p) => chip(`${p.name ? p.name + ":" : ""}${p.containerPort}/${p.protocol || "TCP"}`)).join(" ")]);
    const env = list(c.env).map((e) => {
      const cm = get(e, "valueFrom", "configMapKeyRef");
      const sec = get(e, "valueFrom", "secretKeyRef");
      const field = get(e, "valueFrom", "fieldRef", "fieldPath");
      const v = cm ? `← ConfigMap ${cm.name}[${cm.key}]` : sec ? `← Secret ${sec.name}[${sec.key}]` : field ? `← ${field}` : e.value ?? "";
      return chip(`${e.name}=${v}`);
    });
    for (const e of list(c.envFrom)) env.push(chip(e.configMapRef ? `← all of ConfigMap ${e.configMapRef.name}` : e.secretRef ? `← all of Secret ${e.secretRef.name}` : "envFrom"));
    if (env.length) kv.push(["Env", env.join(" ")]);
    const resources = ["requests", "limits"].map((k) => entries(get(c, "resources", k)).length ? `${k}: ${entries(get(c, "resources", k)).map(([a, b]) => `${a} ${b}`).join(", ")}` : null).filter(Boolean);
    if (resources.length) kv.push(["Resources", esc(resources.join(" · "))]);
    for (const probe of ["livenessProbe", "readinessProbe", "startupProbe"]) {
      const p = c[probe];
      if (!p) continue;
      const how = p.httpGet ? `HTTP GET ${p.httpGet.path || "/"} on ${p.httpGet.port}` : p.tcpSocket ? `TCP ${p.tcpSocket.port}` : p.grpc ? `gRPC ${p.grpc.port}` : p.exec ? `exec ${list(p.exec.command).join(" ")}` : "probe";
      kv.push([probe.replace("Probe", ""), esc(how)]);
    }
    if (list(c.volumeMounts).length) kv.push(["Mounts", list(c.volumeMounts).map((m) => chip(`${m.name} → ${m.mountPath}${m.readOnly ? " (ro)" : ""}`)).join(" ")]);
    return `<div class="ctr"><div class="ch">${esc(c.name)} ${c.init ? chip("init") : ""}</div><dl class="kv">${kv.map(([k, v]) => `<dt>${esc(k)}</dt><dd>${v}</dd>`).join("")}</dl></div>`;
  }

  function renderDetail() {
    const r = model.res.find((x) => x.key === route.sel);
    const el = $("detail");
    if (!r) {
      el.hidden = true;
      return;
    }
    el.hidden = false;
    const d = r.doc;
    const kv = [["API version", esc(d.apiVersion || "—")]];
    if (r.ns) kv.push(["Namespace", esc(r.ns)]);
    kv.push(["Component", `${esc(r.component)} <span class="muted">· wave ${esc(r.wave)}</span>`]);
    const cols = COLUMNS[r.kind] || [];
    for (const [label, f] of cols) if (label !== "Images" && label !== "Ports") kv.push([label, f(r)]);

    let html = `<div class="dh"><div><div class="dk">${esc(r.kind)}</div><h1>${esc(r.name)}</h1></div>
      <a class="close" href="${hrefFor({ sel: null })}" aria-label="Close">×</a></div>
      <dl class="kv" style="margin-top:12px">${kv.map(([k, v]) => `<dt>${esc(k)}</dt><dd>${v}</dd>`).join("")}</dl>`;

    if (r.notes.length) html += `<h2>Checks</h2>${problemsHtml(r.notes.map((n) => ({ ...n, r: null })))}`;

    const tpl = podTemplate(d);
    const cs = containersOf(tpl && tpl.spec);
    if (cs.length) html += `<h2>Containers</h2>${cs.map(containerHtml).join("")}`;

    if (r.out.length || r.in.length) {
      html += `<h2>Relations</h2><ul class="rel">${[
        ...r.out.map((e) => `<li><span class="muted">${esc(e.label)}</span> ${resLink(e.to)}</li>`),
        ...r.in.map((e) => `<li>${resLink(e.from)} <span class="muted">${esc(e.label)} this</span></li>`),
      ].join("")}</ul>`;
    }
    const meta = d.metadata || {};
    html += `<h2>Labels</h2><div>${labelsOf(meta.labels)}</div>`;
    if (entries(meta.annotations).length) html += `<h2>Annotations</h2><div>${labelsOf(meta.annotations)}</div>`;
    if (tpl && entries(get(tpl, "meta", "labels")).length && WORKLOADS.has(r.kind) && r.kind !== "Pod") {
      html += `<h2>Pod labels</h2><div>${labelsOf(get(tpl, "meta", "labels"))}</div>`;
    }
    html += `<div class="yaml-head"><h2>YAML</h2><button type="button" id="copy-yaml">Copy</button></div><pre class="yaml">${yamlHtml(r.yaml || "")}</pre>`;
    el.innerHTML = html;
    $("copy-yaml").onclick = async (e) => {
      try {
        await navigator.clipboard.writeText(r.yaml || "");
        e.target.textContent = "Copied";
      } catch {
        e.target.textContent = "Copy failed";
      }
      setTimeout(() => (e.target.textContent = "Copy"), 1200);
    };
  }

  function renderNamespaces() {
    const sel = $("ns");
    const want = [...new Set(model.res.map((r) => r.ns))].sort();
    const current = sel.value || "*";
    const opts = [["*", "All namespaces"], ...want.map((n) => [n, n || "(no namespace)"])];
    if (sel.options.length !== opts.length || opts.some(([v], i) => sel.options[i].value !== v)) {
      sel.replaceChildren(...opts.map(([v, l]) => new Option(l, v)));
      sel.value = opts.some(([v]) => v === current) ? current : "*";
    }
    sel.hidden = want.length <= 1;
  }

  // ---------- map ----------
  // Resources left to right in the order traffic and configuration flow:
  // entry points, Services, workloads, then what workloads use. One box per
  // namespace. References to resources that aren't in the output are drawn
  // as dashed "ghost" nodes.

  const TIER = {
    Ingress: 0, Gateway: 0, HTTPRoute: 0, GRPCRoute: 0, IngressClass: 0,
    Service: 1, NetworkPolicy: 1, HorizontalPodAutoscaler: 1, PodDisruptionBudget: 1,
    Deployment: 2, StatefulSet: 2, DaemonSet: 2, ReplicaSet: 2, Job: 2, CronJob: 2, Pod: 2, RoleBinding: 2, ClusterRoleBinding: 2,
    ConfigMap: 3, Secret: 3, PersistentVolumeClaim: 3, PersistentVolume: 3, StorageClass: 3, ServiceAccount: 3, Role: 3, ClusterRole: 3,
  };
  const ABBR = {
    Deployment: "deploy", StatefulSet: "sts", DaemonSet: "ds", ReplicaSet: "rs", Job: "job", CronJob: "cj", Pod: "pod",
    Service: "svc", Ingress: "ing", NetworkPolicy: "netpol", ConfigMap: "cm", Secret: "secret", PersistentVolumeClaim: "pvc",
    PersistentVolume: "pv", StorageClass: "sc", ServiceAccount: "sa", Role: "role", RoleBinding: "rb", ClusterRole: "cr",
    ClusterRoleBinding: "crb", HorizontalPodAutoscaler: "hpa", PodDisruptionBudget: "pdb", Namespace: "ns",
    Gateway: "gw", HTTPRoute: "route", CustomResourceDefinition: "crd",
  };
  const CAT_CLASS = { Workloads: "c-work", Network: "c-net", Config: "c-conf", Storage: "c-store", "Access control": "c-rbac", Cluster: "c-cluster" };
  const NODE_W = 220, NODE_H = 54, COL_GAP = 84, ROW_GAP = 14, PAD = 22, HEAD = 30, GROUP_GAP = 26;
  let mapScale = null; // null: fit the pane on first show

  function nodeInfo(r) {
    const d = r.doc;
    switch (r.kind) {
      case "Deployment": case "StatefulSet": case "ReplicaSet": return `${get(d, "spec", "replicas") ?? 1} replica${(get(d, "spec", "replicas") ?? 1) === 1 ? "" : "s"}`;
      case "Service": return `${get(d, "spec", "type") || "ClusterIP"} ${list(get(d, "spec", "ports")).map((p) => ":" + p.port).join(" ")}`;
      case "Ingress": return list(get(d, "spec", "rules")).map((x) => x.host).filter(Boolean).join(", ") || "all hosts";
      case "CronJob": return get(d, "spec", "schedule") || "";
      case "ConfigMap": case "Secret": { const n = keysOf(d).length; return `${n} key${n === 1 ? "" : "s"}`; }
      case "PersistentVolumeClaim": return get(d, "spec", "resources", "requests", "storage") || "";
      case "NetworkPolicy": {
        const types = list(get(d, "spec", "policyTypes"));
        const parts = (types.length ? types : ["Ingress"]).map((t) => {
          const n = list(get(d, "spec", t.toLowerCase())).length;
          return `${t.toLowerCase()}: ${n ? `${n} rule${n === 1 ? "" : "s"}` : "deny all"}`;
        });
        return parts.join(", ");
      }
      case "HorizontalPodAutoscaler": return `${get(d, "spec", "minReplicas") ?? 1}–${get(d, "spec", "maxReplicas") ?? "?"} replicas`;
      default: return d.apiVersion || "";
    }
  }

  function mapLayout(rs) {
    const inView = new Set(rs.map((r) => r.key));
    const nodes = new Map();
    // A duplicated resource is one node: the first copy, which carries the duplicate warning.
    for (const r of rs) if (!nodes.has(r.key)) nodes.set(r.key, { key: r.key, r, kind: r.kind, name: r.name, ns: r.ns, ghost: false, edges: [] });
    const edges = [];
    for (const r of rs) {
      for (const e of r.out) if (inView.has(e.to.key)) edges.push({ from: r.key, to: e.to.key, label: e.label });
      for (const m of r.missing) {
        const key = `ghost:${m.kind}/${CLUSTER_SCOPED.has(m.kind) ? "" : r.ns}/${m.name}`;
        if (!nodes.has(key)) nodes.set(key, { key, r: null, kind: m.kind, name: m.name, ns: CLUSTER_SCOPED.has(m.kind) ? "" : r.ns, ghost: m.level, edges: [] });
        edges.push({ from: r.key, to: key, label: m.label, missing: m.level });
      }
    }
    for (const e of edges) {
      nodes.get(e.from).edges.push(e);
      nodes.get(e.to).edges.push(e);
    }
    const tierOf = (n) => TIER[n.kind] ?? 2;
    const groups = new Map();
    for (const n of nodes.values()) {
      if (!groups.has(n.ns)) groups.set(n.ns, []);
      groups.get(n.ns).push(n);
    }
    const order = [...groups.keys()].sort((a, b) => (a === "") - (b === "") || a.localeCompare(b));
    const tiersUsed = [...new Set([...nodes.values()].map(tierOf))].sort((a, b) => a - b);
    const colOf = (n) => tiersUsed.indexOf(tierOf(n));
    const boxes = [];
    let y = 0;
    for (const ns of order) {
      const cols = tiersUsed.map(() => []);
      for (const n of groups.get(ns)) cols[colOf(n)].push(n);
      for (const c of cols) c.sort((a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name));
      // Barycenter sweeps: order each column by where its neighbors are, to untangle edges.
      const pos = new Map();
      const index = () => cols.forEach((c) => c.forEach((n, i) => pos.set(n.key, i)));
      index();
      for (let sweep = 0; sweep < 6; sweep++) {
        const seq = sweep % 2 ? [...cols.keys()].reverse() : [...cols.keys()];
        for (const ci of seq) {
          const bary = (n) => {
            const ps = n.edges.map((e) => (e.from === n.key ? e.to : e.from)).filter((k) => pos.has(k) && nodes.get(k).ns === ns).map((k) => pos.get(k));
            return ps.length ? ps.reduce((a, b) => a + b, 0) / ps.length : pos.get(n.key);
          };
          const b = new Map(cols[ci].map((n) => [n.key, bary(n)]));
          cols[ci].sort((p, q) => b.get(p.key) - b.get(q.key));
          index();
        }
      }
      const rows = Math.max(...cols.map((c) => c.length), 1);
      const h = HEAD + PAD + rows * NODE_H + (rows - 1) * ROW_GAP + PAD;
      cols.forEach((c, ci) => {
        // Center short columns against the tallest one.
        const off = ((rows - c.length) * (NODE_H + ROW_GAP)) / 2;
        c.forEach((n, i) => {
          n.x = PAD + ci * (NODE_W + COL_GAP);
          n.y = y + HEAD + PAD + off + i * (NODE_H + ROW_GAP);
        });
      });
      const scoped = groups.get(ns).map((n) => CLUSTER_SCOPED.has(n.kind));
      const label = ns ? null : scoped.every(Boolean) ? "cluster-scoped" : scoped.some(Boolean) ? "cluster-scoped / no namespace" : "no namespace set";
      boxes.push({ ns, y, h, label });
      y += h + GROUP_GAP;
    }
    const width = PAD * 2 + tiersUsed.length * NODE_W + (tiersUsed.length - 1) * COL_GAP;
    return { nodes, edges, boxes, width, height: Math.max(y - GROUP_GAP, 1) };
  }

  function renderMap(rs) {
    if (!rs.length) return '<h1>Map</h1><p class="empty">Nothing matches the current filter.</p>';
    const { nodes, edges, boxes, width, height } = mapLayout(rs);
    const nsBoxes = boxes
      .map((b) => `<g class="nsbox"><rect x="0.5" y="${b.y + 0.5}" width="${width - 1}" height="${b.h - 1}" rx="12"/>
        <text x="${PAD}" y="${b.y + 22}">${b.ns ? `namespace <tspan class="nsn">${esc(b.ns)}</tspan>` : esc(b.label)}</text></g>`)
      .join("");
    const edgeSvg = edges
      .map((e, i) => {
        const a = nodes.get(e.from), b = nodes.get(e.to);
        const x1 = a.x + NODE_W, y1 = a.y + NODE_H / 2, x2 = b.x, y2 = b.y + NODE_H / 2;
        const forward = x2 > x1;
        const d = forward
          ? `M${x1},${y1} C${x1 + COL_GAP / 2},${y1} ${x2 - COL_GAP / 2},${y2} ${x2},${y2}`
          : `M${a.x + NODE_W / 2},${a.y + NODE_H} C${a.x + NODE_W / 2},${a.y + NODE_H + 40} ${b.x + NODE_W / 2},${b.y - 40} ${b.x + NODE_W / 2},${b.y}`;
        return `<path class="edge${e.missing ? " missing " + e.missing : ""}" data-i="${i}" data-from="${esc(e.from)}" data-to="${esc(e.to)}" d="${d}" marker-end="url(#arrow${e.missing === "warn" ? "-warn" : ""})"><title>${esc(`${a.kind} ${a.name} ${e.label} ${b.kind} ${b.name}`)}</title></path>`;
      })
      .join("");
    const nodeSvg = [...nodes.values()]
      .map((n) => {
        const r = n.r;
        const warn = r ? r.notes.filter((x) => x.level === "warn").length : 0;
        const cls = ["node", CAT_CLASS[categoryOf(n.kind)] || "c-custom", n.ghost ? `ghost ${n.ghost}` : "", route.sel === n.key ? "sel" : "", warn ? "has-warn" : ""].join(" ");
        const abbr = ABBR[n.kind] || n.kind.slice(0, 6).toLowerCase();
        const sub = n.ghost ? `${n.kind} · not in this output` : `${n.kind} · ${nodeInfo(r)}`;

        return `<g class="${cls}" data-key="${esc(n.key)}" transform="translate(${n.x},${n.y})" tabindex="${r ? 0 : -1}" role="${r ? "button" : "img"}" aria-label="${esc(`${n.kind} ${n.name}`)}">
          <title>${esc(`${n.kind} ${n.name}${n.ns ? ` (namespace ${n.ns})` : ""}${r ? ` · ${r.component}, wave ${r.wave}` : ""}`)}</title>
          <rect class="box" width="${NODE_W}" height="${NODE_H}" rx="9"/>
          <rect class="stripe" width="5" height="${NODE_H}" rx="2"/>
          <circle class="badge" cx="30" cy="${NODE_H / 2}" r="17"/>
          <text class="abbr" x="30" y="${NODE_H / 2 + 4}" text-anchor="middle">${esc(abbr)}</text>
          <text class="nm" x="56" y="23" data-max="${NODE_W - 56 - (warn ? 26 : 10)}">${esc(n.name)}</text>
          <text class="sub" x="56" y="41" data-max="${NODE_W - 56 - (r ? 30 : 10)}">${esc(sub)}</text>
          ${warn ? `<g class="warnb"><circle cx="${NODE_W - 12}" cy="12" r="8"/><text x="${NODE_W - 12}" y="16" text-anchor="middle">!</text></g>` : ""}
          ${r ? `<text class="wave" x="${NODE_W - 10}" y="${NODE_H - 9}" text-anchor="end">w${esc(r.wave)}</text>` : ""}
        </g>`;
      })
      .join("");
    const legend = `<div class="legend"><span><i class="lg c-net"></i>network</span><span><i class="lg c-work"></i>workload</span><span><i class="lg c-conf"></i>config</span><span><i class="lg c-store"></i>storage</span><span><i class="lg c-rbac"></i>access</span><span><i class="lg ghost"></i>not in this output</span></div>`;
    return `<div class="map-head"><h1>Map</h1>${legend}<div class="zoom"><button type="button" data-zoom="out" aria-label="Zoom out">−</button><button type="button" data-zoom="fit">Fit</button><button type="button" data-zoom="in" aria-label="Zoom in">+</button></div></div>
      <div class="map" id="map"><svg id="map-svg" viewBox="0 0 ${width} ${height}" data-w="${width}" data-h="${height}" width="${width * (mapScale || 1)}" height="${height * (mapScale || 1)}" role="group" aria-label="Map of resources">
        <defs>
          <marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="arrowhead" d="M0,0 L10,5 L0,10 z"/></marker>
          <marker id="arrow-warn" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="arrowhead warn" d="M0,0 L10,5 L0,10 z"/></marker>
        </defs>
        ${nsBoxes}<g class="edges">${edgeSvg}</g><g class="nodes">${nodeSvg}</g></svg></div>`;
  }

  function wireMap() {
    const svg = $("map-svg");
    if (!svg) return;
    const setScale = (s) => {
      mapScale = Math.min(2.5, Math.max(0.25, s));
      svg.setAttribute("width", svg.dataset.w * mapScale);
      svg.setAttribute("height", svg.dataset.h * mapScale);
    };
    // Shorten names that don't fit their box (the full name is in the tooltip and detail panel).
    for (const t of svg.querySelectorAll("text[data-max]")) {
      const max = Number(t.dataset.max);
      if (t.getComputedTextLength() <= max) continue;
      let text = t.textContent;
      while (text.length > 1 && t.getComputedTextLength() > max) {
        text = text.slice(0, -1);
        t.textContent = text.trimEnd() + "…";
      }
    }
    const fit = () => setScale(Math.min(1, ($("map").clientWidth - 26) / svg.dataset.w));
    if (mapScale == null) fit();
    for (const b of $("main").querySelectorAll("[data-zoom]")) {
      b.onclick = () => {
        if (b.dataset.zoom === "fit") fit();
        else setScale(mapScale * (b.dataset.zoom === "in" ? 1.2 : 1 / 1.2));
      };
    }
    const nodesEls = [...svg.querySelectorAll(".node")];
    const edgeEls = [...svg.querySelectorAll(".edge")];
    const focus = (key) => {
      if (!key) {
        svg.classList.remove("focus");
        return;
      }
      const near = new Set([key]);
      for (const e of edgeEls) {
        const on = e.dataset.from === key || e.dataset.to === key;
        e.classList.toggle("lit", on);
        if (on) near.add(e.dataset.from).add(e.dataset.to);
      }
      for (const n of nodesEls) n.classList.toggle("lit", near.has(n.dataset.key));
      svg.classList.add("focus");
    };
    for (const n of nodesEls) {
      n.onmouseenter = () => focus(n.dataset.key);
      n.onmouseleave = () => focus(route.sel && route.sel !== n.dataset.key ? route.sel : null);
      const open = () => {
        if (!n.dataset.key.startsWith("ghost:")) location.hash = hrefFor({ sel: n.dataset.key }).slice(1);
      };
      n.onclick = open;
      n.onkeydown = (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), open());
    }
    if (route.sel) focus(route.sel);
  }

  function render() {
    parseHash();
    renderNamespaces();
    const rs = visible();
    renderNav(rs);
    let html;
    if (route.view === "overview") html = renderOverview(rs);
    else if (route.view === "map") html = renderMap(rs);
    else if (route.view === "all") html = renderTable(rs, "All resources");
    else {
      const kind = route.view.slice(5);
      html = renderTable(rs.filter((r) => r.kind === kind), kind);
    }
    $("main").innerHTML = html;
    if (route.view === "map") wireMap();
    for (const tr of $("main").querySelectorAll("tbody tr")) {
      tr.onclick = (e) => {
        if (e.target.closest("a")) return;
        location.hash = hrefFor({ sel: tr.dataset.key }).slice(1);
      };
    }
    renderDetail();
  }

  // ---------- data ----------

  let uiInputs = null; // null: use the command line's inputs
  const snapshot = window.CLEAT_SNAPSHOT;
  const embedded = new URLSearchParams(location.search).has("embed");
  if (embedded) document.body.classList.add("embed");

  function renderInputs(inputs) {
    const box = $("inputs");
    $("inputs-toggle").hidden = !inputs.length;
    $("inputs-toggle").textContent = `Inputs (${inputs.length})`;
    const names = inputs.map((i) => i.name).join("\n");
    if (box.dataset.names === names) return;
    box.dataset.names = names;
    box.replaceChildren(
      ...inputs.map((info) => {
        const label = document.createElement("label");
        label.innerHTML = `<span>${esc(info.name)} <small title="${esc(info.definition || info.ty)}">${esc(info.ty)}</small></span>`;
        const input = document.createElement("input");
        input.name = info.name;
        input.spellcheck = false;
        input.disabled = !!snapshot;
        input.value = uiInputs ? uiInputs[info.name] ?? "" : info.given ?? "";
        input.placeholder = info.default != null ? `${info.default} (default)` : "required";
        input.oninput = () => {
          uiInputs = Object.fromEntries([...box.querySelectorAll("input")].map((i) => [i.name, i.value]));
          clearTimeout(timer);
          timer = setTimeout(poll, 250);
        };
        label.append(input);
        return label;
      }),
    );
    $("inputs-note").textContent = snapshot
      ? "A snapshot: these are the inputs it was rendered with."
      : "Values are decoded by each input's type, as with --set. Leave one empty to use its default.";
  }

  function apply(next) {
    data = next;
    $("file").textContent = next.file || "";
    renderInputs(list(next.inputs));
    const errors = list(next.errors);
    const banner = $("banner");
    if (errors.length) {
      banner.hidden = false;
      const stale = model.res.length ? '<div class="stale">Showing the last version that rendered.</div>' : "";
      banner.innerHTML = `<strong>${errors.length} error${errors.length > 1 ? "s" : ""}</strong> ${stale}<ul>${errors
        .map((e) => `<li>${e.line ? `${esc(next.file)}:${e.line}:${e.col}: ` : ""}${esc(e.msg)}</li>`)
        .join("")}</ul>`;
      $("status").className = "status bad";
      $("status").textContent = "doesn't render";
    } else {
      banner.hidden = true;
      model = build(next);
      const warnings = list(next.warnings).length;
      $("status").className = "status ok";
      $("status").textContent = snapshot || embedded
        ? "snapshot"
        : `live · ${new Date().toLocaleTimeString()}${warnings ? ` · ${warnings} warning${warnings > 1 ? "s" : ""}` : ""}`;
    }
    render();
  }

  let timer = 0;
  let last = "";
  async function poll() {
    clearTimeout(timer);
    try {
      const res = await fetch("/api/state", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(uiInputs ? { inputs: uiInputs } : {}),
      });
      const text = await res.text();
      if (text !== last) {
        last = text;
        apply(JSON.parse(text));
      }
    } catch {
      $("status").className = "status bad";
      $("status").textContent = "cleat view isn't running";
    }
    timer = setTimeout(poll, 1000);
  }

  $("inputs-toggle").onclick = () => {
    const panel = $("inputs-panel");
    panel.hidden = !panel.hidden;
    $("inputs-toggle").setAttribute("aria-expanded", String(!panel.hidden));
  };
  $("ns").onchange = render;
  $("search").oninput = render;
  addEventListener("hashchange", render);
  addEventListener("keydown", (e) => {
    if (e.key === "Escape" && route.sel) location.hash = hrefFor({ sel: null }).slice(1);
  });

  if (snapshot) apply(snapshot);
  else if (embedded) {
    // The embedding page posts `{ type: "cleat-state", state }` whenever its output changes.
    addEventListener("message", (e) => {
      if (e.origin !== location.origin || !e.data || e.data.type !== "cleat-state") return;
      apply(e.data.state);
    });
    const host = window.opener || (window.parent !== window ? window.parent : null);
    if (host) host.postMessage({ type: "cleat-view-ready" }, location.origin);
  } else poll();
})();
