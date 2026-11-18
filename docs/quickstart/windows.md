# Meridian on Windows

Meridian runs on Windows with the same command, documents and plugins as on
Linux and macOS. The full test suite runs on Windows on every change. This page
covers what differs and what to expect of each target.

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

**Finding a tool.** On Windows many tools are scripts: `az` is `az.cmd`, and
Puppet and Chef install `.bat` files. Meridian looks names up as a Windows
shell does, through `PATH`, trying `.exe`, `.cmd`, `.bat` and `.com` in that
order. If a tool runs when you type its name, it runs for Meridian. For a tool
not on `PATH`, set its full path in the target's variable, such as
`MERIDIAN_TERRAFORM`; each [target page](../index.md) names
it.

**Work directories** are plain paths such as `C:\work\.meridian\work\web`,
never the `\\?\C:\...` form, which many tools reject.

**How a plugin is reached.** `meridian` starts each plugin as a process and
talks to it over a private channel. On Linux and macOS that is a Unix socket in
a directory only you can enter. On Windows it is a named pipe that refuses
remote connections and that only your account, administrators and SYSTEM can
write to, so no other account can send a plugin a request. Other local accounts
can open it for reading, which yields nothing, since a plugin only answers
requests. Nothing is left on disk when the plugin exits, and nothing listens on
a network port on any system.

## Which targets make sense on Windows

Compiling works for every target on every system. Planning and applying run the
target's tool locally, so they work wherever that tool does. The table shows
what to expect; [the next section](#what-has-and-has-not-been-tested-on-windows)
shows what has actually been run.

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

CI runs every plugin's compile tests, the plugin protocol, the orchestrator, the
command line, plugin signatures and the runner tests (against stand-ins) on
Windows for every pull request.

No real tool has been run on Windows: the real-tool workflows are Linux jobs. If
a target misbehaves on Windows with its real tool,
[open an issue](https://github.com/ZanattaMichael/meridian-core/issues) with the
tool and version.

A few tests are skipped on Windows, each with its reason in the source: those
that drive a real tool, and the host's broken-plugin tests, whose plugins are
shell scripts Windows cannot run.
