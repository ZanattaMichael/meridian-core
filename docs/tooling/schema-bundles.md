# Schema bundles: tool schemas without the tool

```
meridian schema sync   <paths>... [--prefer bundles|tool] [--locked] [--schema-signatures POLICY]
meridian schema update <paths>... [--target TARGET]
meridian schema fetch  <oci://REGISTRY/TARGET/NAME:VERSION | FILE> [PATHS]... [--no-lock] [--credential NAME] [--insecure]
meridian schema pack   <target> --name NAME --version VERSION [--prefix TEXT]... [--key FILE] [--out FILE]
meridian schema sign   <file> --key FILE [--out FILE]
meridian schema push   <file> <REGISTRY> [--credential NAME] [--insecure] [--no-index]
```

[`meridian schema sync`](schema-sync.md) gives the editor a tool's own types
(`aws_instance`, `apps/v1/Deployment`, `community.general.ini_file`) by asking
the tool, which means the tool has to be installed, often with the network,
a provider download or a cluster, on every machine that wants completion. A
**schema bundle** is what one such sync produced, packed, signed and
published to an OCI registry, so that every other machine gets the same
schemas in seconds, from a registry it already uses, with no tool at all.

The short version, for a project that takes its schemas from a registry:

```yaml
# meridian.yaml
apiVersion: meridian/v1
kind: Module
metadata:
  name: estate
spec:
  schemas:
    registry: ghcr.io/zanattamichael/meridian-schemas
    signatures: require
```

```console
$ meridian schema sync --trusted-key meridian-schemas.pub .
terraform: cached 1503 resource types from the schema bundle terraform/hashicorp-aws:5.60.0 in .meridian/schemas/terraform.json
kubernetes: cached 612 resource types from the schema bundle kubernetes/core:1.31.0 in .meridian/schemas/kubernetes.json
ansible: no schema bundle describes acme.internal.deploy; asking the tool
ansible: asking the tool about 3 resources in 1 document
ansible: cached 2 resource types from ansible-core 2.17.6; ansible.builtin 2.17.6 in .meridian/schemas/ansible.json
recorded in meridian.lock
```

Then commit `meridian.lock`, and every other machine and CI run does:

```console
$ meridian schema sync --locked .
```

and gets exactly the same cache, byte for byte, without a single tool.

