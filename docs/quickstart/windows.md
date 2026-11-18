# Meridian on Windows

Meridian runs on Windows with the same command, documents and plugins. The test suite
runs on Windows on every change.

## Install

```
irm https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.ps1 | iex
```

or another method from [Install](install.md). Releases are built for x86-64 and
arm64 Windows.

## What is different

**Paths.** Backslashes and forward slashes both work. Directory lists in
environment variables use `;`, as `PATH` does, instead of `:`. This applies to
`MERIDIAN_PLUGIN_DIR` and `MERIDIAN_TRUSTED_KEYS`:

```
$env:MERIDIAN_PLUGIN_DIR = 'C:\meridian\plugins;D:\more-plugins'
```

**Plugin binaries** are named `meridian-target-<name>.exe`, and a plugin's
signature is `meridian-target-<name>.exe.sig`.

**Finding a tool.** Meridian searches `PATH` for `.exe`, `.cmd`, `.bat` and `.com`, in
that order, so `az.cmd` and Puppet's and Chef's `.bat` files are found. For a tool not on
`PATH`, set the target's variable, such as `MERIDIAN_TERRAFORM`.

**Work directories** are plain paths such as `C:\work\.meridian\work\web`,
never the `\\?\C:\...` form, which many tools reject.

**Plugin channel.** Each plugin is reached over a named pipe that refuses remote
connections and that only your account, administrators and SYSTEM can write to (a
private Unix socket on Linux and macOS). No network port is opened.

## Which targets make sense on Windows

Compiling works for every target. Planning and applying work wherever the tool does
([what has been tested](#what-has-and-has-not-been-tested-on-windows)).

| Target | Expected on Windows |
|---|---|
| `dsc` | yes: this is where most DSC users are. Needs PowerShell 7 (`pwsh`) and the Dsc.PipelineRunner module |
| `terraform`, `opentofu`, `pulumi`, `packer` | yes |
| `bicep`, `arm` | yes, with the `az` command line |
| `cloudformation` | yes, with the `aws` command line |
| `kubernetes`, `helm` | yes, with `kubectl` and `helm` |
| `docker` | yes, with Docker Desktop or another Docker with the Compose plugin |
| `puppet`, `openvox`, `chef` | yes, where the tool is installed; `bolt` and `chef-run` reach other hosts |
| `ansible` | compile only: Ansible's control node does not run on Windows. Run Meridian in WSL or the [container image](install.md#container-image) to apply |
| `salt` | `salt-call` on the machine itself; `salt-ssh` does not run from Windows |
| `podman` | compile only on the machine itself, since units are for systemd; applying to a Linux host over `ssh` works |
| `cloud-init` | compile, and the hand-off to another stage; the `cloud-init` check needs Linux and can be turned off |

## What has and has not been tested on Windows

- Tested on Windows on every pull request: compile tests, the plugin protocol, the
  orchestrator, the command line, plugin signatures, and runner tests against stand-ins.
- Not tested on Windows: any real tool. Real-tool workflows run on Linux.
  [Open an issue](https://github.com/ZanattaMichael/meridian-core/issues) with the tool and
  version if one misbehaves.
- Skipped on Windows: real-tool tests and the host's broken-plugin tests (shell
  scripts).
