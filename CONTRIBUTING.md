# Contributing to Meridian

Meridian is one language for infrastructure and configuration, compiled to the
tools people already run. Its value grows with every tool it supports well, so
outside contributions matter, and a first one can be small.

This page covers everything from a fresh clone to a merged pull request. By
taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to help

| You want to | Start here |
|---|---|
| Fix something small to learn the codebase | issues labelled [`good first issue`](https://github.com/ZanattaMichael/meridian-core/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) |
| Take on something larger that nobody owns | issues labelled [`help wanted`](https://github.com/ZanattaMichael/meridian-core/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22) |
| Add a tool Meridian does not support yet | [Writing a plugin](docs/contributing/writing-a-plugin.md) |
| Test a plugin against the real tool | [Real-tool tests](docs/contributing/real-tool-tests.md) lists what each target is tested against and what is still unproven: several runners have never reached a remote machine or a real cloud account |
| Improve the documentation | edit the page under [`docs/`](docs/index.md) and open a pull request; it is published as [the documentation site](https://zanattamichael.github.io/meridian-core/) and the wiki on merge, as [how the documentation is published](docs/contributing/documentation-site.md) describes |
| Report a bug or ask for a feature | [open an issue](https://github.com/ZanattaMichael/meridian-core/issues/new/choose) |
| Report a security problem | not in an issue: see [`SECURITY.md`](SECURITY.md) |

Before anything bigger than a small fix, comment on the issue (or open one) to
say you are taking it. This avoids duplicate work and surfaces any different
plans early.

## Set up

You need a stable Rust toolchain and Python 3, nothing else. The tests use
stand-ins for every tool, so Terraform, Ansible, Docker and the rest are not
needed.

```
git clone https://github.com/<you>/meridian-core
cd meridian-core
cargo build --workspace
cargo test --workspace
```

If the tests fail on a fresh clone, that is a bug: please report it.

The kinds of test, and how to add one, are in
[how Meridian is tested](docs/contributing/testing.md).

To try what you built:

```
target/debug/meridian plugins
target/debug/meridian compile examples/ansible/web-server.yaml --out build
```

## Find your way around

| Path | What is there |
|---|---|
| `crates/meridian-cli` | the `meridian` command |
| `crates/meridian-ir`, `-resolve`, `-graph`, `-ast` | the core: parsing, validation, ordering, the tree plugins receive |
| `crates/meridian-orchestrator` | runs a workflow across several tools |
| `crates/meridian-sdk` | everything a plugin is built against |
| `crates/meridian-plugin-template` | a small working plugin to copy |
| `crates/meridian-plugin-testkit` | what plugin tests share: golden files, stand-ins |
| `plugins/<name>` | one crate per tool |
| `examples/<name>` | example documents; a test compiles every one |
| `fixtures/` | documents every plugin is tested against |
| `docs/` | user documentation, and the design under `docs/plans/` |
| `tools/` | `check_links.py`, `new_plugin.py` |

Every crate under `crates/`:

- `meridian-ir`: parses and validates documents
- `meridian-yaml`: YAML scalar rules and a deterministic writer, shared by the parser and YAML-emitting plugins
- `meridian-resolve`: hierarchical data resolution
- `meridian-module`: modules: the `meridian.yaml` manifest, path, git and OCI sources, version
  resolution, `meridian.lock` and content hashes
- `meridian-test`: [`meridian test`](docs/reference/testing.md): `Test` documents that compile or plan documents with given data and check the result
- `meridian-rules`: [compile rules](docs/concepts/compile-rules.md): policies over the layers of a hierarchy, checked before compiling (for example "a more specific layer may not change a variable a less specific layer set")
- `meridian-graph`: dependency graph and topological sort
- `meridian-ast`: the target-agnostic resource tree every plugin consumes
- `meridian-value`: the dynamic value type and its canonical JSON form
- `meridian-sdk`: what plugins build against: the `Emitter`, `Runner` and `Importer` traits,
  `serve`, wire conversion, and helpers for running command-line tools
- `meridian-import`: shared importer code: line-preserving YAML and HCL readers, the document
  writer with `TODO` markers, and the semantic diff used by round-trip tests
- `meridian-plugin-host`: launches, discovers and drives plugins
- `meridian-orchestrator`: runs a workflow: compiles each document, drives its tool, passes
  outputs between stages, stops on the first failure
- `meridian-tooling`: shared editor code: formatter, linter, completion, hover, navigation and
  JSON Schema export, all read from the parser's field tables and the plugins' declarations
- `meridian-lsp`: the language server
- `meridian-mcp`: the MCP server: the compiler and catalogue as tools for an AI model
- `meridian-cli`: the `meridian` command
- `meridian-policy-opa`: the Open Policy Agent engine behind `--policy`
- `meridian-target-fake`: a small complete plugin whose tool is a directory; used by tests and
  a worked example of the SDK
- `meridian-plugin-testkit`: shared test code for first-party plugins
- `meridian-plugin-template`: the working plugin that `tools/new_plugin.py` copies

A plugin depends on `meridian-sdk` (and `meridian-yaml` / `meridian-import` where needed),
never on the compiler. The [design plan](docs/plans/design-plan.md) explains the shape.

## The one rule

> A document either compiles to something that does what it says, or it does
> not compile and the author is told why. Meridian never emits output that
> quietly does less.

Every review comes back to this. If a tool cannot express something, the
plugin refuses the document with a reason. If a plugin declares a capability,
a test proves it. Unsure whether a change keeps the rule? Ask in the pull
request.

## Making a change

1. Branch from `main`.
2. Make the change, with a test. A bug fix comes with a test that fails
   without it.
3. If the change alters what a plugin emits, record the golden files with
   `UPDATE_GOLDEN=1 cargo test -p <crate>` and read the diff before committing
   it. Golden files are reviewed like code.
4. Update the documentation in the same pull request. See
   [Documentation](#documentation).
5. Run the checks.

### The checks

CI runs these four. A pull request is reviewed once they pass:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 tools/check_links.py
```

`--locked` fails when `Cargo.lock` is out of step, so commit `Cargo.lock` when
you add a crate or a dependency. `unsafe` code is forbidden across the
workspace.

### Commits and pull requests

- One subject per pull request. Two small ones are reviewed faster than one
  large one.
- Write the commit subject as a short sentence saying what changes, for
  example `docker: refuse a service with neither image nor build`.
- Fill in the pull request form. Say what you tested and what you did not.
- Open a draft early if you want feedback before it is finished.

## What happens to your pull request

1. CI runs. If it fails and you cannot see why, say so in the pull request and
   someone will help.
2. A maintainer reviews it, aiming to respond within five working days. After
   a week of silence, feel free to ask.
3. Review comments are about the change, not you. Push more commits to the
   same branch; no need to squash.
4. A maintainer merges it once CI is green, the documentation is complete and
   the review is settled.

### Changes that need a second reviewer

Some code is easy to get subtly wrong. A change to any of these needs two
reviewers, whoever wrote it:

- Code that builds a shell command string, or escapes one, in any runner.
- Anything that handles a secret: `secret_arg`, a `sensitive` output, masking.
- What is written to the run-state under `.meridian`.
- The plugin protocol in `proto/`, `PROTOCOL_VERSION` or `MIN_PROTOCOL_VERSION`.
- The table of language versions in `crates/meridian-ir/src/api.rs`, or a migration step.

### Capability claims

A plugin's declared capabilities are published in the
[capability matrix](docs/reference/capability-matrix.md), and users rely on
them. If a reviewer doubts a claim, they write a test that fails if the claim
is false, and the test decides. Ties go to the weaker claim.

## Documentation

Documentation is part of the product and has the same bar as code: complete
and current.

- A change in behaviour updates the page that describes it, in the same pull
  request.
- A new or changed error message is documented where the user will look for
  it, on the target's page or in the reference.
- Every example in the documentation is a file under `examples/`, so that a
  test compiles it.
- The capability matrix is generated. Record it with
  `UPDATE_GOLDEN=1 cargo test -p meridian-cli --test capability_matrix`.
- So is the conformance matrix, by `--test conformance`. Read the rows it
  changes: they are what your change did on every target.
- Relative links and heading anchors are checked by `tools/check_links.py`.

## Where the project is going

Plugin tiers (Supported, Community, Experimental), ownership and dispute
resolution are in the [governance model](docs/plans/governance.md); its test
tiers are in the [testing plan](docs/plans/testing-plan.md). Parts of both
describe intent, not current enforcement. What is enforced today is on this
page.

## Licence

Meridian is under the [MIT licence](LICENSE). By contributing you agree that
your contribution is licensed the same way.
