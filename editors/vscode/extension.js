// Cleat for VS Code. Dependency-free: a minimal LSP client for `cleat lsp`
// (diagnostics, hover, completion, definitions), a live preview of `cleat
// export` output, and the `cleat view` resource map in a webview.
"use strict";

const vscode = require("vscode");
const cp = require("child_process");
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");

let client;
let log;

/** The `cleat` binary: setting, else the workspace's newest cargo build, else PATH. */
function serverPath() {
  const configured = vscode.workspace.getConfiguration("cleat").get("serverPath");
  if (configured) return configured;
  let best = null;
  for (const folder of vscode.workspace.workspaceFolders || []) {
    for (const profile of ["debug", "release"]) {
      const p = path.join(folder.uri.fsPath, "target", profile, "cleat");
      try {
        const mtime = fs.statSync(p).mtimeMs;
        if (!best || mtime > best.mtime) best = { p, mtime };
      } catch {}
    }
  }
  return best ? best.p : "cleat";
}

/** JSON-RPC over the server's stdio with `Content-Length` framing. */
class LspClient {
  constructor(command) {
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = new Map();
    this.buffer = Buffer.alloc(0);
    this.proc = cp.spawn(command, ["lsp"], { stdio: ["pipe", "pipe", "pipe"] });
    this.proc.stdout.on("data", (chunk) => this.onData(chunk));
    this.proc.stderr.on("data", (d) => log.append(d.toString()));
    this.proc.on("error", (e) => {
      log.appendLine(`failed to start ${command}: ${e.message}`);
      vscode.window.showErrorMessage(
        `Cleat: could not start \`${command} lsp\`. Build it with \`cargo build\` or set cleat.serverPath.`
      );
    });
    this.proc.on("exit", (code) => log.appendLine(`cleat lsp exited (${code})`));
  }

  onData(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const sep = this.buffer.indexOf("\r\n\r\n");
      if (sep < 0) return;
      const header = this.buffer.slice(0, sep).toString();
      const m = /Content-Length:\s*(\d+)/i.exec(header);
      if (!m) {
        this.buffer = this.buffer.slice(sep + 4);
        continue;
      }
      const len = Number(m[1]);
      if (this.buffer.length < sep + 4 + len) return;
      const body = this.buffer.slice(sep + 4, sep + 4 + len).toString("utf8");
      this.buffer = this.buffer.slice(sep + 4 + len);
      this.dispatch(JSON.parse(body));
    }
  }

  dispatch(msg) {
    if (msg.id !== undefined && this.pending.has(msg.id)) {
      const { resolve, reject } = this.pending.get(msg.id);
      this.pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method && this.handlers.has(msg.method)) {
      this.handlers.get(msg.method)(msg.params);
    }
  }

  write(msg) {
    if (!this.proc.stdin.writable) return;
    const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...msg }), "utf8");
    this.proc.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.proc.stdin.write(body);
  }

  request(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.write({ id, method, params });
    });
  }

  notify(method, params) {
    this.write({ method, params });
  }

  onNotification(method, handler) {
    this.handlers.set(method, handler);
  }

  async stop() {
    try {
      await Promise.race([this.request("shutdown"), new Promise((r) => setTimeout(r, 500))]);
      this.notify("exit");
    } finally {
      this.proc.kill();
    }
  }
}

const toRange = (r) => new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character);
const isCleat = (doc) => doc.languageId === "cleat";

async function startClient(context, diagnostics) {
  client = new LspClient(serverPath());
  client.onNotification("textDocument/publishDiagnostics", (p) => {
    const diags = p.diagnostics.map((d) => {
      const sev = d.severity === 2 ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
      const diag = new vscode.Diagnostic(toRange(d.range), d.message, sev);
      diag.source = "cleat";
      return diag;
    });
    diagnostics.set(vscode.Uri.parse(p.uri), diags);
  });
  await client.request("initialize", {
    processId: process.pid,
    rootUri: vscode.workspace.workspaceFolders?.[0]?.uri.toString() ?? null,
    capabilities: {},
  });
  client.notify("initialized", {});
  for (const doc of vscode.workspace.textDocuments) if (isCleat(doc)) didOpen(doc);
}

