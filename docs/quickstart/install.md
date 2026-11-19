# Install

Meridian is the `meridian` command plus one plugin binary per target, in one directory.
Releases are built for Linux, macOS and Windows, on x86-64 and arm64.

> The Homebrew tap, Scoop bucket and winget package are not public yet.
> Use the install scripts, the container image, `cargo install`, or
> [build from source](#build-from-source).

| You have | Use |
|---|---|
| Linux or macOS | [the install script](#linux-and-macos), or [Homebrew](#homebrew) |
| Windows | [the install script](#windows), [Scoop or winget](#scoop-and-winget). See also [Meridian on Windows](windows.md) |
| Docker | [the container image](#container-image) |
| a Rust toolchain | [`cargo install`](#cargo-install), or [build from source](#build-from-source) |

## Linux and macOS

```
curl -fsSL https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.sh | sh
```

The script checks the archive against the release checksums and unpacks it into
`~/.meridian/bin`. It needs no root.

| Setting | Meaning |
|---|---|
| `MERIDIAN_VERSION` | the version to install, as in `0.1.0`. Default: the latest release |
| `MERIDIAN_INSTALL_DIR` | where to put it. Default: `~/.meridian/bin` |
| `MERIDIAN_VERIFY` | `cosign` to refuse to install unless [the release's signature](#check-what-you-downloaded) verifies; `none` to skip that check. Default: verify when `cosign` is installed |
| `MERIDIAN_DOWNLOAD_BASE` | a URL or a directory holding the release's files, in place of GitHub: for a mirror or a machine with no route to GitHub |

Linux binaries are statically linked and run on any distribution, including
Alpine.

## Windows

In PowerShell:

```
irm https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.ps1 | iex
```

It installs into `%LOCALAPPDATA%\Meridian\bin` and adds that to your user `PATH`
(`MERIDIAN_NO_PATH` skips this). It needs no administrator rights. See
[Meridian on Windows](windows.md).

## Nightly

The `nightly` pre-release is built from `main` each night it changes. It is unstable, and
has no Sigstore signature: check it with `gh attestation verify <file> --repo
ZanattaMichael/meridian-core`.

```
base=https://github.com/ZanattaMichael/meridian-core/releases/download/nightly
curl -fsSL "$base/install.sh" | MERIDIAN_DOWNLOAD_BASE="$base" sh
```

```
$env:MERIDIAN_DOWNLOAD_BASE = 'https://github.com/ZanattaMichael/meridian-core/releases/download/nightly'
irm "$env:MERIDIAN_DOWNLOAD_BASE/install.ps1" | iex
```

A nightly's version is a pre-release of the next patch, as in `0.1.1-nightly.20261119.g1a2b3c4`.
A module's `spec.meridian` is checked against the release it leads to (`0.1.1`).

## Homebrew

A release carries a Homebrew formula, kept in the project's tap. Once the tap
is public:

```
brew install zanattamichael/meridian/meridian
```

## Scoop and winget

A release carries a Scoop manifest, kept in the project's bucket, and winget
manifests, submitted to winget's community repository. Once the bucket is
public and the winget submission is merged:

```
scoop bucket add meridian https://github.com/ZanattaMichael/scoop-meridian
scoop install meridian
winget install ZanattaMichael.Meridian
```

Neither the archives nor the package-manager listings carry an Apple or
Microsoft code signature. Installed these ways, or with the scripts above,
that changes nothing. An archive downloaded in a browser is quarantined, and
macOS or SmartScreen will warn before it runs; what a release does sign is
under [Check what you downloaded](#check-what-you-downloaded).

## Container image

```
docker run --rm -v "$PWD:/work" ghcr.io/zanattamichael/meridian:latest compile site.yaml
```

The image (`linux/amd64`, `linux/arm64`) holds `meridian` and every plugin, runs
unprivileged, and works in `/work`. It does not include the tools, so add the ones you
need to plan and apply:

```
FROM ghcr.io/zanattamichael/meridian:0.1.0
USER root
RUN apk add --no-cache opentofu
USER meridian
```

## cargo install

Installs into `~/.cargo/bin`. Name `meridian-cli` and each `meridian-target-<name>`:

```
cargo install --locked --git https://github.com/ZanattaMichael/meridian-core \
    meridian-cli meridian-target-terraform meridian-target-ansible
```

Add `--tag v0.1.0` to build a released version. Meridian is not on crates.io.

## Build from source

You need a stable Rust toolchain.

```
git clone https://github.com/ZanattaMichael/meridian-core
cd meridian-core
cargo build --workspace --release
```

This builds `meridian` and one binary per plugin in `target/release`:

```
meridian
meridian-target-ansible
meridian-target-arm
meridian-target-bicep
meridian-target-chef
meridian-target-cloud-init
meridian-target-cloudformation
meridian-target-docker
meridian-target-dsc
meridian-target-helm
meridian-target-kubernetes
meridian-target-opentofu
meridian-target-openvox
meridian-target-packer
meridian-target-podman
meridian-target-pulumi
meridian-target-puppet
meridian-target-salt
meridian-target-terraform
```

Plugins elsewhere are found with `--plugin-dir` (repeatable) or `MERIDIAN_PLUGIN_DIR`
(separated by `:`, or `;` on Windows).

## Check what you downloaded

**Checksums.** `meridian-<version>-checksums.txt` lists the SHA-256 of every
archive. The install scripts refuse a mismatch. By hand:

```
sha256sum --check --ignore-missing meridian-0.1.0-checksums.txt
```

**Signature.** The checksums file is signed keylessly with
[Sigstore](https://www.sigstore.dev). The install scripts check it when
[`cosign`](https://docs.sigstore.dev/cosign/system_config/installation/) is installed. By
hand:

```
cosign verify-blob \
  --bundle meridian-0.1.0-checksums.txt.sigstore.json \
  --certificate-identity-regexp '^https://github\.com/ZanattaMichael/meridian-core/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  meridian-0.1.0-checksums.txt
```

The container image is signed the same way:

```
cosign verify ghcr.io/zanattamichael/meridian:0.1.0 \
  --certificate-identity-regexp '^https://github\.com/ZanattaMichael/meridian-core/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

**Provenance.** Each archive and the image has an attestation naming its commit and
workflow run:

```
gh attestation verify meridian-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo ZanattaMichael/meridian-core
```

**SBOM.** `meridian-<version>-sbom.spdx.json` and `meridian-<version>-sbom.cdx.json`.

**Plugins.** A released `meridian` checks each plugin's signature when it starts it
([plugin signatures](../reference/plugin-signatures.md)).

## Check what is installed

```
meridian plugins
```

lists each plugin's category, capabilities, resource types and signature result.

## The tools themselves

Compiling needs only Meridian. Planning and applying need the tool installed:

| Target | Needs |
|---|---|
| `ansible` | `ansible-playbook` |
| `puppet` | `puppet`, or `bolt` to reach another host |
| `openvox` | OpenVox's `puppet`, or OpenBolt's `bolt` to reach another host |
| `chef` | `chef-client`, or `chef-run` to reach another host |
| `salt` | `salt-call`, or `salt-ssh` to reach another host |
| `dsc` | PowerShell (`pwsh`) with the Dsc.PipelineRunner module |
| `cloud-init` | `cloud-init`, to check the user-data (Linux only; the check can be turned off). `ssh` as well, if an apply is to wait for the machine's first boot |
| `terraform` | `terraform` (or `tofu`) |
| `opentofu` | `tofu`, version 1.8 or later |
| `cloudformation` | the `aws` command line |
| `bicep` | the `az` command line |
| `arm` | the `az` command line |
| `packer` | `packer` 1.7 or later |
| `pulumi` | `pulumi` |
| `docker` | `docker` with the Compose plugin |
| `podman` | `podman` 4.4 or later on a Linux host with systemd |
| `kubernetes` | `kubectl` |
| `helm` | `helm`; `kustomize` when a document has an overlay; `kubectl` when an output reads from the cluster |

Two targets can run `tofu`:

- `opentofu` if OpenTofu is your tool. It writes `main.tofu.json`, which only OpenTofu
  reads, and supports OpenTofu features such as state and plan encryption, variables,
  local values and ephemeral resources.
- `terraform` if the document must also run with `terraform`. It writes `main.tf.json`;
  point it at `tofu` with `MERIDIAN_TERRAFORM`.

The [OpenTofu page](../targets/opentofu.md#opentofu-or-terraform) compares them.

`--policy` needs `opa` (Open Policy Agent 1.0 or later); see [policy](../reference/policy.md).
`--analyze checkov` needs `checkov`, and `--analyze trivy` needs `trivy`; see [analyzers](../reference/analyzers.md).

Each tool uses its own credentials and configuration; Meridian stores none. Each
[target's page](../targets/ansible.md) lists the environment variables that set the
binary or extra arguments.

Next: [your first ResourceSet](first-resourceset.md).
