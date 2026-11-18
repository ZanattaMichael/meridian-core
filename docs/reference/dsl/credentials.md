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
- [Reaching a cloud as a credential](#reaching-a-cloud-as-a-credential)
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

## Reaching a cloud as a credential

An infrastructure document is applied to an account, a subscription or a
project rather than to a host. `spec.endpoint` names what kind of place that
is and the credential it is reached as:

```yaml
spec:
  target: terraform
  endpoint:
    type: cloud
    credential: prod
  credentials:
    - name: prod
      type: aws
      accessKeyId: AKIAIOSFODNN7EXAMPLE
      secretAccessKey: ${{ secret.aws_secret_key }}
      region: eu-west-1
```

| Field | Required | Value |
|---|---|---|
| `type` | yes | `cloud`: an account, subscription or project, reached as an `aws`, `azure` or `gcp` credential. `cluster`: a Kubernetes API, reached as a `kubeconfig` credential (no target takes one yet) |
| `credential` | yes | the name of a credential in `spec.credentials`, of a type the endpoint's kind takes |

- The name is written as it is, not interpolated, as for a host.
- The type must fit: `spec.endpoint.credential: a cloud takes an aws, azure
  or gcp credential; "prod" is ssh`.
- `endpoint` is a field of both kinds of document. A ResourceSet names one
  identity: `targetHost.credential` or `endpoint.credential`, not both.
- Left out, the tool reaches its cloud as whatever login it finds on the
  machine, as before.

A cloud credential is handed to the tool differently from a host's: the
providers take a login from their environment, so that is where it goes,
and the plan line says so. See [below](#handing-a-credential-to-a-tool).

## What each target does

A target that reaches its host as a credential declares the `credentials`
capability, published in the [capability matrix](../capability-matrix.md).
The core refuses, before anything runs, a document that names a credential
for a target that does not:

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

A target that reaches its cloud as a credential declares the `cloud`
capability. The core refuses a document that names a cloud credential for a
target that does not, the same way:

```
spec.endpoint names the credential "prod" for a cloud, and target "bicep" does not take one: it has no way to hand its tool that identity, so the document would be applied as whatever login the tool finds on this machine. Leave the endpoint out to use the tool's own; targets that do: cloudformation, opentofu, packer, pulumi, terraform
```

| Target | Takes | How the credential reaches the tool | What it refuses |
|---|---|---|---|
| `terraform`, `opentofu` | `aws`, `azure`, `gcp` | in the environment of every call of the stage: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_PROFILE`, `AWS_REGION` and `AWS_DEFAULT_REGION`; `ARM_CLIENT_ID`, `ARM_CLIENT_SECRET`, `ARM_TENANT_ID`, `ARM_SUBSCRIPTION_ID`, `ARM_USE_MSI`; `GOOGLE_PROJECT`, `CLOUDSDK_CORE_PROJECT`, `GOOGLE_IMPERSONATE_SERVICE_ACCOUNT`. A `gcp` `serviceAccountKey` and an `azure` `certificate` are written to a file only the user can read, named in `GOOGLE_APPLICATION_CREDENTIALS` and `ARM_CLIENT_CERTIFICATE_PATH`, and removed when the stage ends | an `aws` `roleArn` (declare the profile that assumes it), a plain field with a line break; rule `unsupported-credential` |
| `pulumi` | `aws`, `azure`, `gcp` | as `terraform`: the providers read the same variables | as `terraform` |
| `cloudformation` | `aws` | as `terraform`, for the `aws` variables, in every `aws cloudformation` call | `azure` and `gcp`: CloudFormation is AWS only |
| `packer` | `aws`, `gcp` | as `terraform`, for the `aws` and `gcp` variables, in `packer init`, `validate` and `build` | `azure` (the Azure plugin reads its login from the template), a `gcp` `impersonate` |
| `bicep`, `arm` | nothing yet | the document is refused, as above: `az login --service-principal` takes a secret on its command line only | |
| `fake`, the test target | `aws`, `azure`, `gcp` | as a host's: one JSON file only the user can read, and a record of the delivery | nothing |
| any other target | the document is refused, as above | | |

The compiled files hold nothing of a cloud credential: the plain fields are
not written into a configuration, because the providers read them from the
environment along with the secret ones.

On every target but `ansible` the compiled artifact holds nothing of the
credential, not even the user: the tool is handed the identity at run time,
so the same artifact can be applied as another identity, and a file nobody
expected to be sensitive names no account. Each target's page has a
`Credentials` section with the details.

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
- **Never on the command line, and in the environment only for a cloud.**
  The command line names a private file's path and nothing more. A key for a
  host over SSH is not even in a file: it goes into an SSH agent Meridian
  starts for the stage, through `ssh-add`'s standard input, and the tool is
  told only the agent's socket in `SSH_AUTH_SOCK`. See
  [0028](../../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md).
  A cloud credential is the one exception: the providers take a login from
  the environment, from their own files under the user's home directory or
  from the machine's metadata service, and Meridian writes nothing under a
  home directory, so the credential goes into the environment of the tool's
  process, rank 4 of the [channels](secrets.md#handing-a-secret-to-a-tool),
  the last the plan allows and only where the tool takes a value no other
  way. The value goes from protected memory into the process's environment
  block as it is spawned, and is copied nowhere else. The plan line says so.
  See [0029](../../decisions/0029-a-cloud-credential-reaches-a-tool-in-its-environment.md).
- **For the length of the stage.** The values are fetched at the stage's turn
  and wiped when it ends. A rollback fetches them again at its own turn.

What a plan prints is the name, the type and the plain fields that say who
and where (`user`, `profile`, `region`, `project`, `subscriptionId`), so that
"which identity touches production" can be reviewed, and never a value; for
a cloud credential, also that it goes in the tool's environment:

```
web (ansible) on web01.internal as deploy (ssh, user deploy)
net (Infrastructure, target terraform as prod (aws, region eu-west-1), in the tool's environment)
```

How the values are protected in memory, and what crosses to a plugin, is in
[Secrets in memory](../../concepts/secrets-in-memory.md#a-credential-in-a-run).

## What each command does

| Command | Credentials |
|---|---|
| `validate` | checks every declaration against its type, and `targetHost.credential` and `endpoint.credential` against the declarations. Fetches nothing |
| `compile` | writes what the target knows of the credential: a user in an inventory, a name in a file. Fetches nothing: the artifact never holds a secret field |
| `plan` | fetches the secret fields of the credential the host or the endpoint names, hands them to the tool by its channel, and prints the credential's name, type and plain fields with the stage, and for a cloud that it goes in the tool's environment |
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
| `a cloud takes an aws, azure or gcp credential; "prod" is ssh` | the wrong type for an endpoint; `a cluster takes a kubeconfig credential` |
| `unknown endpoint type "daemon" (expected cloud or cluster)` | a type the language does not have; `host` says to use `targetHost` |
| `the document already names a credential for its host in spec.targetHost.credential; a document is applied as one identity` | a ResourceSet naming both |
| `spec.endpoint names the credential "prod" for a cloud, and target "bicep" does not take one` | a target without the `cloud` capability, at `compile`, `plan` and `apply` |
| `the credential "prod": terraform takes no roleArn from Meridian: the AWS SDK assumes a role through a profile that names it ...; declare that profile instead` | a cloud plugin's refusals, rule `unsupported-credential` |
| `the credential "prod": aws cloudformation reaches AWS only, as an aws credential, not azure` | a credential for another cloud; `packer reaches a cloud, as an aws or gcp credential, not azure` |
| `the credential "deploy": ansible-playbook takes no passphrase for a private key` | the Ansible plugin's refusals, rule `unsupported-credential` |
| `the credential "deploy": ssh takes no password for a host: ssh asks for a password on a terminal, which a run has not; give the credential a privateKey instead` | a plugin whose tool has no private way to take a password (`podman`, `chef`), rule `unsupported-credential` |
| `the credential "deploy": salt-ssh takes no certificate for a key from Meridian; put it beside the key in an SSH agent of your own and set agent: true` | a certificate, on any target reached over SSH |
| `the document names the credential "deploy" for its host, and the host is this machine, which salt-call configures as the user running Meridian; leave the credential out, or name another host` | a credential for `localhost`, at `plan` and `apply` |
| `ssh-add could not add the credential's key to the agent` | the key is not one `ssh-add` reads, or its passphrase is wrong; what `ssh-add` said follows |

## Examples

| Example | Shows |
|---|---|
| [`credential-for-a-host.yaml`](../../../examples/language/credential-for-a-host.yaml) | Ansible reaching a host as the deploy user with a private key from a file: the user in the inventory, the key in a private file passed as `--private-key`, and the plan line that names the identity |
| [`credential-for-a-cloud.yaml`](../../../examples/language/credential-for-a-cloud.yaml) | Terraform reaching AWS as an access key whose secret half is a secret: nothing of it in the configuration, the login in the providers' environment for the length of the stage, and the plan line that says so |

```
meridian plan examples/language/credential-for-a-host.yaml
```
