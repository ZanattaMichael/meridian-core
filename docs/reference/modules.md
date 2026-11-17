# Modules

A module is Meridian configuration packaged so that someone else can use it:
documents, the data they read, and a manifest that gives the package a name and
a version. A project says which modules it needs and where each comes from.
Meridian fetches them, records exactly what it fetched, and checks on every run
that nothing has changed since.

**What it does**

- Reads modules from a directory on disk, a git repository or an OCI registry.
- Picks versions from [SemVer](https://semver.org) requirements, one version of
  each module per project.
- Pins every fetched module in `meridian.lock` by a hash of its content, and
  checks that hash each time the module is used.
- Lets a command name a file inside a module: `@acme/nginx:documents/proxy.yaml`.
- Publishes a module to a registry, and refuses to replace a version that is
  already published with different content.
- Signs a module, and can refuse a fetched module that no key you trust has
  [signed](#signatures).

**What it does not do**

- There is no central index and no hosted registry. A requirement says where
  the module comes from: your git server or your container registry.
- It does not check signatures unless you ask. A pin proves the content has
  not changed since you pinned it, not who wrote it; `--module-signatures`
  adds that, and is off by default.
- `compile`, `plan` and `apply` never fetch. Only `meridian module install`,
  `update` and `push` use the network.
- Installing a module runs nothing from it: no hooks, no scripts.

A complete, runnable project is in
[`examples/modules`](../../examples/modules/README.md).

## A first module

A module is a directory with a file named `meridian.yaml` in it:

```
hardening/
  meridian.yaml
  documents/ssh.yaml
  data/defaults.yaml
```

```yaml
# hardening/meridian.yaml
apiVersion: meridian/v1
kind: Module
metadata:
  name: acme/hardening
  version: 1.2.0
  description: A baseline SSH configuration
```

A project that uses it has a `meridian.yaml` of its own, which lists what it
needs:

```yaml
# site/meridian.yaml
apiVersion: meridian/v1
kind: Module
metadata:
  name: example-site
spec:
  modules:
    - name: acme/hardening
      version: "^1.2"
      git: https://git.example.com/acme/meridian-hardening.git
```

From the project's directory:

```
$ meridian module install
+ acme/hardening 1.2.0 (git+https://git.example.com/acme/meridian-hardening.git)
wrote meridian.lock

$ meridian plan web.yaml @acme/hardening:documents/ssh.yaml \
    --data @acme/hardening:data/defaults.yaml
```

Commit `meridian.yaml` and `meridian.lock`. Do not commit `.meridian/`, which is
where the fetched copy is kept.

## The manifest

`meridian.yaml` is the same file for a module and for a project that uses
modules. A project is a module that nobody depends on.

```yaml
apiVersion: meridian/v1
kind: Module
metadata:
  name: acme/nginx
  version: 1.4.2
  description: An nginx reverse proxy
spec:
  meridian: ">=0.1.0"
  modules:
    - name: acme/base
      version: "^2.1"
      git: https://git.example.com/acme/meridian-base.git
```

| Field | Required | Value |
|---|---|---|
| `apiVersion` | yes | The [language version](dsl/index.md#apiversion): `meridian/v1` |
| `kind` | yes | `Module` |
| `metadata.name` | yes | One to three parts of lower-case letters, digits and `-`, joined by `/`: `nginx`, `acme/nginx`, `acme/web/nginx`. This is the name others require it by |
| `metadata.version` | to be shared | The module's version, as `MAJOR.MINOR.PATCH`. A project that only uses modules can leave it out. Required for a module fetched by version, and by `meridian module pack` |
| `metadata.description` | no | One line saying what the module is for |
| `spec.meridian` | no | The releases of `meridian` the module works with, as a [version requirement](#version-requirements). A release outside it refuses the module by name |
| `spec.modules` | no | The modules this one needs. See [requiring a module](#requiring-a-module) |

A field that is not in this table is an error, not something silently ignored.
Write versions and requirements in quotes when they could be read as a number:
`version: "1.4"`, not `version: 1.4`.

### What goes in a module

Content is found by directory. Nothing has to be listed in the manifest.

| Directory | Holds |
|---|---|
| `documents/` | `Infrastructure` and `ResourceSet` documents |
| `data/` | Data files, as given to `--data` |
| `composites/`, `functions/` | Reserved for the language's composites and functions |
| `README.md` | What the module is for and how to use it |

The directory names are a convention: `@<module>:<path>` reaches any file in
the module. A module cannot hold a symbolic link. `.git` and `.meridian` at the
top of a module are never part of it.

## Requiring a module

Each entry of `spec.modules` has a `name` and exactly one source: `path`, `git`
or `oci`.

| Field | Goes with | Meaning |
|---|---|---|
| `name` | every entry | The module's `metadata.name`. What is found at the source must say the same name |
| `version` | `git`, `oci`; optional with `path` | The versions that will do, as a [requirement](#version-requirements) |
| `path` | | A directory, relative to the manifest that names it |
| `git` | | A repository address: anything `git clone` takes |
| `ref` | `git` | A branch, tag or commit to pin to, instead of `version` |
| `subdir` | `git` | The directory of the repository that holds the module, when it is not the top |
| `oci` | | A registry repository: `registry.example.com/team/module`, with no scheme and no tag |
| `insecure` | `oci` | `true` to use plain HTTP. For a registry on a trusted network only |

### From a directory: `path`

```yaml
- name: acme/base
  path: ../modules/base
```

For a monorepo, and for working on a module next to the project that uses it.
The module is read where it is: nothing is copied and nothing is hashed, so an
edit takes effect on the next command. `version` is optional; when given, the
module's own `metadata.version` must satisfy it.

A module fetched from git or a registry cannot require another by `path`. That
path would point at the disk of whoever fetched it.

### From git: `git`

```yaml
- name: acme/nginx
  version: "^1.4"
  git: https://git.example.com/acme/meridian-nginx.git

- name: acme/tls
  version: "~2.0"
  git: git@git.example.com:acme/platform.git
  subdir: modules/tls

- name: acme/experimental
  git: https://git.example.com/acme/experimental.git
  ref: main
```

- With `version`, the versions on offer are the repository's tags named
  `v1.4.2` or `1.4.2`. Other tags are ignored. The tagged commit's
  `meridian.yaml` must give the same version as the tag.
- With `ref`, there is one candidate: that branch, tag or commit. The commit it
  points at when you install is pinned, and stays pinned when the branch moves,
  until you run `meridian module update`.
- Give `version` or `ref`, not both.

Meridian runs the `git` program on your machine, so every address and login
that works for `git clone` works here: SSH keys, credential helpers, a
`.netrc`. Meridian stores no git credentials and never prompts for a password;
a fetch that cannot authenticate fails. `MERIDIAN_GIT` names a different `git`
binary.

### From a registry: `oci`

```yaml
- name: acme/postgres
  version: "~3.2"
  oci: registry.example.com/acme/meridian-postgres
```

Any registry that implements the OCI distribution specification can hold
modules, which includes the container registry you already run. The versions on
offer are the repository's tags that are versions (`3.2.1` or `v3.2.1`).

Logins are read from the Docker configuration file: `~/.docker/config.json`, or
`config.json` in the directory `DOCKER_CONFIG` names. So `docker login
registry.example.com` (or `podman login`, `oras login`) is how you give access.
Only logins stored in the file's `auths` section are read; credential helpers
are not run. Token and basic authentication are supported. Naming a declared
credential for a registry, so that a login can come from a secret provider, is
planned ([issue 69](https://github.com/ZanattaMichael/meridian-core/issues/69)).

## Version requirements

Requirements are written the way Cargo writes them.

| Requirement | Means |
|---|---|
| `1.4` or `^1.4` | `>=1.4.0, <2.0.0` |
| `^0.3` | `>=0.3.0, <0.4.0`: before `1.0`, the middle number is the breaking one |
| `~1.4` | `>=1.4.0, <1.5.0` |
| `=1.4.2` | exactly `1.4.2` |
| `>=1.2, <1.8` | both |
| `*` | any version |

A pre-release such as `2.0.0-rc.1` is chosen only when the requirement names a
pre-release.

### How a version is chosen

- **One version of each module per project.** If two modules require
  `acme/base`, one version serves both.
- **The newest version that satisfies every requirement on it** is chosen.
- **A version already in `meridian.lock` is kept** for as long as it still
  satisfies every requirement. Adding a module never moves another.
- **A conflict is an error.** Meridian does not pick a compromise:

```
error: no version of acme/base satisfies every requirement on it
    ^2   required by the project
    ^1   required by acme/app 1.0.0
    available: 1.0.0, 1.3.0, 2.1.0
```

Requiring the same module name from two different sources is also a conflict.

## The lockfile

`meridian module install` writes `meridian.lock` beside the manifest:

```yaml
# Written by `meridian module`. Commit it; do not edit it.
lockVersion: 1
modules:
  - name: acme/base
    version: "2.2.1"
    source: git+https://git.example.com/acme/meridian-base.git
    resolved: "9c1d7be0a4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c7"
    integrity: h1:6b1f0a8f3e0c4c7b9a55d3f7c1e2a4b6d8f0a1c3e5b7d9f1a3c5e7b9d1f3a5c7
  - name: acme/nginx
    version: "1.4.2"
    source: git+https://git.example.com/acme/meridian-nginx.git
    resolved: "4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a"
    integrity: h1:91c4d7a2b5e8f1a4c7d0b3e6f9a2c5d8e1b4a7c0d3f6a9b2c5e8d1f4a7b0c3e6
    requires: [acme/base]
```

| Field | Meaning |
|---|---|
| `lockVersion` | The format of the file. A newer `meridian` reads an older lockfile; an older one refuses a newer lockfile |
| `name`, `version` | The module and the version chosen. A module pinned by `ref`, or a `path` module, may have no version |
| `source` | Where it comes from: `git+<address>`, with `//<subdir>` and `#<ref>` when given; `oci+<repository>`; `path+<directory>` |
| `resolved` | Exactly what was fetched: the git commit, or the digest of the registry manifest. This, not the tag, is what a later fetch asks for |
| `integrity` | The [content hash](#integrity) the module must have |
| `requires` | The modules this one requires |

A `path` module has no `resolved` and no `integrity`: it is a working copy.

The file is sorted and holds no timestamps, so the same resolution always
writes the same bytes and a diff shows only what moved. Commit it. With it,
every machine and every CI run uses the same commit of every module.

## Integrity

Every fetched module is pinned by a hash of its files.

`integrity` is `h1:` followed by a SHA-256. It is the hash of a listing with one
line per file in the module, `<SHA-256 of the file>  <path>`, sorted by path. It
is a hash of the files and not of an archive, so the same module has the same
hash whether it came from git or from a registry, and you can move a module to
a mirror without its pin changing.

One file is left out of the listing: `meridian.sig`, the module's
[signature](#signatures), which is made over this hash and so cannot be part
of it.

The hash is checked:

- **when a module is fetched**, against the lockfile, if the module was already
  pinned;
- **every time a module is used** by `validate`, `compile`, `plan` or `apply`;
- **on demand**, by `meridian module verify`.

What each kind of change looks like:

| What happened | What you see |
|---|---|
| A file in `.meridian/modules` was edited | `acme/nginx 1.4.2 in .meridian/modules/acme/nginx/1.4.2 does not match meridian.lock`, with both hashes. Delete that directory and install again |
| A git tag was moved to another commit after you pinned it | Nothing changes: the pinned commit is what is fetched, not the tag |
| A registry serves different bytes for a digest | `the registry … answered a request for sha256:… with different content` |
| The source now serves content that does not match the pin | `what git+… now serves does not match meridian.lock`. If the change is expected, run `meridian module update <name>` |
| The module is pinned but not fetched | `acme/nginx 1.4.2 is not installed; run meridian module install` |

An archive from a registry is unpacked with care: an entry that is not a plain
file, a path that would land outside the module, a duplicate path, and an
archive that unpacks to more than 256 MiB are all refused.

## Signatures

The pin answers "is this what I pinned?". A signature answers "who published
it?". A signed module holds one more file in its top directory,
`meridian.sig`, made by [`meridian module sign`](#sign) with the publisher's
secret key. You decide which public keys you trust and whether a signature is
needed.

It is the mechanism [plugins are signed with](plugin-signatures.md): the same
key files, the same `--trusted-key` option, the same three policies. One key
can sign a team's plugins and its modules.

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--module-signatures POLICY` | `MERIDIAN_MODULE_SIGNATURES` | `off` | `off` does not look. `warn` uses a module that is unsigned or whose signature does not pass, and says so on standard error. `require` uses only modules signed by a trusted key |
| `--trusted-key FILE` | `MERIDIAN_TRUSTED_KEYS` (separated by `:`, or `;` on Windows) | the key a released `meridian` was built with | a public key to trust, or a directory of `*.pub` files; may be repeated |

Both can be given to any command. Set them in the environment of the machines
that run Meridian, so that every command agrees:

```
export MERIDIAN_MODULE_SIGNATURES=require
export MERIDIAN_TRUSTED_KEYS=/etc/meridian/trusted-keys
```

**What is checked, and when.** Every module fetched from git or a registry,
including the ones other modules require:

- by `meridian module install` and `update`, before `meridian.lock` is
  written, so a module that may not be used is not pinned either;
- every time a module is used by `validate`, `compile`, `plan`, `apply` or
  `rules check`, after its hash;
- by `meridian module verify`, which also says which key signed each module.

A `path` module is your own working copy and is never checked.

**What a signature covers.** The signature is over a short statement: the
module's name, its version and its [content hash](#integrity). So it vouches
for exactly one content of one version of one module, and a signature lifted
from another module, or from another version with the same files, does not
pass. Nor does a module's signature pass as a plugin's, or a plugin's as a
module's, although the same key can make both: each names its own kind in what
is signed. `meridian.sig` itself is the one file the content hash leaves out, which
has two useful consequences: signing a module does not change what it is pinned
as, and the signature travels with the module unchanged through git, a
registry or a mirror.

| What you see | What it means |
|---|---|
| `acme/nginx 1.4.2 is not signed: it holds no meridian.sig` | The publisher did not sign this version. Under `require` it is refused |
| `its signature does not pass: it was signed by key 3fa9…, which is not a trusted key` | It is signed, by a key you have not named. If you trust that publisher, get their public key from them and add it with `--trusted-key` |
| `its signature does not pass: the module is not the module that was signed: its contents have changed` | The signature belongs to other content, another version or another module |
| `warning: acme/nginx 1.4.2 is not signed` | The same findings under `warn`: the module was used anyway |

What a signature does not do: it does not say the module is good, only who
published it. It is checked against keys held on the machine, with no network
and no revocation list, so retiring a key means removing its `.pub` file from
where your machines look. And whoever can change the trusted keys or the
environment of the machine can switch the check off; protect those as you
protect `meridian` itself.

## Using a module's files

A file in a module is written `@<module>:<path>`:

```
meridian validate @acme/nginx:documents/proxy.yaml
meridian plan site.yaml @acme/nginx:documents/proxy.yaml
meridian apply site.yaml @acme/nginx:documents/proxy.yaml \
    --data @acme/nginx:data/defaults.yaml --data prod.yaml
```

It can be used wherever a document or a `--data` file is given, mixed freely
with ordinary paths. The files of one command are still one workflow, wherever
they come from.

- The module must be pinned in the project's `meridian.lock`. A module that
  another module requires counts.
- The path is relative to the module and cannot leave it.
- Run the command from the project's directory, the one with `meridian.yaml`.

Data files are merged in the order given, and a later file's key replaces an
earlier one. Giving the module's defaults first and your own file after it is
how you override a module's defaults.

In a shell, `@` needs no quoting.

## Commands

All of them are run from the project's directory.

```
meridian module install [--locked]
meridian module update [NAME]...
meridian module verify
meridian module list
meridian module sign --key FILE
meridian module pack [--out FILE]
meridian module push REPOSITORY [--insecure]
```

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--state-dir DIR` | `MERIDIAN_STATE_DIR` | `.meridian` | Fetched modules are kept in `DIR/modules` |
| `--module-signatures POLICY` | `MERIDIAN_MODULE_SIGNATURES` | `off` | Whether fetched modules must be [signed](#signatures): `off`, `warn` or `require` |
| `--trusted-key FILE` | `MERIDIAN_TRUSTED_KEYS` | | A public key whose signatures are trusted; may be repeated |

### `install`

Resolves what `meridian.yaml` requires, fetches what is missing, and writes
`meridian.lock` if it changed.

```
$ meridian module install
+ acme/nginx 1.4.2 (git+https://git.example.com/acme/meridian-nginx.git)
+ acme/base 2.2.1 (git+https://git.example.com/acme/meridian-base.git)
wrote meridian.lock
```

| Line | Meaning |
|---|---|
| `+ name version (source)` | newly pinned |
| `~ name old -> new` | the pin moved |
| `- name version` | no longer required, and removed from the lockfile |
| `N modules unchanged` | pins that stayed where they were |

Pins that still fit are kept, so running it again changes nothing. On a machine
that has the lockfile and nothing fetched, it fetches exactly what is pinned
and does not look for newer versions.

`--locked` fails, and changes nothing, if `meridian.lock` would have to change.
Use it in CI:

```
meridian module install --locked
meridian plan site.yaml @acme/nginx:documents/proxy.yaml
```

### `update`

Moves pins to the newest version that satisfies the requirements. With names,
only those modules; with none, every module. For a module pinned by `ref`, it
moves the pin to the commit the branch points at now.

```
$ meridian module update acme/nginx
~ acme/nginx 1.4.2 -> 1.5.0
1 module unchanged
wrote meridian.lock
```

To go past what a requirement allows, change the requirement in
`meridian.yaml` and run `install`.

### `verify`

Checks every pinned module against its hash and reports on each. It exits `1`
if any failed. With `--module-signatures warn` or `require` it also checks each
fetched module's [signature](#signatures) and says who signed it:

```
$ meridian module verify --module-signatures require --trusted-key keys/acme.pub
ok      acme/base 2.2.1  signed by key 3fa91c0e5d7b2a48
ok      local
```

Under `warn` a module that is unsigned shows as `ok … not signed`; under
`require` it is `FAILED`.

```
$ meridian module verify
ok      acme/base 2.2.1
FAILED  acme/nginx 1.4.2
    acme/nginx 1.4.2 in .meridian/modules/acme/nginx/1.4.2 does not match meridian.lock
    ...
error: 1 module failed verification
```

### `list`

The pinned modules, their versions and their sources, from `meridian.lock`.

```
$ meridian module list
acme/base   2.2.1  git+https://git.example.com/acme/meridian-base.git
acme/nginx  1.4.2  git+https://git.example.com/acme/meridian-nginx.git
local       -      path+../local
```

### `sign`

Signs the module in the current directory as it is now, and writes
`meridian.sig` in it.

```
$ meridian module sign --key ~/keys/acme.key
signed acme/nginx 1.4.2 with key 3fa91c0e5d7b2a48
  integrity: h1:9b1f…
  wrote:     meridian.sig
```

The key is a secret key made by
[`meridian signing keygen`](cli.md#signing); those who use the module trust
the public half. Sign last: any change to the module's files afterwards,
including to `metadata.version`, makes the signature stale, and `sign` must be
run again. Then commit `meridian.sig` with the release, or push.

### `pack`

Writes the module in the current directory as the archive a registry stores.
The module must have a `metadata.version`.

```
$ meridian module pack
packed acme/nginx 1.4.2 into .meridian/packages/acme-nginx-1.4.2.tar.gz
  size:       1843 bytes
  integrity:  h1:91c4d7a2…
  media type: application/vnd.meridian.module.layer.v1.tar+gzip
```

`--out FILE` writes it somewhere else. Packing is deterministic: the same files
give the same bytes on any machine at any time, because file times, owners and
permissions are left out. The `integrity` line is the hash a consumer's
lockfile will hold.

### `push`

Publishes the module in the current directory to an OCI registry, under its
`metadata.version`.

```
$ meridian module push registry.example.com/acme/meridian-nginx
published acme/nginx 1.4.2 to registry.example.com/acme/meridian-nginx:1.4.2
  digest:     sha256:3f1c…
  integrity:  h1:91c4d7a2…
```

`REPOSITORY` is written as in a requirement's `oci`: no scheme and no tag.
`--insecure` uses plain HTTP. The login comes from the Docker configuration
file, as it does for fetching.

A version is published once:

- Pushing the same content again uploads nothing and says the version is
  already published.
- Pushing different content under a version that is already there is refused.
  Consumers who pinned it would keep the old content while new ones got the
  new, under one version number. Raise `metadata.version` and push that.

`digest` is what a consumer's lockfile records as `resolved`, and `integrity`
is the hash it records for the content.

## Publishing

### To git

Commit the module and tag the commit with its version:

```
git tag v1.4.2
git push origin v1.4.2
```

To publish it signed, sign before the commit that is tagged, and commit
`meridian.sig` with it:

```
meridian module sign --key ~/keys/acme.key
git add meridian.sig && git commit -m "Release 1.4.2"
git tag v1.4.2
```

The tag and `metadata.version` must agree. Never move a tag once it is pushed:
consumers who pinned it keep the old commit, and new consumers get the new one.
Release a new version instead.

### To a registry

A module is stored as an OCI artifact with one layer.

| | |
|---|---|
| Artifact type | `application/vnd.meridian.module.v1` |
| Layer media type | `application/vnd.meridian.module.layer.v1.tar+gzip` |
| Tag | the version: `1.4.2` or `v1.4.2` |

```
meridian module push registry.example.com/acme/meridian-nginx
```

To publish it signed, run `meridian module sign --key FILE` first: the
signature is a file of the module, so it is in what is pushed. A version is
published once, so a version that went out unsigned cannot be signed
afterwards; release the next version signed.

That is all a module needs. A module can also be published with any tool that
pushes OCI artifacts, as long as it uses the two types above. With
[`oras`](https://oras.land):

```
meridian module pack --out module.tar.gz
oras push registry.example.com/acme/meridian-nginx:1.4.2 \
  --artifact-type application/vnd.meridian.module.v1 \
  module.tar.gz:application/vnd.meridian.module.layer.v1.tar+gzip
```

A tag cannot hold `+`, so write build metadata with `_`: the tag `1.4.2_build.5`
is the version `1.4.2+build.5`.

## Where modules are kept

Fetched modules are unpacked under the state directory:

```
.meridian/modules/<name>/<version>/
.meridian/modules/<name>/ref-<commit>/      for a module pinned by ref
```

It is a cache. Deleting it costs a fetch and nothing else, and it should not be
committed. Do not edit files in it: the next command will refuse the module.

## When something goes wrong

| Message | Cause | Fix |
|---|---|---|
| `the project requires modules and has no meridian.lock` | The modules were never installed here | `meridian module install` |
| `meridian.lock is out of date with meridian.yaml: …` | The manifest changed since the lockfile was written | `meridian module install`, and commit the lockfile |
| `… is not installed` | The lockfile is there but the module has not been fetched on this machine | `meridian module install` |
| `no version of … satisfies every requirement on it` | Two requirements cannot both hold, or the source has no such version | Loosen a requirement; the message lists who requires what, and what exists |
| `… is required from two different sources` | Two manifests name different sources for one module | Make them agree |
| `… holds the module X, and it was required as Y` | The `name` in the requirement is not the module's `metadata.name` | Use the module's own name |
| `the version X of … holds a manifest that says Y` | A tag and the module's `metadata.version` disagree | Fix it in the module and release a new version |
| `… needs meridian >=X, and this is meridian Y` | The module's `spec.meridian` excludes this release | Upgrade `meridian`, or use an older version of the module |
| `… requires … by path, which only works between modules on disk` | A fetched module names a `path` source | The module must name a `git` or `oci` source |
| `the registry … did not allow reading …` or `writing to …` | No login, or a refused one | `docker login` for that registry |
| `… is already published in … with different content` | A push of a version that exists, with other files | Raise `metadata.version` and push again |
| `… is not a Meridian module: it has no layer of type …` | The registry tag holds something else, such as a container image | Check the repository; see [publishing to a registry](#to-a-registry) |
| `… is not signed: it holds no meridian.sig` | Signatures are required and this version was published without one | Ask the publisher to sign a release, pin a signed version, or use `--module-signatures warn` |
| `… its signature does not pass: it was signed by key …, which is not a trusted key` | It is signed by a key you have not named | Add the publisher's public key with `--trusted-key` |
| `… is not a module of this project` | `@name:path` names a module that is not pinned | Add it to `spec.modules` and install |
| `the git program was not found` | A git module needs `git` | Install git, or set `MERIDIAN_GIT` |

## For plugin and tool authors

The crate `meridian-module` is the library behind all of this.
`Modules::load` opens a project's pinned modules, checking each (its hash, and
its signature when the `Project` it is given carries a policy);
`meridian_module::sign` signs one;
`Modules::resolve` turns `@<module>:<path>` into a file; and `Modules::roots`
gives each module's directory, for code that confines file access to a set of
directory trees. The design, and what is still to come, is in
[modules and versioning](../plans/modules-and-versioning.md).
