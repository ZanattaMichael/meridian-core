// How the extension's settings become the command that is run and the
// options the language server is started with. Nothing here touches the
// editor, so it is tested without one.

/** The settings a user can change, as the editor hands them over. */
export interface Settings {
  path: string;
  pluginDirs: string[];
  lintEnabled: boolean;
  sortFields: boolean;
}

/** What `meridian lsp` reads from the `initialize` request. */
export interface InitializationOptions {
  pluginDirs: string[];
  lint: boolean;
  sortFields: boolean;
}

/** The process to start. */
export interface ServerCommand {
  command: string;
  args: string[];
}

/**
 * Replaces `${workspaceFolder}` with the first workspace folder and a
 * leading `~` with the home directory. Without a workspace folder the
 * variable is left as written, so the mistake is visible in the error.
 */
export function expand(
  value: string,
  workspaceFolder: string | undefined,
  home: string | undefined,
): string {
  let out = value;
  if (workspaceFolder !== undefined) {
    out = out.split("${workspaceFolder}").join(workspaceFolder);
  }
  if (home !== undefined && (out === "~" || out.startsWith("~/") || out.startsWith("~\\"))) {
    out = home + out.slice(1);
  }
  return out;
}

/** The command for the language server: `<meridian> lsp`. */
export function serverCommand(
  settings: Settings,
  workspaceFolder: string | undefined,
  home: string | undefined,
): ServerCommand {
  const path = settings.path.trim();
  return {
    command: expand(path === "" ? "meridian" : path, workspaceFolder, home),
    args: ["lsp"],
  };
}

/** The options sent to the server when it starts. */
export function initializationOptions(
  settings: Settings,
  workspaceFolder: string | undefined,
  home: string | undefined,
): InitializationOptions {
  return {
    pluginDirs: settings.pluginDirs
      .map((dir) => dir.trim())
      .filter((dir) => dir !== "")
      .map((dir) => expand(dir, workspaceFolder, home)),
    lint: settings.lintEnabled,
    sortFields: settings.sortFields,
  };
}

/**
 * What to tell someone when the server could not be started. `ENOENT` is by
 * far the usual cause: the command is not installed or not on `PATH`.
 */
export function startFailure(command: string, error: unknown): string {
  const code = (error as { code?: string } | undefined)?.code;
  const detail = error instanceof Error ? error.message : String(error);
  if (code === "ENOENT" || /ENOENT/.test(detail)) {
    return (
      `Meridian could not find the \`${command}\` command. ` +
      "Install Meridian, or set `meridian.path` to where it is."
    );
  }
  return `Meridian could not start its language server (\`${command} lsp\`): ${detail}`;
}
