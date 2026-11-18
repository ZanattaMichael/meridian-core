# Other editors

Any LSP client can use [`meridian lsp`](language-server.md):

- the command is `meridian lsp`;
- attach it to YAML files (the server ignores the ones that are not Meridian
  documents);
- optionally pass `pluginDirs`, `lint` and `sortFields` as
  [initialization options](language-server.md#options).

Only the VS Code extension is tested in CI. Without an LSP client, use the
[JSON Schema](json-schema.md), [`meridian lint`](linter.md) and
[`meridian fmt`](formatter.md).

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

It runs beside `yamlls`.

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

Map YAML to `meridian lsp`.

## Pre-commit

With [pre-commit](https://pre-commit.com/):

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

Non-Meridian YAML files are skipped.
