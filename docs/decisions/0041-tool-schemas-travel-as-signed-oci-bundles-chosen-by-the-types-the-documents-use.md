# Tool schemas travel as signed OCI bundles, chosen by the types the documents use and pinned by digest

| | |
|---|---|
| Area | Tooling |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Signed schema bundles: `meridian schema pack`, `sign`, `push`, `fetch` and `sync` from a registry" (issue 165) |
| Where it shows | [Schema bundles](../tooling/schema-bundles.md), [Tool schemas](../tooling/schema-sync.md), [Modules](../reference/modules.md#schema-registries) (`spec.schemas`), the [command line reference](../reference/cli.md#schema-pack-schema-sign-schema-push-schema-fetch), `packaging/schema-registry/` |

## Context

`meridian schema sync` (PR 149, decision 0036) gets a target's schemas by
asking its installed tool, or by fetching a file the vendor publishes. Each
machine does that for itself: it needs the tool, often the network and a
provider download, sometimes a cluster, and it may take minutes. Issue 165
asked for a way to publish what one sync produced so that every other
machine gets it without the tool, and the owner asked (2026-11-18) for a
hosted global repository of them, refreshed weekly by GitHub Actions, with
an answer to "how do we manage dependency injection".

Three questions had to be settled: what a published set of schemas is and
where it lives; how a project says which ones it gets; and what stops a bad
one.

## The choice

**A schema bundle is an OCI artifact of its own kind.** One target's
schemas at one set of pins, as a gzipped tar of `bundle.json` (the header
and an index of one line per type) and one file per type, in the existing
`ResourceSchema` model. Its manifest has the artifact type
`application/vnd.meridian.schema-bundle.v1` and its one layer
`application/vnd.meridian.schema-bundle.layer.v1.tar+gzip`, and the client
reads a manifest only as the kind it asked for, so a module is never taken
for a bundle or the other way round. It lives at
`<registry>/<target>/<name>:<version>` (`terraform/hashicorp-aws:5.60.0`),
or in a directory laid out the same way for a mirror or an air-gapped site.
The OCI client, the logins (`spec.credentials`) and the publish-once rule
are the ones modules already have.

**A registry has one index.** `<registry>/index:latest` (or `index.json` in a
directory) lists every bundle with its target, name, version, pins, digest
and the names of its types. It is the one thing in a registry that is
replaced in place, because it is a pointer to other artifacts; every bundle
version is published once. This revisits decision 0011 (no central module
index) for schemas only: modules still name their source.

**Which bundles a project gets follows from the types its documents use.**
`schema sync` takes the types each target's documents use, less the ones
the plugin itself describes (`provider`, `package`), and for each type no
chosen bundle describes yet takes the newest bundle the first registry's
index lists for it. `meridian.lock` records each bundle by reference and
digest beside the pins, and from then on a sync keeps those bundles while
they still describe what the documents use; `sync --locked` uses exactly
them or fails, and `schema update` chooses again. A target the bundles do
not cover is asked of the tool, as before, and the sync says why.

**The cache a bundle gives is the cache a sync writes, made reproducible.**
The types the documents use are described in full and the rest listed by
name, as the tool path does, so the editor's reading is unchanged and stays
small however large the bundle is: the bundle itself is what is read type
by type. A cache made from bundles carries no time and records the bundles
it came from, so it is the same bytes on every machine and the lock's
`h1:` hash means the same everywhere.

**Trust is the signing modules use, under a statement of its own.** `pack
--key` or `schema sign` writes `meridian.sig` in the bundle, an Ed25519
signature from `meridian-signing` over
`meridian-schema-bundle-statement-v1`, the target, name, version and the
bundle's content hash; it cannot pass for a module's or a plugin's
signature, nor one bundle's for another's. `spec.schemas.signatures` or
`--schema-signatures` is `off`, `warn` or `require`, default `off` as for
modules; the keys are the global `--trusted-key` ones. The lock records the
digest, not the signer (decision 0014). The reader refuses a header or type
file outside the model (unknown fields, a type file that names another type,
nesting past 32 levels, a type over 8 MiB), and nothing in a bundle is ever
run.

**The global repository is a template here, not a service run from here.**
`packaging/schema-registry/` holds the catalogue (exact upstream versions),
the seed documents, the helper script and the three-job workflow (bump in a
pull request; build with no secrets; sign and push in a reviewer-gated
environment, with a provenance attestation) for a separate
`meridian-schemas` repository. Creating that repository, its `schemas`
environment and its signing key is the owner's to do.

## Alternatives

- **A bespoke HTTPS endpoint.** Rejected in issue 165: a second
  distribution protocol with its own authentication, mirroring and signing,
  for nothing a registry lacks.
- **Pins derived per target from the documents** (`hashicorp/aws ~> 5.60`
  resolved against the index, as `terraform init` does). Deferred: each
  target would need a new plugin call to say what its documents pin, and
  choosing by the types used already finds the right bundle for the common
  case of one version per provider. The lock pins the choice either way, and
  a project that needs another version runs `schema fetch` for it.
- **Signing the index.** Not done: the index only points at digests, every
  bundle is verified by its own signature, and the worst an altered index
  can do is offer an older signed bundle, which the lock then shows in a
  diff. Schemas are advice, so the cost is misleading completion, never a
  changed deployment.
- **One cache file holding the whole bundle.** Rejected for the editor's
  sake: an AWS catalogue in full is tens of megabytes read at every start.
- **Default to requiring signatures.** Not done, to match modules; a project
  that names the global registry is shown `signatures: require` in the
  documentation's first example.

## Consequences

- A fresh clone runs `meridian schema sync --locked .` and gets the cache
  the committer had, with no tool installed and nothing asked of a vendor.
- A bad or tampered bundle is refused before anything is written, when
  signatures are required and the publisher's key is trusted.
- A bundle describes what its seed made the tool list. A private provider, a
  cluster's custom resources or a collection outside the catalogue is not
  in it, and the sync goes to the tool for that target; a team can publish
  its own bundles to a second registry (`spec.schemas.registries`) with the
  same `pack` and `push`.
- Revisit when: a target's plugin can say which versions its documents pin,
  or a vendor publishes its own signed schema artifact (Bicep's planned
  `mcr.microsoft.com/bicep/providers/az`), which an exporter can then read
  directly.
