# meridian-core

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/branding/banner/meridian-banner-dark-animated.svg">
  <img src="docs/branding/banner/meridian-banner-light-animated.svg" alt="Meridian: one DSL for everything" width="100%">
</picture>

**Documentation:** <https://zanattamichael.github.io/meridian-core/> · the same pages as the
[wiki](https://github.com/ZanattaMichael/meridian-core/wiki) · the source under [`docs/`](docs/index.md)

Meridian is one declarative language for infrastructure and configuration. You describe
what you want once; Meridian compiles it into the native files of the tool you choose and
then drives that tool to plan and apply it.

It is a transpiler and an orchestrator, not a replacement engine. Ansible still runs the
playbook and Puppet still applies the catalog. Where a tool cannot express what a document
asks for, Meridian refuses at compile time and says why. It never emits output that
quietly does less.

## Status

Early, and moving quickly. The accepted specifications are in [`docs/`](docs/index.md);
start with the [design plan](docs/plans/design-plan.md).

The documentation is published from `docs/` on every merge, as
[the documentation site](https://zanattamichael.github.io/meridian-core/) and as this
repository's [wiki](https://github.com/ZanattaMichael/meridian-core/wiki);
[how the documentation is published](docs/contributing/documentation-site.md) says how.
[How it fits together](https://zanattamichael.github.io/meridian-core/reference/dsl/how-it-fits-together.html)
is the page to start with: it follows one document from a file to a tool that has run.

| Plugin | Category | Compiles to | Runs with |
|---|---|---|---|
| [`ansible`](docs/targets/ansible.md) | configuration management | a playbook and inventory | `ansible-playbook` |
| [`puppet`](docs/targets/puppet.md) | configuration management | a class and node statement | `puppet apply`, or `bolt apply` for another host |
| [`salt`](docs/targets/salt.md) | configuration management | a state file and top file | `salt-call --local`, or `salt-ssh` for another host |
| [`openvox`](docs/targets/openvox.md) | configuration management | a class and node statement | OpenVox's `puppet apply`, or OpenBolt's `bolt apply` for another host |
| [`chef`](docs/targets/chef.md) | configuration management | a cookbook | `chef-client`, or `chef-run` for another host |
| [`dsc`](docs/targets/dsc.md) | configuration management | a DscPipelineRunner configuration | `Invoke-DscRunner` (DSC v2 and v3) |
| [`cloud-init`](docs/targets/cloud-init.md) | configuration management | first-boot user-data and a NoCloud seed | `cloud-init schema` to check it; the machine applies it on its first boot |
| [`terraform`](docs/targets/terraform.md) | infrastructure as code | `main.tf.json` | `terraform`, or `tofu` |
| [`opentofu`](docs/targets/opentofu.md) | infrastructure as code | `main.tofu.json`, with state and plan encryption | `tofu` |
| [`cloudformation`](docs/targets/cloudformation.md) | infrastructure as code | a template | `aws cloudformation` change sets |
| [`bicep`](docs/targets/bicep.md) | infrastructure as code | `main.bicep` | `az deployment group` |
| [`arm`](docs/targets/arm.md) | infrastructure as code | an ARM JSON template | `az deployment group`, or `az deployment sub` |
| [`packer`](docs/targets/packer.md) | infrastructure as code (image building) | `build.pkr.json` | `packer` |
| [`pulumi`](docs/targets/pulumi.md) | infrastructure as code | a Pulumi YAML program | `pulumi preview`, `pulumi up` |
| [`docker`](docs/targets/docker.md) | containerization | a Compose file | `docker compose` |
| [`podman`](docs/targets/podman.md) | containerization | Quadlet units | `systemctl` and `podman`, over `ssh` for another host |
| [`kubernetes`](docs/targets/kubernetes.md) | container orchestration | manifests | `kubectl apply` |
| [`helm`](docs/targets/helm.md) | container orchestration | a Helm chart, with an optional Kustomize overlay | `helm upgrade --install`, with `kustomize build` for an overlay |

Five resource types are Meridian's own and mean the same on every cloud: `network`,
`subnet`, `firewall`, `address` and `vm`. Terraform and OpenTofu (on AWS and Azure) and Bicep turn each
into the cloud's own resources, so one document compiles for each, and the tool's own types
stay usable beside them. See
[the common infrastructure types](docs/reference/dsl/common-infrastructure-types.md).

<p align="center"><img src="docs/graphics/meridian-common-types.svg" alt="The common infrastructure types: network, subnet, firewall, address and vm written once, and what Terraform for AWS, Terraform for Azure and Bicep compile each to" width="100%"></p>

Configuration that already exists can be brought in: `meridian import` reads Terraform and
OpenTofu configuration, Ansible playbooks, Compose files, Kubernetes manifests and DSC
configurations, writes the documents that describe the same thing, and marks whatever it
could not carry over. See [importing existing configuration](docs/operations/importing.md).

What each can and cannot express is in the generated
[capability matrix](docs/reference/capability-matrix.md). To get started, read the
[quickstart](docs/quickstart/install.md); there is a worked example for every plugin under
[`examples/`](examples).

## The `meridian` command

```
cargo build --workspace
meridian validate site.yaml                 # check documents, load no plugin
meridian plugins                            # what is installed, and what each can do
meridian compile site.yaml --out build      # write each target's own files; change nothing
meridian plan site.yaml                     # ask each tool what applying would change
meridian apply site.yaml                    # make it so
meridian import ./compose.yaml -o imported  # turn a tool's existing configuration into documents
meridian test                               # run the project's own tests: compile and plan, then check
```

Plugins are found beside the `meridian` binary, or in the directories named by `--plugin-dir`
or `MERIDIAN_PLUGIN_DIR`. Each tool's working files and Meridian's own run-state live under
`--state-dir` (`.meridian` by default).

A `plan` or `apply` holds a lock on that state, so two runs cannot collide, and every run is
recorded: `meridian status` shows the last one step by step, and `meridian apply --resume`
continues an apply that failed from the stage that failed. `--on-failure rollback` and
`meridian rollback` put back what an apply changed, and `meridian drift` says whether what was
applied is still what is there. A resource's `adopt` takes a thing that already exists under
management instead of creating another ([adopting what exists](docs/reference/dsl/resources.md#adopting-what-exists)).
With `--policy`, a `plan` or `apply` first asks a policy, written
in Rego for Open Policy Agent, whether each change may be made, and an apply that is denied
applies nothing ([policy checks](docs/reference/policy.md)). The record can be kept in a shared place with `--backend`. See
[running safely](docs/concepts/run-safety.md).

Every run can also leave a report and tell other systems what it did: `--report junit=FILE`
(or `json`, `markdown`, `html`) writes one when the run ends, `meridian report` writes one for
any recorded run, and `--sink NAME` sends the run's events to a program that forwards them, as
they happen. A report names steps, resources and outputs, and never holds a value. See
[reports and sinks](docs/reference/reporting.md). To keep every project's runs in one place, the
optional [`meridian-report-server`](docs/reference/report-server.md) receives them through a sink.

Every pipeline engine runs Meridian as one job, with no plugin: the job runs `meridian apply`,
and Meridian does the rest. Inside GitHub Actions or Azure Pipelines, a refused document shows
on the pull request at its line and the run's report becomes the job's summary; on Buildkite
the report is an annotation. [Running Meridian in a pipeline](docs/operations/running-in-a-pipeline.md)
has a tested GitHub Actions workflow and a snippet for each of the other engines, what the engine
owns and what Meridian owns, and how the job's identity is checked before anything runs.
What the reviewers approved is what gets applied: `meridian plan --out` saves the plan, two
actions post it on the pull request and apply it after the approval, and `meridian apply --plan`
refuses if anything has changed since
([a saved plan](docs/operations/pull-requests.md)).

<p align="center"><img src="docs/graphics/meridian-in-a-pipeline.svg" alt="Meridian inside a pipeline engine: the engine's band above Meridian's band, the job running meridian apply, and the events coming back as annotations and a summary" width="100%"></p>

A workflow's Infrastructure documents run first, then its ResourceSets. A ResourceSet whose
`targetHost` comes from an Infrastructure output is compiled for, and applied to, whatever
that output turns out to be; until that infrastructure has been applied it is reported as
deferred. A document can also be handed a value from any other document with `spec.imports`:
a file it compiles to, such as cloud-init user-data for the machine Terraform creates, or an
output. It reaches the tool as the tool's own variable, and the documents run in the order
that needs. Values a tool marks sensitive are masked wherever Meridian prints.

## Editor support

The same command carries the tools for writing documents, and they are the compiler, so
an editor never accepts what `meridian compile` refuses:

```
meridian fmt .                              # one standard layout; never changes a value
meridian lint --strict .                    # every error in a project, plus probable mistakes
meridian schema -o meridian.schema.json     # a JSON Schema, for editors that only know YAML
meridian schema sync .                      # cache each tool's own types, for completion and checking
meridian schema sync --locked .             # the same, from signed schema bundles the lock pins: no tool needed
meridian lsp                                # the language server, started by your editor
meridian mcp                                # the MCP server, started by an AI client
```

![Writing a resource with completion](docs/tooling/images/writing-a-resource.gif)

The language server gives completion, hover documentation, errors as you type, go to
definition, rename, formatting and quick fixes, with resource types and params read from
the plugins you have installed, and a tool's own types (`aws_instance`, `apps/v1/Deployment`)
read once from the tool by `meridian schema sync` and cached in the project, or taken ready-made from
signed [schema bundles](docs/tooling/schema-bundles.md) in an OCI registry. There is a [Visual Studio Code extension](docs/tooling/vscode.md)
in [`editors/vscode`](editors/vscode), and recipes for [other editors](docs/tooling/other-editors.md).
Start at [Tools for writing Meridian documents](docs/tooling/index.md).

## Layout

The core is a Cargo workspace under `crates/`:

- `meridian-ir` — parsing and validation of the documents
- `meridian-yaml` — YAML scalar rules and a deterministic writer, shared by the parser and the plugins that emit YAML
- `meridian-resolve` — hierarchical data resolution
- `meridian-module` — modules: the `meridian.yaml` manifest, path, git and OCI registry sources, version
  resolution, `meridian.lock` and the content hash every pinned module is checked against
- `meridian-test` — [`meridian test`](docs/reference/testing.md): `Test` documents that compile or plan a project's documents with given data and check what came out
- `meridian-rules` — [compile rules](docs/concepts/compile-rules.md): policies checked over the layers of a hierarchy before anything is compiled, such as "a more specific layer may not change a variable or a secret a less specific layer set"
- `meridian-graph` — dependency graph and topological sort
- `meridian-ast` — the target-agnostic resource tree every plugin consumes
- `meridian-value` — the dynamic value type and its canonical JSON form, shared by the core and the SDK
- `meridian-sdk` — what a plugin author builds against: the `Emitter`, `Runner` and `Importer`
  traits, `serve`, the wire conversion, and helpers for running a command-line tool
- `meridian-import` — what importers share: readers for YAML and HCL that keep line numbers, the
  writer that renders an imported document with its `TODO` markers, and the semantic diff the
  round-trip tests compare with
- `meridian-plugin-host` — launching, discovering and driving plugins
- `meridian-orchestrator` — runs a workflow: compiles each document, drives its tool through a
  plugin, hands outputs from one stage to the next, and stops on the first failure
- `meridian-tooling` — what the editor tools share: the formatter, the linter, completion, hover
  and navigation, and the JSON Schema export, all read from the parser's own field tables and
  from what the plugins declare
- `meridian-lsp` — the language server
- `meridian-mcp` — the MCP server: the compiler and the catalogue as tools for an AI model
- `meridian-cli` — the `meridian` command
- `meridian-policy-opa` — the policy engine for Open Policy Agent, which `--policy` asks whether
  a change may be made
- `meridian-target-fake` — a small, complete plugin whose tool is a directory, used by the tests
  and readable as a worked example of the SDK
- `meridian-plugin-testkit` — what the first-party plugins' tests share
- `meridian-plugin-template` — a small working plugin that `tools/new_plugin.py` copies to start a new one

The plugins are under `plugins/`, one crate each, built as a binary named
`meridian-target-<name>`. A plugin's library depends on `meridian-sdk` and, where it writes
YAML or imports, on `meridian-yaml` and `meridian-import`: nothing of the compiler's, which is
the same position a third-party plugin is in.

A plugin is a separate process. The host launches the binary, reads one handshake line
saying where it is listening (a Unix socket, or a named pipe on Windows), agrees a
protocol version with it, and then talks to it over gRPC. Before it starts a plugin, the host can
check the binary's [signature](docs/reference/plugin-signatures.md); a released `meridian`
starts only the plugins it was released with, unless told to trust another key.
The protocol is defined in [`proto/`](proto/meridian/plugin/v1/plugin.proto), so a plugin
can be written in any language that can serve it. A plugin binary is not meant to be run
by hand: its stdout carries the protocol, and it exits when the host closes its stdin.

Every plugin compiles in four stages, in this order: transform (map each resource to the
tool's own vocabulary, or refuse it), sort (apply the tool's ordering semantics), validate
(the tool's own constraints) and emit (pure serialisation). Running the tool is a separate
half of the plugin, so compiling stays free of side effects. A third, optional half imports:
it reads the tool's existing configuration and returns documents as data, which `meridian
import` writes out.

## Tests

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
python3 tools/check_links.py
```

Each plugin's tests compile the shared documents in `fixtures/` and its own examples, and
compare the output byte for byte with the golden files in `plugins/<name>/testdata`.
Refusals are golden files too, because the wording an operator reads is output like any
other. Set `UPDATE_GOLDEN=1` to record them after a deliberate change. The runners are
tested against stand-ins for the tools, so the suite needs nothing installed but Rust.
Seventeen of the eighteen targets are also tested against the tool itself, each in a
workflow of its own: [Real-tool tests](docs/contributing/real-tool-tests.md) lists what
each one runs and what it cannot prove.

## Contributing

Meridian gets better with every tool it speaks well, so contributions from
outside are welcome, and a first one can be small.

1. Read [`CONTRIBUTING.md`](CONTRIBUTING.md): set up, find your way around, and what a
   pull request needs.
2. Pick something: an issue labelled
   [`good first issue`](https://github.com/ZanattaMichael/meridian-core/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
   or [`help wanted`](https://github.com/ZanattaMichael/meridian-core/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22),
   or a tool Meridian does not support yet.
3. To add a tool, run `python3 tools/new_plugin.py <name>` and follow
   [Writing a plugin](docs/contributing/writing-a-plugin.md). You start from a plugin
   that already builds and passes its tests.
4. Open a pull request. A draft is fine.

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
Security reports go through [`SECURITY.md`](SECURITY.md), not the issue tracker.
