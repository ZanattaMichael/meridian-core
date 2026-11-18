# Tool schemas: completion for a tool's own types

```
meridian schema sync   <paths>... [--target TARGET] [--data FILE] [--set KEY=VALUE] [--state-dir DIR] [--check] [--no-lock]
meridian schema status [--state-dir DIR] [--json]
meridian schema clean  [TARGET]... [--state-dir DIR]
```

A target plugin declares the resource types Meridian itself defines, so the
[editor tools](index.md) complete and check `package`, `file` or `service` on
a configuration management target from the moment the plugin is installed.
The tool's own types are another matter. Terraform has thousands of resource
types across its providers, a Kubernetes cluster describes hundreds of kinds
and whatever custom resources are installed, a Pulumi package has its own
schema, and a DSC module lists its resources' properties. They are too many,
and change too often, to be written into a plugin. Without them an editor
knows `aws_instance` only as a name that fits `<provider>_<type>`: it can
offer the type, but not its params, and it cannot say that `instance_typo`
is wrong.

`meridian schema sync` asks each tool for those schemas once, through its
plugin, and caches the answer in the project. The language server, `meridian
lint` and `meridian schema` then read the cache and never run a tool.

## Running a sync

```console
$ meridian schema sync .
terraform: asking the tool about 3 resources in 2 documents
terraform: cached 2 resource types from terraform 1.13.1; hashicorp/aws 5.60.0; hashicorp/local 2.5.2 in .meridian/schemas/terraform.json
  note: 1412 other types listed by name only; a type is described in full once a document uses it and the schemas are synced again
kubernetes: asking the tool about 4 resources in 1 document
kubernetes: cached 4 resource types from kubectl 1.31.0; server 1.30.2 in .meridian/schemas/kubernetes.json
recorded in meridian.lock
```

The paths are the documents of the project, or directories to search for
them. The sync reads every document, works out which targets they use and
which resource types each target is asked about, and calls each target's
plugin once. The plugin runs its tool the way the tool is normally run on
that machine: `terraform init` fetches the providers, `kubectl` talks to the
cluster the documents name, `pulumi package get-schema` fetches a provider
plugin it does not have. It may take a while the first time and it may use
the network; nothing an editor does ever will.

| Option | Meaning |
|---|---|
| `--target TARGET` | sync one target and leave the others' caches as they are |
| `--data FILE`, `--set KEY=VALUE` | values for `when` conditions, as for `compile`; without them every resource is included |
| `--state-dir DIR` | where the cache lives (default `.meridian`, or `MERIDIAN_STATE_DIR`) |
| `--check` | change nothing; fail if any target's cache is missing or is not what `meridian.lock` records. For CI |
| `--no-lock` | cache the schemas but do not record them in `meridian.lock` |

A target whose plugin does not export schemas is reported and skipped. A
tool that cannot be run is an error for that target, and the sync goes on
with the others and exits `1` at the end.

## What is cached

The cache is one JSON file per target under the state directory:

```
.meridian/
  schemas/
    terraform.json           the schemas, with the tool and versions they came from
    kubernetes.json
    work/
      terraform/             the plugin's own working directory: a configuration
                             requiring the providers, .terraform/, the lock file
```

The `work` directory is the plugin's, kept between syncs so the second sync
does not download what the first did. It is never read by an editor. Both
directories belong in `.gitignore` with the rest of `.meridian`, as the
quickstart sets up.

Each file records:

| Field | Meaning |
|---|---|
| `target` | the target the file is for |
| `tool`, `toolVersion` | the tool that answered, such as `terraform 1.13.1` |
| `pins` | the exact versions of the parts the schemas came from: a provider, a package, an API server, a PowerShell module |
| `syncedAt` | when the sync ran |
| `notes` | what the plugin wanted the operator to know |
| `schemas` | one entry per resource type, under the exact name a document writes, with its description, params, states and where it came from |

A param carries its kind, whether it is required, the values it allows,
whether it is `computed` (set by the tool, read with `${id.path}` and never
written), `sensitive` or `deprecated`, and its own nested params for a map
or a list of maps. That is the same description a plugin gives for its own
types, so everything the editor can do with a plugin's type it can do with a
tool's.

### Described in full, or by name

A type the documents use is described in full. Every other type the tool
knows is listed by name with its description, so completion still offers
the whole catalogue of a provider, a package or a cluster; when a document
starts using one, the next sync describes it in full. The split exists
because a provider such as AWS has over a thousand types with hundreds of
params each, and a cache holding every param of every one would be tens of
megabytes for an editor to read at each start. Hover on a type that is known
by name says so, and says to sync again.

## The lock file

A sync records what it cached in `meridian.lock`, beside the project's
[modules](../reference/modules.md):

```yaml
schemas:
  - target: terraform
    tool: terraform
    toolVersion: "1.13.1"
    pins:
      hashicorp/aws: "5.60.0"
      hashicorp/local: "2.5.2"
    integrity: h1:3b8f...
```

The lock file is committed, and the cache is not. So a colleague who checks
out the project is told, by `meridian schema status` and by the editor, that
schemas are recorded but not cached here, and one `meridian schema sync`
reproduces them from the same versions. The integrity hash is of the cache
file, so a cache that was edited by hand, or synced from other versions, is
reported as differing from the lock until it is synced again. `meridian
module install` leaves the `schemas` section alone, and a sync leaves the
`modules` section alone.

## Status and clean

```console
$ meridian schema status
terraform: 2 resource types from terraform 1.13.1; hashicorp/aws 5.60.0; hashicorp/local 2.5.2, synced 2026-11-17T20:16:02Z: current
kubernetes: recorded in meridian.lock but not on disk; run `meridian schema sync`
pulumi: not synced
```

