# Other editors

Any editor with a Language Server Protocol client can use
[`meridian lsp`](language-server.md). The recipe is always the same:

- the command is `meridian lsp`;
- attach it to YAML files (the server ignores the ones that are not Meridian
  documents);
- optionally pass `pluginDirs`, `lint` and `sortFields` as
  [initialization options](language-server.md#options).

The configurations below follow each editor's documentation. Only the VS Code
extension is built and tested in this repository's CI; if one of these needs
a correction, please open an issue.

For editors with no language server client, use the
[JSON Schema](json-schema.md), and run [`meridian lint`](linter.md) and
[`meridian fmt`](formatter.md) from the command line or a pre-commit hook.

## Neovim

Neovim 0.11 or later, with no plugin:

```lua
vim.lsp.config('meridian', {
  cmd = { 'meridian', 'lsp' },
  filetypes = { 'yaml' },
  root_markers = { '.git' },
  init_options = {
    pluginDirs = {},
    lint = true,
    sortFields = true,
  },
})
vim.lsp.enable('meridian')
```

It runs beside `yamlls` if you use that too.

## Helix

In `~/.config/helix/languages.toml`:

```toml
[language-server.meridian]
command = "meridian"
args = ["lsp"]
config = { lint = true, sortFields = true }

[[language]]
name = "yaml"
language-servers = ["meridian", "yaml-language-server"]
```

Helix sends `config` as the initialization options.

## Emacs

With Eglot, which is built in from Emacs 29:

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '((yaml-mode yaml-ts-mode) . ("meridian" "lsp"))))
```

Then `M-x eglot` in a Meridian document.

## Zed, Sublime Text, Kate and others

Each has a setting that maps a language to a server command. Give it
`meridian lsp` for YAML.

## Pre-commit

To check documents before each commit, with [pre-commit](https://pre-commit.com/):

```yaml
repos:
  - repo: local
    hooks:
      - id: meridian-fmt
        name: meridian fmt
        entry: meridian fmt --check
        language: system
        files: \.(ya?ml)$
      - id: meridian-lint
        name: meridian lint
        entry: meridian lint --strict
        language: system
        files: \.(ya?ml)$
```

Both commands take the files pre-commit passes, and both pass over a YAML file
that is not a Meridian document.
