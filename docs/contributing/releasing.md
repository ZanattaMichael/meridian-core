# Releasing Meridian

This is the maintainer's guide to a release: what the pipeline builds, what
it checks, what has to be switched on before anything is published, and the
steps to cut a release. The policy behind version numbers is in the
[release and versioning plan](../plans/release-and-versioning.md); this page
is the mechanics.

Everything is in one workflow, [`.github/workflows/release.yml`](../../.github/workflows/release.yml),
with its scripts in [`tools/release/`](../../tools/release) and its packaging
files in [`packaging/`](../../packaging).

## Two kinds of run

**A rehearsal** builds and tests everything and publishes nothing. It runs on
every pull request that touches the workflow, `packaging/`, `tools/release/`
or the root `Cargo.toml`, and on demand from the Actions tab (Run workflow).
What it makes stays in the run as workflow artifacts, kept for seven days.

**A release** is the same build and the same tests, followed by the `publish`
job. It happens only when both of these are true:

- a tag `v<version>` was pushed, where `<version>` is the workspace version in
  `Cargo.toml`, and
- the repository variable `MERIDIAN_RELEASE_PUBLISH` is `true`.

A tag pushed without the variable is a rehearsal, and says so in the run. A
tag that does not match `Cargo.toml` fails at once.

Even a release publishes nothing final: it opens a **draft** GitHub release.
Nobody is notified and `releases/latest` does not move until you publish the
draft by hand.

## What is built

| Job | What it makes | How it is checked in every run |
|---|---|---|
| `build` (six times) | `meridian-<version>-<target>.tar.gz` or `.zip`: `meridian`, every plugin under `plugins/`, each plugin's `.sig`, `release-key.pub` | on the four platforms the runners can execute: unpacked and run; every plugin must start and verify with no flag; a plugin changed by one byte must be refused |
| `assemble` | `meridian-<version>-checksums.txt`, `VERSION`, the Homebrew formula, Scoop manifest and winget manifests, two SBOMs, the install scripts | checksums re-checked; each manifest parsed; `install.sh` through `shellcheck`; the SBOMs must list known dependencies |
| `install` (four systems) | nothing | `install.sh` or `install.ps1` run as a person would run it, against this run's files; then again against a corrupted archive, which must be refused |
| `cargo-install` | nothing | `cargo install` of the command and one plugin from this checkout |
| `image` | the container image for `linux/amd64` and `linux/arm64` | run on both; every plugin must verify; a document is compiled as the unprivileged user |
| `publish` | signatures, attestations, the pushed image, the draft release | runs only in a release |

The targets are `x86_64` and `aarch64` for each of `unknown-linux-musl`
(static, so one binary serves every distribution), `apple-darwin` and
`pc-windows-msvc`. The x86-64 macOS and arm64 Windows archives are
cross-compiled and are not executed in CI, because the runners cannot run
them.

Binaries are built with the `dist` profile in the root `Cargo.toml`: the
release profile with symbols stripped and thin LTO. A release build never uses
a build cache.

## How the plugins are signed

Each `build` job makes a new Ed25519 key pair, compiles the public key into
that job's `meridian` (through `MERIDIAN_RELEASE_PUBLIC_KEY`), signs the
plugins with the secret key, and deletes the secret key before the job ends.
The key never leaves the runner and is never stored, so there is no signing
secret to configure, rotate or leak. Each platform's archive has its own key.

What that gives a user, and how they trust a plugin from elsewhere, is in
[Plugin signatures](../reference/plugin-signatures.md).

## What `publish` does

In order, and only in a release:

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

Steps 1, 2 and 4 write to Sigstore's public transparency log, and step 3
publishes an image. None of it can be taken back, which is why the job sits
behind the variable and an environment.

## Settings a release needs

Nothing has to be set for rehearsals. For a release:

| What | Where | Value |
|---|---|---|
| The switch | Settings, Secrets and variables, Actions, Variables | `MERIDIAN_RELEASE_PUBLISH` = `true` |
| An approval before publishing (recommended) | Settings, Environments, `release` | add yourself under Required reviewers. The `publish` job then waits for your approval in the run. The environment is created the first time the job runs if you have not made it |
| Restrict the environment to tags (recommended) | the same page, Deployment branches and tags | allow the tag pattern `v*` only |
| Make the image public | after the first release: your profile, Packages, `meridian`, Package settings | change visibility to Public, and connect the package to this repository. A package pushed by a workflow starts private |

