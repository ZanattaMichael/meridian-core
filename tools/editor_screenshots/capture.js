// Regenerates the images in docs/tooling/images.
//
// Nothing in the pictures is drawn by hand. The script starts the real
// `meridian lsp`, connects it to the Monaco editor (the editor component
// Visual Studio Code is built on) in a headless Chromium, and photographs
// what the server answers.
//
//   cargo build --workspace
//   cd tools/editor_screenshots && npm ci && node capture.js
//
// It needs Playwright with a Chromium (`npm i playwright && npx playwright
// install chromium`) and, for the animation, ImageMagick's `convert`.

const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { chromium } = require("playwright");

const root = path.resolve(__dirname, "../..");
const meridian = process.env.MERIDIAN || path.join(root, "target/debug/meridian");
const out = path.join(root, "docs/tooling/images");
const monaco = path.join(__dirname, "node_modules/monaco-editor/min");

// ---------------------------------------------------------------- the server

class Server {
  constructor() {
    this.child = spawn(meridian, ["lsp"], { stdio: ["pipe", "pipe", "inherit"] });
    this.buffer = Buffer.alloc(0);
    this.next = 1;
    this.waiting = new Map();
    this.onNotification = () => {};
    this.child.stdout.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      for (;;) {
        const end = this.buffer.indexOf("\r\n\r\n");
        if (end < 0) return;
        const length = Number(/Content-Length: (\d+)/.exec(this.buffer.subarray(0, end).toString())[1]);
        if (this.buffer.length < end + 4 + length) return;
        const message = JSON.parse(this.buffer.subarray(end + 4, end + 4 + length).toString());
        this.buffer = this.buffer.subarray(end + 4 + length);
        if (message.id !== undefined && message.method === undefined) {
          this.waiting.get(message.id)?.(message.result ?? null);
          this.waiting.delete(message.id);
        } else if (message.id === undefined) {
          this.onNotification(message.method, message.params);
        }
      }
    });
  }
  send(message) {
    const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...message }));
    this.child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.child.stdin.write(body);
  }
  request(method, params) {
    const id = this.next++;
    return new Promise((resolve) => {
      this.waiting.set(id, resolve);
      this.send({ id, method, params });
    });
  }
  notify(method, params) {
    this.send({ method, params });
  }
}

// ------------------------------------------------------------------ the page

const PAGE = `<!doctype html>
<html><head><meta charset="utf-8">
<style>
  html, body { margin: 0; height: 100%; background: #1f1f1f; overflow: hidden; }
  #tab { height: 35px; background: #181818; border-bottom: 1px solid #2b2b2b;
         font: 13px -apple-system, "Segoe UI", sans-serif; color: #ccc; display: flex; }
  #tab span { padding: 9px 16px; background: #1f1f1f; border-top: 1px solid #0078d4;
              border-right: 1px solid #2b2b2b; }
  #editor { position: absolute; top: 36px; bottom: 0; left: 0; right: 0; }
</style></head>
<body><div id="tab"><span id="name"></span></div><div id="editor"></div>
<script src="/vs/loader.js"></script>
<script>
require.config({ paths: { vs: "/vs" } });
require(["vs/editor/editor.main"], () => {
  const uri = monaco.Uri.parse(window.documentUri);
  const model = monaco.editor.createModel("", "yaml", uri);
  const editor = monaco.editor.create(document.getElementById("editor"), {
    model, theme: "vs-dark", fontSize: 14, minimap: { enabled: false },
    automaticLayout: true, tabSize: 2, scrollBeyondLastLine: false,
    quickSuggestions: false, wordBasedSuggestions: "off", lightbulb: { enabled: "on" },
    suggest: { showStatusBar: false }, hover: { above: false }, padding: { top: 8 },
  });
  const range = (r) => new monaco.Range(r.start.line + 1, r.start.character + 1,
                                        r.end.line + 1, r.end.character + 1);
  const position = (p) => ({ line: p.lineNumber - 1, character: p.column - 1 });
  const at = (p) => ({ textDocument: { uri: window.documentUri }, position: position(p) });
  const kinds = { 5: "Field", 20: "EnumMember", 18: "Reference", 7: "Class", 15: "Snippet" };

  let version = 1;
  model.onDidChangeContent(() => window.lspNotify("textDocument/didChange", {
    textDocument: { uri: window.documentUri, version: ++version },
    contentChanges: [{ text: model.getValue() }],
  }));

  monaco.languages.registerCompletionItemProvider("yaml", {
    triggerCharacters: [" ", ":", "{", "$", ".", "-"],
    async provideCompletionItems(_, p) {
      const reply = await window.lspRequest("textDocument/completion", at(p));
      const items = Array.isArray(reply) ? reply : reply?.items ?? [];
      return { suggestions: items.map((i) => ({
        label: i.label,
        kind: monaco.languages.CompletionItemKind[kinds[i.kind] ?? "Text"],
        detail: i.detail,
        documentation: i.documentation ? { value: i.documentation.value } : undefined,
        sortText: i.sortText, filterText: i.filterText,
        insertText: i.textEdit.newText, range: range(i.textEdit.range),
        insertTextRules: i.insertTextFormat === 2
          ? monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet : undefined,
      })) };
    },
  });
  monaco.languages.registerHoverProvider("yaml", {
    async provideHover(_, p) {
      const reply = await window.lspRequest("textDocument/hover", at(p));
      return reply && { range: range(reply.range), contents: [{ value: reply.contents.value }] };
    },
  });
  monaco.languages.registerCodeActionProvider("yaml", {
    async provideCodeActions(_, r, context) {
      const reply = await window.lspRequest("textDocument/codeAction", {
        textDocument: { uri: window.documentUri },
        range: { start: position(r.getStartPosition()), end: position(r.getEndPosition()) },
        context: { diagnostics: context.markers.map((m) => m.lsp).filter(Boolean) },
      });
      return { dispose() {}, actions: (reply ?? []).map((a) => ({
        title: a.title, kind: a.kind, isPreferred: a.isPreferred,
        edit: { edits: Object.values(a.edit?.changes ?? {}).flat().map((e) => ({
          resource: uri, versionId: undefined,
          textEdit: { range: range(e.range), text: e.newText },
        })) },
      })) };
    },
  });
  window.setDiagnostics = (list) => monaco.editor.setModelMarkers(model, "meridian", list.map((d) => ({
    ...range(d.range), message: d.message, code: d.code, source: d.source, lsp: d,
    severity: [0, monaco.MarkerSeverity.Error, monaco.MarkerSeverity.Warning,
               monaco.MarkerSeverity.Info, monaco.MarkerSeverity.Hint][d.severity],
    startLineNumber: d.range.start.line + 1, startColumn: d.range.start.character + 1,
    endLineNumber: d.range.end.line + 1, endColumn: d.range.end.character + 1,
  })));
  window.editor = editor;
  window.ready = true;
});
</script></body></html>`;