function didOpen(doc) {
  client?.notify("textDocument/didOpen", {
    textDocument: { uri: doc.uri.toString(), languageId: "cleat", version: doc.version, text: doc.getText() },
  });
}

/** `app.inputs.json` (or .yaml) next to `app.cleat`, whether or not it exists. */
function inputsFile(file) {
  const stem = file.replace(/\.cleat$/, "");
  return [".inputs.json", ".inputs.yaml", ".inputs.yml"].map((e) => stem + e).find((p) => fs.existsSync(p)) || stem + ".inputs.json";
}

/** The inputs file, if there is one, and the cleat.exportArgs setting. */
function inputArgs(file) {
  const inputs = inputsFile(file);
  return [...(fs.existsSync(inputs) ? ["--input", inputs] : []), ...vscode.workspace.getConfiguration("cleat").get("exportArgs")];
}

/** The editor buffer if the file is open (unsaved edits included), else the file. */
function sourceText(uri) {
  const buffer = vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri.toString());
  return buffer ? buffer.getText() : fs.readFileSync(uri.fsPath, "utf8");
}

// ---------- output preview ----------

const PREVIEW_SCHEME = "cleat-output";

class OutputProvider {
  constructor() {
    this.emitter = new vscode.EventEmitter();
    this.onDidChange = this.emitter.event;
    this.open = new Set(); // source URIs with a preview
    this.timers = new Map();
  }

  /** Re-render a source's preview shortly after its last edit. */
  schedule(source) {
    const key = source.toString();
    if (!this.open.has(key)) return;
    clearTimeout(this.timers.get(key));
    this.timers.set(key, setTimeout(() => this.refresh(source), 300));
  }

  refreshAll() {
    for (const key of this.open) this.refresh(vscode.Uri.parse(key));
  }

  previewUri(source) {
    return vscode.Uri.from({ scheme: PREVIEW_SCHEME, path: source.fsPath + ".yaml", query: source.toString() });
  }

  refresh(source) {
    this.emitter.fire(this.previewUri(source));
  }

  provideTextDocumentContent(uri) {
    const source = vscode.Uri.parse(uri.query);
    this.open.add(source.toString());
    const file = source.fsPath;
    // The unsaved buffer goes through stdin; the file name still places imports.
    const args = ["export", file, "--stdin", ...inputArgs(file)];
    const inputs = inputsFile(file);
    return new Promise((resolve) => {
      const child = cp.execFile(serverPath(), args, { cwd: path.dirname(file) }, (err, stdout, stderr) => {
        const header = `# cleat ${args.map((a) => (a === file ? path.basename(a) : a)).join(" ")}\n`;
        if (err && !stdout) {
          const msg = (stderr || err.message).trimEnd().split("\n").map((l) => `# ${l}`).join("\n");
          const hint = fs.existsSync(inputs)
            ? ""
            : `\n#\n# Provide inputs in ${path.basename(inputs)} or the cleat.exportArgs setting.`;
          resolve(`${header}#\n# export failed:\n${msg}${hint}\n`);
        } else {
          resolve(header + stdout);
        }
      });
      child.stdin.end(sourceText(source));
    });
  }
}

// ---------- resource map ----------
// The `cleat view` page in a webview beside the file. The extension runs
// `cleat view --state` on the buffer and posts the result; selecting a
// resource in the page reveals where it's defined.

/** The page with a content security policy that only runs its own scripts (tagged with `nonce`). */
function withCsp(page, nonce) {
  const csp = `default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}'; img-src data:;`;
  return page
    .replace("<head>", `<head>\n  <meta http-equiv="Content-Security-Policy" content="${csp}">`)
    .replaceAll("<script>", `<script nonce="${nonce}">`);
}

class ResourceMaps {
  constructor() {
    this.panels = new Map(); // source URI -> webview panel
    this.timers = new Map();
    this.page = null;
  }