**No secrets are needed.** The workflow uses the run's own `GITHUB_TOKEN` to
create the release and push the image, and Sigstore's keyless signing, which
uses the run's identity token in place of a key. The `publish` job asks for the
four permissions it needs (`contents`, `packages`, `id-token`, `attestations`:
write) itself; every other job is read-only.

Attestations work as they are on a public repository. On a private repository
they need a GitHub Enterprise Cloud plan.

## Cutting a release

1. Set the version: `version` under `[workspace.package]` in the root
   `Cargo.toml`. Run `cargo build` so `Cargo.lock` follows. Merge that.
2. Wait for CI on `main` to be green, including the `release` rehearsal.
3. Tag the merged commit and push the tag:
   ```
   git tag v0.1.0
   git push origin v0.1.0
   ```
4. Watch the `release` run. If you set a required reviewer, approve the
   `publish` job when the rest is green.
5. Open the draft under Releases. Read the notes, check the files are all
   there (six archives, checksums and its `.sigstore.json`, `VERSION`,
   `install.sh`, `install.ps1`, two SBOMs, the package manifests), and press
   Publish.
6. Check it as a user would: run the install script on a machine, then the
   commands under [Check what you downloaded](../quickstart/install.md#check-what-you-downloaded).
7. Update the package managers, below.

If a run fails after the tag was pushed, fix the cause on `main`, delete the
tag and the draft, and tag again. Do not move a tag once its release has been
published: the signatures and attestations name the commit.

## Package managers

A release carries the manifests in
`meridian-<version>-package-manifests.tar.gz`. They are generated, with the
release's URLs and checksums filled in, by `tools/release/manifests.py`. The
pipeline does not send them anywhere: each of these needs something outside
this repository, and is a decision for the owner.

| Manager | File | What publishing it takes |
|---|---|---|
| Homebrew | `packaging/homebrew/meridian.rb` | a tap repository, `ZanattaMichael/homebrew-meridian`, with the file at `Formula/meridian.rb`. Users then run `brew install zanattamichael/meridian/meridian` |
| Scoop | `packaging/scoop/meridian.json` | a bucket repository, for example `ZanattaMichael/scoop-meridian`, with the file at `bucket/meridian.json`. The manifest has `checkver` and `autoupdate`, so the bucket can update itself |
| winget | `packaging/winget/*.yaml` | a pull request to `microsoft/winget-pkgs` adding the three files under `manifests/z/ZanattaMichael/Meridian/<version>/`. Microsoft's validation runs on the pull request |
| crates.io | none | not set up. `cargo install --git` works today. Publishing to crates.io would need the crate names reserved, a version on every path dependency between the crates, and a `CARGO_REGISTRY_TOKEN` secret |

The formula and the manifests are checked for syntax in every run. They have
not been installed through Homebrew, Scoop or winget, because that needs a
published release to download from. Do that once, by hand, at the first
release.

## Known limits

- **macOS notarisation.** The macOS binaries are not signed with an Apple
  Developer ID or notarised. Installed with the script, Homebrew or `curl`
  they run. Downloaded with a browser, Gatekeeper will block them until the
  quarantine flag is removed. Fixing that needs an Apple Developer account and
  its certificate as secrets.
- **Windows code signing.** The Windows binaries are not Authenticode-signed,
  so SmartScreen may warn about an archive downloaded with a browser. Fixing
  that needs a code-signing certificate.
- **Actions are pinned by tag,** such as `actions/checkout@v4`, as in the
  repository's other workflows. Pinning the release workflow's actions by
  commit would remove the trust placed in those tags.
- **One key per platform.** Plugins and `meridian` must come from the same
  archive. Shipping a plugin on its own schedule, as the versioning plan
  intends, will need a longer-lived signing identity than a per-build key.

## Running the pieces by hand

Everything the workflow does to make an archive can be done on a workstation,
which is the quickest way to debug it:

```
cargo build --release -p meridian-cli
target/release/meridian signing keygen --secret release.key --public release.pub
export MERIDIAN_RELEASE_PUBLIC_KEY="$(cut -d' ' -f2 release.pub)"
cargo build --profile dist --locked --target x86_64-unknown-linux-musl --workspace --bins
python3 tools/release/package.py --version 0.1.0 --target x86_64-unknown-linux-musl \
    --bin-dir target/x86_64-unknown-linux-musl/dist --out dist \
    --signer target/release/meridian --key release.key
```

`tools/release/manifests.py --version 0.1.0 --dir dist` then writes the
checksums and manifests, once `dist` holds all six archives, and
`MERIDIAN_DOWNLOAD_BASE=$PWD/dist sh packaging/install/install.sh` installs
from it.
