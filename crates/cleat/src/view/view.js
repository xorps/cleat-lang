// cleat view: the exported resources as a cluster-style dashboard.
// Served by `cleat view` (polls /api/state), or embedded in a snapshot
// (window.CLEAT_SNAPSHOT). No dependencies.
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
        key: `${kind}/${ns}/${name}`, out: [], in: [], notes: [],
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
      else if (!optional) note(r, "note", `${label} ${kind} \`${name}\`, which isn't in this output (it may already exist in the cluster)`);
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
          else note(r, "warn", `scales ${t.kind} \`${t.name}\`, which isn't in this output`);
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
    let html = link("overview", "Overview") + link("all", "All resources", rs.length);
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

  function render() {
    parseHash();
    renderNamespaces();
    const rs = visible();
    renderNav(rs);
    let html;
    if (route.view === "overview") html = renderOverview(rs);
    else if (route.view === "all") html = renderTable(rs, "All resources");
    else {
      const kind = route.view.slice(5);
      html = renderTable(rs.filter((r) => r.kind === kind), kind);
    }
    $("main").innerHTML = html;
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
      $("status").textContent = snapshot
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
  else poll();
})();
