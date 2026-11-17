# Tools for writing Meridian documents

Meridian comes with the tools an author needs while writing, all in the one
`meridian` command:

| Tool | Command | What it is for |
|---|---|---|
| [Language server](language-server.md) | `meridian lsp` | completion, documentation, errors and navigation in your editor |
| [Visual Studio Code extension](vscode.md) | | the language server, set up for you in VS Code |
| [Other editors](other-editors.md) | | the same server in Neovim, Helix, Emacs and anything else that speaks the protocol |
| [Formatter](formatter.md) | `meridian fmt` | one standard layout, so diffs show changes and not taste |
| [Linter](linter.md) | `meridian lint` | the compiler's errors for a whole project, plus things that compile and are probably mistakes |
| [JSON Schema](json-schema.md) | `meridian schema` | validation in editors that only know YAML |

![Writing a resource with completion: the type, its params, and an error for a param the target does not take](images/writing-a-resource.gif)

## They are the compiler

None of these tools has its own idea of the language. There is one description
of a document's fields, and one of each target's resource types, and everything
reads those:

```mermaid
flowchart LR
    tables["The language's field tables<br/>(meridian-ir)"]
    plugins["What each target plugin declares<br/>(resource types, params, states)"]
    compiler["meridian compile"]
    lsp["meridian lsp"]
    fmt["meridian fmt"]
    lint["meridian lint"]
    schema["meridian schema"]
    tables --> compiler
    tables --> lsp
    tables --> fmt
    tables --> lint
    tables --> schema
    plugins --> compiler
    plugins --> lsp
    plugins --> lint
    plugins --> schema
```

- **The language's fields** (`apiVersion`, `kind`, `spec.resources`,
  `dependsOn`, and the rest) are listed once, in tables the parser itself
  reads to decide which fields a document may have. Completion, hover
  documentation, the formatter's field order and the JSON Schema are produced
  from the same tables. A field added to the language appears in every tool
  the moment the compiler accepts it.
- **Resource types, params and states** come from the target plugins. Each
  plugin says what it takes when it is loaded, and the tools ask the plugins
  you have installed. A plugin that is added or updated changes what your
  editor offers, with no change to the tools.
- **Errors** are the compiler's. The language server and the linter validate
  your documents with the same code as `meridian validate`, and then hand each
  one to its target's real plugin, exactly as `meridian compile` does, without
  writing any file or running any tool.

The result is that an editor never accepts what `meridian compile` refuses, and
never refuses what it accepts.

## What a target describes

How much the tools can say about a resource depends on how much its target
says about itself.

| Targets | What the tools know |
|---|---|
| ansible, puppet, openvox, chef, salt, cloud-init | Every resource type, every param with its meaning and kind, which are required, the states and the notify actions. An unknown param or state is an error in the editor. |
| terraform, opentofu, cloudformation, bicep, arm, pulumi, packer, docker, podman, kubernetes, helm, dsc | The resource types by name or by shape (such as `<provider>_<type>`). Params belong to the tool's own providers and are not listed, so they are not completed or checked in the editor. The plugin still checks them when the document is compiled, and those errors are shown. |

A plugin author adds descriptions with one method; see
[Writing a plugin](../contributing/writing-a-plugin.md#describe-your-resource-types-for-editors).

## About the pictures

The pictures in these pages show the Monaco editor, the editor Visual Studio
Code is built on, connected to a real `meridian lsp` with the real plugins.
Everything in them is what the server answered.
[`tools/editor_screenshots`](../../tools/editor_screenshots/capture.js)
produces them again after a change.
