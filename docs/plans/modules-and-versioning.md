# Modules and Versioning

> Status: **proposed**. Extends [release and versioning](release-and-versioning.md); where
> the two differ, that plan wins until this one is accepted.
> Amend by pull request against this file.

This note designs two things that belong together because each needs the other to be
safe:

1. **Modules**: a way to package Meridian configuration, give it a version, fetch it from
   somewhere else, and know that what was fetched is what was published.
2. **Versioning that is enforced, not only promised**: what a build does with a
   document's `apiVersion`, and how a host and a plugin agree on a protocol version
   before either says anything else.

A module is only shareable if a consumer can tell which language version and which core
it was written for. That is why the two ship as one design.

---

## 1. Modules

### 1.1 What a module is

**Decision: a module is a directory with a `meridian.yaml` manifest at its root. Nothing
else makes a directory a module, and a module has no build step.**

```yaml
apiVersion: meridian/v1
kind: Module
metadata:
  name: acme/nginx
  version: 1.4.2
  description: An nginx reverse proxy, as a ResourceSet and its default data
spec:
  meridian: ">=0.1.0"
  modules:
    - name: acme/base
      version: "^2.1"
      git: https://git.example.com/acme/meridian-base.git
```

| Field | Required | Meaning |
|---|---|---|
| `metadata.name` | yes | One to three segments of `a-z`, `0-9` and `-`, joined by `/`. The name a consumer refers to the module by |
| `metadata.version` | to be shared | The module's own [SemVer](https://semver.org). A project that only consumes modules may leave it out |
| `metadata.description` | no | One line, shown by `meridian module list` |
| `spec.meridian` | no | The core releases the module works with, as a SemVer requirement. A build outside it refuses the module by name instead of failing somewhere inside it |
| `spec.modules` | no | The modules this one needs. See [§1.3](#13-sources) |

The same file serves the project that consumes modules and the module that is consumed:
a project is a module nobody depends on. One file format means one parser, one set of
rules, and a project that can later be published without being rewritten.

The manifest is a document of the language, so it carries `apiVersion` and is covered by
the same support window as every other document ([§2](#2-the-language-version-apiversion)).

### 1.2 What a module holds

**Decision: content is found by directory, not listed in the manifest.**

| Directory | Holds | Status |
|---|---|---|
| `documents/` | `Infrastructure` and `ResourceSet` documents | implemented with modules |
| `data/` | Data files, as given to `--data` and to hierarchy layers | implemented with modules |
| `composites/` | Reusable composed units | reserved for the language design |
| `functions/` | Functions | reserved for the language design |
| `README.md` | What the module is for | recommended |

Composites, functions and layered data are being designed in the language thread. The
module system does not interpret them: it guarantees only that every file of a pinned
module is present, unmodified, at a known path, and that a name of the form
`<module>:<path>` resolves to exactly one file. Whatever the language decides a composite
is, it is found the way a document is found. That keeps the two designs independent: the
language can change what a composite looks like without the lockfile changing shape.

A document or a data file in a module is named on the command line as
`@<module>:<path>`:

```
meridian compile site.yaml @acme/nginx:documents/proxy.yaml --data @acme/nginx:data/defaults.yaml
```

The leading `@` cannot be confused with a path, and the module must be one the project's
manifest requires, directly or through another module, and that the lockfile pins.

### 1.3 Sources

**Decision: three sources, and a requirement names exactly one.**

```yaml
spec:
  modules:
    - name: acme/base
      path: ../modules/base                       # a directory on disk

    - name: acme/nginx
      version: "^1.4"
      git: https://git.example.com/acme/meridian-nginx.git
      subdir: modules/nginx                       # optional

    - name: acme/pinned
      git: https://git.example.com/acme/meridian-pinned.git
      ref: 4f2a9c1                                # a commit, branch or tag instead of a version

    - name: acme/postgres
      version: "~3.2"
      oci: registry.example.com/acme/meridian-postgres
```

| Source | Versions come from | Pinned in the lockfile by |
|---|---|---|
| `path` | The manifest found there | Nothing: a path is a working copy, and it is read where it is |
| `git` | Tags named `v1.2.3` or `1.2.3`; or the one `ref` | The commit and the content hash |
| `oci` | The repository's tags that are SemVer versions | The manifest digest and the content hash |

- **`path`** is for a monorepo and for developing a module beside its consumer. It is
  never copied and never hashed, exactly because the point of it is that an edit takes
  effect at once. A module fetched from `git` or `oci` may not require a `path` module
  at all: that path would point at the disk of whoever fetched it, and would mean
  something different on every machine.
- **`git`** uses the `git` program already on the machine, so every transport and
  credential that works for `git clone` works here, and Meridian stores no git
  credentials.
- **`oci`** stores a module as an OCI artifact: one manifest whose artifact type is
  `application/vnd.meridian.module.v1` with one layer of media type
  `application/vnd.meridian.module.layer.v1.tar+gzip`, a gzipped tar of the module
  directory. Any registry that implements the OCI distribution specification holds it,
  which means an organisation's existing registry, with its existing access control, is
  already a Meridian module registry. **Meridian does not run, and this design does not
  need, a hosted registry service.** Credentials are read from the Docker configuration
  file (`~/.docker/config.json`, or `$DOCKER_CONFIG/config.json`), `auths` entries only.

There is no central index and no short names that resolve through one. A requirement
says where the module comes from. A central index is a service someone must run, secure
and keep available, and a name that silently resolves through it is a supply-chain
decision made on the user's behalf.

### 1.4 Version resolution

**Decision: one version of each module per project, the highest that satisfies every
requirement on it, and a conflict is an error and not a compromise.**

Requirements use Cargo's SemVer syntax: `1.4` and `^1.4` mean `>=1.4.0, <2.0.0`; `~1.4`
means `>=1.4.0, <1.5.0`; `=1.4.2` is exact; comparisons can be joined with commas. A
pre-release is chosen only when a requirement names one.

Resolution walks the requirement graph from the project's manifest. For each module name
it gathers every requirement on it, lists the versions its source offers, and takes the
highest that satisfies all of them. If none does, the error names each requirement and
who made it:

```
error: no version of acme/base satisfies every requirement on it
    ^2.1   required by acme/nginx 1.4.2
    ^1.0   required by the project
    available: 1.0.0, 1.3.0, 2.1.0, 2.2.1
```

Two modules that require the same name from different sources are also a conflict. One
version per name is deliberate: two copies of a module that declares the same resources
would both be applied, and the user would have no way to see why.

A version already in the lockfile is kept for as long as it still satisfies every
requirement, so adding one module never moves another. `meridian module update` is the
only thing that moves a pinned version.

### 1.5 The lockfile

**Decision: `meridian.lock` beside the manifest, written by Meridian, committed by the
user.**

```yaml
# Written by `meridian module`. Commit it; do not edit it.
lockVersion: 1
modules:
  - name: acme/base
    version: 2.2.1
    source: git+https://git.example.com/acme/meridian-base.git
    resolved: 9c1d7be0a4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c7
    integrity: h1:6b1f...e0a2
  - name: acme/nginx
    version: 1.4.2
    source: git+https://git.example.com/acme/meridian-nginx.git//modules/nginx
    resolved: 4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a
    integrity: h1:91c4...77bd
    requires: [acme/base]
```

It is sorted by name and has no timestamps, so the same resolution always writes the same
bytes and a diff of the lockfile shows only what moved. `lockVersion` is a monotonic
integer under the same rules as the run-state schema
([release and versioning §4](release-and-versioning.md#4-run-state-schema-migration)): a
newer build reads an older lockfile, an older build refuses a newer one.

### 1.6 Integrity

**Decision: every fetched module is pinned by a hash of its content, and a mismatch stops
the run.**

`integrity` is `h1:` followed by the SHA-256, in hex, of a listing of the module: one
line per file, `<sha-256 of the file>  <path>`, sorted by path, with `/` as the separator
whatever the platform. The hash is of the unpacked files, not of an archive, so the same
module has the same hash whether it came from git or from a registry, and a mirror can be
swapped in without the lockfile changing. `.git` is not part of a module. A module
holding a symbolic link, or an archive holding a path that leaves the module, is refused.

The hash is checked when a module is fetched and again every time it is used, so a file
edited in the module store is caught at the next command and named. `resolved` records
where the content came from (a commit or a manifest digest) and is what is asked for on a
re-fetch, so a moved tag cannot substitute different content silently: it fails the
integrity check instead.

**Signing** uses the mechanism plugins are signed with (the `meridian-signing` crate,
Ed25519), so there is one trust model and one set of trusted keys for both
([issue 68](https://github.com/ZanattaMichael/meridian-core/issues/68)).

- `meridian module sign --key FILE` writes `meridian.sig` in the module: a signature over
  a statement of the module's name, version and integrity hash. Naming all three means a
  signature cannot be moved to another module, another version or other content.
- `meridian.sig` is the one file the integrity hash leaves out. So signing does not change
  a module's pin, the signature needs no storage of its own in a registry, and git, OCI
  and mirrors all carry it the same way. The cost: the pin does not protect the signature
  file, which is why the signature is checked wherever the pin is.
- `--module-signatures off|warn|require` (default `off`) with the existing
  `--trusted-key`. The check runs on install and update before the lockfile is written,
  on every use of a module, and in `module verify`. `path` modules are not checked.
- The policy belongs to the machine and not to the lockfile: nothing about signatures is
  recorded in `meridian.lock`, so a project's pins do not differ between someone who
  checks and someone who does not, and the check cannot be skipped by a lockfile that
  claims it was made.
- A version is published once, so a version cannot be signed after it is published.

### 1.7 Where modules are kept

Fetched modules are unpacked under the state directory, in
`.meridian/modules/<name>/<version>/`. It is a cache: deleting it costs a re-fetch and
nothing else, and it is not committed.

### 1.8 Commands

```
meridian module install [--locked]   fetch what the manifest requires, write meridian.lock
meridian module update [NAME]...     move pinned versions to the newest that still fit
meridian module verify               check every pinned module against its integrity hash
meridian module list                 the pinned modules, their versions and sources
meridian module pack [--out FILE]    write this module as the archive a registry stores
meridian module push REPOSITORY      publish this module to a registry, under its version
```

`--locked` is for CI: it fails if the lockfile would have to change. `compile`, `plan`
and `apply` never fetch. A run that needs a module that is not installed stops and says
to run `meridian module install`, so a run never changes what it runs half way through,
and never reaches the network unasked.

`meridian module push REPOSITORY` publishes the module in the current directory under its
version. **A version is published once**: pushing other content under a version the
repository already holds is refused, because consumers who pinned it would keep the old
content while new ones got the new. Any tool that pushes OCI artifacts with the media
types above can publish a module too. The user-facing reference for all of this is
[Modules](../reference/modules.md).

### 1.9 What installing a module can and cannot do

Installing runs nothing from the module: no hooks, no scripts, no templates evaluated.
A module is data until a command is pointed at one of its documents, and at that point it
is subject to everything a local document is. Fetching needs the network; nothing else
does.

---

## 2. The language version: `apiVersion`

[Release and versioning §2](release-and-versioning.md#2-ir-apiversion) sets the policy.
This is how the build carries it out.

**Decision: every build holds a table of the language versions it knows, each in one of
three states, and a document's `apiVersion` is looked up in it before anything else in
the document is read.**

| State | Behaviour |
|---|---|
| Supported | The document compiles |
| Deprecated | The document compiles. One warning per run names the successor and the command that migrates to it |
| Removed | Refused, naming the release that removed it and the migration |

A value that is not in the table is refused, and the message says which of three things
is wrong, because the fix differs:

```
unsupported apiVersion "meridian/v3": it is newer than this build, which understands
    meridian/v1 to meridian/v2; upgrade meridian
unsupported apiVersion "meridian/v1.2": versions are whole numbers, as in "meridian/v1"
unsupported apiVersion "acme/v1": expected "meridian/v1"
```

**One warning per run, not per document**, as the policy requires. The warning counts the
documents and names the first.

**`meridian migrate`** rewrites documents from one version to the next. A migration is a
list of steps registered beside the version table; each step either rewrites the document
or reports, per document, the decision only a person can make. It never guesses. With
`--check` it changes nothing and exits non-zero if anything would change, for CI. While
`meridian/v1` is the only version there is nothing to migrate, and the command says so;
it exists now so that the release that introduces `v2` cannot ship without it.

**`meridian version`** prints every version stream of the build: the core, the language
versions and their states, the SDK, the plugin protocol range and the lockfile format.
The run-state schema joins them when the run-state gains its version stamp.

---

## 3. The plugin protocol version

Today a host refuses any plugin whose protocol number is not exactly its own. That makes
every protocol change a flag day: every plugin must be rebuilt in the same release as the
core.

**Decision: the host and the plugin each support a range of protocol versions, and they
agree on the highest both have before the first call.**

1. The host starts the plugin with `MERIDIAN_PLUGIN_PROTOCOLS` set to the versions it
   speaks, lowest first: `2,3`.
2. The plugin takes the highest version in that list that it also speaks, and announces
   it in its handshake line, in the field that has always carried the protocol version:
   `MERIDIAN-PLUGIN|3|0.4.0|unix|/tmp/…`.
3. The host checks that the announced version is one it offered. From then on both sides
   speak that version.

A plugin built before negotiation existed ignores the variable and announces its one
version, which the host accepts if it is in its range: old plugins keep loading. A new
plugin started by an old host sees no variable and announces its newest version, which
the old host accepts or refuses exactly as it always did. Neither side needs the other to
have been upgraded first.

When there is no version in common the plugin still announces its newest, and the host
refuses it with a message that says which side is behind:

```
the plugin speaks protocol version 1 and this host speaks 2 to 3;
    the plugin is older than this host supports: rebuild it against a current SDK
the plugin speaks protocol version 5 and this host speaks 2 to 3;
    the plugin is newer than this host: upgrade meridian
```

**The SDK version is checked too.** The handshake already carries the SDK release the
plugin was built against. A plugin built against a newer SDK than the host ships, or
against a different major, is refused by name, as
[release and versioning §3.1](release-and-versioning.md#31-the-promise) requires. Before
`1.0` the minor version is the breaking position, as SemVer says.

**How many versions a host carries.** A host speaks at most two adjacent protocol
versions, matching the limit on SDK majors, and a protocol version is removed only
through the [deprecation policy](release-and-versioning.md#6-deprecation-policy).

**The compatibility matrix** is a reference page generated from the constants in the
code and checked by a test, so it cannot drift:
[compatibility](../reference/compatibility.md).

---

## 4. How this lands

| Step | Holds |
|---|---|
| 1 | This note. `apiVersion` table, messages, per-run deprecation warning, `meridian migrate`, `meridian version`. Protocol negotiation, the SDK check, the compatibility page |
| 2 | Modules: the crate `meridian-module` (manifest, `path`, `git` and `oci` sources, resolution, lockfile, integrity), the `meridian module` commands including `push`, `@module:path` |
| 3 | Composites and functions resolved from modules, as the language work builds them |

## 5. Open questions

- **A public index.** Deliberately absent. If one is ever wanted, it is a hosted service
  and a decision for the owner.
- **Signing beyond keys on disk.** Built: Ed25519 signatures shared with plugins
  (section 1.6). Open: verifying registry artifacts with cosign/Sigstore as well, which
  would be new work with a new dependency; a project-level statement of which key may
  sign which module; and revocation, which today means removing a key file.
- **Credentials for registries.** Only `auths` entries of the Docker configuration are read
  at first. Decided: a module source will also be able to name a credential declared in
  the language's model ([language expansion §12](language-expansion.md#12-credentials),
  the `registry`, `usernamePassword` and `token` types), with the Docker configuration as
  the ambient default. Secrets and credentials are a design and not yet code, so this
  waits for them; [issue 69](https://github.com/ZanattaMichael/meridian-core/issues/69).
- **Removed-field messages** ([release and versioning §6](release-and-versioning.md#6-deprecation-policy))
  need a table of removed fields per `apiVersion`. It has no entries until something is
  removed, so it lands with the first removal.
