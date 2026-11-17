# Secrets

A secret is a value a document needs and must not contain: a password, a
token, a private key. A document says **where a secret comes from**, by name,
and reads it by that name. The value is fetched when a run needs it, held in
protected memory, and never written anywhere Meridian writes.

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

How the value is held in memory, and what is and is not promised about it, is
on [its own page](../../concepts/secrets-in-memory.md).

> **Where this stands.** A secret can be declared, fetched, read by a `when`,
> and placed in a resource's `params` for a target that
> [takes one](#handing-a-secret-to-a-tool): today `ansible`, `puppet`,
> `openvox`, `salt`, `chef`, `terraform` and `opentofu`. For any other target
> a secret in `params` is checked and `compile` writes its name in its place,
> but `plan` and `apply` stop at it, because that target has no channel that
> is not the compiled files. A run never writes a secret into a compiled file
> to get past this.

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

There is no field for a value, and one written under any other name is refused
as an unknown field without being repeated in the error.

Where a secret comes from is written out. `key`, `path`, `command`,
`service`, `account` and `attributes` cannot hold an
[interpolation](interpolation.md), in a `fallback` entry either, so that
`validate` can read every declaration without any data.

A field that means nothing to the provider is refused (`path` on `from: env`,
for example), so a declaration never silently ignores part of what was written.

## The providers

| `from` | Reads | Refuses | Notes |
|---|---|---|---|
| `env` | the environment variable `key` | a variable that is not set (an empty one is an empty secret, not a missing one) | the variable is **removed from Meridian's environment** as soon as it is read, so no plugin or tool started afterwards inherits it. The value is kept in protected memory until the command ends, so a second document of the run that declares the same variable still reads it. It is the weakest provider: the environment a process started with can be read by the same user while the process lives |
| `file` | the file at `path`, without its last line ending | a file that does not exist; a file its group or others can read or write, unless `allowWideMode: true`; a file larger than 256 KiB; a directory | a relative `path` is relative to where `meridian` is run |
| `exec` | what `command` prints, without its last line ending | a command that is not found, exits with a failure, is stopped by a signal, or prints nothing | the command is run **without a shell**: `command: [pass, show, site/db]` runs `pass` with two arguments, and nothing in them is expanded. What the command writes to its standard error is discarded. This is how a password manager or a cloud CLI is used |

### The operating system's stores

A long-lived credential on a workstation is best kept in the store the
operating system already protects, and on a server in what the service
manager hands the unit. Each is a provider like the three above, reads
straight into protected memory, and puts nothing it read into an error.

| `from` | Reads | Refuses | Says it is unavailable when | Notes |
|---|---|---|---|---|
| `keyring` | the key of type `user` with description `key` from the Linux kernel keyring: the session keyring, then the user keyring | a key larger than 256 KiB | the kernel refuses the keyring itself, as a container's sandbox often does; on another operating system | a key never touches a disk and can be given a lifetime with `keyctl timeout`. Meridian writes this store: [`secret set`](../cli.md#secret-set) puts a value in the user keyring, readable by its owner (`keyctl setperm ... 0x3f3f0000`), so that a later command reads it from any session. A key added with `keyctl add` is readable by a possessor alone, which a session that does not link the user keyring is not |
| `systemd-creds` | the credential `key` that systemd passed to the unit (`LoadCredential=`, `LoadCredentialEncrypted=`), from `$CREDENTIALS_DIRECTORY`, without its last line ending unless `keepTrailingNewline: true` | a `key` that is a path and not one name | `CREDENTIALS_DIRECTORY` is not set: Meridian is not running as a unit that was given credentials | the directory is readable by the unit alone, and an encrypted credential is decrypted by systemd into memory that is never swapped, so the file's mode is not checked |
| `secret-service` | the item of the freedesktop Secret Service (GNOME Keyring, KWallet) stored with `attributes`, through `secret-tool lookup` | an item the service holds nothing for is missing; a tool that fails otherwise | `DBUS_SESSION_BUS_ADDRESS` is not set (a server or a pipeline has no session bus, and the tool would wait for one); `secret-tool` is not installed (package `libsecret-tools`) | the attributes find the item and are not secret: the service stores them unencrypted itself. Meridian writes this store: `secret set` hands the value to `secret-tool store` on its standard input |
| `keychain` | the generic password for `service` and `account` from the macOS keychain, through `/usr/bin/security find-generic-password` | a locked keychain, or a read the keychain did not allow | on any system but macOS | the keychain asks the user the first time a program reads an item it did not make; add the item with `security add-generic-password -A` to allow every program, or answer "Always Allow". Meridian does not write the keychain |

Only `plan` and `apply` ask the Secret Service and the keychain, since asking
can open a prompt to unlock them; `secret check` says `not checked` for both.

The Windows Credential Manager and remote stores such as Vault are still to
come as providers; until then, `exec` with the store's own command line tool
reaches them.

## Several places for one secret

A secret may name the places it could be in, in order, so that one document
works on a laptop, as a service and in a pipeline. `from` and its fields are
the first place; `fallback` is a list of the rest, each written as a place is:

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

The first place that has the secret answers, and the places after it are not
asked. A place that has nothing, or that this machine does not have at all
(no session bus, no unit credentials, another operating system), is passed
over for the next. A place that is there and refuses, such as a file others
can read or a command that fails, stops the run: that is a mistake to fix, not
one to hide behind the next place.

When no place has it, the error gives every place's reason, in order, and the
secret is missing if at least one place could have held it, or unavailable if
none can be used on this machine. An `optional` secret that no place has is
`null`.

A `fallback` entry is a place: it has `from` and the fields its provider
needs, and no `name`, `optional` or `fallback` of its own. Each is checked by
the same rules as the first, and an error names it by where it is, as
`spec.secrets[0].fallback[1].key`.

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

What an expression holds when it reads a secret is not the secret. It is a
reference to it: which secret of the run is meant. The value stays in protected
memory, and everything made from the expression's result (a plan, a log line,
an error, the run's state) can only ever show `(sensitive)`.

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

That covers every other function (`length`, `sha256`, `toString`, `toJson`,
`replace`, `split`, `contains`, `regexMatch` and the rest) and every other
operator (`<`, `matches`, `in`, arithmetic). The list is short on purpose: each
entry does its work on protected memory, and a function that would have to
copy the value out to work on it is not on the list. The length of a secret
and a hash of it are refused because both say something about the value.

A secret mark never comes off. There is no function that turns a secret back
into plain text.

## Where a secret cannot be used

A secret is refused anywhere a value is shown, stored, or decides the shape of
the document:

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

`validate` finds a secret written directly in one of these places. One that
gets there through a variable is found when the document is resolved, with the
same message.

## Handing a secret to a tool

A secret in a resource's `params` is meant for the tool: a password in a
database resource, a key in a file. It never reaches the tool through the
compiled files. The target compiles a **reference** to the secret, by name, and
hands the tool the **value** by a private channel when the stage runs.

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
| `salt` | `{{ pillar['meridian_secret_db_password'] \| yaml_encode }}`, rendered by Jinja before Salt reads the file as YAML. A file state given one gets `show_changes: false` and a command `output_loglevel: quiet`, so Salt prints neither the diff nor the command's output | a pillar root only the user can read, a pillar file and the top file that applies it, passed as `--pillar-root <dir>` to `salt-call` and named as `pillar_roots` in the `salt-ssh` configuration, and removed when the run ends |
| `chef` | `node['meridian_secret_db_password']`, with `sensitive true` on the resource, so Chef prints neither its diff nor its content | on this machine: an attributes file only the user can read, passed as `--json-attributes <path>` to `chef-client`, with a `client.rb` beside it that keeps the node object local mode writes back (attributes and all) in the same private directory, and removed when the run ends. Through `chef-run`, which takes attributes on its command line only, the apply stops and says so |
| any other target (`cloud-init`, `dsc`, the containers and the infrastructure targets) | the stage stops, and says which targets take a secret | |

Only a bare secret is handed over under its own name. A value made from a
secret and anything else is a secret of its own, since the whole of it is
protected: `"password=${{ secret.db_password }}\n"` is one secret, named
`derived_1`, `derived_2` and so on in the order the document's resources use
them. The tool receives the whole text under that name.

Three things hold wherever a secret is handed over:

- **The value is in no file Meridian keeps.** Not in the compiled artifact,
  not in the state directory, not in the plan, not in an event. The one file
  that holds it is the tool's private file, which exists for the length of the
  run.
- **Never on the command line, never in the environment.** The command line
  names the file's path and nothing more, and the tool's environment holds
  nothing of the value.
- **The tool's own state is the tool's own.** A tool that keeps what it was
  given keeps it the way it always has: Terraform writes a resource's
  arguments into its state file, whichever variable they came from. A document
  that hands Terraform a secret should declare a `backend` that protects its
  state.

`compile` writes the same reference, so what it produces is what `plan` and
`apply` hand the tool, and a review of the compiled files sees exactly where
each secret goes. For a target that takes no secret, `compile` writes the
secret's name instead, as `password=(secret db_password)`, and `plan` and
`apply` stop at the stage:

```
resource "app_env": params.content holds a secret, and target "cloud-init" does not take one: it has no private channel to hand a secret to its tool, so the value would be written into a file or dropped. A secret can still decide a when for it; targets that do: ansible, chef, openvox, opentofu, puppet, salt, terraform
```

A rollback hands the tool the document's secrets again, fetched at the
stage's turn: the artifact being put back refers to them by name.

## A secret in a credential

A [credential](credentials.md) is an identity: a user and a key, a user and
a password, a token. Its secret fields are declared secrets, each read as
exactly `${{ secret.<name> }}`, and they follow the same route to the tool
as a secret in a param, by the tool's own channel for an identity: Ansible
takes a key as `--private-key <private file>` and a password in the
extra-vars file. A literal in a secret field is refused when the document is
validated. See [Credentials](credentials.md).

## When a secret is fetched

- **Only `plan` and `apply` fetch.** `validate` and `compile` never read a
  provider, so they can run on a machine that has no access to the secrets.
- **Just before the stage that declares it.** A workflow of several documents
  fetches each document's secrets as its stage starts, not all of them up
  front.
- **Wiped when the stage ends.** The memory is overwritten as soon as the
  stage that declared the secret has finished, whether it succeeded or failed,
  not at the end of the run.
- **Not for a document that is left out.** A document whose `spec.when` is
  false fetches nothing, as long as that `when` does not itself read a secret.
  Neither does a stage that is not run because an earlier one failed.
- **Once, even with a policy.** `apply --policy` looks at every stage before
  it applies the first, and that first pass fetches nothing. A document that
  reads secrets is put to the [policy](../policy.md#when-a-policy-is-asked)
  at its own turn instead.
- **Not for a document that is only imported from.** A document compiled
  because another [imports](outputs-and-hand-offs.md#imports) a file from it is read
  as `compile` reads it: nothing is fetched, and a secret in its params is
  written as its name.
- **`drift` fetches as `plan` does**, because it plans each stage. `rollback`
  fetches nothing: it puts back artifacts that were kept.
- **Never cached.** Nothing is written to save a second fetch. The next run
  fetches again.
- **Locked in memory, or the run says so.** A secret is kept out of swap.
  Where the machine's limit on locked memory does not allow it, the run warns
  once, naming the secrets; with
  [`--strict-secrets`](../cli.md#secrets) the stage stops instead.

## Storing a secret

[`meridian secret set <name> <files>...`](../cli.md#secret-set) puts a value
in the first place the secret names that Meridian can write and that this
machine has: the kernel keyring (`from: keyring`) or the Secret Service
(`from: secret-service`). The value is typed at a prompt that does not show
it, or piped in; it is never an argument, since arguments are in the process
list for every user to read. A place written with its own tool (`env`,
`file`, `exec`, `systemd-creds`, `keychain`) is passed over, and a secret
that names no writable place is refused with every place it does name.

```
$ meridian secret set deploy_token site.yaml
Value for deploy_token (not shown):
stored deploy_token in the kernel keyring, key meridian:deploy-token
```

## Checking before a run

[`meridian secret check`](../cli.md#secret-check) lists every secret the
documents declare, where each comes from, and whether that place would answer.
It reads no value and runs no command, so it is safe in a pipeline step that
only wants to know the run is set up:

```
$ meridian secret check site.yaml
site (site.yaml)
  db_password   env    ok: the environment variable SITE_DB_PASSWORD is set
  deploy_token  file   absent, optional: the file /run/secrets/deploy-token does not exist
```

A secret with several places is reported by the first place that would
answer, or, when none would, with every place's reason. A store this machine
does not have is `UNAVAILABLE`.

## What each command does

| Command | A `when` that reads a secret | A secret in `params` |
|---|---|---|
| `validate` | checked: the name is declared, the expression is one a secret allows where it can be known | checked the same way |
| `compile` | refused: `(secret db_password) is not fetched here: a secret is fetched by plan and apply, never by validate or compile` | written as its name: `password=(secret db_password)` |
| `secret check` | not read | not read: only where each declared secret comes from is looked at |
| `plan`, `apply` | decided. The report shows the condition as the document wrote it, never the value | [handed to the tool](#handing-a-secret-to-a-tool) by a private channel, for a target that takes one; for any other, the stage stops: `resource "conf": params.content holds a secret, and target "cloud-init" does not take one: ...` |

What `compile --out` writes for a secret depends on the target. For one that
takes secrets it is the reference the tool will resolve, `{{
meridian_secret_db_password }}` for Ansible, so the compiled files are what
the tool is run with. For any other target it is a placeholder for reading,
not something a tool can run: `password=(secret db_password)`. A secret that
went through a function is written with the function around it:
`postgres://app:urlEncode((secret db_password))@db/app`.

## Errors

No error holds any part of a secret's value, or of what a command printed, or
a command's arguments.

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
| A secret in `params` of a target that takes none | `plan`, `apply` | `resource "conf": params.content holds a secret, and target "cloud-init" does not take one: it has no private channel to hand a secret to its tool, so the value would be written into a file or dropped. A secret can still decide a when for it; targets that do: ansible, chef, openvox, opentofu, puppet, salt, terraform` |
| A secret in `params` of a `chef` document for another machine | `apply` | `chef-run has no private way to hand a secret to web01: it takes attributes on its command line only, where every user of this machine could read them. Run chef-client on the host, with the local transport, or keep the secret out of this document` |
| A secret inside a list or a map, on `puppet` or `openvox` | `compile` | `param "groups" holds a secret inside a list or a map; on this target a secret is a whole param, looked up as one Sensitive value [unsupported-secret-place]` |
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

All of them are held in place by tests: the first is planned and applied for
each row of the table above and everything the runs print and leave behind is
searched for the secrets; the second is compiled with a password in the
environment that must not appear anywhere; the third is run on four machines
that keep the token in different places, or nowhere, and the token is found
in nothing they print or write; the fourth is compiled with the real Ansible
plugin, and the handover itself is proved against a real `ansible-playbook`
and against the test target, whose tool keeps a copy of what it was handed so
the test can read it back.