  async show(doc) {
    const key = doc.uri.toString();
    const existing = this.panels.get(key);
    if (existing) {
      existing.reveal(vscode.ViewColumn.Beside, true);
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      "cleat.resources",
      `Resources: ${path.basename(doc.uri.fsPath)}`,
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true },
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [] }
    );
    this.panels.set(key, panel);
    panel.onDidDispose(() => this.panels.delete(key));
    panel.webview.onDidReceiveMessage((m) => this.onMessage(doc.uri, m));
    try {
      panel.webview.html = await this.html();
    } catch (e) {
      panel.webview.html = `<!doctype html><body><p>Cleat: could not load the resource view (${String(e.message || e).replace(/[<&]/g, "")}). Is <code>cleat</code> up to date?</p></body>`;
    }
  }

  /** The page from `cleat view --page`, with a content security policy that only allows its own inline code. */
  async html() {
    if (!this.page) {
      this.page = await new Promise((resolve, reject) =>
        cp.execFile(serverPath(), ["view", "--page"], { maxBuffer: 16 << 20 }, (err, stdout) => (err ? reject(err) : resolve(stdout)))
      );
    }
    return withCsp(this.page, crypto.randomBytes(18).toString("base64"));
  }

  schedule(uri) {
    const key = uri.toString();
    if (!this.panels.has(key)) return;
    clearTimeout(this.timers.get(key));
    this.timers.set(key, setTimeout(() => this.refresh(uri), 300));
  }

  scheduleAll() {
    for (const key of this.panels.keys()) this.schedule(vscode.Uri.parse(key));
  }

  refresh(uri) {
    const panel = this.panels.get(uri.toString());
    if (!panel) return;
    const file = uri.fsPath;
    const compareWith = vscode.workspace.getConfiguration("cleat").get("compareWith");
    const args = ["view", file, "--stdin", "--state", ...inputArgs(file), ...(compareWith ? ["--diff", compareWith] : [])];
    const child = cp.execFile(serverPath(), args, { cwd: path.dirname(file), maxBuffer: 256 << 20 }, (err, stdout, stderr) => {
      let state;
      try {
        state = JSON.parse(stdout);
      } catch {
        state = { file, errors: [{ msg: (stderr || (err && err.message) || "cleat view --state failed").trim() }] };
      }
      panel.webview.postMessage({ type: "cleat-state", state });
    });
    child.stdin.end(sourceText(uri));
  }

  async onMessage(uri, m) {
    if (m.type === "cleat-view-ready") return this.refresh(uri);
    if (m.type === "reveal" && Number.isInteger(m.line) && m.line > 0) {
      const doc = await vscode.workspace.openTextDocument(uri);
      const visible = vscode.window.visibleTextEditors.find((e) => e.document.uri.toString() === uri.toString());
      const editor = await vscode.window.showTextDocument(doc, { viewColumn: visible ? visible.viewColumn : vscode.ViewColumn.One, preserveFocus: true });
      // Spans are 1-based line/column in characters; VS Code positions are 0-based UTF-16.
      const lineText = doc.lineAt(Math.min(m.line - 1, doc.lineCount - 1)).text;
      const col = [...lineText].slice(0, Math.max(0, (m.col || 1) - 1)).join("").length;
      const pos = new vscode.Position(m.line - 1, col);
      editor.selection = new vscode.Selection(pos, pos);
      editor.revealRange(new vscode.Range(pos, pos), vscode.TextEditorRevealType.InCenterIfOutsideViewport);
    }
  }
}

// ---------- activation ----------

