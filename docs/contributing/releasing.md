# Releasing Meridian

How to cut a release. Policy: [release and versioning plan](../plans/release-and-versioning.md).
Workflow: [`.github/workflows/release.yml`](../../.github/workflows/release.yml); scripts in
[`tools/release/`](../../tools/release); packaging in [`packaging/`](../../packaging).

## Three kinds of run

**A rehearsal** builds and tests, publishing nothing (artifacts kept 7 days). It runs on pull
requests touching the workflow, `packaging/`, `tools/release/` or root `Cargo.toml`, and on
demand.

**A release** adds the `publish` job. It runs when:

- **`cut release`** is run from the Actions tab ([below](#cutting-a-release)), or `release` is
  dispatched at a `v*` tag; or
- a `v<version>` tag is pushed by hand **and** `MERIDIAN_RELEASE_PUBLISH` is `true`.

A tag not matching `Cargo.toml` fails. A release opens a **draft**; nothing is public until
you publish it.

**A nightly** is a rehearsal at a nightly version, published as the `nightly` pre-release
([below](#nightly)).

## What is built

| Job | What it makes | How it is checked in every run |
|---|---|---|
| `build` (six times) | `meridian-<version>-<target>.tar.gz` or `.zip`: `meridian`, every plugin under `plugins/`, each plugin's `.sig`, `release-key.pub` | on the four platforms the runners can execute: unpacked and run; every plugin must start and verify with no flag; a plugin changed by one byte must be refused |
| `assemble` | `meridian-<version>-checksums.txt`, `VERSION`, the Homebrew formula, Scoop manifest and winget manifests, two SBOMs, the install scripts | checksums re-checked; each manifest parsed; `install.sh` through `shellcheck`; the SBOMs must list known dependencies |
| `install` (four systems) | nothing | `install.sh` or `install.ps1` run as a person would run it, against this run's files; then again against a corrupted archive, which must be refused |
| `cargo-install` | nothing | `cargo install` of the command and one plugin from this checkout |
| `image` | the container image for `linux/amd64` and `linux/arm64` | run on both; every plugin must verify; a document is compiled as the unprivileged user |
| `publish` | signatures, attestations, the pushed image, the draft release | runs only in a release |

Targets: `x86_64` and `aarch64` for `unknown-linux-musl` (static), `apple-darwin` and
`pc-windows-msvc`. x86-64 macOS and arm64 Windows are cross-compiled and not run in CI.
Profile `dist` (stripped, thin LTO), no build cache.

## How the plugins are signed

Each `build` job makes an Ed25519 key pair, embeds the public key
(`MERIDIAN_RELEASE_PUBLIC_KEY`), signs the plugins and deletes the secret key. No signing
secret exists. See [plugin signatures](../reference/plugin-signatures.md).

## What `publish` does

In order:

1. Signs the checksums file with Sigstore's keyless signing
   (`cosign sign-blob`). The certificate names this workflow and the tag. The
   bundle is released as `meridian-<version>-checksums.txt.sigstore.json`.
2. Records a build provenance attestation for every archive, and an SBOM
   attestation, with GitHub's `attest-build-provenance` and `attest-sbom`.
3. Builds the image again from the same archives and pushes it to
   `ghcr.io/<owner>/meridian:<version>`, and `:latest` unless the version is a
   pre-release (has a `-` in it).
4. Signs the image with `cosign sign` and records its provenance in the
   registry.
5. Opens the draft release with every file attached.

Steps 1 to 4 cannot be undone.

## Settings a release needs

For a release only:

| What | Where | Value |
|---|---|---|
| The switch for tags pushed by hand | Settings, Secrets and variables, Actions, Variables | `MERIDIAN_RELEASE_PUBLISH` = `true`. Not needed when `cut release` makes the tag |
| An approval before publishing (recommended) | Settings, Environments, `release` | add yourself under Required reviewers. The `publish` job then waits for your approval in the run. The environment is created the first time the job runs if you have not made it |
| Restrict the environment to tags (recommended) | the same page, Deployment branches and tags | allow the tag pattern `v*` only |
| Make the image public | after the first release: your profile, Packages, `meridian`, Package settings | change visibility to Public, and connect the package to this repository. A package pushed by a workflow starts private |

**No secrets are needed**: `GITHUB_TOKEN` and keyless Sigstore. Only `publish` has write
permissions (`contents`, `packages`, `id-token`, `attestations`). Attestations on a private
repository need GitHub Enterprise Cloud.

## Cutting a release

1. Set `version` in root `Cargo.toml` (`[workspace.package]`) and
   `editors/vscode/package.json`, run `cargo build`, and merge.
2. Wait for green CI on `main`.
3. Actions tab, **cut release**, **Run workflow**:
   - **version**: must match `Cargo.toml` (default).
   - **ref**: default `main`.
   - **require_green_ci**: on; refuses a commit whose `rust` run is not `success`.

   It checks SemVer, the tag is new and CI passed, then tags `v<version>` and starts
   `release`. (By hand: `git tag -a v0.1.0 -m "Meridian 0.1.0"` and push, with the variable
   `true`.)
4. Approve `publish` if a reviewer is required. This is the last point to stop.
5. Check the draft (six archives, checksums and `.sigstore.json`, `VERSION`, `install.sh`,
   `install.ps1`, two SBOMs, package manifests) and publish.
6. Install it and run [Check what you downloaded](../quickstart/install.md#check-what-you-downloaded).
7. Update the package managers.

If a run fails after tagging, fix `main`, delete the tag and draft, and cut again. Never move
a published tag.

### Why a tag made by the workflow needs the dispatch

A push with a run's own token starts no workflow, so `cut release` dispatches `release` at
the tag. See [decision 21](../decisions/0021-a-dispatch-at-a-tag-is-a-release.md).

## Nightly

[`nightly.yml`](../../.github/workflows/nightly.yml) runs at 03:17 UTC and on demand (input
`force` rebuilds an unchanged `main`). It must run on `main`. A pull request changing it or
`set_version.py` runs `plan` and `build` only.

| Step | What happens |
|---|---|
| `plan` | stops if `main` is the commit the `nightly` tag points at; else sets the version `<next patch>-nightly.<YYYYMMDD>.g<sha7>`, as in `0.1.1-nightly.20261119.g1a2b3c4` |
| `build` | calls `release.yml` with that version: every job above, `publish` skipped. `tools/release/set_version.py` writes the version into `Cargo.toml` and `Cargo.lock` first, so `meridian --version` prints it |
| `publish` | attests build provenance for the archives, deletes the `nightly` release and tag, and creates them again at the built commit with every file of `release-files` |

The `nightly` release is a **pre-release**: `releases/latest` and the install scripts' default
do not move. It has no Sigstore bundle (install scripts accept only a `v*` tag's signature),
no image and no package-manager listings. It needs no settings. To stop it, disable the
workflow in the Actions tab.

Real-tool tests are not repeated nightly: they run on every push to `main` that touches what
they test ([testing](testing.md)).

## Package managers

A release carries the manifests in
`meridian-<version>-package-manifests.tar.gz`. They are generated, with the
release's URLs and checksums filled in, by `tools/release/manifests.py`. Each
package manager reads them from a repository of its own, and the
**`package managers`** workflow (Actions tab, Run workflow) puts them there
once a release is published:

| Manager | Where the workflow sends it | What a user then runs |
|---|---|---|
| Homebrew | `Formula/meridian.rb` in your tap repository `homebrew-meridian` | `brew install zanattamichael/meridian/meridian` |
| Scoop | `bucket/meridian.json` in your bucket repository `scoop-meridian` | `scoop bucket add meridian https://github.com/ZanattaMichael/scoop-meridian`, then `scoop install meridian` |
| winget | `manifests/z/ZanattaMichael/Meridian/<version>/` on a branch `meridian-<version>` of your fork of `microsoft/winget-pkgs` | `winget install ZanattaMichael.Meridian`, once Microsoft merges the pull request |
| crates.io | nowhere | not set up. `cargo install --git` works today. Publishing to crates.io would need the crate names reserved, a version on every path dependency between the crates, and a `CARGO_REGISTRY_TOKEN` secret |

The workflow takes the version to send (default: the latest release) and
whether to open the pull request to `microsoft/winget-pkgs`. It refuses a
draft release, because the manifests point at files nobody can download yet.
An empty tap or bucket gets the files as its first commit; one with history
gets a branch and a draft pull request, so a changed formula is read before it
lands. By hand, `tools/release/package_managers.py` lays the same trees out
from the tarball, and `tools/release/send_tree.sh` pushes one.

### What it needs

| What | Where | Value |
|---|---|---|
| The tap | a repository `ZanattaMichael/homebrew-meridian`, empty | private while you decide; public when you want others to `brew install` |
| The bucket | a repository `ZanattaMichael/scoop-meridian`, empty | the same |
| The winget fork | Fork `microsoft/winget-pkgs` on GitHub, keeping the name | forks are always public; the manifests sit on a branch there until you open the pull request |
| The token | Settings, Secrets and variables, Actions, Secrets | `MERIDIAN_PACKAGING_TOKEN`: a fine-grained personal access token with **Contents: read and write** and **Pull requests: read and write** on those three repositories (and read on `meridian-core`, to download the release). The run's own token cannot reach other repositories |

### Nothing public until you say

The tap and the bucket can stay private: `brew tap` and `scoop bucket add`
then work only for someone whose git credentials can read them, which is
how to try them out before anyone else can. Make the repositories public
under their Settings when you are ready; the files do not change. The winget
pull request is opened as a draft only when the workflow is asked to, and
Microsoft's validation runs on it; until it is merged, `winget install`
does not find the package.

### Code signing is not required by any of them

None of the three checks for a code signature. Homebrew installs the archive
with `curl`, which sets no quarantine flag, so Gatekeeper does not look for
notarisation. Scoop and winget install the zip as it is; winget's validation
checks the URL, the hash and that the files run, not a signature. The user
sees a warning only when they download an archive **in a browser**, which
the package managers never do. What a release does sign (Sigstore, and
GitHub's attestations) is explained under
[Check what you downloaded](../quickstart/install.md#check-what-you-downloaded).
Paid signing (Apple notarisation, an Authenticode certificate) only removes
the browser warnings; see [Known limits](#known-limits).

## Known limits

- **No macOS notarisation.** Browser downloads are blocked by Gatekeeper.
- **No Windows code signing.** SmartScreen may warn. SignPath Foundation signs open-source projects for free, if that ever matters.
- **Actions are pinned by tag**, not commit.
- **One key per platform build**, so plugins cannot ship separately yet.

## Running the pieces by hand

To debug locally:

```
cargo build --release -p meridian-cli
target/release/meridian signing keygen --secret release.key --public release.pub
export MERIDIAN_RELEASE_PUBLIC_KEY="$(cut -d' ' -f2 release.pub)"
cargo build --profile dist --locked --target x86_64-unknown-linux-musl --workspace --bins
python3 tools/release/package.py --version 0.1.0 --target x86_64-unknown-linux-musl \
    --bin-dir target/x86_64-unknown-linux-musl/dist --out dist \
    --signer target/release/meridian --key release.key
```

With all six archives in `dist`, `tools/release/manifests.py --version 0.1.0 --dir dist`
writes checksums and manifests, and `MERIDIAN_DOWNLOAD_BASE=$PWD/dist sh
packaging/install/install.sh` installs from it.
