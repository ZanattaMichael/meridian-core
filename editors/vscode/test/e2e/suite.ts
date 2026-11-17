// Runs inside Visual Studio Code. Everything asserted here went through the
// editor, the extension and a real `meridian lsp`.

import assert from "node:assert/strict";
import * as path from "node:path";
import * as vscode from "vscode";

function codeOf(diagnostic: vscode.Diagnostic): string {
  const code = diagnostic.code;
  return typeof code === "object" && code !== null ? String(code.value) : String(code ?? "");
}

async function until<T>(what: string, probe: () => T | undefined | Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + 60_000;
  for (;;) {
    const found = await probe();
    if (found !== undefined) {
      return found;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}

function labels(list: vscode.CompletionList): string[] {
  return list.items.map((item) => (typeof item.label === "string" ? item.label : item.label.label));
}

export async function run(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  assert.ok(folder, "the test workspace is open");
  const uri = vscode.Uri.file(path.join(folder, "web.yaml"));
  const document = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(document);

  const extension = vscode.extensions.getExtension("meridian.meridian");
  assert.ok(extension, "the extension is installed");
  await extension.activate();

  // Diagnostics: the linter's finding and the target's, each in its place.
  const diagnostics = await until("diagnostics", () => {
    const found = vscode.languages.getDiagnostics(uri);
    const codes = found.map(codeOf);
    return codes.includes("unquoted-mode") && codes.includes("unsupported-param") ? found : undefined;
  });
  const mode = diagnostics.find((d) => codeOf(d) === "unquoted-mode");
  assert.equal(mode?.range.start.line, 18);
  assert.equal(mode?.severity, vscode.DiagnosticSeverity.Warning);
  const param = diagnostics.find((d) => codeOf(d) === "unsupported-param");
  assert.equal(param?.range.start.line, 19);
  assert.equal(param?.severity, vscode.DiagnosticSeverity.Error);

  // Completion: the resource types of the ansible plugin, at `type: `.
  const types = await vscode.commands.executeCommand<vscode.CompletionList>(
    "vscode.executeCompletionItemProvider",
    uri,
    new vscode.Position(11, 12),
  );
  for (const expected of ["package", "file", "service"]) {
    assert.ok(labels(types).includes(expected), `completion offers ${expected}: ${labels(types)}`);
  }

  // Hover: the documentation of a field.
  const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
    "vscode.executeHoverProvider",
    uri,
    new vscode.Position(20, 8),
  );
  const text = hovers
    .flatMap((hover) => hover.contents)
    .map((content) => (typeof content === "string" ? content : content.value))
    .join("\n");
  assert.match(text, /dependsOn/);

  // Go to definition: from a dependency to the resource it names.
  const definitions = await vscode.commands.executeCommand<(vscode.Location | vscode.LocationLink)[]>(
    "vscode.executeDefinitionProvider",
    uri,
    new vscode.Position(21, 24),
  );
  assert.equal(definitions.length, 1);
  const target = definitions[0];
  const line = "targetRange" in target ? target.targetRange.start.line : target.range.start.line;
  assert.equal(line, 10);

  // A quick fix for the mode.
  const actions = await vscode.commands.executeCommand<vscode.CodeAction[]>(
    "vscode.executeCodeActionProvider",
    uri,
    mode!.range,
  );
  assert.ok(
    actions.some((action) => action.title.includes('"0644"')),
    `a quick fix quotes the mode: ${actions.map((a) => a.title)}`,
  );

  // Formatting: the over-indented list entry is the only thing wrong.
  const edits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
    "vscode.executeFormatDocumentProvider",
    uri,
    { tabSize: 2, insertSpaces: true },
  );
  assert.ok(edits !== undefined && edits.length > 0, "the formatter has something to change");
  const edit = new vscode.WorkspaceEdit();
  edit.set(uri, edits);
  await vscode.workspace.applyEdit(edit);
  assert.match(document.getText(), /\n {6}dependsOn:\n {8}- resource: nginx\n/);
  assert.match(document.getText(), /mode: 0644/, "formatting changes no value");

  // The server's own command is registered and runs.
  await vscode.commands.executeCommand("meridian.reloadPlugins");
  await vscode.commands.executeCommand("meridian.restartServer");
  await until("diagnostics after a restart", () =>
    vscode.languages.getDiagnostics(uri).some((d) => codeOf(d) === "unquoted-mode") ? true : undefined,
  );

  // A YAML file that is not a Meridian document gets nothing.
  const other = vscode.Uri.file(path.join(folder, "other.yaml"));
  await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(other));
  await new Promise((resolve) => setTimeout(resolve, 2_000));
  assert.deepEqual(vscode.languages.getDiagnostics(other), []);
  const none = await vscode.commands.executeCommand<vscode.TextEdit[] | undefined>(
    "vscode.executeFormatDocumentProvider",
    other,
    { tabSize: 2, insertSpaces: true },
  );
  assert.ok(none === undefined || none.length === 0, "other YAML is not formatted");
}
