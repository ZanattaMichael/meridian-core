# The global schema repository: a template

This folder is the starting point of `meridian-schemas`, the repository that
builds and publishes Meridian's [schema bundles](../../docs/tooling/schema-bundles.md)
for the common upstreams every week. It lives here, beside the code it
drives, until the owner creates that repository; nothing in this folder runs
in `meridian-core` itself.

| File | What it is |
|---|---|
| `catalogue.yaml` | every bundle to build, each at exactly one upstream version, with the seed document that makes `meridian schema sync` read it and the type prefixes that belong in it |
| `seeds/` | the seed documents: a `provider`, a package's explicit provider, a `cluster`, a `stack`, one module of a collection; `@VERSION@`, `@NAME@`, `@SOURCE@` and `@LOCAL@` are replaced before the sync |
| `tools/catalogue.py` | the workflow's helper: the build matrix, the seed for one bundle, one field of one bundle, the weekly look-up of newer versions, and the Markdown diff of two bundles |
| `workflows/publish.yml` | the workflow, to go in `.github/workflows/` of the new repository |

## How it runs

1. **bump** (Mondays, 04:17 UTC) looks up each upstream's newest release: the
   Terraform registry for providers, GitHub releases for Pulumi packages, PyPI
   for `ansible-core`, Ansible Galaxy for collections, the Kubernetes release
   channel, and today's date for CloudFormation and the Azure types, which have
   no version. It opens one pull request, `bump/weekly`, that moves
   `catalogue.yaml`, listing each move.
2. **build** runs on that pull request, one job per bundle, with read-only
   permissions and no secrets: it builds `meridian` and the target's plugin
   from `meridian-core` at the ref the catalogue names, installs the tool at
   the pinned version, runs `meridian schema sync --all-types --prefer tool`
   on the seed and `meridian schema pack` with the bundle's prefixes, and
   writes into the run summary the types and params added and removed
   against the bundle published before it. The reviewer of the pull request
   reads that summary.
3. Once the pull request is merged, **build** runs again on `main`, and
   **publish** waits for the `schemas` environment's reviewer. Approved, it
   signs each bundle the build made with `meridian schema sign`, pushes it
   with `meridian schema push` (which also updates
   `ghcr.io/zanattamichael/meridian-schemas/index:latest`), and attests the
   bundles' provenance with `actions/attest-build-provenance`, tying each to
   the run and the catalogue commit.

A run started by hand from the Actions tab (with `only` naming
`target/name` pairs, or empty for all) builds and publishes the same way,
for an upstream that cannot wait a week, such as a provider's security fix.

## Setting it up

These are the owner's to do; none can be done from `meridian-core`.

1. Create the repository (proposed: `ZanattaMichael/meridian-schemas`, or the
   same name under a GitHub organisation, which makes the environment's
   reviewer rule cleaner) and copy this folder's contents into it, moving
   `workflows/publish.yml` to `.github/workflows/publish.yml`.
2. Make the signing key on a trusted machine:
   `meridian signing keygen --secret schemas.key --public schemas.pub`.
   Publish `schemas.pub` in the new repository's README, so projects can
   trust it with `--trusted-key`.
3. Create the environment `schemas` in the repository's settings, with the
   owner as required reviewer and deployments allowed from `main` only, and
   give it the secret `MERIDIAN_SCHEMAS_KEY` holding the text of
   `schemas.key`. Delete the local copy, or keep it offline.
4. Allow GitHub Actions to create pull requests (Settings, Actions, General,
   "Allow GitHub Actions to create and approve pull requests") so the bump
   job can open its pull request.
5. After the first publish, make the `meridian-schemas` packages public in
   the owner's GitHub packages settings, so projects can read them without a
   login.
6. Once a release of `meridian-core` ships `meridian schema pack`, set
   `meridian.ref` in `catalogue.yaml` to that release's tag.

## Adding a bundle

Add an entry to `catalogue.yaml` with a seed that makes the target's tool
list the upstream's types, the prefixes that belong to it, and an `upstream`
of one of the kinds `tools/catalogue.py bump` knows (`terraform-registry`,
`github-release`, `pypi`, `galaxy`, `kind-node`, `date`). Open a pull request;
the build job on it shows what the bundle holds before anything is
published. A team publishing private bundles can use the same folder against
its own registry by changing `registry` in the catalogue.

## What is not decided

The repository's name and home, who holds the signing key, and whether the
first catalogue is the one here (Terraform AWS, AzureRM and Google; Pulumi
AWS, Azure Native and GCP; Kubernetes; CloudFormation `us-east-1`; the Azure
types; `ansible.builtin` and `community.general`) are open questions for the
owner, listed in issue 165 and in
[decision 0041](../../docs/decisions/0041-tool-schemas-travel-as-signed-oci-bundles-chosen-by-the-types-the-documents-use.md).
The versions in the catalogue are a starting point; the first bump moves
them to the newest.
