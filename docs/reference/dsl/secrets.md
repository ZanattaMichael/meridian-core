# Secrets

A document declares **where a secret comes from** and reads it by name. The value is
fetched when a run needs it, held in protected memory, and never written by Meridian.

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
    - name: deploy_token
      from: file
      path: /run/secrets/deploy-token
      optional: true
  resources:
    - id: token-login
      type: file
      when: secret.deploy_token != null       # only where a token was provided
      params:
        path: /etc/site/login-enabled
        content: "yes\n"
```

See also [how a secret is held in memory](../../concepts/secrets-in-memory.md).

> **Status.** Every target except `cloud-init` and `dsc`
> [takes a secret in `params`](#handing-a-secret-to-a-tool). On those two, `compile`
> writes the secret's name and `plan` and `apply` stop.

## Contents

- [Declaring a secret](#declaring-a-secret)
- [The providers](#the-providers)
- [Several places for one secret](#several-places-for-one-secret)
- [Reading a secret](#reading-a-secret)
- [Storing a secret](#storing-a-secret)
- [What an expression can do with one](#what-an-expression-can-do-with-one)
- [Where a secret cannot be used](#where-a-secret-cannot-be-used)
- [Handing a secret to a tool](#handing-a-secret-to-a-tool)
- [A secret in a credential](#a-secret-in-a-credential)
- [When a secret is fetched](#when-a-secret-is-fetched)
- [Checking before a run](#checking-before-a-run)
- [What each command does](#what-each-command-does)
- [Errors](#errors)
- [Examples](#examples)

## Declaring a secret

`spec.secrets` is a list, in either kind of document. Each entry is one secret.

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | the name it is read by, as `secret.<name>`: letters, digits and underscores, not starting with a digit |
| `from` | yes | the provider: `env`, `file`, `exec`, `keyring`, `systemd-creds`, `secret-service` or `keychain` |
| `key` | for `env`, `keyring`, `systemd-creds` | the environment variable; the key's description in the kernel keyring; the name of the credential the unit was given |
| `path` | for `file` | the file |
| `command` | for `exec` | the program and its arguments, as a list |
| `attributes` | for `secret-service` | the attributes the item was stored with, as a mapping of text |
| `service`, `account` | for `keychain` | the keychain item's service and account |
| `optional` | no | `true`: a secret that no place has is `null` instead of an error |
| `allowWideMode` | no, `file` only | `true`: read the file even though others than its owner can read it |
| `keepTrailingNewline` | no, `file` and `systemd-creds` only | `true`: the value's last line ending is part of the secret |
| `fallback` | no | the places to look next, in order, as a list of `{from: ..., ...}`: see [several places](#several-places-for-one-secret) |

There is no value field; any unknown field is refused without repeating it.

`key`, `path`, `command`, `service`, `account` and `attributes` cannot hold an
[interpolation](interpolation.md), in a `fallback` entry either.

A field the provider does not use (`path` on `from: env`) is refused.

## The providers

| `from` | Reads | Refuses | Notes |
|---|---|---|---|
| `env` | the environment variable `key` | a variable that is not set (an empty one is an empty secret, not a missing one) | the variable is **removed from Meridian's environment** as soon as it is read, so no plugin or tool started afterwards inherits it. The value is kept in protected memory until the command ends, so a second document of the run that declares the same variable still reads it. It is the weakest provider: the environment a process started with can be read by the same user while the process lives |
| `file` | the file at `path`, without its last line ending | a file that does not exist; a file its group or others can read or write, unless `allowWideMode: true`; a file larger than 256 KiB; a directory | a relative `path` is relative to where `meridian` is run |
| `exec` | what `command` prints, without its last line ending | a command that is not found, exits with a failure, is stopped by a signal, or prints nothing | the command is run **without a shell**: `command: [pass, show, site/db]` runs `pass` with two arguments, and nothing in them is expanded. What the command writes to its standard error is discarded. This is how a password manager or a cloud CLI is used |

### The operating system's stores

Each reads into protected memory and puts nothing it read into an error.

| `from` | Reads | Refuses | Says it is unavailable when | Notes |
|---|---|---|---|---|
| `keyring` | the key of type `user` with description `key` from the Linux kernel keyring: the session keyring, then the user keyring | a key larger than 256 KiB | the kernel refuses the keyring itself, as a container's sandbox often does; on another operating system | a key never touches a disk and can be given a lifetime with `keyctl timeout`. Meridian writes this store: [`secret set`](../cli.md#secret-set) puts a value in the user keyring, readable by its owner (`keyctl setperm ... 0x3f3f0000`), so that a later command reads it from any session. A key added with `keyctl add` is readable by a possessor alone, which a session that does not link the user keyring is not |
| `systemd-creds` | the credential `key` that systemd passed to the unit (`LoadCredential=`, `LoadCredentialEncrypted=`), from `$CREDENTIALS_DIRECTORY`, without its last line ending unless `keepTrailingNewline: true` | a `key` that is a path and not one name | `CREDENTIALS_DIRECTORY` is not set: Meridian is not running as a unit that was given credentials | the directory is readable by the unit alone, and an encrypted credential is decrypted by systemd into memory that is never swapped, so the file's mode is not checked |
| `secret-service` | the item of the freedesktop Secret Service (GNOME Keyring, KWallet) stored with `attributes`, through `secret-tool lookup` | an item the service holds nothing for is missing; a tool that fails otherwise | `DBUS_SESSION_BUS_ADDRESS` is not set (a server or a pipeline has no session bus, and the tool would wait for one); `secret-tool` is not installed (package `libsecret-tools`) | the attributes find the item and are not secret: the service stores them unencrypted itself. Meridian writes this store: `secret set` hands the value to `secret-tool store` on its standard input |
| `keychain` | the generic password for `service` and `account` from the macOS keychain, through `/usr/bin/security find-generic-password` | a locked keychain, or a read the keychain did not allow | on any system but macOS | the keychain asks the user the first time a program reads an item it did not make; add the item with `security add-generic-password -A` to allow every program, or answer "Always Allow". Meridian does not write the keychain |

Only `plan` and `apply` ask the Secret Service and the keychain; `secret check` says
`not checked` for both.

The Windows Credential Manager and Vault are not providers yet; use `exec` with their
command line tools.

## Several places for one secret

`from` and its fields are the first place; `fallback` lists the rest, in order:

```yaml
  secrets:
    - name: deploy_token
      from: secret-service
      attributes: {app: meridian, name: deploy-token}
      optional: true
      fallback:
        - from: keyring
          key: meridian:deploy-token
        - from: systemd-creds
          key: deploy-token
        - from: env
          key: DEPLOY_TOKEN