function serve() {
  const types = { ".js": "text/javascript", ".css": "text/css", ".ttf": "font/ttf" };
  const server = http.createServer((request, response) => {
    const url = request.url.split("?")[0];
    if (url === "/") {
      response.writeHead(200, { "content-type": "text/html" });
      return response.end(PAGE);
    }
    const file = path.join(monaco, url);
    if (!file.startsWith(monaco) || !fs.existsSync(file)) {
      response.writeHead(404);
      return response.end();
    }
    response.writeHead(200, { "content-type": types[path.extname(file)] ?? "application/octet-stream" });
    fs.createReadStream(file).pipe(response);
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

// ---------------------------------------------------------------- the scenes

const HEAD = `apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: nginx
      type: package
      params:
        name: nginx
`;

const SITE = `    - id: site
      type: file
      params:
        path: /etc/nginx/conf.d/site.conf
        content: "server { listen 80; }"
`;

const PROBLEMS = `${HEAD}    - id: site
      type: file
      params:
        path: /etc/nginx/conf.d/site.conf
        mode: 0644
        colour: blue
      dependsOn:
        - resource: ngnix
    - id: web
      type: service
      state: bouncing
      params:
        name: nginx
`;

async function main() {
  fs.mkdirSync(out, { recursive: true });
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "meridian-shots-"));
  const file = path.join(work, "web.yaml");
  const documentUri = `file://${file}`;

  const lsp = new Server();
  const web = await serve();
  const browser = await chromium.launch();
  const page = await browser.newPage({
    viewport: { width: 980, height: 560 },
    deviceScaleFactor: 2,
  });
  await page.addInitScript((uri) => { window.documentUri = uri; }, documentUri);
  await page.exposeFunction("lspRequest", (method, params) => lsp.request(method, params));
  await page.exposeFunction("lspNotify", (method, params) => lsp.notify(method, params));
  lsp.onNotification = (method, params) => {
    if (method === "textDocument/publishDiagnostics" && params.uri === documentUri) {
      page.evaluate((list) => window.setDiagnostics?.(list), params.diagnostics).catch(() => {});
    }
  };

  await lsp.request("initialize", {
    processId: process.pid,
    rootUri: `file://${work}`,
    capabilities: {},
    initializationOptions: {},
  });
  lsp.notify("initialized", {});
  lsp.notify("textDocument/didOpen", {
    textDocument: { uri: documentUri, languageId: "yaml", version: 1, text: "" },
  });

  await page.goto(`http://127.0.0.1:${web.address().port}/`);
  await page.waitForFunction("window.ready === true");
  await page.evaluate(() => { document.getElementById("name").textContent = "web.yaml"; });

  const settle = (ms = 700) => page.waitForTimeout(ms);
  const set = async (text, line, column) => {
    await page.evaluate(([t, l, c]) => {
      window.editor.setValue(t);
      window.editor.setPosition({ lineNumber: l, column: c });
      window.editor.revealLineInCenterIfOutsideViewport(l);
      window.editor.focus();
    }, [text, line, column]);
    await settle();
  };
  const run = (action) => page.evaluate((a) => window.editor.trigger("shots", a, {}), action);
  const shot = (name) => page.screenshot({ path: path.join(out, name) });
  const endOf = (text) => {
    const lines = text.split("\n");
    return [lines.length, lines[lines.length - 1].length + 1];
  };

  if (!process.env.ONLY_ANIMATION) {
  // 1. Resource types, as the target's plugin lists them.
  let text = `${HEAD}    - id: site\n      type: `;
  await set(text, ...endOf(text));
  await run("editor.action.triggerSuggest");
  await settle();
  await run("toggleSuggestionDetails");
  await settle();
  await shot("completion-types.png");

  // 2. The params the type takes on this target.
  await page.keyboard.press("Escape");
  text = `${HEAD}    - id: site\n      type: file\n      params:\n        path: /etc/nginx/conf.d/site.conf\n        `;
  await set(text, ...endOf(text));
  await run("editor.action.triggerSuggest");
  await settle();
  await shot("completion-params.png");

  // 3. The resources a dependency can name.
  await page.keyboard.press("Escape");
  text = `${HEAD}${SITE}      dependsOn:\n        - resource: `;
  await set(text, ...endOf(text));
  await run("editor.action.triggerSuggest");
  await settle();
  await shot("completion-references.png");

  // 4. Documentation on hover.
  await page.keyboard.press("Escape");
  text = `${HEAD}${SITE}      notifies:\n        - resource: nginx\n          action: restart\n`;
  await set(text, 20, 9);
  await run("editor.action.showHover");
  await settle();
  await shot("hover.png");

  // 5. The compiler's errors and the linter's findings, where they are.
  await page.keyboard.press("Escape");
  await set(PROBLEMS, 22, 24);
  await settle(1200);
  await run("editor.action.showHover");
  await settle();
  await shot("diagnostics.png");

  // 6. A quick fix.
  await page.keyboard.press("Escape");
  await set(PROBLEMS, 19, 16);
  await settle(1200);
  await run("editor.action.quickFix");
  await settle();
  await shot("quick-fix.png");
  await page.keyboard.press("Escape");
  }

  // 7. Writing a resource, as an animation.
  const frames = path.join(work, "frames");
  fs.mkdirSync(frames);
  let frame = 0;
  const still = async (count = 1) => {
    const name = path.join(frames, `${String(frame++).padStart(4, "0")}.png`);
    await page.screenshot({ path: name, scale: "css" });
    for (let i = 1; i < count; i++) {
      fs.copyFileSync(name, path.join(frames, `${String(frame++).padStart(4, "0")}.png`));
    }
  };
  const type = async (keys) => {
    for (const key of keys) {
      await page.keyboard.type(key);
      await settle(120);
      await still();
    }
  };
  const accept = async (pause = 6) => {
    await settle(500);
    await still(pause);
    await page.keyboard.press("Enter");
    await settle(300);
    await still(3);
  };
  // Monaco keeps the previous line's indent, which after `- id:` is the
  // dash's column; the two spaces put the cursor under `id`.
  const debug = async (label) => {
    if (process.env.DEBUG) {
      const tail = await page.evaluate(() => window.editor.getValue().split("\n").slice(14).join("\n"));
      console.error(`--- ${label}\n${tail}`);
    }
  };
  text = `${HEAD}    `;
  await set(text, ...endOf(text));
  await still(4);
  await type("- id: site");
  await page.keyboard.press("Enter");
  await type("  ty");
  await run("editor.action.triggerSuggest");
  await accept();
  await debug("type");
  await run("editor.action.triggerSuggest");
  await type("fi");
  await accept();
  await debug("file");
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await type("pa");
  await run("editor.action.triggerSuggest");
  await accept();
  await debug("params");
  await run("editor.action.triggerSuggest");
  await accept(8);
  await debug("path");
  await type("/etc/motd");
  await page.keyboard.press("Enter");
  await run("editor.action.triggerSuggest");
  await settle(500);
  await still(5);
  await type("mo");
  await accept();
  await page.keyboard.press("Tab");
  await page.keyboard.press("End");
  await debug("mode");
  await page.keyboard.press("Enter");
  await type("colour: blue");
  await settle(1200);
  await still(4);
  await page.keyboard.press("Home");
  await run("editor.action.showHover");
  await settle(600);
  await debug("end");
  await still(20);

  const made = spawnSync("convert", [
    "-delay", "14", "-loop", "0", path.join(frames, "*.png"),
    "-layers", "Optimize", path.join(out, "writing-a-resource.gif"),
  ], { stdio: "inherit" });
  if (made.status !== 0) {
    console.error("convert failed; the animation was not written");
    process.exitCode = 1;
  }

  await browser.close();
  web.close();
  await lsp.request("shutdown", null);
  lsp.notify("exit", null);
  fs.rmSync(work, { recursive: true, force: true });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
