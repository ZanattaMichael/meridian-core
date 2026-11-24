# Tool schemas: completion for a tool's own types

```
meridian schema sync   <paths>... [--target TARGET] [--data FILE] [--set KEY=VALUE] [--state-dir DIR]
                                  [--check | --locked] [--no-lock] [--all-types]
                                  [--prefer bundles|tool] [--schema-signatures POLICY]
meridian schema update <paths>... [the same options, less --check and --locked]
meridian schema status [--state-dir DIR] [--json]
meridian schema clean  [TARGET]... [--state-dir DIR]
```

Plugins describe Meridian's shared types. A tool's own types (Terraform providers,
Kubernetes kinds, Pulumi packages, DSC modules) are too many to build in.
`meridian schema sync` asks each tool through its plugin and caches the answer; the
[editor tools](index.md), `meridian lint` and `meridian schema` read the cache. Signed
[schema bundles](schema-bundles.md) from an OCI registry provide the same schemas with no
tool installed.

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

The sync reads the documents, finds the targets and types used, and calls each plugin
once. The plugin runs its tool normally (`terraform init`, `kubectl`,
`pulumi package get-schema`), which may use the network. Editors never do.

| Option | Meaning |
|---|---|
| `--target TARGET` | sync one target and leave the others' caches as they are |
| `--data FILE`, `--set KEY=VALUE` | values for `when` conditions, as for `compile`; without them every resource is included |
| `--state-dir DIR` | where the cache lives (default `.meridian`, or `MERIDIAN_STATE_DIR`) |
| `--check` | change nothing; fail if any target's cache is missing or is not what `meridian.lock` records. For CI |
| `--locked` | use only what `meridian.lock` pins: the [schema bundles](schema-bundles.md#the-lock-file) it names, by digest, or for a target synced from its tool, the tool and pins it records (a tool that answers from other versions fails the target). Nothing new is chosen. For CI |
| `--no-lock` | cache the schemas but do not record them in `meridian.lock` |
| `--all-types` | describe every type of the providers, packages or cluster the documents use, not only the types they write: the tool is asked a second time for all of them. For building a [schema bundle](schema-bundles.md#making-and-publishing-a-bundle); the cache can be large |
| `--prefer bundles\|tool` | where to take a target's schemas from first (default `bundles`, or `MERIDIAN_SCHEMA_PREFER`). With `bundles`, a project whose `meridian.yaml` names [schema registries](schema-bundles.md#naming-the-registries-specschemas), or whose lock pins bundles, gets them from there and asks the tool only for a target no bundle set describes. Without a registry named, both mean the tool |
| `--schema-signatures off\|warn\|require` | how strict to be about a bundle's signature; defaults to `spec.schemas.signatures`, else `off` |

`meridian schema update` takes the same paths and options and chooses the
schema bundles again, the newest that describe what the documents use,
moving the lock to them; a target synced from its tool is synced again.

Targets without an exporter are skipped. A tool that fails is an error for its target; the
sync continues and exits `1`.

## What is cached

One JSON file per target under the state directory:

```
.meridian/
  schemas/
    terraform.json           the schemas, with the tool and versions they came from
    kubernetes.json
    work/
      terraform/             the plugin's own working directory: a configuration
                             requiring the providers, .terraform/, the lock file
```

`work` is the plugin's download cache. Both belong in `.gitignore`. Each file records:

| Field | Meaning |
|---|---|
| `target` | the target the file is for |
| `tool`, `toolVersion` | the tool that answered, such as `terraform 1.13.1` |
| `pins` | the exact versions of the parts the schemas came from: a provider, a package, an API server, a PowerShell module |
| `syncedAt` | when the sync ran |
| `notes` | what the plugin wanted the operator to know |
| `schemas` | one entry per resource type, under the exact name a document writes, with its description, params, states and where it came from |

A param records kind, required, allowed values, `computed` (read, never written),
`sensitive`, `deprecated` and nested params, as plugins describe their own types.

### Described in full, or by name

Used types are described in full; the rest are listed by name and description to keep the
cache small. The next sync describes a newly used type.

## The lock file

A sync records the cache in `meridian.lock`, beside [modules](../reference/modules.md):

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

Commit the lock, not the cache; `meridian schema sync` reproduces it. The integrity hash
flags edited or differently synced caches. `module install` and `schema sync` leave each
other's sections alone.

A cache made from [schema bundles](schema-bundles.md#the-lock-file) is
recorded the same way, with a `bundles` list giving each bundle's reference
and digest. Such a cache holds no sync time, so it is the same bytes on
every machine and a colleague's `--locked` sync reproduces the committed
hash exactly. A cache the tool wrote holds the time of the sync, so its
hash is reproduced only by the same file: on another machine, `--locked`
holds the tool to the recorded versions instead.

## Status and clean

```console
$ meridian schema status
terraform: 2 resource types from terraform 1.13.1; hashicorp/aws 5.60.0; hashicorp/local 2.5.2, synced 2026-11-17T20:16:02Z: current
kubernetes: recorded in meridian.lock but not on disk; run `meridian schema sync`
pulumi: not synced
```

A cache made from bundles says so:

```console
$ meridian schema status
terraform: 1503 resource types from terraform 1.13.1; registry.terraform.io/hashicorp/aws 5.60.0, fetched from the schema bundle terraform/hashicorp-aws:5.60.0: current
```

States: `current`, `not recorded in meridian.lock`, `recorded in meridian.lock but not on
disk`, `differs from what meridian.lock records`. `--json` prints cache and lock per target,
each with `bundles` (empty when the tool wrote the cache).

`meridian schema clean [target]` removes caches and lock records.

## In the editor

The [language server](language-server.md) rereads the cache when it changes. In
[VS Code](vscode.md), **Meridian: Sync Tool Schemas** runs a sync. With a cache:

- params complete, required first, nested too;
- hover shows types and params;
- `unsupported-param`, `missing-param`, `unsupported-value`, `wrong-param-kind` and
  `computed-param` are reported;
- `${id.` completes params, computed ones included.

`meridian lint` reports the same, and `meridian schema` includes cached types. The cache is
advice: the plugin still decides what compiles, the tool what applies.

## What each target exports

| Target | Asks | Described in full | Listed by name | Pins |
|---|---|---|---|---|
| `terraform`, `opentofu` | `terraform providers schema -json` (`tofu` for OpenTofu), after `init -backend=false` of a configuration that requires the providers the documents use, with the `version` a `provider` resource gives | the `<provider>_<type>`, `data.<type>` and `ephemeral.<type>` types the documents use: every attribute and nested block, with Terraform's own meta-arguments (`count`, `for_each`, `provider`, `lifecycle`, `provisioner`, `connection`) | every other resource and data source of those providers, and `terraform_data` | each provider's version from `.terraform.lock.hcl` |
| `kubernetes` | `kubectl get --raw /openapi/v2` against the context and kubeconfig the `cluster` resource names | the `<apiVersion>/<Kind>` types the documents use, `$ref`s resolved, `status` marked computed | every other kind the server knows, built-in and custom alike | the server's version, and the context |
| `pulumi` | `pulumi package get-schema <package>[@version]` for each package the documents use, the version from `resourceOptions.version` when one is given | the resource and `invoke.<function>` types the documents use: input properties, output-only properties as computed, `resourceOptions` and `invokeOptions` | every other resource and function of those packages | each package's version |
| `ansible` | `ansible-doc -t module --json <module>` for each module the documents name in full, `ansible-doc -t module --list --json` for the rest, and `ansible-galaxy collection list --format json` for the collections' versions | the modules the documents use: every option with its type, whether it is required, its choices, its sub-options, and `no_log` options as sensitive | every other module `ansible-doc` finds, builtin and installed collections alike | ansible-core's version as `ansible.builtin`, and each installed collection's version |
| `puppet`, `openvox` | `puppet describe --list` for the names, then `puppet describe <type>` for each Puppet type the documents use, with the target's own program and arguments | the types the documents use, under the name a document writes (`augeas`, `native/file`): every parameter and property with its description, the values `describe` lists, the providers and features | every other type `describe --list` knows, the shared names under `native/` | Puppet's (or OpenVox's) version |
| `chef` | `chef-resource-inspector` (or `cinc-resource-inspector`), whose one JSON document lists every resource the client has, and `chef-client --version` | the resources the documents use (`template`, `native/package`): every property with its kind from the Ruby types, whether it is required, its `equal_to` values; the resource's actions beside `nothing` | every other resource the inspector knows, the six shared names under `native/` | the client's version |
| `salt` | `salt-call --local --out=json sys.list_state_functions` for the names, then `sys.state_argspec` and `sys.state_doc` for each state function the documents use, with the target's own program and arguments | the functions the documents use (`file.symlink`, `pkgrepo.managed`): their arguments, those without a default required, described from the docstring; open to keyword arguments | every other state function the minion has | Salt's version |
| `cloudformation` | nothing: it fetches the registry of resource provider schemas CloudFormation publishes for the stack's region (`schema.cloudformation.<region>.amazonaws.com/CloudformationSchema.zip`); `aws cloudformation describe-type` only for a type outside the public registry | the `Vendor::Service::Type` types the documents use: every property, the read-only ones as set by CloudFormation, the create-only ones marked as replacing the resource, the write-only ones as sensitive | every other type of the region's registry | the region, to the zip's SHA-256 |
| `bicep`, `arm` | nothing: it fetches the Azure type definitions Bicep compiles against from [Azure/bicep-types-az](https://github.com/Azure/bicep-types-az), the index and the `types.json` of each provider and API version the documents use | the types the documents use, property by property, at the API version the type (Bicep) or the `apiVersion` param (ARM) names; ARM's `apiVersion` lists every version the index has | every other API version of a used type and the newest stable version of every other type (Bicep); every other type with its API versions (ARM) | the index and each `types.json`, to their SHA-256 |
| `cloud-init` | `cloud-init --version` for the tag, then it fetches the cloud-config schema from the cloud-init repository at that tag (or `main`) | every module beside the six: its keys as params, or the one param `value` for a module whose value is a string or a list | nothing: every module is described | the cloud-init version or branch, and the schema's SHA-256 |
| `docker` | `docker compose version` for the version, then it fetches the Compose Specification's `compose-spec.json` | `service`, `network`, `volume`, `config` and `secret`: every attribute with its kind, open to `x-` extensions | nothing: the five are the whole catalogue | the schema's SHA-256 and the installed Compose's version |
| `helm` | `kubectl get --raw /openapi/v2` through the target's `kubectl` settings, as `kubernetes` | the `<apiVersion>/<Kind>` types the documents use | every other kind the server knows | the server's version |
| `dsc` | `Get-DscResource -Module` in PowerShell for each module the documents use (the module's `[DscResource()]` classes, where that lists nothing, as on Linux), and `dsc resource list` and `dsc resource schema` when DSC v3 is installed | every resource of each PowerShell module, from its property types, mandatory flags and enum values; the DSC v3 resources the documents use, from their manifests' JSON Schema | every other DSC v3 resource | PowerShell's version, each module's version, each DSC v3 resource's version |

`packer` and `podman` have no exporter; their plugins' own tables are used. See
[Writing a plugin](../contributing/writing-a-plugin.md#a-schema-source).

### Sources that read a published file

CloudFormation, Bicep, ARM, cloud-init and Docker fetch a published file
([decision 0036](../decisions/0036-a-published-schema-file-is-fetched-over-https-pinned-by-hash-and-reused-offline.md)):
HTTPS only, pinned by SHA-256, refetched by `ETag`, last copy used offline (a first sync
offline fails).

Equivalent spellings match, such as Pulumi's `random:RandomPet` and
`random:index:RandomPet`.

## In CI

Check the cache against the committed lock:

```yaml
- run: meridian schema sync --check .
```

It runs no tool. To refresh, run `meridian schema sync .` where the tools are and commit the
lock; with [bundles](schema-bundles.md#in-ci), CI runs `meridian schema sync --locked .` with
no tools. Exporters are tested with real tools; see
[Real-tool tests](../contributing/real-tool-tests.md).

## Limits

- Unparseable documents are skipped; with `--data`, resources whose `when` is false are
  left out.
- It describes the locally installed tool versions; [bundles](schema-bundles.md) give every
  machine the same schemas.
- Meridian's own types (`module`, `variable`, `provider`, `cluster`, `stack`) and
  `spec.settings` keys come from the plugin.