```

- The first place with the secret answers.
- A place that is empty or absent on this machine is skipped.
- A place that refuses (a world-readable file, a failing command) stops the run.
- When none has it, the error lists every reason. An `optional` secret found nowhere is
  `null`.
- A `fallback` entry has `from` and its provider's fields, no `name`, `optional` or
  `fallback`. Errors name it as `spec.secrets[0].fallback[1].key`.

## Reading a secret

A secret is read as `secret.<name>` in an [expression](expressions.md): in a
`when`, a [named condition](interpolation.md#named-conditions), a
[variable](interpolation.md#variables), or an interpolation in a resource's
`params`.

```yaml
spec:
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  conditions:
    password_is_default: secret.db_password == 'changeme'
  resources:
    - id: warn
      type: file
      when: cond.password_is_default
      params:
        path: /etc/motd
        content: "This host still has the default database password.\n"
```

A name that is not declared is refused by `validate`, with the names that are:

```
site.yaml:14:7: spec.resources[0].when: secret.db_pasword is not declared (spec.secrets declares: db_password)
```

An expression holds a reference to the secret, not its value, so anything made from it
shows `(sensitive)`.

## What an expression can do with one

| With a secret you can | The result is |
|---|---|
| compare it with `==` or `!=`, to text or to another secret | a plain `true` or `false`. The comparison takes the same time wherever the two differ |
| compare it with `null` (an optional secret that was not found is `null`) | a plain `true` or `false` |
| join it to text or another secret with `+` | a secret |
| place it inside text, as in `"password=${{ secret.db_password }}"` | the whole text is a secret |
| give it to `urlEncode`, `base64Encode`, `base64Decode` or `trim` | a secret |
| choose it with `? :`, `??`, `default`, `coalesce` or `required` | whichever value was chosen |
| test it with `defined` | a plain `true` or `false` |

Everything else refuses a secret, and says what would have worked:

```
upper(...) cannot be given a secret; the functions that can are base64Decode, base64Encode, trim, urlEncode, and a secret can be joined to text with + or placed inside text
```

That includes `length`, `sha256`, `toString`, `toJson`, `replace`, `split`, `contains`,
`regexMatch`, `<`, `matches`, `in` and arithmetic. Nothing turns a secret back into plain
text.

## Where a secret cannot be used

A secret is refused anywhere a value is shown, stored, or shapes the document:

| Place | Why |
|---|---|
| `metadata.labels` | labels are printed and matched by selectors |
| `spec.target`, `spec.targetVersion` | they pick the plugin |
| `spec.targetHost.host` | it is printed with every stage |
| a resource's `state` | it is part of the plan |
| an output's `value` | outputs are stored in the run's state and handed to other documents |
| `id`, `type`, a mapping key, an edge's resource name | the document's skeleton takes no interpolation at all |

```
site.yaml:1:1: metadata.labels.owner: a secret cannot be used here: this field is shown, stored or shapes the document. A secret can be read by a when, and placed in a resource's params
```

`validate` finds a direct use; one through a variable is found when the document is
resolved.

## Handing a secret to a tool

The target compiles a **reference** to the secret, and hands the tool the **value** by a
private channel when the stage runs. The compiled files never hold it.

```yaml
spec:
  target: ansible
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_conf
      type: file
      params:
        path: /etc/app/db.conf
        mode: "0600"
        content: ${{ secret.db_password }}
