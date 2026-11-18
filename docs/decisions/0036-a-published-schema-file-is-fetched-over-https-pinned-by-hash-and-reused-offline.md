# A tool's published schema file is fetched over HTTPS, pinned by its hash, and the last copy stands in when the publisher cannot be reached

| | |
|---|---|
| Area | Tooling |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Schema exporters for CloudFormation, Bicep, ARM, cloud-init, Compose and Helm, reading what each vendor publishes" (issue 164); `meridian_sdk::fetch` |
| Where it shows | [Tool schemas](../tooling/schema-sync.md#what-each-target-exports), the "Editor schemas" section of [CloudFormation](../targets/cloudformation.md#editor-schemas), [Bicep](../targets/bicep.md#editor-schemas), [ARM](../targets/arm.md#editor-schemas), [cloud-init](../targets/cloud-init.md#editor-schemas) and [Docker](../targets/docker.md#editor-schemas), [Writing a plugin](../contributing/writing-a-plugin.md#a-schema-source) |

## Context

`meridian schema sync` (PR 149) asks each target's plugin for the schemas of
its tool's own resource types and caches them for the editor tools. The
first exporters ran a tool: `terraform providers schema`, `kubectl get
--raw`, `pulumi package get-schema`, `ansible-doc`. Several tools do not
describe their types through a command at all, but publish the description
as a file: CloudFormation's registry of resource provider schemas is a zip
per region, the Azure type definitions Bicep compiles against are files in
a repository, cloud-init and the Compose Specification ship a JSON Schema.
Issue 164 asked for exporters that read those files, and left one question
open: a file source downloads from a vendor during a sync, and whether a
sync should be able to refuse to.

## The choice

A schema source that reads a published file does so through one helper,
`meridian_sdk::fetch`, which keeps three rules for all of them:

- **HTTPS only**, and a redirect may not leave HTTPS. A source that would
  read a plain `http://` address is refused before anything is fetched.
- **The file is pinned by its SHA-256**, recorded in `meridian.lock` as the
  version of what the cache was read from (`sha256:` and twelve hex digits),
  the way a provider's version is for Terraform. A file that changed under
  the same URL makes a cache that differs from the lock, and the sync says
  so, instead of silently describing other types.
- **The last copy stands in when the publisher cannot be reached.** The
  file is kept in the source's work directory with the `ETag` the server
  sent; the next sync asks whether it changed (`If-None-Match`) and
  downloads nothing when it has not. When the server does not answer at
  all, the copy from the last sync is used and the export carries a note
  saying so. The first sync on a machine with no copy fails for that
  target, as a tool that cannot be run does.

There is no `--offline` flag. A sync is run by a person who asked for one,
the tool-run sources already use the network (`terraform init`, `pulumi
package get-schema`), and the offline answer is the signed schema bundle of
issue 165, which needs no vendor at all. The fallback to the last copy
covers the case the flag would have: a machine that is offline for the
moment keeps the cache it had.

What a file source does not do: it does not verify a vendor's signature,
because none of the four publishers signs the file; the pin and TLS are
what hold it. It does not read a tool's private types from a file: a
CloudFormation type outside the public registry is asked of `aws
cloudformation describe-type`, which needs credentials, and is a note when
that fails.

## Alternatives

- **Run the tool anyway.** `aws cloudformation describe-type` per type, `az
  bicep` with a probe file, `docker compose config` with a probe. Each needs
  the tool installed and, for the first two, an account, to describe a
  type the vendor already publishes for anyone. Rejected: the published file
  is the same data without the account.
- **Fail when the network is away, with `--offline` to say so.** Rejected
  for now; the last copy standing in gives the same cache with one fewer
  flag, and issue 165's bundles are the real offline mode.
- **Pin to a release or tag instead of a hash.** The CloudFormation zip and
  the Azure index have no version; GitHub's `main` moves. The hash is the
  only pin that means the same thing for all four, and cloud-init's tag is
  recorded beside it where there is one.

## Status

Proposed. Accepting it means `meridian schema sync` may fetch from the
vendors named on each target's page, and the lock records a hash rather
than a version for those targets. Changing it would mean an `--offline`
flag, or a signature check once a vendor signs its file.
