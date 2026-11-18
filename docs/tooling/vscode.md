# Visual Studio Code

The extension starts [`meridian lsp`](language-server.md) for completion, hover, errors,
navigation, formatting and quick fixes. It lives in [`editors/vscode`](../../editors/vscode)
and is not on the Marketplace or Open VSX.

## Install

Install `meridian` and your plugins ([Install](../quickstart/install.md)), then get
`meridian.vsix`:

**From GitHub:** the `meridian-vscode` artifact of the latest *Actions, vscode* run.

**From a checkout** (Node.js 18+):

```sh
cd editors/vscode
npm ci
npm run package        # writes meridian.vsix
```

Then:

```sh
code --install-extension meridian.vsix
```

or **Extensions: Install from VSIX...**.

## Use

It starts on the first YAML file; the status bar shows **Meridian** with a tick. Only files
with `apiVersion: meridian/...` are touched.

| To | Do |
|---|---|
| Complete a field, type, param, function or reference | type, or press `Ctrl+Space` |
| Complete and check a tool's own types (`aws_instance`, `apps/v1/Deployment`) | run **Meridian: Sync Tool Schemas** once; see [Tool schemas](schema-sync.md) |
| Read the documentation of a field, type or param | hover over it |
| See every problem in the workspace's open files | open the Problems panel (`Ctrl+Shift+M`) |
| Apply a quick fix | click the light bulb, or press `Ctrl+.` |
| Go to the resource a `dependsOn` names | `F12`, or `Ctrl`-click |
| Find everything that names a resource | `Shift+F12` |
| Rename a resource and everything that names it | `F2` |
| Jump between resources | the Outline view, or `Ctrl+Shift+O` |
| Format the document | `Shift+Alt+F` |

See [the language server](language-server.md). Format on save:

```json
{
  "[yaml]": {
    "editor.defaultFormatter": "meridian.meridian",
    "editor.formatOnSave": true
  }
}
```

## Settings

| Setting | Default | What it does |
|---|---|---|
| `meridian.path` | `meridian` | The command to run as the language server. A bare name is looked up on `PATH`. |
| `meridian.pluginDirs` | `[]` | Directories to load target plugins from, searched before the directory the command is in. |
| `meridian.lint.enabled` | `true` | Show [lint findings](../reference/lint.md) alongside the compiler's errors. |
| `meridian.format.sortFields` | `true` | When formatting, write fields in the language's own order. Turn it off for `meridian fmt --keep-order`. |
| `meridian.stateDir` | `.meridian` | Where each workspace folder's [synced tool schemas](schema-sync.md) are read from, relative to the folder; the `--state-dir` the sync was run with. |
| `meridian.trace.server` | `off` | Log the messages exchanged with the server to the Meridian output channel: `off`, `messages` or `verbose`. |

`${workspaceFolder}` and `~` are expanded in `meridian.path` and `meridian.pluginDirs`. For
a local build:

```json
{
  "meridian.path": "${workspaceFolder}/target/debug/meridian"
}
```

Changing a setting restarts the server.

## Commands

| Command | What it does |
|---|---|
| **Meridian: Restart Language Server** | stops the server and starts it again |
| **Meridian: Reload Target Plugins** | loads the plugins again, after you install or rebuild one |
| **Meridian: Sync Tool Schemas** | runs `meridian schema sync` on the workspace folder in a terminal, so the tools' output is in front of you; the server reads the result when it is written |
| **Meridian: Show Language Server Output** | opens the output channel, where the server's messages go |

## With the YAML extension

It can run alongside; do not also give it the [JSON Schema](json-schema.md).

## Troubleshooting

**"Meridian could not find the `meridian` command."** Set `meridian.path`.

**No type completion, or `unknown-target` everywhere.** No plugin found in
`meridian.pluginDirs`, `MERIDIAN_PLUGIN_DIR` or `meridian`'s directory; run
`meridian plugins`.

**"A plugin directory named in the editor's settings only loads plugins signed by a
trusted key."** Sign it, or set `MERIDIAN_LSP_TRUST_EDITOR_PLUGIN_DIRS=true`
([why](language-server.md#plugin-signatures)).

**No params for Terraform or Kubernetes types.** Run `meridian schema sync`
([what a target describes](index.md#what-a-target-describes)).

**Nothing happens.** Check `apiVersion: meridian/v1` and that the language mode is YAML.

## Working on the extension

```sh
cd editors/vscode
npm ci
npm run typecheck      # type-check the sources
npm test               # unit tests for the settings logic
npm run build          # bundle to out/extension.js
MERIDIAN_BIN=../../target/debug/meridian npm run test:e2e
```

The last command runs VS Code with the extension and checks diagnostics, completion,
hover, definition, a quick fix and formatting. Build first with
`cargo build -p meridian-cli -p meridian-target-ansible`. Press `F5` in `editors/vscode` to
debug. The `vscode` workflow runs these; the server is tested in `crates/meridian-lsp` and
`crates/meridian-cli/tests/tools.rs`.
