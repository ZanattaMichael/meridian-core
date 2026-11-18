# Releasing Meridian

How to cut a release. Policy: [release and versioning plan](../plans/release-and-versioning.md).
Workflow: [`.github/workflows/release.yml`](../../.github/workflows/release.yml); scripts in
[`tools/release/`](../../tools/release); packaging in [`packaging/`](../../packaging).

## Two kinds of run

**A rehearsal** builds and tests, publishing nothing (artifacts kept 7 days). It runs on pull
requests touching the workflow, `packaging/`, `tools/release/` or root `Cargo.toml`, and on
demand.

**A release** adds the `publish` job. It runs when:

- **`cut release`** is run from the Actions tab ([below](#cutting-a-release)), or `release` is
  dispatched at a `v*` tag; or
- a `v<version>` tag is pushed by hand **and** `MERIDIAN_RELEASE_PUBLISH` is `true`.

A tag not matching `Cargo.toml` fails. A release opens a **draft**; nothing is public until
you publish it.

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

## Package managers

`tools/release/manifests.py` writes `meridian-<version>-package-manifests.tar.gz`. Nothing is
submitted automatically:

| Manager | File | What publishing it takes |
|---|---|---|
| Homebrew | `packaging/homebrew/meridian.rb` | a tap repository, `ZanattaMichael/homebrew-meridian`, with the file at `Formula/meridian.rb`. Users then run `brew install zanattamichael/meridian/meridian` |
| Scoop | `packaging/scoop/meridian.json` | a bucket repository, for example `ZanattaMichael/scoop-meridian`, with the file at `bucket/meridian.json`. The manifest has `checkver` and `autoupdate`, so the bucket can update itself |
| winget | `packaging/winget/*.yaml` | a pull request to `microsoft/winget-pkgs` adding the three files under `manifests/z/ZanattaMichael/Meridian/<version>/`. Microsoft's validation runs on the pull request |
| crates.io | none | not set up. `cargo install --git` works today. Publishing to crates.io would need the crate names reserved, a version on every path dependency between the crates, and a `CARGO_REGISTRY_TOKEN` secret |

Syntax is checked each run; real installs are untested until a release exists.

## Known limits

- **No macOS notarisation.** Browser downloads are blocked by Gatekeeper.
- **No Windows code signing.** SmartScreen may warn.
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
