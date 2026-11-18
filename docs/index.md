# Meridian

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="branding/banner/meridian-banner-dark-animated.svg">
  <img src="branding/banner/meridian-banner-light-animated.svg" alt="Meridian: one DSL for everything" width="100%">
</picture>

Meridian is one declarative language that compiles to Ansible, Puppet, Chef, DSC,
cloud-init, Terraform, CloudFormation, Bicep, ARM templates, Pulumi, Docker Compose, Kubernetes and Helm, and then drives
the tool you chose to plan and apply it.

These pages are published at <https://zanattamichael.github.io/meridian-core/>, with
search and a sidebar, and as the repository's
[wiki](https://github.com/ZanattaMichael/meridian-core/wiki). Both are built from
[`docs/`](https://github.com/ZanattaMichael/meridian-core/tree/main/docs) on every merge.

<p align="center"><img src="graphics/meridian-architecture.svg" alt="Meridian architecture: documents go into the core, the core speaks to one plugin per target over the plugin protocol, and each plugin drives its own tool" width="100%"></p>

The core knows the language and nothing about any tool. Each plugin knows one tool and
nothing about the others. [How it fits together](reference/dsl/how-it-fits-together.md)
follows one document through both.

## Honest scope statement

Meridian is a **transpiler and orchestrator**, not a replacement execution engine. It
does not reimplement Ansible's SSH orchestration, Terraform's state graph, or Puppet's
catalog model — it compiles a single declarative intent into the native artifact each of
those tools already knows how to run.

The consequence, stated up front rather than discovered later: these tools differ in
execution model, state tracking, dependency expression, and idempotency guarantees.
Meridian abstracts intent, not those differences. Where a feature has no clean mapping
on a target, Meridian emits a documented workaround or fails at compile time. It never
degrades silently. See [the capability model](plans/design-plan.md#8-the-notifies-property--capability-matrix).

## Project status

Early implementation. The core, the plugin protocol and the plugins are written in Rust.
The first implementation was in Go; the core language was changed to Rust on 2026-11-17
and the Go code was removed once the Rust core and plugins reproduced its output byte for
byte. The reasons, the layout and the order of work are in
[design plan §11](plans/design-plan.md#11-system-architecture-rust) and
[§12.1](plans/design-plan.md#121-rust-migration-sequencing).

## User documentation

| Start here | |
|---|---|
| [Install](quickstart/install.md) | install a release or build from source, check what you downloaded, and what each target needs |
| [Meridian on Windows](quickstart/windows.md) | what is different on Windows, and which targets make sense there |
| [Your first ResourceSet](quickstart/first-resourceset.md) | one document, from validate to apply, and the same document on another tool |
| [A two-stage workflow](quickstart/first-two-stage.md) | one tool creates a machine, another configures it |
| [Worked workflows](quickstart/workflows.md) | eight workflows across every target: three tiers on four tools, a cluster platform, a golden image, Windows on Azure, an edge site, one file for every environment, one file for every customer, and the vertical slice CI applies for real |

| Bringing in what you have | |
|---|---|
| [Importing existing configuration](operations/importing.md) | `meridian import`: Terraform, OpenTofu, Ansible, Compose, Kubernetes and DSC configuration into documents, with everything that could not be carried over marked |

| Operating | |
|---|---|
| [Running Meridian in a pipeline](operations/running-in-a-pipeline.md) | one job per engine: a tested GitHub Actions workflow and reviewed snippets for Azure Pipelines, GitLab CI, Buildkite, Jenkins, Argo Workflows, Tekton and Octopus; what the engine owns and what Meridian owns; annotations and job summaries; checking the job's identity first; sharing the record between jobs |

| Concepts | |
|---|---|
| [Documents, resources and targets](concepts/ir-overview.md) | the two kinds of document, and the two families of resource types |
| [Dependencies and notifications](concepts/dependencies.md) | `dependsOn` and `notifies`, and what each tool makes of them |
| [The capability model](concepts/capability-model.md) | why a document either does what it says or does not compile |
| [The two-stage architecture](concepts/two-stage-architecture.md) | Infrastructure, ResourceSet and the hand-off between them |
| [Running safely](concepts/run-safety.md) | one run at a time, the record of every run, resuming a failed run, putting one back, finding drift, asking a policy first, sharing state |
| [Compile rules](concepts/compile-rules.md) | policies over layered configuration: what a layer may and may not override, checked before anything is compiled |
| [How a secret is held in memory](concepts/secrets-in-memory.md) | locked, wiped memory, the hardened process, how a secret reaches a tool, and what is not promised |

| In pictures | |
|---|---|
| [The architecture](reference/dsl/how-it-fits-together.md#the-same-thing-as-an-architecture) | documents, the core, the plugin protocol, the plugins by category, and the tools |
| [The path of a document](reference/dsl/how-it-fits-together.md) | nine steps from a file to a tool that has run, and how far each command goes |
| [Kinds and categories](concepts/ir-overview.md#two-kinds) | which kind of document each plugin category takes |
| [The two stages](concepts/two-stage-architecture.md) | an Infrastructure stage hands one declared value to a ResourceSet stage |
| [A secret's path](concepts/secrets-in-memory.md) | from a provider into locked memory, as a reference everywhere else, to the tool by a private file |
| [A run](concepts/run-safety.md) | the lock, the policy check, the record, the run-state, and what happens when a stage fails |
| [Modules](reference/modules.md) | the manifest, the sources, the lockfile, and what every run verifies |
| [Inside a pipeline engine](operations/running-in-a-pipeline.md) | the engine's band above Meridian's: the job runs `meridian apply`, and the events come back as annotations and a summary |

Each picture is drawn by [`tools/wiki/graphics.py`](https://github.com/ZanattaMichael/meridian-core/blob/main/tools/wiki/graphics.py)
and checked against the file in every build, so none can fall behind the script that describes it.

| Editor and authoring tools | |
|---|---|
| [Overview](tooling/index.md) | what the tools are, and why they never disagree with the compiler |
| [The language server](tooling/language-server.md) | completion, hover documentation, errors as you type, navigation |
| [Visual Studio Code](tooling/vscode.md) | the extension: install, settings, commands |
| [Other editors](tooling/other-editors.md) | Neovim, Helix, Emacs, and pre-commit |
| [The formatter](tooling/formatter.md) | `meridian fmt`: one layout, and the guarantee that it changes nothing else |
| [The linter](tooling/linter.md) | `meridian lint`: every error in a project, plus probable mistakes |
| [The JSON Schema](tooling/json-schema.md) | `meridian schema`: validation for editors that only know YAML |
| [Tool schemas](tooling/schema-sync.md) | `meridian schema sync`: a tool's own types, read once from the tool and cached, so the editor completes and checks them |
| [The MCP server](tooling/mcp-server.md) | `meridian mcp`: the compiler and the catalogue as tools for an AI model, read-only, with the one diagnostic shape a small model can act on |

| Reference | |
|---|---|
| [The `meridian` command](reference/cli.md) | commands, options, exit status |
| [Machine-readable output](reference/cli-json.md) | `--output json`: the one object every command prints for a pipeline, its schema, and `plan --detailed-exitcode` |
| [Lint rules](reference/lint.md) | every rule of `meridian lint`, with an example and what to do |
| [Plugin signatures](reference/plugin-signatures.md) | how `meridian` decides a plugin may be started, trusting a plugin from elsewhere, and signing your own |
| [Policy checks](reference/policy.md) | asking a policy before a change is made: the options, the input a policy sees, Rego with Open Policy Agent, and how to write an engine |
| [State, runs and backends](reference/state.md) | what Meridian keeps about its runs, the events, and how to write a state backend |
| [Reports and sinks](reference/reporting.md) | report files in JSON, JUnit, Markdown and HTML, sending a run's events elsewhere as they happen, and how to write a sink |
| [The report server](reference/report-server.md) | an optional server that collects the runs of every project and shows them; running it, securing it, its HTTP interface |
| [The Meridian language](reference/dsl/index.md) | every field of a document, what it must hold, and which check happens when |
| [How it fits together](reference/dsl/how-it-fits-together.md) | a document's path from file to running tool, with diagrams and a worked example |
| The language item by item | [resources](reference/dsl/resources.md), [`dependsOn`](reference/dsl/dependson.md), [`notifies`](reference/dsl/notifies.md), [conditions](reference/dsl/conditions.md), [references](reference/dsl/references.md), [outputs and hand-offs](reference/dsl/outputs-and-hand-offs.md) |
| The language by segment | how `type`, `state`, `params`, references and notifies are written for [configuration management](reference/dsl/configuration-management.md), [infrastructure as code](reference/dsl/infrastructure-as-code.md), [containerization](reference/dsl/containerization.md) and [container orchestration](reference/dsl/container-orchestration.md) |
| [Configuration management resource types](reference/resource-types.md) | `package`, `file`, `service`, `user`, `group`, `exec`, and how each target differs |
| [Modules](reference/modules.md) | packaging configuration to share it: the manifest, sources (path, git, registry), versions, the lockfile, integrity, the `meridian module` commands |
| [Capability matrix](reference/capability-matrix.md) | generated from the plugins |
| [Plugin settings](reference/plugin-settings.md) | every setting of every target's runner, how a value is found, how argument lists are split; generated from the plugins |
| [Conformance matrix](reference/conformance-matrix.md) | the same document on every configuration management target, cell by cell; generated from the plugins |
| [Refusal rules](reference/refusal-rules.md) | every rule a compile-time refusal can carry, what it refuses and what to write instead; generated from the registry and the plugins |
| [Compatibility](reference/compatibility.md) | which language versions a build reads, and which plugins it loads; generated from the code |
| [Compile rules](reference/compile-rules.md) | the rules file, every rule and every refusal, and `meridian rules check` |
| [Error kinds and codes](reference/errors.md) | every kind of error the crates produce, with its fixed code: expressions, stages of a run, modules, compile rules, layered data, keys and signatures |

| Targets | |
|---|---|
| Configuration management | [ansible](targets/ansible.md), [puppet](targets/puppet.md), [openvox](targets/openvox.md), [chef](targets/chef.md), [dsc](targets/dsc.md), [salt](targets/salt.md), [cloud-init](targets/cloud-init.md) |
| Infrastructure as code | [terraform](targets/terraform.md), [opentofu](targets/opentofu.md), [cloudformation](targets/cloudformation.md), [bicep](targets/bicep.md), [arm](targets/arm.md), [pulumi](targets/pulumi.md), [packer](targets/packer.md) |
| Containers | [docker](targets/docker.md), [podman](targets/podman.md), [kubernetes](targets/kubernetes.md), [helm](targets/helm.md) (with Kustomize) |

Every target page opens with what the target does and does not support. Every example the
pages show is a file under [`examples/`](../examples) that a test compiles with the real
plugins.

## Planning documents

| Document | What it settles |
|---|---|
| [Design plan](plans/design-plan.md) | IR schema, pipeline stages, plugin SDK, architecture, build milestones |
| [Testing plan](plans/testing-plan.md) | The five test tiers, the shared fixture library, CI wiring |
| [Documentation plan](plans/documentation-plan.md) | Doc set structure, audiences, generated capability matrix |
| [Governance & contribution model](plans/governance.md) | Plugin tiers, ownership, capability disputes, triage |
| [Migration & adoption path](plans/migration-and-adoption.md) | Forward-compile scope decision, coexistence, pilot guidance |
| [Observability, logging & diagnostics](plans/observability.md) | Log schema, plan output, fact provenance, telemetry, audit |
| [Release, versioning & compatibility](plans/release-and-versioning.md) | apiVersion, SDK compatibility, run-state migration, channels |
| [Modules and versioning](plans/modules-and-versioning.md) | Built, awaiting the owner's review: shareable modules (sources, lockfile, integrity), how `apiVersion` is enforced, protocol negotiation |
| [Security & secrets handling](plans/security.md) | Threat model, `secret: true` guarantees, disclosure process |
| [Run safety](plans/run-safety.md) | Built: state and locking, resume, rollback, drift, policy checks, values handed between stages |
| [Reporting](plans/reporting.md) | Built: streaming, report files, sinks, and the report server |
| [Language expansion](plans/language-expansion.md) (accepted; phases 1, 2 and most of 6a built) | Expressions and functions, conditions, templating, configurations with inputs, endpoints and inventory, secrets, workflows |
| [Decision records](decisions/README.md) | Design choices made while building, one record each, with whether the owner has accepted them |

## Contributing

| | |
|---|---|
| [`CONTRIBUTING.md`](../CONTRIBUTING.md) | from a fresh clone to a merged pull request: set up, the checks, review, and the changes that need a second reviewer |
| [Writing a plugin](contributing/writing-a-plugin.md) | add a tool to Meridian, step by step, starting from a template that already works |
| [How Meridian is tested](contributing/testing.md) | unit tests and golden files, property tests, cross-target conformance, examples, coverage; and how to add to each |
| [Real-tool tests](contributing/real-tool-tests.md) | what each target is tested against for real, what that proves and what it cannot, and the settings the Azure job needs |
| [Releasing](contributing/releasing.md) | what the release pipeline builds and checks, the settings a release needs, and the steps to cut one |
| [Code of Conduct](../CODE_OF_CONDUCT.md) | how we treat each other |

Security reports go through [`SECURITY.md`](../SECURITY.md), not the issue tracker.

These pages are published as [the documentation site](https://zanattamichael.github.io/meridian-core/)
and as the repository's [wiki](https://github.com/ZanattaMichael/meridian-core/wiki) by a
pipeline; [how the documentation is published](contributing/documentation-site.md)
describes it, what a new page needs, and how the graphics and the animated theme are made.

## Reading order

New to the project: start with the design plan's problem statement and guiding
principles (§1–§2), then the IR schema overview (§4). Everything else is downstream of
those.
