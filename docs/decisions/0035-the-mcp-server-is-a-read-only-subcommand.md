# The MCP server is a subcommand of `meridian`, read-only, speaking the protocol itself

| | |
|---|---|
| Area | Tooling |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "An MCP server so smaller AI models can write Meridian documents" (issue 122) |
| Where it shows | [The MCP server](../tooling/mcp-server.md), [`meridian mcp`](../reference/cli.md#mcp), crate `meridian-mcp` |

## Context

The owner asked for an MCP "plugin" so that lesser AI models can develop
infrastructure as code with Meridian. Issue 122 left three questions open:
whether it is a plugin or built in, whether `plan` is offered in the first
version, and whether to depend on a Rust MCP library or serve the protocol
directly as the language server does.

## The choice

**A subcommand of the main binary**, `meridian mcp`, beside `meridian lsp`,
in a crate `meridian-mcp` that the command depends on. Meridian's plugins are
target plugins with a fixed protocol and a category; an MCP server is not a
target and has nothing to compile, so it is a front on `meridian-tooling`
like the language server, and ships wherever the binary ships. The tools it
offers are the same whether it is built in or shipped as a second binary, so
nothing is lost if that changes.

**Read-only by default, `plan` opt-in, no `apply`.** `compile` runs the
emitter in memory, the call the language server already makes on every
keystroke. `plan` runs the real tool and takes the lock, so it is offered
only behind `--allow-plan`, and when allowed it runs the `meridian` binary
itself with `--output json`: what the model sees is exactly the object
`meridian plan` prints, and nothing is formatted a second time. `apply`,
`rollback` and `destroy` are not tools, because a person applies and a model
must not be able to be talked into it.

**The protocol is served directly**, one JSON-RPC message per line over
standard input and output, with `serde_json` alone. The server needs
`initialize`, `tools/*`, `prompts/*` and `resources/*`, which is a few
hundred lines; a library would bring an async runtime and a dependency to
keep current for a protocol that is still changing yearly. The server
answers the protocol revision the client asks for when it knows it, and the
newest it knows otherwise.

**One diagnostic shape** across `validate`, `compile`, `lint` and `import`:
the fields of `meridian lint --output json`, plus the resource the problem
falls in, what the target accepts instead (read from the message, where the
compiler lists it), the linter's fix, and the `explain` call. `accepted` is
read from the message rather than carried on the error because the rules
that list accepted values do so in their text today; when the rule registry
(issue 88) carries them as data, the server should read that instead.

**Examples and pages are embedded** in the binary at build time, so
`get_example` and the resources work with no checkout beside the binary.
Every example is compiled by the test suite, so what the model is handed
compiles.

## Alternatives

- A target plugin. Rejected: a plugin declares a category and compiles
  documents; this does neither, and the plugin protocol would need a new
  service for nothing.
- A separate `meridian-mcp` binary. Not rejected, only not chosen: the tools
  would be the same, and the main binary already carries `lsp`. Revisit if
  the binary's size or its release cadence makes a case.
- A Rust MCP library. Rejected for now, as above.
- Offering `plan` by default. Rejected: a model that plans by default takes
  the state lock and runs a tool against real credentials in the project's
  environment without a person deciding to.
- Offering `apply` behind a flag. Rejected: the owner's words were that a
  person applies, and a flag is a thing a model can ask a person to set.

## Status

Proposed. Open to the owner: whether `plan` belongs in the first version at
all; whether the server should also ship as a separate binary; and the
model test set's results, which need model access and are recorded on the
page once run.