async function activate(context) {
  log = vscode.window.createOutputChannel("Cleat");
  const diagnostics = vscode.languages.createDiagnosticCollection("cleat");
  const output = new OutputProvider();
  const maps = new ResourceMaps();
  context.subscriptions.push(log, diagnostics);

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((doc) => isCleat(doc) && didOpen(doc)),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (/\.inputs\.(json|ya?ml)$/.test(e.document.uri.fsPath)) {
        output.refreshAll();
        maps.scheduleAll();
      }
      if (!isCleat(e.document)) return;
      // Any open map may import this file.
      maps.scheduleAll();
      output.schedule(e.document.uri);
      // Other previews may import this file.
      for (const key of output.open) if (key !== e.document.uri.toString()) output.schedule(vscode.Uri.parse(key));
      client?.notify("textDocument/didChange", {
        textDocument: { uri: e.document.uri.toString(), version: e.document.version },
        contentChanges: [{ text: e.document.getText() }],
      });
    }),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      if (!isCleat(doc)) return;
      client?.notify("textDocument/didClose", { textDocument: { uri: doc.uri.toString() } });
      diagnostics.delete(doc.uri);
    }),
    vscode.workspace.onDidSaveTextDocument((doc) => isCleat(doc) && output.refresh(doc.uri)),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      if (doc.uri.scheme === PREVIEW_SCHEME) output.open.delete(doc.uri.query);
    }),
    vscode.languages.registerHoverProvider("cleat", {
      async provideHover(doc, pos) {
        if (!client) return null;
        const r = await client.request("textDocument/hover", {
          textDocument: { uri: doc.uri.toString() },
          position: { line: pos.line, character: pos.character },
        });
        if (!r) return null;
        return new vscode.Hover(new vscode.MarkdownString(r.contents.value), r.range && toRange(r.range));
      },
    }),
    vscode.languages.registerCompletionItemProvider(
      "cleat",
      {
        async provideCompletionItems(doc, pos) {
          if (!client) return null;
          const r = await client.request("textDocument/completion", {
            textDocument: { uri: doc.uri.toString() },
            position: { line: pos.line, character: pos.character },
          });
          if (!r) return null;
          // LSP CompletionItemKind numbers are one more than VS Code's.
          return r.items.map((i) => {
            const item = new vscode.CompletionItem(i.label, (i.kind || 1) - 1);
            item.detail = i.detail;
            if (i.documentation) item.documentation = new vscode.MarkdownString(i.documentation.value);
            item.sortText = i.sortText;
            item.filterText = i.filterText;
            item.insertText = i.textEdit.newText;
            item.range = toRange(i.textEdit.range);
            if (i.textEdit.newText.endsWith(": ") || i.textEdit.newText.endsWith("(")) {
              item.command = { command: "editor.action.triggerSuggest", title: "" };
            }
            return item;
          });
        },
      },
      ".",
      '"',
      "{",
      ","
    ),
    vscode.languages.registerDefinitionProvider("cleat", {
      async provideDefinition(doc, pos) {
        if (!client) return null;
        const r = await client.request("textDocument/definition", {
          textDocument: { uri: doc.uri.toString() },
          position: { line: pos.line, character: pos.character },
        });
        return r ? new vscode.Location(vscode.Uri.parse(r.uri), toRange(r.range)) : null;
      },
    }),
    vscode.workspace.registerTextDocumentContentProvider(PREVIEW_SCHEME, output),
    vscode.commands.registerCommand("cleat.showOutput", async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || !isCleat(editor.document)) {
        vscode.window.showInformationMessage("Open a .cleat file first.");
        return;
      }
      await editor.document.save();
      const doc = await vscode.workspace.openTextDocument(output.previewUri(editor.document.uri));
      await vscode.languages.setTextDocumentLanguage(doc, "yaml");
      await vscode.window.showTextDocument(doc, { viewColumn: vscode.ViewColumn.Beside, preview: true, preserveFocus: true });
    }),
    vscode.commands.registerCommand("cleat.showResources", async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || !isCleat(editor.document)) {
        vscode.window.showInformationMessage("Open a .cleat file first.");
        return;
      }
      await maps.show(editor.document);
    }),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("cleat")) maps.scheduleAll();
    }),
    vscode.commands.registerCommand("cleat.restartServer", async () => {
      await client?.stop();
      diagnostics.clear();
      await startClient(context, diagnostics);
      vscode.window.showInformationMessage("Cleat language server restarted.");
    })
  );

  await startClient(context, diagnostics);
}

async function deactivate() {
  await client?.stop();
}

module.exports = { activate, deactivate, withCsp };