```

| Target | What the artifact holds | How the value reaches the tool |
|---|---|---|
| `ansible` | `{{ meridian_secret_db_password }}`, and the task is marked `no_log` so Ansible prints neither its arguments, its diff nor its result | an extra-vars file only the user can read, held in memory where the system allows, passed as `--extra-vars @<path>` and removed when the run ends |
| `terraform`, `opentofu` | `${var.meridian_secret_db_password}`, with the variable declared `sensitive` | a variables file of the same kind, passed to `plan` as `-var-file=<path>` and removed when the plan is saved. The saved plan holds every variable's value, as every Terraform plan file does, so it is overwritten and removed as soon as it has been applied or shown |
| `puppet`, `openvox` | `Sensitive(lookup('meridian_secret_db_password'))`: a Hiera lookup, wrapped so that Puppet redacts the value in its report and its diffs. A secret is a whole param; one inside a list or a map is refused, since a lookup is one value | a Hiera data file only the user can read, with a `hiera.yaml` beside it that names that directory, passed as `--hiera_config <path>` to `puppet apply` and `--hiera-config <path>` to `bolt apply`, and removed when the run ends |
| `salt` | `{{ pillar['meridian_secret_db_password'] \| yaml_encode }}`, rendered by Jinja before Salt reads the file as YAML. A file state given one gets `show_changes: false`, so Salt prints no diff. An `exec` is refused one (`unsupported-secret-place`): Salt prints the command line in its report, and nothing turns that off | a pillar root only the user can read, a pillar file and the top file that applies it, passed as `--pillar-root <dir>` to `salt-call` and named as `pillar_roots` in the `salt-ssh` configuration, and removed when the run ends |
| `chef` | `node['meridian_secret_db_password']`, with `sensitive true` on the resource, so Chef prints neither its diff nor its content | on this machine: an attributes file only the user can read, passed as `--json-attributes <path>` to `chef-client`, with a `client.rb` beside it that keeps the node object local mode writes back (attributes and all) in the same private directory, and removed when the run ends. Through `chef-run`, which takes attributes on its command line only, the apply stops and says so |
| `packer` | `${var.meridian_secret_db_password}`, with the variable declared `sensitive`, so Packer prints `<sensitive>` in its place | a variables file only the user can read, passed as `-var-file=<path>` to `validate` and `build`, and removed when the build ends |
| `cloudformation` | `Ref: MeridianSecretDbPassword` (or `${MeridianSecretDbPassword}` in an `Fn::Sub`), a parameter declared `NoEcho`, so CloudFormation shows `****` in its place. A parameter's name is letters and digits, so the secret's name is spelled as a logical id | a parameters file only the user can read, passed as `--parameters file://<path>` to `create-change-set` and removed once the change set exists; the change set carries the values to `execute-change-set` |
| `bicep` | `meridian_secret_db_password`, a parameter declared `@secure()`, which the compiler makes a `securestring` | a Resource Manager parameters file only the user can read, passed as `--parameters @<path>` to `what-if` and `create`, and removed when the stage ends |
| `arm` | `[parameters('meridian_secret_db_password')]`, a parameter declared `securestring` | the same parameters file, passed the same way |
| `pulumi` | `${meridian_secret_db_password}`, a configuration value declared `secret: true`, so Pulumi encrypts it in its state and prints `[secret]` in its place | `pulumi config set --secret <key>` with the value on standard input, before the preview; `pulumi config rm <key>` when the stage is over, so the settings file names no secret between runs |
| `kubernetes` | `"${meridian_secret_db_password}"`, a token inside the quotes of a string, as a value of a `v1/Secret`'s `stringData` and nowhere else: anywhere else the value would be written into an object for every reader to see | `kubectl apply --filename -`, for the plan's dry run and the apply alike, with the manifests on standard input and each token filled in as a JSON string on the way; the file on disk holds the token |
| `helm` | the same token in `chart/values.yaml`, read by a template with `${values.<path>}`; a secret in an object or a setting is refused, and so is one in the values of a document with a Kustomize overlay, whose rendered manifests are written to a file | `--values -` on `helm template` and `helm upgrade --install`, with the values on standard input and each token filled in the same way |
| `docker` | `${meridian_secret_db_password}`, Compose's own interpolation of a variable, in a service; a `secret` element's `value` becomes `environment: meridian_secret_db_password`, so Compose mounts the content under `/run/secrets` | an environment file only the user can read, one quoted `meridian_secret_<name>="..."` line per secret, given to every `docker compose` call of the stage as `--env-file <path>` and removed when the stage ends |
| `podman` | `Secret=meridian_secret_db_password,type=env,target=PASSWORD` in the container's unit, in place of the `Environment` variable that held it; a secret anywhere else in a unit is refused, since the unit file is written onto the host | an apply creates the secret in Podman on the host, `podman secret create <name> -`, reading the value from the apply script's own standard input before any unit is written, and replaces one that exists; a plan creates nothing |
| any other target (`cloud-init`, `dsc`) | the stage stops, and says which targets take a secret | |

