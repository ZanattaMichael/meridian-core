// The Meridian extension: starts `meridian lsp` and connects the editor to
// it. Everything the editor shows (completion, hover, diagnostics,
// navigation, formatting, quick fixes) comes from that server, which is the
// compiler itself, so the extension holds no knowledge of the language.

import * as os from "node:os";
import * as vscode from "vscode";
import {
  Executable,
  LanguageClient,
  LanguageClientOptions,
  RevealOutputChannelOn,
  State,
} from "vscode-languageclient/node";
import {
  Settings,
  initializationOptions,
  serverCommand,
  startFailure,
} from "./settings";

let client: LanguageClient | undefined;
let output: vscode.OutputChannel | undefined;
let status: vscode.StatusBarItem | undefined;

function settings(): Settings {
  const config = vscode.workspace.getConfiguration("meridian");
  return {
    path: config.get<string>("path", "meridian"),
    pluginDirs: config.get<string[]>("pluginDirs", []),
    lintEnabled: config.get<boolean>("lint.enabled", true),
    sortFields: config.get<boolean>("format.sortFields", true),
  };
}

function workspaceFolder(): string | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
}

function show(text: string, tooltip: string): void {
  if (status === undefined) {
    return;
  }
  status.text = text;
  status.tooltip = tooltip;
  status.show();
}

async function start(): Promise<void> {
  const current = settings();
  const folder = workspaceFolder();
  const home = os.homedir();
  const { command, args } = serverCommand(current, folder, home);
  const run: Executable = { command, args, options: { cwd: folder } };

  const options: LanguageClientOptions = {
    // The server answers only for documents whose `apiVersion` is a
    // Meridian one, so other YAML files are left alone.
    documentSelector: [
      { scheme: "file", language: "yaml" },
      { scheme: "untitled", language: "yaml" },
    ],
    initializationOptions: initializationOptions(current, folder, home),
    outputChannel: output,
    revealOutputChannelOn: RevealOutputChannelOn.Never,
  };

  const next = new LanguageClient("meridian", "Meridian", { run, debug: run }, options);
  next.onDidChangeState((event) => {
    if (event.newState === State.Running) {
      show("$(check) Meridian", "The Meridian language server is running.");
    } else if (event.newState === State.Starting) {
      show("$(sync~spin) Meridian", "The Meridian language server is starting.");
    } else {
      show("$(warning) Meridian", "The Meridian language server is not running.");
    }
  });
  client = next;
  try {
    await next.start();
  } catch (error) {
    client = undefined;
    show("$(warning) Meridian", "The Meridian language server is not running.");
    const choice = await vscode.window.showErrorMessage(
      startFailure(command, error),
      "Open Settings",
      "Show Output",
    );
    if (choice === "Open Settings") {
      await vscode.commands.executeCommand("workbench.action.openSettings", "meridian.path");
    } else if (choice === "Show Output") {
      output?.show();
    }
  }
}

async function stop(): Promise<void> {
  const old = client;
  client = undefined;
  if (old !== undefined) {
    try {
      await old.stop();
    } catch {
      // A server that already died has nothing left to stop.
    }
  }
}

async function restart(): Promise<void> {
  await stop();
  await start();
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  output = vscode.window.createOutputChannel("Meridian");
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 0);
  status.command = "meridian.showOutput";
  context.subscriptions.push(
    output,
    status,
    vscode.commands.registerCommand("meridian.restartServer", restart),
    vscode.commands.registerCommand("meridian.showOutput", () => output?.show()),
    // `meridian.reloadPlugins` is the server's own command; the client
    // registers it once the server has said it handles it.
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      // The server reads its options once, when it starts.
      if (
        event.affectsConfiguration("meridian.path") ||
        event.affectsConfiguration("meridian.pluginDirs") ||
        event.affectsConfiguration("meridian.lint") ||
        event.affectsConfiguration("meridian.format")
      ) {
        await restart();
      }
    }),
  );
  await start();
}

export async function deactivate(): Promise<void> {
  await stop();
}
