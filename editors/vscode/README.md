# Meridian for Visual Studio Code

Editor support for [Meridian](https://github.com/ZanattaMichael/meridian-core)
documents: one language that compiles to Terraform, Ansible, Kubernetes and
the other tools your estate already runs.

The extension is a thin wrapper. It starts `meridian lsp`, the language
server built into the `meridian` command, and everything you see comes from
it. The server is the compiler itself, so the editor and `meridian compile`
never disagree, and a new field or a new plugin shows up in the editor
without a new version of this extension.

## What you get

- **Completion** for fields, kinds, targets, resource types, states and the
  params each resource type takes, read from the target plugins you have
  installed. Inside `dependsOn`, `notifies` and `${...}` it offers the
  resources and outputs that exist.
- **Hover documentation** for every field, resource type and param.
- **Diagnostics from the real compiler**, as you type, plus the findings of
  `meridian lint`, with quick fixes where a fix is safe.
- **Go to definition**, **find references** and **rename** for resource ids
  and outputs, across the files of the workspace.
- **Formatting** with `meridian fmt` (Format Document, or format on save).
- An **outline** of each document's resources and outputs.

It works on any YAML file whose `apiVersion` is `meridian/v1`. Other YAML
files are left alone.

## Requirements

The `meridian` command, with the target plugins you use beside it. Build
both from a checkout:

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

## Settings

| Setting | Default | What it does |
| --- | --- | --- |
| `meridian.path` | `meridian` | The command to run. A bare name is looked up on `PATH`. |
| `meridian.pluginDirs` | `[]` | Extra directories to load target plugins from, searched before the directory the command is in. |
| `meridian.lint.enabled` | `true` | Show lint findings alongside the compiler's errors. |
| `meridian.format.sortFields` | `true` | Write fields in the language's own order when formatting. |
| `meridian.trace.server` | `off` | Log the protocol messages to the Meridian output channel. |

`${workspaceFolder}` and a leading `~` are expanded in `meridian.path` and
`meridian.pluginDirs`. Changing a setting restarts the server.

## Commands

- **Meridian: Restart Language Server**
- **Meridian: Reload Target Plugins**, after you install or rebuild a plugin
- **Meridian: Show Language Server Output**

## Installing

The extension is not on a marketplace. Build the package and install it:

```sh
cd editors/vscode
npm ci
npm run package
code --install-extension meridian.vsix
```

Every run of the `vscode` workflow on GitHub also attaches a ready-made
`meridian.vsix` to the run.

The full guide is in
[docs/tooling/vscode.md](https://github.com/ZanattaMichael/meridian-core/blob/main/docs/tooling/vscode.md).
