# Install

Meridian is the `meridian` command and one plugin binary per target. They are
installed together, in one directory, and `meridian` finds the plugins beside
it. Releases are built for Linux, macOS and Windows, each on x86-64 and arm64.

> **No release has been published yet.** Until the first one is, [build from
> source](#build-from-source). The other ways to install on this page are how
> a release is installed, and CI runs each of them against every change to the
> release pipeline; they start working when the first release is published.

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
release's checksums, and unpacks it into `~/.meridian/bin`. It installs
nothing anywhere else and does not need root. It then tells you how to put
that directory on your `PATH` if it is not there.

| Setting | Meaning |
|---|---|
| `MERIDIAN_VERSION` | the version to install, as in `0.1.0`. Default: the latest release |
| `MERIDIAN_INSTALL_DIR` | where to put it. Default: `~/.meridian/bin` |
| `MERIDIAN_VERIFY` | `cosign` to refuse to install unless [the release's signature](#check-what-you-downloaded) verifies; `none` to skip that check. Default: verify when `cosign` is installed |
| `MERIDIAN_DOWNLOAD_BASE` | a URL or a directory holding the release's files, in place of GitHub: for a mirror or a machine with no route to GitHub |

The Linux binaries are statically linked, so they run on any distribution,
including Alpine.

## Windows

In PowerShell:

```
irm https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.ps1 | iex
```

It installs into `%LOCALAPPDATA%\Meridian\bin`, adds that directory to your
user `PATH`, and takes the same settings as the script above, as environment
variables. Set `MERIDIAN_NO_PATH` to leave your `PATH` alone. No administrator
rights are needed. [Meridian on Windows](windows.md) covers what differs there.

## Homebrew

A release carries a Homebrew formula. Once the project's tap is published:

```
brew install zanattamichael/meridian/meridian
```

## Scoop and winget

A release carries a Scoop manifest and winget manifests. Once they are
published to a bucket and to winget:

```
scoop install meridian
winget install ZanattaMichael.Meridian
```

## Container image

```
docker run --rm -v "$PWD:/work" ghcr.io/zanattamichael/meridian:latest compile site.yaml
```

The image is for `linux/amd64` and `linux/arm64`. It holds `meridian` and every
plugin, runs as an unprivileged user, and uses `/work` as its working
directory. It compiles and validates as it stands. Planning and applying run
the target's own tool, which the image does not carry; build an image of your
own from it and add the tools you use:

```
FROM ghcr.io/zanattamichael/meridian:0.1.0
USER root
RUN apk add --no-cache opentofu
USER meridian
```

## cargo install

With a Rust toolchain, the command and the plugins you want can be built and
installed into `~/.cargo/bin` in one step. Name `meridian-cli` and each
plugin's package, `meridian-target-<name>`:

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

That produces the `meridian` command and one binary per plugin, all in
`target/release`:

```
meridian
meridian-target-ansible
meridian-target-cloud-init
meridian-target-openvox
meridian-target-terraform
meridian-target-opentofu
meridian-target-pulumi
meridian-target-helm
meridian-target-packer
...
```

`meridian` finds plugins in the directory it sits in, so the build output works
as it stands. To keep plugins somewhere else, name the directories with
`--plugin-dir` (it can be given more than once) or `MERIDIAN_PLUGIN_DIR`
(separated by `:`, or by `;` on Windows).

## Check what you downloaded

Every release file can be traced to the commit and the workflow run that built
it. Three checks are available, from the one every install does to the one
that needs nothing but GitHub's own tool.

**Checksums.** `meridian-<version>-checksums.txt` lists the SHA-256 of every
archive. The install scripts refuse an archive that does not match. By hand:

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

**Build provenance.** Each archive and the image have a provenance attestation
saying which commit and which workflow run produced them:

```
gh attestation verify meridian-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo ZanattaMichael/meridian-core
```

**What it is made of.** A release lists every crate it was built from, by
version, in `meridian-<version>-sbom.spdx.json` and `meridian-<version>-sbom.cdx.json`
(SPDX and CycloneDX).

**The plugins.** A released `meridian` checks each plugin's signature every
time it starts one, and does not start a plugin that fails.
[Plugin signatures](../reference/plugin-signatures.md) explains what is checked
and how to use a plugin from somewhere else.

## Check what is installed

```
meridian plugins
```

lists every plugin Meridian can find, with its category, what it can do
(compile, plan, apply, outputs), the resource types it takes, and what checking
its signature found.

## The tools themselves

Meridian compiles a document into a tool's own files, and then runs that tool.
Compiling needs nothing but Meridian. Planning and applying need the tool
installed on the machine Meridian runs on, exactly as if you were running it by
hand:

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

Two targets can run `tofu`. Pick `opentofu` when OpenTofu is the tool you use: it writes
`main.tofu.json`, which only OpenTofu reads, and it gives you OpenTofu's own features,
such as state and plan encryption, variables, local values and ephemeral resources. Pick
`terraform` when the same document must also run with `terraform`; it writes
`main.tf.json` and can be pointed at `tofu` with `MERIDIAN_TERRAFORM`. The
[OpenTofu page](../targets/opentofu.md#opentofu-or-terraform) compares the two.

The build also produces `meridian-policy-opa`, which `--policy` uses to ask
[a policy](../reference/policy.md) whether a change may be made. It needs
`opa`, Open Policy Agent 1.0 or later, and only when you pass `--policy`.

Each tool keeps using its own credentials and configuration. Meridian does not
store or pass any: if `aws sts get-caller-identity` or `kubectl get nodes` works
in your shell, the plugin will work too. Each [target's page](../targets/ansible.md)
lists the environment variables that point a plugin at a different binary or add
arguments to every call.

Next: [your first ResourceSet](first-resourceset.md).
