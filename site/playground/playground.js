// The Cleat playground: cleat-core compiled to WebAssembly (crates/cleat-wasm),
// with files held in memory. No dependencies and no build step.

const EXAMPLES = {
  basics: {
    title: "Basics",
    files: ["basics.cleat"],
    inputs: { env: "prod" },
  },
  quickstart: {
    title: "Kubernetes: quickstart",
    files: ["examples/quickstart.cleat"],
    inputs: { env: "prod", image: "ghcr.io/acme/api:1.4.2" },
  },
  webapp: {
    title: "Kubernetes: web app and database",
    files: ["examples/webapp.cleat"],
    inputs: { env: "dev", image: "ghcr.io/acme/api:1.4.2" },
  },
  overlays: {
    title: "Kubernetes: shared base and prod overlay",
    files: ["examples/overlays/prod.cleat", "examples/overlays/base.cleat"],
    inputs: { image: "ghcr.io/acme/api:1.4.2" },
  },
};
const SCHEMA = "schemas/kubernetes-1.37.json";

const $ = (id) => document.getElementById(id);
const els = {
  example: $("example"), target: $("target"), status: $("status"), share: $("share"),
  files: $("files"), text: $("text"), hl: $("hl"), marks: $("marks"), lines: $("lines"),
  measure: $("measure"), problems: $("problems"), inputs: $("inputs"), output: $("output"),
  copy: $("copy"), tip: $("tip"), complete: $("complete"), completeList: $("complete-list"),
  completeDoc: $("complete-doc"),
};

// ---------- WebAssembly ----------

let wasm;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

async function loadWasm() {
  const res = fetch("cleat.wasm");
  try {
    return (await WebAssembly.instantiateStreaming(res, {})).instance.exports;
  } catch {
    // Served without `application/wasm` (e.g. some local servers).
    return (await WebAssembly.instantiate(await (await res).arrayBuffer(), {})).instance.exports;
  }
}

function call(req) {
  const bytes = encoder.encode(JSON.stringify(req));
  const ptr = wasm.cleat_alloc(bytes.length);
  new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
  wasm.cleat_call(ptr, bytes.length);
  const out = new Uint8Array(wasm.memory.buffer, wasm.cleat_result_ptr(), wasm.cleat_result_len());
  const res = JSON.parse(decoder.decode(out));
  if (res && res.error) throw new Error(res.error);
  return res;
}

// ---------- state ----------

const state = {
  example: "basics",
  files: {},      // path -> text
  current: null,  // path shown in the editor
  inputs: {},     // name -> raw string
  result: null,   // the last `analyze` response
};

const hostName = (path) => "/" + path;
const mainFile = () => EXAMPLES[state.example].files[0];

function setFile(path, text) {
  state.files[path] = text;
  call({ op: "set_file", name: hostName(path), text });
}

// ---------- analysis ----------

let timer = 0;
function scheduleAnalyze(delay = 120) {
  clearTimeout(timer);
  timer = setTimeout(analyze, delay);
}

function analyze() {
  if (!wasm) return;
  const t0 = performance.now();
  let r;
  try {
    r = call({ op: "analyze", file: hostName(mainFile()), inputs: state.inputs, target: els.target.value });
  } catch (e) {
    els.status.textContent = `internal error: ${e.message}`;
    return;
  }
  const ms = performance.now() - t0;
  state.result = r;
  const errors = (r.errors || []).length + (r.eval_errors || []).length;
  els.status.textContent = errors
    ? `${errors} problem${errors === 1 ? "" : "s"} · ${ms.toFixed(0)} ms`
    : `OK · ${ms.toFixed(0)} ms`;
  if (r.inputs) renderInputs(r.inputs);
  renderOutput(r);
  renderProblems(r);
  renderMarks();
}

// ---------- editor ----------

const KEYWORDS = new Set(
  "import from as with refine if then else match for in where export after type input let fn".split(" "),
);
const CONSTS = new Set(["true", "false", "null"]);

const esc = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
const span = (cls, s) => `<span class="t-${cls}">${esc(s)}</span>`;

