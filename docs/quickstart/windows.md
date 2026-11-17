# Meridian on Windows

Meridian runs on Windows as it does on Linux and macOS: the same command, the
same documents, the same plugins. The whole test suite runs on Windows on
every change. This page is what is different there, and what to expect of
each target.

## Install

```
irm https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.ps1 | iex
```

or Scoop, winget or a build from source: see [Install](install.md). Releases
are built for x86-64 and for arm64 Windows.

## What is different

**Paths.** Give paths the Windows way or with forward slashes; both work. A
list of directories in an environment variable is separated by `;`, as `PATH`
is, where the other systems use `:`. That applies to `MERIDIAN_PLUGIN_DIR` and
`MERIDIAN_TRUSTED_KEYS`:

```
$env:MERIDIAN_PLUGIN_DIR = 'C:\meridian\plugins;D:\more-plugins'
```

**Plugin binaries** are named `meridian-target-<name>.exe`, and a plugin's
signature is `meridian-target-<name>.exe.sig`.

**Finding a tool.** Meridian runs each target's tool by name, and on Windows a
tool is as often a script as a program: `az` is `az.cmd`, and Puppet and Chef
install `.bat` files. Meridian looks a name up as a Windows shell does, through
`PATH`, trying `.exe`, `.cmd`, `.bat` and `.com` in that order in each
directory. So a tool that runs when you type its name in a terminal runs for
Meridian too. To use a tool that is not on the `PATH`, give its full path in
the target's own variable, such as `MERIDIAN_TERRAFORM`; each
[target page](../index.md#user-documentation) names its variable.

**The work directory** a tool is given is a plain path such as
`C:\work\.meridian\work\web`, never the `\\?\C:\...` form, which many
command-line tools do not accept.

**How a plugin is reached.** `meridian` starts each plugin as a process and
talks to it over a private channel. On Linux and macOS that is a Unix socket
in a directory only you can enter. On Windows it is a named pipe: it refuses
connections from other machines, and only your own account, administrators
and the system can write to it, so no other account can send a plugin a
request. Another local account can still open the pipe for reading, which
gets it nothing a plugin says unasked, since a plugin only answers requests.
Nothing is left on disk when the plugin exits. Nothing listens on a network
port on any system.

## Which targets make sense on Windows

Compiling works for every target on every system: it needs nothing but
Meridian. Planning and applying run the target's own tool on the machine
Meridian runs on, so they work where that tool does. The table says what to
expect from how each tool behaves on Windows; [the next section](#what-has-and-has-not-been-tested-on-windows)
says what has actually been run there.

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

Every plugin's compile tests, the plugin protocol, the orchestrator, the
command line and plugin signatures run on Windows in CI, on every pull
request. The runner tests, which drive a stand-in for each tool, run there
too.

What has not been done on Windows is a run against the real tools. The
workflows that install a real tool and apply with it are Linux jobs. If a
target misbehaves on Windows with its real tool,
[open an issue](https://github.com/ZanattaMichael/meridian-core/issues) and say
which tool and version.

A few tests are left out on Windows, each with its reason beside it in the
source: the ones that drive a real tool, and the host's tests of a broken
plugin, whose broken plugins are shell scripts that Windows cannot start as
programs.