A value built from a secret and other text, such as
`"password=${{ secret.db_password }}\n"`, is a new secret named `derived_1`, `derived_2`,
... in order of use.

Everywhere:

- **The value is in no file Meridian keeps**, only the tool's private file, for the run.
- **Never on the command line or in the environment**, except a
  [cloud credential](credentials.md#reaching-a-cloud-as-a-credential), which the
  providers take from the environment only; the plan line says so.
- **The tool's own state keeps what it is given:**
  - Terraform writes resource arguments into its state file, whatever variable
    they came from. Declare a `backend` that protects the state.
  - Pulumi encrypts them with the stack's secrets provider, which protects
    nothing without a passphrase. Name a `secretsProvider`.
  - CloudFormation masks a `NoEcho` parameter.
  - Azure keeps a secure parameter out of the deployment record.
  - A cluster stores a Secret base64-encoded, readable by anyone allowed to read
    Secrets in its namespace; Helm keeps release values the same way.
  - Podman's default driver keeps a secret base64-encoded in the user's
    directory.
- **A setting is not a param.** A tool-settings resource (`stack`,
  `deployment`, `project`, `cluster`, `release`, a `variable` or `config`
  declaration) is compiled into the command line or written verbatim, so a
  secret there is refused at compile (`unsupported-secret-place`).
- **Tokens are reserved.** A document that writes `${meridian_secret_<name>}` itself is
  refused (`reserved-name`).

`compile` writes the same reference the tool runs with. For a target that takes no
secret, it writes `password=(secret db_password)`, and `plan` and `apply` stop:

```
resource "app_env": params.content holds a secret, and target "cloud-init" does not take one: it has no private channel to hand a secret to its tool, so the value would be written into a file or dropped. A secret can still decide a when for it; targets that do: ansible, arm, bicep, chef, cloudformation, docker, helm, kubernetes, openvox, opentofu, packer, podman, pulumi, puppet, salt, terraform
```

A rollback fetches the secrets again at the stage's turn.

## A secret in a credential

A [credential](credentials.md)'s secret fields are each exactly `${{ secret.<name> }}`
and reach the tool through its identity channel (for Ansible, `--private-key <private
file>` and the extra-vars file). A literal is refused.

## When a secret is fetched

- **Only `plan` and `apply` fetch.**
- **Just before the declaring stage.** A multi-document workflow fetches each
  document's secrets when its stage starts, not all up front.
- **Wiped when the stage ends**, success or failure, not at the end of the run.
- **Not for an excluded document.** A document whose `spec.when` is false
  fetches nothing (unless that `when` reads a secret). Nor does a stage skipped
  after an earlier failure.
- **Once, even with a policy.** The first `apply --policy` pass fetches
  nothing; a document that reads secrets is put to the
  [policy](../policy.md#when-a-policy-is-asked) at its own turn.
- **Not for a document only imported from.** A document compiled because
  another [imports](outputs-and-hand-offs.md#imports) a file from it is read as
  `compile` reads it: nothing is fetched, and secrets in its params are written
  as names.
- **`drift` fetches as `plan` does**, because it plans each stage. `rollback`
  fetches nothing: it puts back artifacts that were kept.
- **Never cached.** Nothing is written to save a second fetch. The next run
  fetches again.
- **Locked in memory.** If the locked-memory limit prevents it, the run warns once, or
  stops with [`--strict-secrets`](../cli.md#secrets).

## Storing a secret

[`meridian secret set <name> <files>...`](../cli.md#secret-set) stores a value in
the first of the secret's places that Meridian can write and this machine has:
the kernel keyring (`from: keyring`) or the Secret Service
(`from: secret-service`). The value is typed at a hidden prompt or piped in, never an argument. Other places are
skipped; a secret with no writable place is refused.

```
$ meridian secret set deploy_token site.yaml
Value for deploy_token (not shown):
stored deploy_token in the kernel keyring, key meridian:deploy-token
```

## Checking before a run

[`meridian secret check`](../cli.md#secret-check) lists each declared secret, its place,
and whether it would answer. It reads no value and runs no command:

```
$ meridian secret check site.yaml
site (site.yaml)
  db_password   env    ok: the environment variable SITE_DB_PASSWORD is set
  deploy_token  file   absent, optional: the file /run/secrets/deploy-token does not exist
```

A store this machine lacks is `UNAVAILABLE`.

**In a pipeline**, read the engine's secrets with `from: env`, and its OIDC identity as a
credential [`from: ambient`](credentials.md#from-ambient). See
[running in a pipeline](../../operations/running-in-a-pipeline.md).

## What each command does

| Command | A `when` that reads a secret | A secret in `params` |
|---|---|---|
| `validate` | checked: the name is declared, the expression is one a secret allows where it can be known | checked the same way |
| `compile` | refused: `(secret db_password) is not fetched here: a secret is fetched by plan and apply, never by validate or compile` | written as its name: `password=(secret db_password)` |
| `secret check` | not read | not read: only where each declared secret comes from is looked at |
| `plan`, `apply` | decided. The report shows the condition as the document wrote it, never the value | [handed to the tool](#handing-a-secret-to-a-tool) by a private channel, for a target that takes one; for any other, the stage stops: `resource "conf": params.content holds a secret, and target "cloud-init" does not take one: ...` |

`compile --out` writes the tool's reference on targets that take secrets
(`{{ meridian_secret_db_password }}` for Ansible), and a placeholder otherwise
(`password=(secret db_password)`, or
`postgres://app:urlEncode((secret db_password))@db/app` through a function).

## Errors

No error holds a secret's value, a command's output or its arguments.

| What is wrong | Caught by | Message |
|---|---|---|
| No name | `validate` | `spec.secrets[0].name: a secret needs a name; it is read as secret.<name>` |
| A name that cannot follow `secret.` | `validate` | `"has-dash" is not a usable name: a secret is read as secret.<name>, so its name is letters, digits and underscores, and does not start with a digit` |
| The same name twice | `validate` | `spec.secrets[1].name: the secret "a" is declared twice` |
| No provider, or one this version does not have | `validate` | `"vault" is not a provider this version has; from is one of env, exec, file, keychain, keyring, secret-service, systemd-creds` |
| The provider's field is missing | `validate` | `spec.secrets[0].key: from: env needs key, the environment variable to read` |
| A field the provider does not use | `validate` | `spec.secrets[0].path: path means nothing to from: env; remove it` |
| A further place that is wrong | `validate` | `spec.secrets[0].fallback[1].key: from: keyring needs key, the description of the key in the kernel keyring` |
| An interpolation in a declaration | `validate` | `where a secret comes from is written out, so that it can be read without any data; it cannot be interpolated` |
| A secret that is read and not declared | `validate` | `secret.x is not declared (spec.secrets declares none)` |
| A secret in a place that is shown or stored | `validate`, or when resolved | `a secret cannot be used here: ...` |
| A function or operator that does not take a secret | `plan`, `apply` | `length(...) cannot be given a secret; the functions that can are ...` |
| The environment variable is not set | `plan`, `apply` | `spec.secrets: secret "db_password" (from: env): the environment variable SITE_DB_PASSWORD is not set` |
| The file can be read by others | `plan`, `apply` | `secret "token" (from: file): ... has mode 0644 ... allowWideMode: true` |
| A secret cannot be locked in memory, with `--strict-secrets` | `plan`, `apply` | `the secret "db_password" could not be locked in memory and may reach swap, and --strict-secrets is set. ...` |
| The command failed | `plan`, `apply` | `secret "flag" (from: exec): the command sh exited with status 4` |
| No place has the secret | `plan`, `apply` | `secret "deploy_token" (from: secret-service, then keyring, then env): the Secret Service needs a session bus, and DBUS_SESSION_BUS_ADDRESS is not set; ...; the kernel keyring holds no key meridian:deploy-token; the environment variable DEPLOY_TOKEN is not set` |
| A store this machine does not have | `plan`, `apply` | `secret "deploy_key" (from: keychain): the keychain is only on macOS` |
| The kernel keyring holds the key and this session may not read it | `plan`, `apply` | `secret "x" (from: keyring): the kernel keyring holds the key ..., and this session may not read it: the key is readable only by a possessor, and this session's keyring does not link the user keyring. Let the owner read it with: keyctl setperm ... 0x3f3f0000, or store it with meridian secret set` |
| `secret set` on a secret with no writable place | `secret set` | `"deploy_token" names no place meridian can write. It is looked for in the environment variable DEPLOY_TOKEN, then in the file token.txt. meridian writes the kernel keyring (from: keyring) and the Secret Service (from: secret-service); every other place is written with its own tool` |
| A secret that is not base64 is given to `base64Decode` | `plan`, `apply` | `base64Decode(...): the secret is not base64` |
| A secret in `params` of a target that takes none | `plan`, `apply` | `resource "conf": params.content holds a secret, and target "cloud-init" does not take one: it has no private channel to hand a secret to its tool, so the value would be written into a file or dropped. A secret can still decide a when for it; targets that do: ansible, arm, bicep, chef, cloudformation, docker, helm, kubernetes, openvox, opentofu, packer, podman, pulumi, puppet, salt, terraform` |
| A secret in `params` of a `chef` document for another machine | `apply` | `chef-run has no private way to hand a secret to web01: it takes attributes on its command line only, where every user of this machine could read them. Run chef-client on the host, with the local transport, or keep the secret out of this document` |
| A secret inside a list or a map, on `puppet` or `openvox` | `compile` | `param "groups" holds a secret inside a list or a map; on this target a secret is a whole param, looked up as one Sensitive value [unsupported-secret-place]` |
| A secret in a tool's settings resource (`stack`, `deployment`) or a declaration (`variable`, `parameter`, `config`), on `packer`, `cloudformation`, `bicep`, `arm` or `pulumi` | `compile` | `param "region" of the stack holds a secret, and the stack's settings are the command line aws is given, where a secret cannot go. A secret goes in a resource's params, which the tool is handed by a private channel [unsupported-secret-place]` |
| Two secrets, or a secret and a resource, spelled the same as a CloudFormation parameter | `compile` | `secret "db_password" would be the parameter MeridianSecretDbPassword, which is also what "meridian_secret_db_password" is called in the template; rename one of them [invalid-name]` |
| A secret in an `exec`, on `salt` | `compile` | `a secret cannot be used in an exec on this target: Salt prints the command line in its report, and nothing turns that off. Put the secret in a file the command reads, or in a runtimeWhen [unsupported-secret-place]` |
| A secret anywhere but a `v1/Secret`'s `stringData`, on `kubernetes` | `compile` | `param "data" of "v1/ConfigMap" holds a secret, and it would be written into the object for every reader of it to see. A secret goes in the stringData of a v1/Secret, which the workload reads by name [unsupported-secret-place]` |
| A secret anywhere but the `values`, or in the values beside a Kustomize overlay, on `helm` | `compile` | `param "stringData" of "v1/Secret" holds a secret, and an object is written into a template file as it stands, where a secret cannot go. Put the secret in the values and read it here with ${values.<path>}, in the stringData of a v1/Secret [unsupported-secret-place]` |
| A secret in the `project`, on `docker` | `compile` | `param "name" of the project holds a secret, and the project's settings are the command line docker is given, where a secret cannot go. A secret goes in a service's params, or as the value of a secret element, which Compose reads from a private environment file [unsupported-secret-place]` |
| A secret anywhere but the whole value of a container's `Environment` variable, on `podman` | `compile` | `param "Label" of the container holds a secret, and a unit file is written onto the host as it stands, where a secret cannot go; put it in Environment as a map, where it becomes a Podman secret the container reads into the variable [unsupported-secret-place]` |
| Text that spells a secret's token, on `kubernetes` or `helm` | `compile` | `param "stringData" holds the text "${meridian_secret_", which is how this target writes a secret's place in a manifest; write the secret as ${{ secret.<name> }} or choose other text [reserved-text]` |
| A secret that is not text, handed to a tool that reads a variables file | `plan`, `apply` | `writing the extra-vars file: the secret is not text, so it cannot be written as a JSON string` |

## Examples

Under [`examples/language/`](../../../examples/language), each with the data
file it reads beside it:

| Example | Shows |
|---|---|
| [`features-by-secret.yaml`](../../../examples/language/features-by-secret.yaml) | three secrets, one from each provider, read only by conditions: an optional licence, a default password that is warned about, a secret beside ordinary data. It can be planned and applied today |
| [`secret-in-a-setting.yaml`](../../../examples/language/secret-in-a-setting.yaml) | a password placed in a database URL through a variable and `urlEncode`, for a target that takes no secret yet, and what `compile` writes in its place |
| [`secret-to-a-tool.yaml`](../../../examples/language/secret-to-a-tool.yaml) | a password handed to Ansible: the playbook refers to it by name, and the value reaches `ansible-playbook` in a private file |
| [`secret-from-a-store.yaml`](../../../examples/language/secret-from-a-store.yaml) | one deploy token with four places: the desktop's Secret Service, the kernel keyring, the credential systemd gave the unit, and last a variable. The same document is planned on a laptop, as a service and in a pipeline, and a machine with none of them deploys nothing |

The first is planned with its password in the environment:

```
SITE_DB_PASSWORD=... meridian plan examples/language/features-by-secret.yaml \
    --data examples/language/features-by-secret.data.yaml
```

On a machine with no licence file and a password that is not the default, the
plan reports each condition in the document's own words and no value:

```
site (ResourceSet, target ansible on web01.example.com)
  when: 3 removed
    resource "premium_conf": cond.licensed is false
    resource "password_warning": cond.default_password is false
    "deploy_conf" depends on "premium_conf": cond.licensed is false
```

What each machine is given:

| The machine has | `premium_conf` | `password_warning` | `deploy_conf` | `hardening` |
|---|---|---|---|---|
| every secret, in production | kept | removed | kept | kept |
| no licence | removed | removed | kept | kept |
| no deploy token | kept | removed | removed | kept |
| the default password | kept | kept | removed | removed |
| a strong password outside production | removed | removed | removed | removed |

The second is compiled, which reads no secret. Its target, `cloud-init`,
takes no secret (its user-data is the compiled file, for a machine that does
not exist yet), so the name is written where the value belongs:

```
meridian compile examples/language/secret-in-a-setting.yaml \
    --data examples/language/secret-in-a-setting.data.yaml --out out
```

```
DATABASE_URL=postgres://app:urlEncode((secret db_password))@db.internal:5432/app
SIGNING_KEY=base64Encode((secret signing_key))
LOG_LEVEL=info
```

The fourth is compiled too, and its target takes secrets, so it is written as
Ansible will be run with it:

```
meridian compile examples/language/secret-to-a-tool.yaml --out out
```

```yaml
    - name: db_conf
      ansible.builtin.copy:
        content: '{{ meridian_secret_db_password }}'
        dest: /etc/app/db.conf
        mode: "0600"
      no_log: true
```

The third is checked, planned and applied with the token in each place in
turn:

```
$ meridian secret check examples/language/secret-from-a-store.yaml
deploys (examples/language/secret-from-a-store.yaml)
  deploy_token  systemd-creds  ok: this unit was given the credential deploy-token
```

Tests cover all of them:

1. Planned and applied for each row of the table above; all output and leftover
   files are searched for the secrets.
2. Compiled with a password in the environment that must appear nowhere.
3. Run on four machines keeping the token in different places (or none); the
   token appears in nothing they print or write.
4. Compiled with the real Ansible plugin; the handover is proved against a real
   `ansible-playbook` and the test target, whose tool keeps a copy for the test
   to read back.
