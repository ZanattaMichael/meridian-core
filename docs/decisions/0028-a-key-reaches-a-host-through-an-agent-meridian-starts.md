# A key reaches a host through an agent Meridian starts

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Phase 7 part 6: credentials on Podman, Salt, Puppet, OpenVox and Chef through an SSH agent" |
| Where it shows | [Credentials](../reference/dsl/credentials.md#what-each-target-does), [Podman](../targets/podman.md#credentials), [Salt](../targets/salt.md#credentials), [Puppet](../targets/puppet.md#credentials), [OpenVox](../targets/openvox.md#credentials), [Chef](../targets/chef.md#credentials), [Secrets in memory](../concepts/secrets-in-memory.md#a-credential-in-a-run) |

## Context

A document reaches its host as a credential, `targetHost.credential`, whose private key is
a secret of the document. The tools these targets reach a host with (`ssh`, `salt-ssh`,
`bolt`, `chef-run`) each take a key in two ways: from a file named on the command line, or
from an SSH agent named by `SSH_AUTH_SOCK`. The [plan's table](../plans/language-expansion.md#116-handing-a-secret-to-a-tool)
ranks the agent first and a private file second. The Ansible target, built earlier, went
by the file (`--private-key`).

## The choice

On these five targets a key never touches a disk. The plugin starts an agent of its own for
the stage, `ssh-agent -D -a <socket>` in a directory only the user can enter, adds the key
with `ssh-add -` from standard input, answers a passphrase from a `0600` file through an
askpass program that prints that file, tells the tool the socket in `SSH_AUTH_SOCK` and
nothing else, and kills the agent and removes the directory when the stage ends. The SDK
does it once (`meridian_sdk::agent`), and a plugin adds one line.

The rest of the credential goes the way each tool takes it. The user is an option (`ssh -l`,
`chef-run --user`) or a line in a file the plugin already writes for the run (`salt-ssh`'s
roster, Bolt's inventory). A password goes only where the tool reads it from a private file:
`salt-ssh`'s roster and Bolt's inventory, which are then `0600` files named on the command
line and removed with the stage. `ssh` reads a password from a terminal only and `chef-run`
from its command line only, so `podman` and `chef` refuse a password at compile and say to
use a key. Every target refuses a `certificate` (it would need a file beside the key) and a
`domain` (WinRM), and a credential for the machine Meridian runs on is refused at run time,
because the local tool runs as the user running Meridian whoever the document names.

The compiled artifact holds nothing of the credential on these targets, not even the user,
unlike Ansible's inventory: the identity is a run-time matter, and the same artifact can be
applied as another one.

## Alternatives

- A `0600` key file named on the command line, as Ansible does. Rejected: the agent is the
  table's rank 1, the key then exists nowhere but in two processes' memory, and a passphrase
  works without a terminal, which Ansible's file route refuses.
- Using the user's own agent for every key (`ssh-add` into `SSH_AUTH_SOCK`). Rejected: the
  key would outlive the stage in an agent Meridian does not control, and a run with no agent
  would have none. `agent: true` still names a key the user keeps there.
- Passing a password to `ssh` with `sshpass`, or to `chef-run` as `--password`. Rejected:
  `sshpass` is another program to require, and `--password` puts the value where every
  user of the machine reads it.
- Moving Ansible to the agent too. Not done here: Ansible's route is built, documented and
  tested against the real tool; changing it is a choice for the owner.

## Status

Proposed. Open to the owner: whether Ansible should take the agent route as well, and
whether a credential for `localhost` should instead be ignored with a warning.
