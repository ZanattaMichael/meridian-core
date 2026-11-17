# Visual Studio Code

The Meridian extension gives VS Code completion, hover documentation, errors
as you type, navigation, formatting and quick fixes for Meridian documents. It
is a thin wrapper: it starts [`meridian lsp`](language-server.md) and connects
the editor to it. It holds no knowledge of the language itself, so a new field
or a new plugin shows up in the editor without a new version of the extension.

It lives in [`editors/vscode`](../../editors/vscode) and is packaged from
there. It is not published to the Visual Studio Marketplace or to Open VSX.

## Install

You need the `meridian` command and the plugins for the targets you use
([Install](../quickstart/install.md)). Then install the extension package,
`meridian.vsix`, in one of two ways.

**From a build on GitHub.** Every run of the `vscode` workflow attaches the
package. Open the latest run under *Actions, vscode*, download the
`meridian-vscode` artifact and unzip it.

**From a checkout.** With Node.js 18 or later:

```sh
cd editors/vscode
npm ci
npm run package        # writes meridian.vsix
```

Then:

```sh
code --install-extension meridian.vsix
```

or, in VS Code, run **Extensions: Install from VSIX...** from the command
palette and choose the file.

## Use

Open a folder that holds Meridian documents and open one. The extension starts
when a YAML file is opened, and the status bar shows **Meridian** with a tick
once the server is running. It acts on any YAML file whose `apiVersion` starts
with `meridian/` and leaves every other YAML file alone.

| To | Do |
|---|---|
| Complete a field, type, param or reference | type, or press `Ctrl+Space` |
| Read the documentation of a field, type or param | hover over it |
| See every problem in the workspace's open files | open the Problems panel (`Ctrl+Shift+M`) |
| Apply a quick fix | click the light bulb, or press `Ctrl+.` |
| Go to the resource a `dependsOn` names | `F12`, or `Ctrl`-click |
| Find everything that names a resource | `Shift+F12` |
| Rename a resource and everything that names it | `F2` |
| Jump between resources | the Outline view, or `Ctrl+Shift+O` |
| Format the document | `Shift+Alt+F` |

What each of these does is described, with pictures, in
[the language server](language-server.md).

To format on save, add this to your settings:

```json
{
  "[yaml]": {
    "editor.defaultFormatter": "meridian.meridian",
    "editor.formatOnSave": true
  }
}
```

The Meridian formatter leaves YAML files that are not Meridian documents
untouched.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `meridian.path` | `meridian` | The command to run as the language server. A bare name is looked up on `PATH`. |
| `meridian.pluginDirs` | `[]` | Directories to load target plugins from, searched before the directory the command is in. |
| `meridian.lint.enabled` | `true` | Show [lint findings](../reference/lint.md) alongside the compiler's errors. |
| `meridian.format.sortFields` | `true` | When formatting, write fields in the language's own order. Turn it off for `meridian fmt --keep-order`. |
| `meridian.trace.server` | `off` | Log the messages exchanged with the server to the Meridian output channel: `off`, `messages` or `verbose`. |

`${workspaceFolder}` and a leading `~` are expanded in `meridian.path` and
`meridian.pluginDirs`. To use a build from a checkout of this repository:

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
| **Meridian: Show Language Server Output** | opens the output channel, where the server's messages go |

## With the YAML extension

The Red Hat YAML extension can be installed alongside. Do not give it the
[Meridian JSON Schema](json-schema.md) for the same files, or each problem is
reported twice: the language server already checks everything the schema does.

## Troubleshooting

**"Meridian could not find the `meridian` command."** The command is not on
the `PATH` VS Code was started with. Set `meridian.path` to its full path.

**No completion for resource types, or `unknown-target` on every document.**
The server found no plugin for the target. Plugins are looked for in
`meridian.pluginDirs`, then `MERIDIAN_PLUGIN_DIR`, then the directory the
`meridian` binary is in. `meridian plugins` in a terminal lists what is found
and where.

**Params are not completed for a Terraform or Kubernetes resource.** Those
targets take their tool's own types, and do not list params.
[What a target describes](index.md#what-a-target-describes) says which do.

**Nothing happens in a file.** The server only acts on documents with
`apiVersion: meridian/v1`. Check the language mode in the status bar says
YAML.

## Working on the extension

```sh
cd editors/vscode
npm ci
npm run typecheck      # type-check the sources
npm test               # unit tests for the settings logic
npm run build          # bundle to out/extension.js
MERIDIAN_BIN=../../target/debug/meridian npm run test:e2e
```

The last command downloads a copy of VS Code, starts it with the extension and
the `meridian` binary named, opens a document and checks what the editor
shows: diagnostics, completion, hover, go to definition, a quick fix and
formatting. Build the binary and a plugin first with
`cargo build -p meridian-cli -p meridian-target-ansible`.

Open `editors/vscode` in VS Code and press `F5` to run the extension in a
second window. The `vscode` workflow runs all of these and packages the
extension on every pull request that touches it or the language server. The server's behaviour is
tested in Rust, in `crates/meridian-lsp` and `crates/meridian-cli/tests/tools.rs`,
where a real `meridian lsp` process is driven over standard input and output.
