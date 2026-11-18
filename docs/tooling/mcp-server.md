# The MCP server

`meridian mcp` is a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdin and stdout. A model uses its tools to look up types, draft a document,
see what is wrong and fix it. It accepts exactly what `meridian compile` accepts.

- **Read-only by default.** `plan` needs `--allow-plan`; there is no `apply`, `rollback` or
  `destroy`.
- **No secrets fetched**; documents compile with no data.
- **No outbound connections**; file arguments stay under the start directory.

## Setting up a client

Run `meridian mcp` in the project directory, with plugins beside `meridian` or in
`--plugin-dir`.

**Claude Code**

```
claude mcp add meridian -- meridian mcp
```

**Claude Desktop** (`claude_desktop_config.json`), **Cursor** (`.cursor/mcp.json`)
and any client that takes the common JSON shape:

```json
{
  "mcpServers": {
    "meridian": {
      "command": "meridian",
      "args": ["mcp"]
    }
  }
}
```

**VS Code** (`.vscode/mcp.json`):

```json
{
  "servers": {
    "meridian": {
      "type": "stdio",
      "command": "meridian",
      "args": ["mcp"]
    }
  }
}
```

For `plan`, add `"--allow-plan"` ([Safety](#safety)).

### Options

| Option | Meaning |
|---|---|
| `--allow-plan` | offer the `plan` tool, which runs the real tool and takes the state lock. Also `MERIDIAN_MCP_ALLOW_PLAN=1` |
| `--root DIR` | the directory file arguments are confined to; the current directory by default |
| `--state-dir DIR` | where `meridian schema sync` cached the tool schemas, relative to the root; `.meridian` by default |
| `--plugin-dir`, `--plugin-signatures`, `--trusted-key` | the plugin options every command takes |

With no plugins, documents are checked against the language only and `list_targets` is
empty.

## The tools

Few tools, flat arguments, and a fix with every error. Results are structured content plus
the same JSON as text.

| Tool | What it does | Notes |
|---|---|---|
| `list_targets` | the installed plugins: name, category, the document kinds each takes, what it can do, its tool versions, how many resource types it has, the formats it imports | from each plugin's `Describe`, as [`meridian plugins`](../reference/cli.md#plugins) |
| `list_resource_types` | the resource types of one target, with a one-line description each | paged (`page`, `pageSize` up to 200); `search` keeps the types whose name or description holds a word; a tool's own types from the [synced schemas](schema-sync.md) are included |
| `describe_resource_type` | one type's params (name, kind, required, allowed values, nested params three levels down), states, default state, notify actions, and a minimal example document, checked | from the plugin's `resource_schemas`; a type that fits a pattern such as `<provider>_<type>` says so |
| `describe_document` | the fields of a document kind, from the parser's own field tables | the same source as [`meridian schema`](json-schema.md) |
| `get_example` | a worked example by name, or found by `target` and `search`; with no arguments, every example | the [examples](https://github.com/ZanattaMichael/meridian-core/tree/main/examples) are embedded in the binary; every one compiles and passes `fmt --check` |
| `validate` | checks a document against the language and against what its target declares about types, params and states | starts no plugin process; the same checks the language server runs before it compiles |
| `compile` | compiles with the target's real plugin, in memory, and returns the emitted files or the refusal | writes nothing; `emittedFile` returns one file in full, otherwise each is cut at 12,000 characters with a count of what was left out |
| `lint` | every error, plus the [lint rules](../reference/lint.md), with a quick fix where there is one; `fixed` is the document with the fixes applied | the same as [`meridian lint`](linter.md) |
| `format` | the document in the standard layout | [`meridian fmt`](formatter.md) |
| `explain` | what a rule means, why the target refuses it, and what to write instead | the lint rules explain themselves; the compiler's shared rules are described in the server; a target's own rule is named as such |
| `import` | turns a tool's own configuration into documents, with what could not be carried over | [`meridian import`](../reference/cli.md#import) on text; `fileName` tells the importer the format |
| `plan` | asks the tool what applying a file would change | **off unless** `--allow-plan`; runs `meridian plan --output json` on the file and returns [its object](../reference/cli-json.md#compile-plan-apply-drift-rollback) |

Give `document` (text) or `file` (under the root, may hold several documents).

### One shape for every diagnostic

Every refusal is a list of one object shape, sharing fields with
[`meridian lint --output json`](../reference/cli-json.md#lint).

```json
{
  "file": "document.yaml",
  "line": 25,
  "column": 14,
  "endLine": 25,
  "endColumn": 22,
  "severity": "error",
  "code": "unsupported-state",
  "source": "ansible",
  "message": "resource type \"service\" does not take state \"rebooted\" on target \"ansible\" (it takes: running, stopped, restarted, reloaded)",
  "fixable": false,
  "resource": "nginx_svc",
  "accepted": ["running", "stopped", "restarted", "reloaded"],
  "explain": "explain(rule='unsupported-state', target='ansible')"
}
```

| Field | Meaning |
|---|---|
| `code` | the rule: a compiler rule such as `unsupported-param`, a lint rule, `syntax`, or `invalid-document` |
| `source` | who says so: `meridian` for the language, the target's name for its refusal, `meridian-lint` for a lint |
| `resource` | the id of the resource the diagnostic falls in, when it falls in one |
| `accepted` | what the target takes instead, when the message lists it: states, params, types, versions |
| `fix` | `{line, column, endLine, endColumn, replace}`: a replacement the linter knows is safe, with `fixable: true` |
| `explain` | the `explain` call to make for more |

Server codes: `needs-data` (the document reads data or a secret, so only the language was
checked; [decision 0003](../decisions/0003-editors-treat-every-when-as-true.md)) and
`import-source` (the source could not be read).

### The prompt and the resources

- The prompt `authoring-guide` (optional `target` and `task`) gives the loop: pick a
  target, read an example, describe types, draft, `validate`, `compile`, fix, `lint`,
  `format`. Attach it.
- Resources: the [DSL reference](../reference/dsl/index.md), the
  [capability matrix](../reference/capability-matrix.md), the
  [lint rules](../reference/lint.md) and target pages, as `meridian://docs/<path>`
  (`meridian://docs/targets/ansible`).

## Safety

- **Read-only.** `compile` emits in memory. `plan` runs the real tool, takes the
  [lock](../concepts/run-safety.md) and writes a run record, so it is opt-in; it runs
  `meridian plan --output json`.
- **Secrets.** A compile reads no environment, file or `exec`. The `literal-secret` lint
  flags passwords a model writes.
- **No outbound connections** ([observability plan](../plans/observability.md#4-telemetry)).
- **Paths** are confined to the root, through symlinks too.
- **Size.** Documents over 512,000 characters are refused; long results are paged or cut.

See [decision 0035](../decisions/0035-the-mcp-server-is-a-read-only-subcommand.md).

## How we know it works

Tasks in
[`tools/mcp_eval/`](https://github.com/ZanattaMichael/meridian-core/tree/main/tools/mcp_eval)
measure whether a small model does better with the server; `check.py` scores answers. They
run on demand, not in CI. No results are recorded yet.

CI tests the protocol in `crates/meridian-mcp/tests/protocol.rs` and
`crates/meridian-cli/tests/mcp.rs`.