The states are `current`, `not recorded in meridian.lock` (cached with
`--no-lock`, or before the lock was committed), `recorded in meridian.lock
but not on disk` and `differs from what meridian.lock records`. `--json`
prints one object per target, with the cache and the lock entry side by
side.

`meridian schema clean` removes every target's cache and its record in the
lock; `meridian schema clean terraform` removes one target's.

## In the editor

The [language server](language-server.md) reads the cache of each workspace
folder when it starts, reads it again when a sync changes it, and tells you
once when schemas are recorded but not cached or are not current. In
[Visual Studio Code](vscode.md) the command **Meridian: Sync Tool Schemas**
runs the sync in a terminal, so what the tools print is in front of you.
With the cache in place:

- completion under `params` offers the tool's params, required ones first,
  and inside a map param its nested params;
- hover on a type shows its description, where it came from and a table of
  its params; hover on a param shows its kind, whether it is required and
  the values it takes;
- a param the type does not take, a required param that is missing, a value
  the param does not allow, a value of the wrong kind, and a computed param
  written in a document are reported in their place
  (`unsupported-param`, `missing-param`, `unsupported-value`,
  `wrong-param-kind`, `computed-param`);
- `${id.` completes the type's params, including the computed ones, which
  is how a document reads what the tool set.

`meridian lint` reports the same, so what the editor underlines is what CI
sees. `meridian schema` writes the cached types into the JSON Schema it
prints, so an editor that only knows YAML checks them too.

What the cache does not change: a plugin's own transform stage still decides
what compiles, and the tool still decides what applies. The cache is advice
to the editor, read from the tool itself, and a plugin never promises in it
what its tool refuses.

## What each target exports

| Target | Asks | Described in full | Listed by name | Pins |
|---|---|---|---|---|
| `terraform`, `opentofu` | `terraform providers schema -json` (`tofu` for OpenTofu), after `init -backend=false` of a configuration that requires the providers the documents use, with the `version` a `provider` resource gives | the `<provider>_<type>`, `data.<type>` and `ephemeral.<type>` types the documents use: every attribute and nested block, with Terraform's own meta-arguments (`count`, `for_each`, `provider`, `lifecycle`, `provisioner`, `connection`) | every other resource and data source of those providers, and `terraform_data` | each provider's version from `.terraform.lock.hcl` |
| `kubernetes` | `kubectl get --raw /openapi/v2` against the context and kubeconfig the `cluster` resource names | the `<apiVersion>/<Kind>` types the documents use, `$ref`s resolved, `status` marked computed | every other kind the server knows, built-in and custom alike | the server's version, and the context |
| `pulumi` | `pulumi package get-schema <package>[@version]` for each package the documents use, the version from `resourceOptions.version` when one is given | the resource and `invoke.<function>` types the documents use: input properties, output-only properties as computed, `resourceOptions` and `invokeOptions` | every other resource and function of those packages | each package's version |
| `ansible` | `ansible-doc -t module --json <module>` for each module the documents name in full, `ansible-doc -t module --list --json` for the rest, and `ansible-galaxy collection list --format json` for the collections' versions | the modules the documents use: every option with its type, whether it is required, its choices, its sub-options, and `no_log` options as sensitive | every other module `ansible-doc` finds, builtin and installed collections alike | ansible-core's version as `ansible.builtin`, and each installed collection's version |
| `dsc` | `Get-DscResource -Module` in PowerShell for each module the documents use (the module's `[DscResource()]` classes, where that lists nothing, as on Linux), and `dsc resource list` and `dsc resource schema` when DSC v3 is installed | every resource of each PowerShell module, from its property types, mandatory flags and enum values; the DSC v3 resources the documents use, from their manifests' JSON Schema | every other DSC v3 resource | PowerShell's version, each module's version, each DSC v3 resource's version |

The other targets do not export schemas yet; `meridian schema sync` says so
and the editor goes on knowing their types by name or by shape, as before.
The protocol is the same for every target, so a plugin that gains an
exporter needs no change anywhere else; see
[Writing a plugin](../contributing/writing-a-plugin.md#a-schema-source).

A type a document writes in a form the tool does not use is matched by
meaning where the tool has one: Pulumi's `random:index:RandomPet`,
`random:RandomPet` and `random:index/randomPet:RandomPet` are one type.

## In CI

Commit `meridian.lock` and run the sync as a check, so a cache that is stale
fails the build before a document that relies on it does:

```yaml
- run: meridian schema sync --check .
```

It changes nothing and runs no tool. To refresh the cache in CI instead, run
`meridian schema sync .` on a machine that has the tools and the network
they need, and commit the lock file it writes. The first-party plugins'
exporters are tested against the real tools in the same workflows as their
runners; see [Real-tool tests](../contributing/real-tool-tests.md).

## Limits

- A sync reads the documents as `meridian compile` would, so a document that
  does not parse is reported and skipped, and a resource whose `when` is
  false under the `--data` given is left out. Without `--data`, every
  resource is included.
- The sync describes the tool as installed on the machine it runs on. A
  colleague with another provider version gets a cache that differs from
  the lock, and is told so.
- Meridian's own types on a target (`module`, `variable`, `provider`,
  `cluster`, `stack`) and its `spec.settings` keys are the plugin's to
  describe, and are not in the cache.
- Composite resources, user-defined functions and file templates are not in
  the language yet; when they are, their completion will come from the
  workspace and the installed modules, not from this cache.