/** Syntax highlighting, line by line, tracking `"""` strings across lines. */
function highlight(text) {
  let out = "";
  let triple = false;
  for (const line of text.split("\n")) {
    let i = 0;
    while (i < line.length) {
      if (triple) {
        const end = line.indexOf('"""', i);
        const stop = end < 0 ? line.length : end + 3;
        out += span("str", line.slice(i, stop));
        triple = end < 0;
        i = stop;
        continue;
      }
      const rest = line.slice(i);
      let m;
      if (rest.startsWith("//")) {
        out += span("com", rest);
        break;
      }
      if (rest.startsWith('"""')) {
        out += span("str", '"""');
        triple = true;
        i += 3;
        continue;
      }
      if (rest[0] === '"') {
        let j = 1;
        while (j < rest.length && rest[j] !== '"') j += rest[j] === "\\" ? 2 : 1;
        out += span("str", rest.slice(0, j + 1));
        i += j + 1;
        continue;
      }
      if ((m = /^-?\d[\d_]*(\.\d+)?([eE][+-]?\d+)?/.exec(rest)) && !/[A-Za-z0-9_]/.test(line[i - 1] || "")) {
        out += span("num", m[0]);
        i += m[0].length;
        continue;
      }
      if ((m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(rest))) {
        const w = m[0];
        const after = line.slice(i + w.length);
        let cls = null;
        if (KEYWORDS.has(w)) cls = "kw";
        else if (CONSTS.has(w) || w === "self") cls = "const";
        else if (/^[A-Z]/.test(w)) cls = "type";
        else if (/^\s*\(/.test(after)) cls = "fn";
        else if (/^\??\s*:(?!:)/.test(after) && line[i - 1] !== ".") cls = "field";
        out += cls ? span(cls, w) : esc(w);
        i += w.length;
        continue;
      }
      out += esc(line[i]);
      i += 1;
    }
    out += "\n";
  }
  return out + " ";
}

/** Characters (Unicode scalar values, as Cleat counts columns) to a UTF-16 index in `line`. */
function charToIndex(line, col) {
  let i = 0;
  for (let c = 0; c < col && i < line.length; c++) i += line.codePointAt(i) > 0xffff ? 2 : 1;
  return i;
}

function renderMarks() {
  const text = els.text.value;
  const marks = [];
  const r = state.result;
  if (r && state.current === mainFile()) {
    for (const d of r.errors || []) marks.push({ ...d, cls: "" });
    for (const d of r.warnings || []) marks.push({ ...d, cls: "warn" });
    for (const d of r.eval_errors || []) if (d.line) marks.push({ ...d, cls: "" });
  }
  const byLine = new Map();
  for (const m of marks) {
    if (!byLine.has(m.line)) byLine.set(m.line, []);
    byLine.get(m.line).push(m);
  }
  const out = text.split("\n").map((line, n) => {
    const ms = (byLine.get(n + 1) || []).sort((a, b) => a.col - b.col);
    let html = "";
    let at = 0;
    for (const m of ms) {
      const a = Math.max(at, charToIndex(line, m.col - 1));
      const b = Math.max(a + 1, charToIndex(line, m.col - 1 + m.len));
      const seg = line.slice(a, b) || " ";
      html += esc(line.slice(at, a)) + `<mark class="${m.cls}">${esc(seg)}</mark>`;
      at = Math.min(line.length, b);
    }
    return html + esc(line.slice(at));
  });
  els.marks.innerHTML = out.join("\n") + "\n ";
}

function renderEditor() {
  const text = els.text.value;
  els.hl.innerHTML = highlight(text);
  const n = text.split("\n").length;
  if (els.lines.childElementCount !== n) {
    els.lines.textContent = Array.from({ length: n }, (_, i) => i + 1).join("\n");
  }
  renderMarks();
  syncScroll();
}

function syncScroll() {
  const { scrollTop, scrollLeft } = els.text;
  for (const layer of [els.hl, els.marks]) {
    layer.scrollTop = scrollTop;
    layer.scrollLeft = scrollLeft;
  }
  els.lines.style.transform = `translateY(${-scrollTop}px)`;
}

function openFile(path) {
  state.current = path;
  els.text.value = state.files[path] ?? "";
  els.text.scrollTop = 0;
  renderTabs();
  renderEditor();
}

function renderTabs() {
  els.files.replaceChildren(
    ...EXAMPLES[state.example].files.map((path, i) => {
      const b = document.createElement("button");
      b.type = "button";
      b.role = "tab";
      b.textContent = path.split("/").pop();
      b.title = i === 0 ? `${path} (exported)` : path;
      if (i === 0) b.classList.add("main");
      b.setAttribute("aria-selected", String(path === state.current));
      b.onclick = () => openFile(path);
      return b;
    }),
  );
}

// Positions: 0-based line, column in characters.
function cursor() {
  const before = els.text.value.slice(0, els.text.selectionStart).split("\n");
  return { line: before.length - 1, col: [...before[before.length - 1]].length };
}

let metrics;
function measure() {
  const r = els.measure.getBoundingClientRect();
  const cs = getComputedStyle(els.text);
  metrics = {
    ch: r.width / els.measure.textContent.length,
    line: parseFloat(cs.lineHeight),
    padLeft: parseFloat(cs.paddingLeft),
    padTop: parseFloat(cs.paddingTop),
  };
}

function pointToPos(x, y) {
  const r = els.text.getBoundingClientRect();
  const line = Math.floor((y - r.top - metrics.padTop + els.text.scrollTop) / metrics.line);
  const col = Math.floor((x - r.left - metrics.padLeft + els.text.scrollLeft) / metrics.ch);
  return { line, col };
}

function posToPoint(line, col) {
  const r = els.text.getBoundingClientRect();
  return {
    x: r.left + metrics.padLeft + col * metrics.ch - els.text.scrollLeft,
    y: r.top + metrics.padTop + (line + 1) * metrics.line - els.text.scrollTop,
  };
}

// ---------- hover ----------

function renderDoc(md) {
  return esc(md).replace(/`([^`]+)`/g, "<code>$1</code>");
}

function hoverAt(line, col) {
  const r = state.result;
  if (!r || state.current !== mainFile()) return null;
  const L = line + 1;
  const C = col + 1;
  const inside = (d) => d.line === L && d.col <= C && C < d.col + Math.max(1, d.len || 1);
  const err = [...(r.errors || []), ...(r.eval_errors || []), ...(r.warnings || [])].find(inside);
  if (err) return { error: true, html: `<pre>${esc(err.msg)}</pre>` };
  const hs = (r.hovers || []).filter(inside).sort((a, b) => a.len - b.len);
  if (!hs.length) return null;
  const h = hs[0];
  return { html: `<pre>${highlight(h.text).trimEnd()}</pre>` + (h.doc ? `<div class="doc">${renderDoc(h.doc)}</div>` : "") };
}

function showTip(x, y, tip) {
  els.tip.innerHTML = tip.html;
  els.tip.classList.toggle("error", !!tip.error);
  els.tip.hidden = false;
  const w = els.tip.offsetWidth;
  const h = els.tip.offsetHeight;
  els.tip.style.left = `${Math.max(8, Math.min(x, innerWidth - w - 8))}px`;
  els.tip.style.top = `${y + 18 + h > innerHeight ? y - h - 8 : y + 18}px`;
}

els.text.addEventListener("mousemove", (e) => {
  if (!metrics) return;
  const { line, col } = pointToPos(e.clientX, e.clientY);
  const tip = line >= 0 && col >= 0 ? hoverAt(line, col) : null;
  if (tip) showTip(e.clientX, e.clientY, tip);
  else els.tip.hidden = true;
});
els.text.addEventListener("mouseleave", () => (els.tip.hidden = true));

// ---------- completion ----------

const KIND_ICON = { field: "▸", value: "\"", variable: "x", function: "ƒ", module: "◇", type: "T" };
// `anchor`: the caret position the items were computed for; they're stale anywhere else.
const comp = { items: [], index: 0, replaceLen: 0, anchor: -1 };

function requestCompletion(explicit) {
  const { line, col } = cursor();
  let r;
  try {
    r = call({ op: "complete", file: hostName(state.current), line, col });
  } catch {
    return hideCompletion();
  }
  const prefix = els.text.value.slice(els.text.selectionStart - r.replace_len, els.text.selectionStart);
  const p = prefix.replace(/^"/, "").toLowerCase();
  let items = r.items.filter((i) => i.label.toLowerCase().startsWith(p));
  if (!items.length) items = r.items.filter((i) => i.label.toLowerCase().includes(p));
  items.sort((a, b) => (b.preferred - a.preferred) || 0);
  if (!items.length || (!explicit && items.length === 1 && items[0].insert === prefix)) return hideCompletion();
  Object.assign(comp, { items, index: 0, replaceLen: r.replace_len, anchor: els.text.selectionStart });
  renderCompletion();
  const pt = posToPoint(line, col - [...prefix].length);
  els.complete.hidden = false;
  const h = els.complete.offsetHeight;
  els.complete.style.left = `${Math.min(pt.x - 12, innerWidth - els.complete.offsetWidth - 8)}px`;
  els.complete.style.top = `${pt.y + h + 4 > innerHeight ? pt.y - metrics.line - h - 2 : pt.y + 2}px`;
}

function renderCompletion() {
  els.completeList.replaceChildren(
    ...comp.items.slice(0, 200).map((it, i) => {
      const li = document.createElement("li");
      li.role = "option";
      li.setAttribute("aria-selected", String(i === comp.index));
      if (it.preferred) li.classList.add("preferred");
      li.innerHTML = `<span class="kind">${esc(KIND_ICON[it.kind] || "")}</span><span class="label">${esc(it.label)}</span>` +
        (it.detail ? `<span class="detail">${esc(it.detail)}</span>` : "");
      li.onmousedown = (e) => {
        e.preventDefault();
        comp.index = i;
        acceptCompletion();
      };
      return li;
    }),
  );
  const cur = comp.items[comp.index];
  els.completeDoc.innerHTML = cur && cur.doc ? renderDoc(cur.doc) : "";
  els.completeList.children[comp.index]?.scrollIntoView({ block: "nearest" });
}

function hideCompletion() {
  els.complete.hidden = true;
  comp.items = [];
}

function acceptCompletion() {
  const it = comp.items[comp.index];
  if (!it) return;
  const end = els.text.selectionStart;
  els.text.setRangeText(it.insert, end - comp.replaceLen, end, "end");
  hideCompletion();
  edited();
}

// ---------- typing ----------

function edited() {
  els.tip.hidden = true;
  setFile(state.current, els.text.value);
  renderEditor();
  els.output.classList.add("stale");
  scheduleAnalyze();
}

els.text.addEventListener("input", (e) => {
  edited();
  const ch = e.data && e.data[e.data.length - 1];
  if (e.inputType === "insertText" && ch && /[A-Za-z0-9_.:"]/.test(ch)) requestCompletion(false);
  else hideCompletion();
});

els.text.addEventListener("keydown", (e) => {
  if (!els.complete.hidden && els.text.selectionStart !== comp.anchor) hideCompletion();
  if (!els.complete.hidden) {
    const n = Math.min(comp.items.length, 200);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      comp.index = (comp.index + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
      renderCompletion();
      return e.preventDefault();
    }
    if (e.key === "Enter" || e.key === "Tab") {
      acceptCompletion();
      return e.preventDefault();
    }
    if (e.key === "Escape") {
      hideCompletion();
      return e.preventDefault();
    }
    if (/^(ArrowLeft|ArrowRight|Home|End|PageUp|PageDown)$/.test(e.key)) hideCompletion();
  }
  if (e.key === " " && (e.ctrlKey || e.metaKey)) {
    requestCompletion(true);
    return e.preventDefault();
  }
  if (e.key === "Tab" && !e.shiftKey) {
    els.text.setRangeText("  ", els.text.selectionStart, els.text.selectionEnd, "end");
    edited();
    return e.preventDefault();
  }
  if (e.key === "Enter") {
    // Keep the current line's indentation.
    const start = els.text.value.lastIndexOf("\n", els.text.selectionStart - 1) + 1;
    const indent = /^[ \t]*/.exec(els.text.value.slice(start))[0];
    const prev = els.text.value[els.text.selectionStart - 1];
    const extra = prev === "{" || prev === "[" || prev === "(" ? "  " : "";
    els.text.setRangeText("\n" + indent + extra, els.text.selectionStart, els.text.selectionEnd, "end");
    edited();
    return e.preventDefault();
  }
});

els.text.addEventListener("scroll", () => {
  syncScroll();
  hideCompletion();
});
els.text.addEventListener("blur", () => setTimeout(hideCompletion, 100));
els.text.addEventListener("click", hideCompletion);
document.addEventListener("selectionchange", () => {
  if (!els.complete.hidden && els.text.selectionStart !== comp.anchor) hideCompletion();
});

// ---------- panels ----------

function renderInputs(infos) {
  const known = new Set([...els.inputs.querySelectorAll("input")].map((i) => i.name));
  const same = infos.length === known.size && infos.every((i) => known.has(i.name));
  if (same) return;
  els.inputs.replaceChildren(
    ...infos.flatMap((info) => {
      const label = document.createElement("label");
      label.htmlFor = `in-${info.name}`;
      label.innerHTML = `${esc(info.name)}<small title="${esc(info.definition || info.ty)}">${esc(info.ty)}</small>`;
      const input = document.createElement("input");
      input.id = `in-${info.name}`;
      input.name = info.name;
      input.spellcheck = false;
      input.value = state.inputs[info.name] ?? "";
      input.placeholder = info.default != null ? `${info.default} (default)` : "required";
      input.oninput = () => {
        state.inputs[info.name] = input.value;
        els.output.classList.add("stale");
        scheduleAnalyze(200);
      };
      return [label, input];
    }),
  );
}

function renderOutput(r) {
  if (r.output != null) {
    els.output.classList.remove("stale");
    els.output.innerHTML = r.output
      .split("\n")
      .map((l) => {
        if (l === "---") return `<span class="doc">---</span>`;
        const m = /^(\s*(?:- )?)([^\s:"'][^:]*|"[^"]*"):(\s|$)/.exec(l);
        return m ? `${esc(m[1])}<span class="k">${esc(m[2])}</span>${esc(l.slice(m[1].length + m[2].length))}` : esc(l);
      })
      .join("\n");
  } else {
    els.output.classList.add("stale");
  }
}

function renderProblems(r) {
  const rows = [];
  const add = (d, sev, cls) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `problem ${cls}`;
    b.innerHTML = `<span class="sev">${sev}</span>` + (d.line ? `<span class="at">${d.line}:${d.col}</span>` : "") +
      `<span class="msg">${esc(d.msg)}</span>`;
    if (d.line) b.onclick = () => jumpTo(d.line, d.col);
    rows.push(b);
  };
  for (const d of r.errors || []) add(d, "error", "error");
  for (const d of r.eval_errors || []) add(d, "error", "error");
  for (const d of r.warnings || []) add(d, "warning", "warn");
  els.problems.replaceChildren(...rows);
}

function jumpTo(line, col) {
  if (state.current !== mainFile()) openFile(mainFile());
  const lines = els.text.value.split("\n");
  let i = 0;
  for (let n = 0; n < line - 1 && n < lines.length; n++) i += lines[n].length + 1;
  i += charToIndex(lines[line - 1] || "", col - 1);
  els.text.focus();
  els.text.setSelectionRange(i, i);
  const top = (line - 3) * metrics.line;
  if (top < els.text.scrollTop || top > els.text.scrollTop + els.text.clientHeight - 4 * metrics.line) {
    els.text.scrollTop = Math.max(0, top);
  }
}

els.copy.onclick = async () => {
  await navigator.clipboard.writeText(state.result?.output ?? "");
  els.copy.textContent = "Copied";
  setTimeout(() => (els.copy.textContent = "Copy"), 1200);
};

els.target.onchange = () => analyze();

// ---------- examples and sharing ----------

async function fetchText(path) {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`${path}: ${res.status}`);
  return res.text();
}

async function loadExample(name, saved) {
  const ex = EXAMPLES[name] ? name : "basics";
  state.example = ex;
  els.example.value = ex;
  els.tip.hidden = true;
  hideCompletion();
  for (const path of Object.keys(state.files)) call({ op: "remove_file", name: hostName(path) });
  state.files = {};
  const texts = await Promise.all(EXAMPLES[ex].files.map((p) => (saved?.files?.[p] != null ? saved.files[p] : fetchText(p))));
  EXAMPLES[ex].files.forEach((p, i) => setFile(p, texts[i]));
  state.inputs = { ...(saved?.inputs ?? EXAMPLES[ex].inputs) };
  els.inputs.replaceChildren();
  if (saved?.target) els.target.value = saved.target;
  openFile(mainFile());
  analyze();
}

const b64url = {
  encode: (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""),
  decode: (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0)),
};

async function pipe(bytes, stream) {
  return new Uint8Array(await new Response(new Blob([bytes]).stream().pipeThrough(stream)).arrayBuffer());
}

els.share.onclick = async () => {
  const data = { example: state.example, files: state.files, inputs: state.inputs, target: els.target.value };
  const packed = await pipe(encoder.encode(JSON.stringify(data)), new CompressionStream("deflate-raw"));
  const url = `${location.origin}${location.pathname}#code=${b64url.encode(packed)}`;
  history.replaceState(null, "", url);
  await navigator.clipboard.writeText(url).catch(() => {});
  els.share.textContent = "Link copied";
  setTimeout(() => (els.share.textContent = "Share"), 1500);
};

async function fromHash() {
  const m = /#code=([A-Za-z0-9_-]+)/.exec(location.hash);
  if (!m) return null;
  try {
    return JSON.parse(decoder.decode(await pipe(b64url.decode(m[1]), new DecompressionStream("deflate-raw"))));
  } catch {
    return null;
  }
}

// ---------- start ----------

for (const [name, ex] of Object.entries(EXAMPLES)) els.example.add(new Option(ex.title, name));
els.example.onchange = () => {
  history.replaceState(null, "", location.pathname);
  loadExample(els.example.value);
};
addEventListener("resize", measure);

(async () => {
  try {
    els.status.textContent = "Loading Cleat and the Kubernetes 1.37 schema…";
    const [exports, schema] = await Promise.all([loadWasm(), fetchText(SCHEMA)]);
    wasm = exports;
    call({ op: "set_file", name: hostName(SCHEMA), text: schema });
    measure();
    const saved = await fromHash();
    await loadExample(saved?.example ?? "basics", saved);
  } catch (e) {
    els.status.textContent = `Failed to load: ${e.message}`;
    throw e;
  }
})();
