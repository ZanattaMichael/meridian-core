# The MCP server

`meridian mcp` is a [Model Context Protocol](https://modelcontextprotocol.io)
server. An AI client such as Claude Desktop, Claude Code, Cursor or VS Code
starts it, and the model it runs calls the server's tools to look up what
exists, draft a document, be told exactly what is wrong and what is accepted
instead, and correct. It speaks the protocol on standard input and output,
needs no network and no account, and is the compiler: what it accepts is what
`meridian compile` accepts.

It is for models that have never seen a Meridian document. There is no body
of public Meridian files for a model to have learned from, so an unaided
model writes plausible, wrong YAML. With the server a small model does not
need to know the language: it asks.

- It is **read-only by default**. `compile` emits in memory and writes
  nothing; `plan` is off unless the server is started with `--allow-plan`;
  `apply`, `rollback` and `destroy` are not offered. A person applies.
- It **never fetches a secret**. A document is compiled with no data, as the
  [language server](language-server.md#errors-and-warnings) compiles it.
- It makes **no outbound connection**, and a file argument is confined to
  the directory the server was started in.

## Setting up a client

The command is `meridian mcp`. The `meridian-target-*` plugin binaries must
be beside the `meridian` binary, or named with `--plugin-dir`, as for every
command; start it in the project's directory so that the tool schemas
`meridian schema sync` cached are read.

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

To offer `plan`, add `"--allow-plan"` to the arguments; see
[Safety](#safety) first.

### Options

| Option | Meaning |
|---|---|
| `--allow-plan` | offer the `plan` tool, which runs the real tool and takes the state lock. Also `MERIDIAN_MCP_ALLOW_PLAN=1` |
| `--root DIR` | the directory file arguments are confined to; the current directory by default |
| `--state-dir DIR` | where `meridian schema sync` cached the tool schemas, relative to the root; `.meridian` by default |
| `--plugin-dir`, `--plugin-signatures`, `--trusted-key` | the plugin options every command takes |

When no plugin is found the server still runs: documents are checked against
the language only, `list_targets` is empty, and the server says so in its
instructions to the client.

## The tools

Designed for small models: few tools, flat arguments, short structured
results, and every error carrying its own fix. Each result is returned both
as structured content and as the same JSON in a text block, so a client that
shows only text sees the same thing.

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

A document is given as text (`document`) or as a file under the root
(`file`); a file may hold several documents separated by `---`.

### One shape for every diagnostic

Small models fail on prose, so every tool that can refuse returns a list of
the same object. The fields it shares with
[`meridian lint --output json`](../reference/cli-json.md#lint) have the same
names and meanings there.

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

Two codes are the server's own. `needs-data` (info) says a document reads
`data.<key>` or a secret in a condition or an interpolation, so it was
checked against the language and not compiled, as the language server does
(see [decision 0003](../decisions/0003-editors-treat-every-when-as-true.md)).
`import-source` says the importer could not read the source text.

### The prompt and the resources

- The prompt `authoring-guide` is the loop to follow, in under a page: pick
  a target, read an example, list and describe the types, draft, `validate`,
  `compile`, fix from `accepted`, repeat, then `lint` and `format`. It takes
  an optional `target` and `task`. Small models follow a stated procedure far
  better than they infer one, so a client should attach it.
- The resources are the [DSL reference](../reference/dsl/index.md) pages, the
  [capability matrix](../reference/capability-matrix.md), the
  [lint rules](../reference/lint.md) and every target's page, embedded in the
  binary and served as `meridian://docs/<path>`, such as
  `meridian://docs/reference/dsl/resources` and `meridian://docs/targets/ansible`.

## Safety

- **Read-only.** `compile` runs the plugin's emitter in memory, the same call
  the language server makes for every keystroke; nothing is written to
  `--out` or to the state directory. `apply`, `rollback` and `destroy` do not
  exist as tools. `plan` is opt-in because it runs the real tool, takes the
  project's [lock](../concepts/run-safety.md) and writes a run record; when
  allowed it runs the `meridian` binary itself with `--output json`, so what
  the model sees is exactly what `meridian plan` prints.
- **Secrets.** A document given to the server is compiled with no data and no
  secret is fetched: no environment variable is read, no file opened and no
  `exec` command run because a model asked for a compile. A document that
  reads a secret or data is checked against the language and reported as
  `needs-data`. Values a model writes into a document are its own; the
  `literal-secret` lint says when one looks like a password.
- **No outbound connections**, in keeping with the
  [observability plan](../plans/observability.md#4-telemetry). The server
  is a local process the client starts; it has no hosted mode.
- **Paths.** `file` and `plan` arguments are resolved under the root and
  refused outside it, through symbolic links too.
- **Size.** A document argument is refused past 512,000 characters; lists are
  paged; an emitted file, an example and a page are cut with a count of what
  was left out.

The design and its alternatives are in
[decision 0035](../decisions/0035-the-mcp-server-is-a-read-only-subcommand.md).

## How we know it works

The server is worth shipping when a small model with it does clearly better
than the same model without. That is measured, not assumed: a test set of
tasks in plain words sits under
[`tools/mcp_eval/`](https://github.com/ZanattaMichael/meridian-core/tree/main/tools/mcp_eval),
each with the target it is for, the resource types a correct answer uses, and
a check that the answer compiles. `tools/mcp_eval/check.py` runs the check
over a directory of answers through the server itself, so it needs no model
access and any client can be scored. Running the tasks with a model needs
that model, so it runs on demand and not in CI; the results, when they are
recorded, go in this section with the share that compiled, the share that
used the expected types, and the number of turns.

The protocol itself is tested in CI: `crates/meridian-mcp/tests/protocol.rs`
speaks to the server with real plugins loaded in process, and
`crates/meridian-cli/tests/mcp.rs` starts `meridian mcp` as a client would and
compiles and plans against a plugin binary.
