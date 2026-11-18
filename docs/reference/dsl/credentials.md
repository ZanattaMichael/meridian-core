# Credentials

A **credential** is a typed identity for a machine, cloud, cluster or registry. Its
secret fields are [secrets](secrets.md).

```yaml
spec:
  target: ansible
  secrets:
    - name: deploy_key
      from: file
      path: /run/secrets/deploy_key
      keepTrailingNewline: true
  credentials:
    - name: deploy
      type: ssh
      user: deploy
      privateKey: ${{ secret.deploy_key }}
  targetHost:
    from: inline
    host: web01.internal
    credential: deploy
```

The user is plain text (here `ansible_user=deploy` in the inventory). The key reaches
`ansible-playbook` in a user-only file for the run.

> **Status.** Usable through `targetHost.credential` on `ansible`, `chef`, `openvox`,
> `podman`, `puppet`, `salt` and the test target. Every type can be declared and
> checked. Other uses are in [the plan](../../plans/language-expansion.md#12-credentials).

## Contents

- [Declaring a credential](#declaring-a-credential)
- [The types](#the-types)
- [Secret fields](#secret-fields)
- [Plain fields and flags](#plain-fields-and-flags)
- [`from: ambient`](#from-ambient)
- [Reaching a host as a credential](#reaching-a-host-as-a-credential)
- [What each target does](#what-each-target-does)
- [Handing a credential to a tool](#handing-a-credential-to-a-tool)
- [What each command does](#what-each-command-does)
- [Errors](#errors)
- [Examples](#examples)

## Declaring a credential

`spec.credentials` is a list, in either kind of document. Each entry:

| Field | Required | Value |
|---|---|---|
| `name` | yes | the name it is used by: letters, digits and underscores, not starting with a digit. Unique within the document |
| `type` | yes | one of [the types](#the-types) |
| `from` | no | `ambient`: the tool finds the credential for itself. See [`from: ambient`](#from-ambient) |
| the type's fields | by type | text for a plain field, a secret for a secret field, `true` or `false` for a flag |

Secret material is always a reference to a declared secret ([secret fields](#secret-fields)).

A credential must be **complete** unless `from: ambient`: an `ssh` credential needs a
`privateKey`, a `password` or `agent: true`; an `aws` one needs `accessKeyId` and
`secretAccessKey`, a `profile`, or a `roleArn`. `validate` says what is missing.

## The types

A fixed list; a misspelt field is a validation error.

| Type | Plain fields | Secret fields | Flags | Complete with | Used for |
|---|---|---|---|---|---|
| `ssh` | `user`, `certificate` | `privateKey`, `password`, `passphrase` | `agent` | `privateKey`, or `password`, or `agent: true` | hosts over SSH |
| `usernamePassword` | `user`, `domain` | `password` | | `password` | WinRM hosts, registries, basic authentication |
| `token` | `scheme` | `token` | | `token` | APIs, notification sinks, secret stores |
| `certificate` | `certificate`, `ca` | `privateKey` | | `certificate` and `privateKey` | mutual TLS, DSC |
| `aws` | `accessKeyId`, `profile`, `region`, `roleArn` | `secretAccessKey`, `sessionToken` | | `accessKeyId` and `secretAccessKey`, or `profile`, or `roleArn` | cloud endpoints on AWS, AWS secret providers |
| `azure` | `clientId`, `tenantId`, `subscriptionId` | `clientSecret`, `certificate` | `managedIdentity` | `clientId` and `clientSecret`, or `clientId` and `certificate`, or `managedIdentity: true` | cloud endpoints on Azure, Key Vault |
| `gcp` | `project`, `impersonate` | `serviceAccountKey` | | `serviceAccountKey`, or `impersonate` | cloud endpoints on Google Cloud, Secret Manager |
| `kubeconfig` | `server`, `context`, `ca` | `kubeconfig`, `token` | | `kubeconfig`, or `server` and `token` | cluster endpoints |
| `registry` | `server`, `user` | `password` | | `password` | daemon endpoints, image pulls, module registries |

`certificate` is plain in `ssh` and `certificate` credentials and secret in `azure`.

`registry`, `token` and `usernamePassword` can also be declared in a project manifest for
a module registry ([registry logins](../modules.md#registry-logins)).

## Secret fields

A secret field is exactly `${{ secret.<name> }}`.

```yaml
secrets:
  - name: win_admin
    from: keyring
    key: meridian:win_admin
credentials:
  - name: admin
    type: usernamePassword
    user: Administrator
    password: ${{ secret.win_admin }}
```

Refused at validation:

- **A literal.** `spec.credentials[0].password: a password is written in the
  document; declare a secret and read it with ${{ secret.<name> }}`. The error
  does not repeat what was written. A number or a list there is refused the
  same way.
- **A secret the document does not declare.** `password reads
  secret.root_password, which spec.secrets does not declare (declared:
  win_admin)`.
- **Anything but the one secret**: a secret joined with text, two secrets, a
  value from data or a variable. `password is one declared secret, read as it
  is: ${{ secret.<name> }} and nothing more, so that the whole of it is
  protected`. Store a composed password whole, or build it with `exec`.

The field then follows the [secret rules](secrets.md#where-a-secret-cannot-be-used) and is
fetched only by `plan`, `apply` and `rollback`.

An `optional` secret that is not found stops the stage: `spec.credentials[0].password:
password reads a secret, and the expression gives null`.

## Plain fields and flags

A plain field (user, region, server, certificate path) reaches the tool in the open.

- It may be [interpolated](interpolation.md): `user: ${{ data.deploy_user }}`.
- A secret cannot go in it: `spec.credentials[0].user: a secret cannot be used
  here` at check time; a value that only becomes secret once resolved is
  refused then, as `user is not secret: it reaches the tool in the open, so a
  secret cannot be placed in it`.
- Targets check what they can write; Ansible refuses a user name it cannot put on an
  inventory line.

A flag is `true` or `false` (default `false`). `agent: yes` is refused.

## `from: ambient`

The tool finds the credential itself (environment, profile, agent, default key), as when
no credential is named. Meridian holds no value.

```yaml
credentials:
  - name: ops
    type: ssh
    from: ambient
    user: ops
```

[`meridian secret check`](../cli.md#secret-check) reports whether it would be found,
without reading a value
([in a pipeline](../../operations/running-in-a-pipeline.md#checking-the-jobs-identity-first)).

An ambient credential needs no secret field. Its plain fields are still written
(`ansible_user=ops` above). `from` takes no other value.

## Reaching a host as a credential

`targetHost.credential` names the credential the host is reached as:

```yaml
targetHost:
  from: inline
  host: web01.internal
  credential: deploy
```

- It is a literal name, not interpolated.
- The credential must be declared in the same document's `spec.credentials`.
- A host takes an `ssh` or a `usernamePassword` credential. Any other type is
  refused: `spec.targetHost.credential: a host takes an ssh or
  usernamePassword credential; "cloud" is aws`.
- Left out, the tool's own identity is used.

A declared credential the host does not name is checked, not fetched.

## What each target does

Targets with the `credentials` capability ([capability matrix](../capability-matrix.md))
take one. For any other target the document is refused:

```
spec.targetHost names the credential "deploy", and target "dsc" does not take one: it has no way to reach its host as that identity, so the document would be applied as whoever the tool is configured to be. Leave the credential out to use the tool's own; targets that do: ansible, chef, openvox, podman, puppet, salt
```

| Target | What the artifact holds | How the secret fields reach the tool | What it refuses |
|---|---|---|---|
| `ansible` | `ansible_user=<user>` after the host in `inventory.ini`, when the credential has a user | a `privateKey` in a file only the user can read, passed as `--private-key <path>`; a `password` as `ansible_password` in the same extra-vars file that carries the document's [secrets](secrets.md#handing-a-secret-to-a-tool), passed as `--extra-vars @<path>`. Both are removed when the run ends. Password authentication over SSH needs `sshpass` on the machine that runs `ansible-playbook`, as it does for Ansible itself | a `passphrase` (add the key to an agent and set `agent: true`), a `certificate` (put it beside the key in the agent), a `domain` (for WinRM hosts, which the plugin does not reach), a user name that cannot be written on an inventory line; rule `unsupported-credential` |
| `chef` | nothing: `chef-run` is given the user as `--user` at run time | a `privateKey` through an SSH agent Meridian starts for the stage (`ssh-agent -D -a <socket>`, loaded with `ssh-add -` from standard input, the `passphrase` answered from a file only the user can read), the tool told the socket in `SSH_AUTH_SOCK` and nothing else; stopped and removed when the stage ends | a `password` (`chef-run` takes one on its command line only), a `certificate`, a `domain`, a user name with a space or line break; rule `unsupported-credential`. A credential for this machine is refused at run time: `chef-client` runs as you |
| `openvox` | nothing: OpenBolt reads an inventory written for the run | the user and a `password` in a Bolt inventory only the user can read, passed as `--inventoryfile <path>` and removed when the run ends; a `privateKey` through an SSH agent Meridian starts for the stage (`ssh-agent -D -a <socket>`, loaded with `ssh-add -` from standard input, the `passphrase` answered from a file only the user can read), the tool told the socket in `SSH_AUTH_SOCK` and nothing else; stopped and removed when the stage ends | a `certificate`, a `domain`, a user name with a space or line break; rule `unsupported-credential`. A credential for this machine is refused at run time: `puppet apply` runs as you |
| `podman` | nothing: `ssh` is given the user as `-l` at run time | a `privateKey` through an SSH agent Meridian starts for the stage (`ssh-agent -D -a <socket>`, loaded with `ssh-add -` from standard input, the `passphrase` answered from a file only the user can read), the tool told the socket in `SSH_AUTH_SOCK` and nothing else; stopped and removed when the stage ends | a `password` (`ssh` asks for one on a terminal, which a run has not), a `certificate`, a `domain`, a user name with a space or line break; rule `unsupported-credential`. A credential for this machine is refused at run time: the scripts run as you |
| `puppet` | nothing: Bolt reads an inventory written for the run | as `openvox` | as `openvox` |
| `salt` | nothing: `salt-ssh` reads a roster written for the run | the user as the roster's `user:`; a `password` as its `passwd:`, the roster then written to a file only the user can read, passed as `--roster-file <path>` and removed when the run ends; a `privateKey` as `priv: agent-forwarding` in the roster, the key itself through an SSH agent Meridian starts for the stage (`ssh-agent -D -a <socket>`, loaded with `ssh-add -` from standard input, the `passphrase` answered from a file only the user can read), the tool told the socket in `SSH_AUTH_SOCK` and nothing else; stopped and removed when the stage ends | a `certificate`, a `domain`, a user name with a space or line break; rule `unsupported-credential`. A credential for this machine is refused at run time: `salt-call` runs as you |
| `fake`, the test target | `credential.txt`: the name, type, user and the names of the secret fields | one JSON file only the user can read, and a record of the delivery that names the fields and never a value | nothing |
| any other target (`cloud-init`, `dsc`, and the infrastructure and containers targets but `podman`) | the document is refused, as above | | |

Except on `ansible`, the compiled artifact holds nothing of the credential, not even the
user. Each target page has a `Credentials` section, including any login the tool caches.

## Handing a credential to a tool

Secret fields follow the [secret route](secrets.md#handing-a-secret-to-a-tool):

- In no file Meridian keeps. The run record names the credential only.
- Never on the command line or in the environment. An SSH key goes into an SSH agent
  Meridian starts for the stage, through `ssh-add`'s stdin; the tool gets `SSH_AUTH_SOCK`
  ([0028](../../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md)).
- Fetched at the stage's turn and wiped when it ends.

A plan prints the name, type and user:

```
web (ansible) on web01.internal as deploy (ssh, user deploy)
```

See [secrets in memory](../../concepts/secrets-in-memory.md#a-credential-in-a-run).

## What each command does

| Command | Credentials |
|---|---|
| `validate` | checks every declaration against its type, and `targetHost.credential` against the declarations. Fetches nothing |
| `compile` | writes what the target knows of the credential: a user in an inventory, a name in a file. Fetches nothing: the artifact never holds a secret field |
| `plan` | fetches the secret fields of the credential the host names, hands them to the tool by its private channel, and prints the credential's name, type and user with the stage |
| `apply` | the same |
| `rollback` | fetches them again, at the stage's turn |
| `secret check` | reports each declared secret, including the ones credentials read |

## Errors

| Error | Meaning |
|---|---|
| `a credential needs a name; it is used by that name` | `name` is missing |
| `"has-dash" is not a usable name` | the name is not letters, digits and underscores |
| `the credential "deploy" is declared twice (first at site.yaml:12:7)` | two entries share a name |
| `a credential needs a type: one of aws, azure, ...` | `type` is missing |
| `"vault" is not a credential type this version has; type is one of ...` | an unknown type |
| `from is ambient, or left out: ambient means the tool finds the credential for itself, as it does when none is named` | `from` is something else |
| `region is not a field of an ssh credential; its fields are agent, certificate, passphrase, password, privateKey, user` | a field another type has |
| `unknown field "port"` | a field no type has, refused when the file is read |
| `a password is written in the document; declare a secret and read it with ${{ secret.<name> }}` | a literal in a secret field |
| `password reads secret.root_password, which spec.secrets does not declare` | an undeclared secret |
| `password is one declared secret, read as it is: ${{ secret.<name> }} and nothing more` | an expression in a secret field |
| `agent is true or false` | a flag that is not |
| `user is text, found a list` | a plain field that is not text |
| `a secret cannot be used here` | a secret read by a plain field |
| `an ssh credential needs privateKey, or password, or agent: true; or from: ambient, for one the tool finds itself` | an incomplete credential |
| `no credential named "deploy" is declared in spec.credentials` | `targetHost.credential` names no declaration |
| `a host takes an ssh or usernamePassword credential; "cloud" is aws` | the wrong type for a host |
| `a credential is named as written ... it is not interpolated` | an interpolation in `targetHost.credential` |
| `spec.targetHost names the credential "deploy", and target "dsc" does not take one` | a target without the capability, at `compile`, `plan` and `apply` |
| `the credential "deploy": ansible-playbook takes no passphrase for a private key` | the Ansible plugin's refusals, rule `unsupported-credential` |
| `the credential "deploy": ssh takes no password for a host: ssh asks for a password on a terminal, which a run has not; give the credential a privateKey instead` | a plugin whose tool has no private way to take a password (`podman`, `chef`), rule `unsupported-credential` |
| `the credential "deploy": salt-ssh takes no certificate for a key from Meridian; put it beside the key in an SSH agent of your own and set agent: true` | a certificate, on any target reached over SSH |
| `the document names the credential "deploy" for its host, and the host is this machine, which salt-call configures as the user running Meridian; leave the credential out, or name another host` | a credential for `localhost`, at `plan` and `apply` |
| `ssh-add could not add the credential's key to the agent` | the key is not one `ssh-add` reads, or its passphrase is wrong; what `ssh-add` said follows |

## Examples

| Example | Shows |
|---|---|
| [`credential-for-a-host.yaml`](../../../examples/language/credential-for-a-host.yaml) | Ansible reaching a host as the deploy user with a private key from a file: the user in the inventory, the key in a private file passed as `--private-key`, and the plan line that names the identity |

```
meridian plan examples/language/credential-for-a-host.yaml
```
