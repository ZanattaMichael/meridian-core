# Install

Meridian is the `meridian` command plus one plugin binary per target, installed
together in one directory. `meridian` finds the plugins beside it. Releases are
built for Linux, macOS and Windows, on x86-64 and arm64.

> The Homebrew tap, Scoop bucket and winget package are not published yet.
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

The script downloads the archive for your machine, checks it against the
release checksums, and unpacks it into `~/.meridian/bin`. It writes nothing
else and needs no root. If that directory is not on your `PATH`, it tells you
how to add it.

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

It installs into `%LOCALAPPDATA%\Meridian\bin`, adds that to your user `PATH`,
and takes the same environment variables as the script above. Set
`MERIDIAN_NO_PATH` to leave `PATH` alone. No administrator rights are needed.
[Meridian on Windows](windows.md) covers what differs there.

## Homebrew

Each release carries a Homebrew formula. Once the tap is published:

```
brew install zanattamichael/meridian/meridian
```

## Scoop and winget

Each release carries Scoop and winget manifests. Once they are published:

```
scoop install meridian
winget install ZanattaMichael.Meridian
```

## Container image

```
docker run --rm -v "$PWD:/work" ghcr.io/zanattamichael/meridian:latest compile site.yaml
```

The image is for `linux/amd64` and `linux/arm64`. It holds `meridian` and every
plugin, runs as an unprivileged user, and works in `/work`. It can compile and
validate. Planning and applying need the target's own tool, which the image
does not include, so build on it and add the tools you use:

```
FROM ghcr.io/zanattamichael/meridian:0.1.0
USER root
RUN apk add --no-cache opentofu
USER meridian
```

## cargo install

With a Rust toolchain, build and install the command and the plugins you want
into `~/.cargo/bin` in one step. Name `meridian-cli` and each plugin package,
`meridian-target-<name>`:

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

`meridian` finds plugins in its own directory, so the build output works as is.
To keep plugins elsewhere, use `--plugin-dir` (repeatable) or
`MERIDIAN_PLUGIN_DIR` (separated by `:`, or `;` on Windows).

## Check what you downloaded

Every release file traces back to the commit and workflow run that built it.

**Checksums.** `meridian-<version>-checksums.txt` lists the SHA-256 of every
archive. The install scripts refuse a mismatch. By hand:

```
sha256sum --check --ignore-missing meridian-0.1.0-checksums.txt
```

**The release's signature.** The checksums file is signed with
[Sigstore](https://www.sigstore.dev), with no long-lived key: the signature
names the workflow that made it. The install scripts check it when
[`cosign`](https://docs.sigstore.dev/cosign/system_config/installation/) is
installed. By hand:

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

**Build provenance.** Each archive and the image has an attestation naming the commit
and workflow run that produced it:

```
gh attestation verify meridian-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo ZanattaMichael/meridian-core
```

**What it is made of.** Every crate in the build is listed, by version, in `meridian-<version>-sbom.spdx.json` and `meridian-<version>-sbom.cdx.json`
(SPDX and CycloneDX).

**The plugins.** A released `meridian` checks a plugin's signature every time it
starts one, and refuses a plugin that fails.
[Plugin signatures](../reference/plugin-signatures.md) explains what is checked
and how to use a plugin from elsewhere.

## Check what is installed

```
meridian plugins
```

lists every plugin Meridian finds: its category, what it can do (compile, plan,
apply, outputs), its resource types, and its signature check result.

## The tools themselves

Compiling needs only Meridian. Planning and applying need the tool installed
on the machine running Meridian, as if you ran it by hand:

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

The build also produces `meridian-policy-opa`, which `--policy` uses to ask
[a policy](../reference/policy.md) whether a change is allowed. It needs
`opa` (Open Policy Agent 1.0 or later), only when you pass `--policy`.

Each tool uses its own credentials and configuration. Meridian stores none: if
`aws sts get-caller-identity` or `kubectl get nodes` works in your shell, the
plugin works too. Each [target's page](../targets/ansible.md) lists the
environment variables that point a plugin at another binary or add arguments
to every call.

Next: [your first ResourceSet](first-resourceset.md).