The registry named above is the proposed home of the global repository
(see [Publishing the global repository](#publishing-the-global-repository));
it is not running yet. Any registry, or a directory, serves the same way.

## What a bundle is

A bundle holds one target's schemas at one set of pins, such as Terraform's
`hashicorp/aws` provider at `5.60.0` or a Kubernetes API server at `1.31.0`.
It is named `<target>/<name>:<version>`:

| Example | What it holds |
|---|---|
| `terraform/hashicorp-aws:5.60.0` | every resource, data source and ephemeral type of one provider at one version |
| `pulumi/aws:6.45.0` | every resource and function of one Pulumi package |
| `kubernetes/core:1.31.0` | every kind a Kubernetes 1.31 API server serves |
| `ansible/community.general:9.5.0` | every module of one collection |
| `cloudformation/us-east-1:2026-11-16` | the region's registry of resource types, as CloudFormation published it that day |
| `bicep/az:2026-11-16` | the Azure type definitions, as `bicep-types-az` stood that day |

Where the vendor has no version, the version is the date it was read on.

Inside, it is a small directory, packed as a gzipped tar:

```
bundle.json           the header (target, name, version, tool, pins) and an
                      index: one line per type, with its description
types/<hash>.json     one type's full schema each, in the same model as the
                      plugin's own types and the cache
meridian.sig          the publisher's signature, when signed
```

A reader opens `bundle.json` and reads a type's file only when it needs that
type. That is why a whole provider can be in one bundle while the editor
still reads a small cache: `sync` writes to the cache only the types the
documents use in full, and the rest of the bundle's types by name, exactly
as a sync from the tool does (see [Described in full, or by
name](schema-sync.md#described-in-full-or-by-name)).

In a registry, a bundle is an OCI artifact of its own kind (artifact type
`application/vnd.meridian.schema-bundle.v1`, one layer of type
`application/vnd.meridian.schema-bundle.layer.v1.tar+gzip`) at
`<registry>/<target>/<name>:<version>`. Meridian never reads a module as a
bundle or a bundle as a module. Each registry also has an **index**,
`<registry>/index:latest` (artifact type
`application/vnd.meridian.schema-index.v1`), one JSON file listing every
bundle with its pins, its digest and the names of its types, which is how
a sync finds the bundle that describes a type.

A directory serves the same way, for a mirror, a file share or a machine
with no network at all:

```
schemas-mirror/
  index.json
  terraform/hashicorp-aws/5.60.0.tar.gz
  kubernetes/core/1.31.0.tar.gz
```

## Naming the registries: `spec.schemas`

A project names where its bundles come from in `meridian.yaml`:

```yaml
spec:
  schemas:
    registry: ghcr.io/zanattamichael/meridian-schemas   # looked in first
    signatures: require                                 # off | warn | require
    registries:                                         # then each of these, in order
      - registry: registry.example.com/platform/schemas
        credential: internal                            # from spec.credentials
      - registry: file://vendor/schemas                 # relative to the project
```

| Field | Meaning |
|---|---|
| `registry` | an OCI repository prefix with no scheme and no tag (`oci://` may be written), or `file://DIR` |
| `credential` | a credential from [`spec.credentials`](../reference/modules.md#registry-logins) to log in with; without one, the login `docker login` stored for the host is used, if any |
| `insecure` | `true` talks plain HTTP, for a registry on a trusted network only |
| `signatures` | `off` (do not look), `warn` (use an unsigned or wrongly signed bundle and say so) or `require` (use only bundles signed by a trusted key). The command line's `--schema-signatures` wins over it; without either it is `off`, as for modules |
| `registries` | more registries, each with its own `registry`, `credential` and `insecure`, looked in after `registry` |

A project with no `spec.schemas` is synced from the tools, as before.

## How a sync chooses bundles

For each target, `meridian schema sync`:

1. takes the types its documents use, less the ones the plugin itself
   describes param by param (Meridian's own `provider`, `cluster`, `stack`,
   or `package` on a configuration management target);
2. keeps the bundles `meridian.lock` already pins for the target, fetched by
   their digest, as long as they still describe something the documents use;
3. for each type none of those describes, reads the registries' indexes in
   order and takes the **newest** bundle of the first registry that lists
   the type (versions compare as versions, `5.10.0` after `5.9.0`, and dates
   as dates);
4. checks each bundle's signature by the policy, writes the cache from them,
   and records each in `meridian.lock` by its reference and digest.

When some type is described by no bundle (a private provider, a cluster's
custom resources, a collection the registry does not carry), the whole
target is asked of its tool instead, as it was before bundles, and the sync
says which type sent it there. A target no registry can be read for is
asked of the tool too, with a warning naming the registry.

Two different bundles can serve one target: a project that uses AWS and
Google gets `terraform/hashicorp-aws` and `terraform/hashicorp-google`, and
the cache holds both.

`--prefer tool` (or `MERIDIAN_SCHEMA_PREFER=tool`) skips bundles and asks
the tools, whatever `spec.schemas` says.

## The lock file

A cache made from bundles is recorded with the bundles it came from:

```yaml
schemas:
  - target: terraform
    tool: terraform
    toolVersion: "1.13.1"
    pins:
      registry.terraform.io/hashicorp/aws: "5.60.0"
    integrity: h1:5c0e...
    bundles:
      - reference: oci+ghcr.io/zanattamichael/meridian-schemas/terraform/hashicorp-aws:5.60.0
        digest: sha256:9a1f...
```

| Field | Meaning |
|---|---|
| `bundles[].reference` | where the bundle came from: `oci+<registry>/<target>/<name>:<version>`, or `file+<path>` for a packed file |
| `bundles[].digest` | the OCI manifest's digest (for a file, the archive's), which is what a later fetch asks for |
| `integrity` | the hash of the cache file, as for a sync from the tool |

A cache made from bundles holds no time of its own, so the same bundles and
the same documents give the same bytes on every machine, and the
`integrity` means the same everywhere: `meridian schema sync --check` passes
on a colleague's clone after their `--locked` sync.

| Command | Does |
|---|---|
| `meridian schema sync .` | keeps the pinned bundles while they still describe the documents' types, and adds bundles for types they do not |
| `meridian schema sync --locked .` | uses exactly the bundles the lock pins, by digest, or fails; for a target synced from its tool, holds the tool to the version and pins the lock records. Nothing new is chosen |
| `meridian schema update .` | chooses again, from the indexes, the newest bundles that describe the documents' types, and moves the lock to them |
| `meridian schema sync --check .` | changes nothing and fetches nothing; fails if a cache is missing or is not what the lock records |

Fetched bundles are kept unpacked by digest under
`.meridian/schemas/bundles/`, so a second sync, or a `--locked` sync after a
`schema clean`, does not download them again.

## Signatures

A publisher signs a bundle with the same keys and the same tool as plugins
and modules:

```console
$ meridian signing keygen --secret schemas.key --public schemas.pub
$ meridian schema pack terraform --name hashicorp-aws --version 5.60.0 --key schemas.key
```

or signs a bundle that is already packed, as the global repository's
publishing job does once a reviewer approves it:

```console
$ meridian schema sign terraform-hashicorp-aws-5.60.0.tar.gz --key schemas.key
signed terraform/hashicorp-aws:5.60.0 with key 3f2a9c1e5b3d8f6a into terraform-hashicorp-aws-5.60.0.tar.gz
  digest:  sha256:9a1f...
```

The signature is over the bundle's target, name, version and the hash of
its contents, under a statement of its own (`meridian-schema-bundle-statement-v1`),
so a module's or a plugin's signature never passes for a bundle's, and a
bundle's never for another bundle's. A consumer trusts the publisher's
public key with the global `--trusted-key FILE` (or `MERIDIAN_TRUSTED_KEYS`),
and says how strict to be with `spec.schemas.signatures` or
`--schema-signatures`:

| Policy | An unsigned bundle | A bundle whose signature does not pass |
|---|---|---|
| `off` | used | used |
| `warn` | used, with a warning | used, with a warning |
| `require` | refused, and nothing is written | refused, and nothing is written |

`meridian.lock` records each bundle's digest, not who signed it
([decision 0014](../decisions/0014-lock-file-does-not-record-the-signer.md)).

## What a bundle may hold

A bundle comes from somewhere else, so it is read as input:

- it is a gzipped tar of plain files only, at most 256 MiB unpacked, with no
  links and no path outside the bundle (the same unpacking modules use);
- `bundle.json` must have exactly the fields this page lists, in format 1; a
  newer format is refused with a message to upgrade `meridian`;
- each index entry must point at the file its type's name hashes to, and the
  file must describe that type and no other;
- a type's file is at most 8 MiB, and its params nest at most 32 levels;
- nothing in a bundle is ever run.

Schemas are advice. A bad bundle can mislead completion and `lint`; it never
changes what `compile` writes or what a tool applies.

## Making and publishing a bundle

Any team can publish bundles, for its private providers, its own Ansible
collections or a cluster with its custom resources:

```console
$ meridian schema sync --all-types --prefer tool seed.yaml
terraform: asking the tool about 1 resource in 1 document
terraform: asking the tool again to describe all 1504 types in full
terraform: cached 1504 resource types from terraform 1.13.1; registry.terraform.io/acme/internal 2.1.0 in .meridian/schemas/terraform.json
$ meridian schema pack terraform --name acme-internal --version 2.1.0 \
    --prefix internal_ --prefix data.internal_ --key schemas.key
packed terraform/acme-internal:2.1.0 (212 resource types from terraform 1.13.1; registry.terraform.io/acme/internal 2.1.0) into .meridian/schemas/packed/terraform-acme-internal-2.1.0.tar.gz
  digest:  sha256:c4e1...
  signed:  key 3f2a9c1e5b3d8f6a
$ meridian schema push .meridian/schemas/packed/terraform-acme-internal-2.1.0.tar.gz \
    registry.example.com/platform/schemas --credential internal
published terraform/acme-internal:2.1.0 (212 resource types) to registry.example.com/platform/schemas
  reference: oci+registry.example.com/platform/schemas/terraform/acme-internal:2.1.0
  digest:    sha256:4b77...
  listed in the registry's index
```

| Step | Notes |
|---|---|
| `sync --all-types` | asks the tool twice: once for the documents' types, which lists every other type of the providers, packages or cluster in use by name, then again for all of them in full. A seed document that only names the provider (or package, or cluster) is enough. The cache can be large |
| `pack` | writes the target's cache as a bundle. `--name` and `--version` name it; `--prefix` keeps only the types whose names start with the text (one provider's, one collection's); `--key` signs it; `--out` says where (default `.meridian/schemas/packed/<target>-<name>-<version>.tar.gz`). It says how many types are only named, since a bundle is most use with each described |
| `sign` | signs a packed bundle, for a pipeline whose signing step is apart from its build |
| `push` | publishes to `<registry>/<target>/<name>:<version>` and lists the bundle in the registry's index. A version is published once: the same content again changes nothing, other content under the same version is refused. `--credential NAME` logs in with a credential from this directory's `meridian.yaml`; `--insecure` talks plain HTTP; `--no-index` leaves the index alone. A `file://DIR` registry is written the same way |

`meridian schema fetch` takes one bundle by reference, for a project that
wants a particular one, or an air-gapped site that copies the archive in:

```console
$ meridian schema fetch oci://ghcr.io/zanattamichael/meridian-schemas/pulumi/aws:6.45.0 .
$ meridian schema fetch ./terraform-hashicorp-aws-5.60.0.tar.gz
```

With documents, it describes their types in full and the bundle's other
types by name; without, every type in full. It replaces that target's cache,
records the bundle in the lock unless `--no-lock`, and checks the signature
by `--schema-signatures` or `spec.schemas.signatures`. A registry that
`spec.schemas` names is reached with its own login and `insecure` setting;
for any other, `--credential NAME` and `--insecure` work as they do for
`push`.

## Publishing the global repository

Issue 165 proposes a global repository of bundles for the common upstreams,
refreshed every week. Its ready-made template is
[`packaging/schema-registry/`](https://github.com/ZanattaMichael/meridian-core/tree/main/packaging/schema-registry)
in this repository, to be copied into a repository of its own
(`meridian-schemas`), so the weekly job, its history and its secrets stay
apart from the core:

<p align="center"><img src="../graphics/meridian-schema-bundles.svg" alt="Schema bundles: a catalogue is built into signed bundles and pushed with an index to an OCI registry; a project's schema sync picks the newest bundle for each type its documents use, checks the signature, writes the cache and pins each bundle in meridian.lock, and asks the tool only for what no bundle describes" width="100%"></p>

- **`catalogue.yaml`** lists each bundle at exactly one upstream version:
  Terraform's AWS, AzureRM and Google providers, Pulumi's AWS, Azure Native
  and GCP packages, Kubernetes, CloudFormation's `us-east-1` registry, the
  Azure types Bicep reads, `ansible.builtin` and `community.general`.
- **bump**, every Monday, looks up each upstream's newest version (the
  Terraform registry, GitHub releases, PyPI, Ansible Galaxy, the Kubernetes
  release channel) and opens one pull request that moves the catalogue.
  Nothing is built from what it found until a person merges it.
- **build** runs on that pull request and on the merge, one job per bundle,
  with no secrets: it installs the tool at the pinned version, runs `sync
  --all-types` on the bundle's seed document and `pack`, and writes into the
  run's summary which types and params each bundle adds or removes against
  the bundle published before it.
- **publish** runs only after a merge, in a GitHub environment (`schemas`)
  whose required reviewer approves each run. It alone holds the signing key
  and a registry token. It signs each bundle with `schema sign`, pushes it
  with `schema push`, which updates the index, and attests the bundles'
  provenance with GitHub's OIDC identity, so a bundle can be traced back to
  the workflow run and the catalogue commit that made it.

What is the owner's to decide before it runs: the repository's name and
owner (a GitHub organisation makes the environment's reviewer rule cleaner),
who holds the signing key (proposed: an environment secret with the owner as
required reviewer, and the public key published in the repository's README),
and whether the first catalogue is the one above. The template's README
lists the steps.

## In CI

```yaml
- run: meridian schema sync --locked --trusted-key keys/meridian-schemas.pub .
- run: meridian lint .
```

`--locked` fetches exactly what the lock pins and never chooses anything
new, so a pull request cannot change which schemas `lint` checks against
without changing `meridian.lock` in its diff.

## Limits

- Bundles are chosen by the types the documents use, newest first. A
  document's own version constraint (`version: "~> 5.60"` on a `provider`)
  is not read when choosing; the lock pins whatever was chosen, and
  `schema fetch` takes a particular version. Reading each target's pins
  from its documents is a follow-up.
- A bundle describes what its seed made the tool list. A cluster's custom
  resources, a private provider or a collection the catalogue does not carry
  is not in a public bundle; the sync goes to the tool for that target, or a
  team publishes its own bundle to a second registry.
- The index is not signed; each bundle is, and the lock pins digests. An
  altered index can at most offer an older signed bundle, which shows as a
  change in `meridian.lock`.
- A vendor's own published schema artifact (such as Bicep's planned
  `mcr.microsoft.com/bicep/providers/az`) is not read yet; once one exists,
  that target's exporter can read it directly.

See also: [Tool schemas](schema-sync.md), [Modules](../reference/modules.md)
for the OCI client and logins bundles share,
[decision 0041](../decisions/0041-tool-schemas-travel-as-signed-oci-bundles-chosen-by-the-types-the-documents-use.md).
