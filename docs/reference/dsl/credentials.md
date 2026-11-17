# Credentials

A [secret](secrets.md) is one hidden value. A **credential** is an identity: a
typed bundle that says how to prove who you are to a machine, a cloud, a
cluster or a registry. Declaring a credential by name keeps "who am I to this
system" out of every document that only needs to say "reach the host as the
deploy identity".

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

The user is plain text and goes where the tool reads it: here, into the
inventory as `ansible_user=deploy`. The key is a secret: it is never written
in the document, never in a compiled file, and reaches `ansible-playbook` in
a file only the user can read, for the length of the run.

> **Where a credential can be used today.** A credential is used by
> `targetHost.credential`, by a target that
> [takes one](#what-each-target-does): today `ansible`, and the test target.
> Every credential type is declared and checked, so a document can name the
> identities it will use before every use of them is built: endpoints, a
> target's settings, secret stores and notification sinks are in
> [the plan](../../plans/language-expansion.md#12-credentials).

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

A credential has no field for a value. Every field that holds secret material
is a reference to a declared secret, and nothing else: see
[secret fields](#secret-fields).

A credential must be **complete**: one of the ways its type lists must be
wholly there, unless the credential is `from: ambient`. An `ssh` credential
needs a `privateKey`, or a `password`, or `agent: true`. An `aws` one needs
`accessKeyId` and `secretAccessKey`, or a `profile`, or a `roleArn`.
`validate` refuses one that is not complete, and says what would complete it.

## The types

Credential types are a fixed, checked list, so a misspelt field is an error
when the document is validated and not an authentication failure an hour
later. Every type is declared and checked today; where each can be used is in
[what each target does](#what-each-target-does).

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

A `certificate` field is plain in an `ssh` or a `certificate` credential (a
path to, or the text of, a public certificate) and secret in an `azure` one
(the certificate holds the key). The table says which.

Three of the types, `registry`, `token` and `usernamePassword`, can also be
declared outside a run, in a project's manifest, for a module registry: see
[registry logins](../modules.md#registry-logins). The manifest's rules are the
same.

## Secret fields

A field that holds secret material is **exactly one declared secret, read as
it is**: `${{ secret.<name> }}` and nothing more.

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

Three things are refused when the document is validated, before anything is
fetched:

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
  protected`. A password that has to be made from parts is a secret of its
  own: make it with `exec`, or store it whole.

Once the document is resolved, the field *is* the secret: the same
[rules](secrets.md#where-a-secret-cannot-be-used) hold as for a secret
anywhere else. It is never in a compiled file, a plan, a log, an event or the
run record, and it is fetched only by `plan`, `apply` and `rollback`, at the
stage's turn.

An `optional` secret that no place has makes the credential incomplete at that
moment: `spec.credentials[0].password: password reads a secret, and the
expression gives null`, and the stage stops. A credential is an identity, and
half of one is no use.

## Plain fields and flags

A plain field is text: a user name, a region, a server, a path to a
certificate. It reaches the tool in the open, on its command line or in its
inventory, so:

- It may be [interpolated](interpolation.md) from data and variables, like
  any other text: `user: ${{ data.deploy_user }}`.
- A secret cannot be placed in it. `spec.credentials[0].user: a secret cannot
  be used here`, when the document is checked; a value that only becomes a
  secret once resolved is refused then, as `user is not secret: it reaches
  the tool in the open, so a secret cannot be placed in it`.
- It is not checked beyond being text. A target checks what it can write: the
  Ansible plugin refuses a user name it cannot put on an inventory line.

A flag is `true` or `false`, and is `false` when left out. `agent: yes` is
refused: `agent is true or false`.

## `from: ambient`

`from: ambient` means "whatever the tool finds for itself": its environment,
its profile, its agent, its default key. It is what happens today when a
document names no credential, and it lets a credential be named and typed
without Meridian ever holding a value of it.

```yaml
credentials:
  - name: ops
    type: ssh
    from: ambient
    user: ops
```

An ambient credential needs no secret field and need not be complete: the tool
has the rest. Its plain fields still go where they go, so the inventory above
says `ansible_user=ops` and the SSH agent supplies the key. `from` takes no
other value: a credential's secrets say where they come from, one by one.

## Reaching a host as a credential

`targetHost.credential` names the credential the host is reached as:

```yaml
targetHost:
  from: inline
  host: web01.internal
  credential: deploy
```

- It is a name, written as it is: not interpolated, so that which identity
  reaches which host can be read off the document.
- The credential must be declared in the same document's `spec.credentials`.
- A host takes an `ssh` or a `usernamePassword` credential. Any other type is
  refused: `spec.targetHost.credential: a host takes an ssh or
  usernamePassword credential; "cloud" is aws`.
- Left out, the host is reached as the tool's own identity, as before.

A credential that is declared but not named by the host is checked like any
other, and is fetched and handed to no one.

## What each target does

A target that reaches its host as a credential declares the `credentials`
capability, published in the [capability matrix](../capability-matrix.md).
The core refuses, before anything runs, a document that names a credential
for a target that does not:

```
spec.targetHost names the credential "deploy", and target "chef" does not take one: it has no way to reach its host as that identity, so the document would be applied as whoever the tool is configured to be. Leave the credential out to use the tool's own; targets that do: ansible
```

| Target | What the artifact holds | How the secret fields reach the tool | What it refuses |
|---|---|---|---|
| `ansible` | `ansible_user=<user>` after the host in `inventory.ini`, when the credential has a user | a `privateKey` in a file only the user can read, passed as `--private-key <path>`; a `password` as `ansible_password` in the same extra-vars file that carries the document's [secrets](secrets.md#handing-a-secret-to-a-tool), passed as `--extra-vars @<path>`. Both are removed when the run ends. Password authentication over SSH needs `sshpass` on the machine that runs `ansible-playbook`, as it does for Ansible itself | a `passphrase` (add the key to an agent and set `agent: true`), a `certificate` (put it beside the key in the agent), a `domain` (for WinRM hosts, which the plugin does not reach), a user name that cannot be written on an inventory line; rule `unsupported-credential` |
| `fake`, the test target | `credential.txt`: the name, type, user and the names of the secret fields | one JSON file only the user can read, and a record of the delivery that names the fields and never a value | nothing |
| any other target | the document is refused, as above | | |

A credential is delivered the way the tool takes it, so what the tool keeps
is the tool's own: Ansible keeps nothing, and a tool that caches a login is
documented on its page.

## Handing a credential to a tool

A credential's secret fields follow the same route as any
[secret handed to a tool](secrets.md#handing-a-secret-to-a-tool), with the
same three promises:

- **The value is in no file Meridian keeps.** Not in the compiled artifact,
  not in the state directory, not in the plan, not in an event, not in the
  run record. The record of a run names the credential, as `deploy`, and
  nothing of it.
- **Never on the command line, never in the environment.** The command line
  names a private file's path and nothing more.
- **For the length of the stage.** The values are fetched at the stage's turn
  and wiped when it ends. A rollback fetches them again at its own turn.

What a plan prints is the name, the type and the user, so that "which
identity touches production" can be reviewed, and never a value:

```
web (ansible) on web01.internal as deploy (ssh, user deploy)
```

How the values are protected in memory, and what crosses to a plugin, is in
[Secrets in memory](../../concepts/secrets-in-memory.md#a-credential-in-a-run).

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
| `spec.targetHost names the credential "deploy", and target "chef" does not take one` | a target without the capability, at `compile`, `plan` and `apply` |
| `the credential "deploy": ansible-playbook takes no passphrase for a private key` | the Ansible plugin's refusals, rule `unsupported-credential` |

## Examples

| Example | Shows |
|---|---|
| [`credential-for-a-host.yaml`](../../../examples/language/credential-for-a-host.yaml) | Ansible reaching a host as the deploy user with a private key from a file: the user in the inventory, the key in a private file passed as `--private-key`, and the plan line that names the identity |

```
meridian plan examples/language/credential-for-a-host.yaml
```
